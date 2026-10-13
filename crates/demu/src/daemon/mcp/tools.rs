//! The MCP session tools and the passthrough command of the daemon.
//!
//! These tools are the entire MCP tool surface: session management,
//! lookup, and one passthrough that carries the demu-shell command set
//! over the session worker queue.

use std::path::PathBuf;
use std::sync::Arc;

use demu_shell::service::CommandError;
use rmcp::{
    ErrorData,
    handler::server::wrapper::Parameters,
    model::{CallToolResult, ContentBlock},
    schemars, tool, tool_router,
};
use serde::Deserialize;
use serde_json::Value;

use crate::daemon::{
    confinement::ConfinementDir,
    log,
    registry::Registry,
    session::{ConfigSource, SessionError, SessionName, SessionSpec},
};

use super::DemuMcp;

/// The maximum admitted decoded ROM size, in bytes.
const MAX_ROM_BYTES: usize = 64 * 1024 * 1024;

impl DemuMcp {
    /// The session tool server over one registry.
    ///
    /// The factory of the MCP transport calls this per MCP connection;
    /// the registry and data root stay shared, so sessions live
    /// daemon-wide and survive client disconnects.
    pub(crate) fn new(registry: Arc<Registry>, data_root: PathBuf) -> Self {
        Self {
            registry,
            data_root,
            tool_router: Self::tool_router(),
        }
    }
}

impl From<SessionError> for ErrorData {
    fn from(error: SessionError) -> Self {
        // Worker and engine faults are daemon-side; every other
        // rejection names a client fix, and a validation failure
        // carries its report lines.
        let message = match &error {
            SessionError::Validation(report) => {
                let detail = report
                    .errors
                    .iter()
                    .map(|issue| issue.to_string())
                    .collect::<Vec<_>>()
                    .join("; ");
                if detail.is_empty() {
                    error.to_string()
                } else {
                    format!("configuration validation failed: {detail}")
                }
            }
            _ => error.to_string(),
        };
        match error {
            // Worker and engine faults are daemon-side; every other
            // rejection names a client fix.
            SessionError::Worker(_) | SessionError::Internal(_) => {
                ErrorData::internal_error(message, None)
            }
            _ => ErrorData::invalid_params(message, None),
        }
    }
}

/// The arguments of `session_create`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct SessionCreateArgs {
    /// The session name: 1 to 64 characters of ASCII letters, digits,
    /// hyphen, and underscore. The name doubles as a path component
    /// under the data root.
    pub(crate) name: String,
    /// The TOML system-configuration text. Supply exactly one of
    /// `config_toml` and `config_triple`.
    #[serde(default)]
    pub(crate) config_toml: Option<String>,
    /// A bundled configuration triple such as `sm83-nintendo-gameboy`.
    /// Supply exactly one of `config_toml` and `config_triple`.
    #[serde(default)]
    pub(crate) config_triple: Option<String>,
    /// The cartridge ROM image, base64 encoded. The decoded payload
    /// is at most 64 MiB.
    #[serde(default)]
    pub(crate) rom_base64: Option<String>,
    /// The `.opl` symbol-table JSON text (version envelope 1), loaded
    /// after the machine boots.
    #[serde(default)]
    pub(crate) symbols_text: Option<String>,
    /// The session output directory override. The default is
    /// `<data-root>/<name>`. Command paths confine to this directory,
    /// so it must not contain whitespace.
    #[serde(default)]
    pub(crate) output_dir: Option<String>,
}

/// The arguments of `session_info` and `session_destroy`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct SessionNameArgs {
    /// The session name.
    pub(crate) name: String,
}

/// The arguments of the passthrough `command`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct CommandArgs {
    /// The session name.
    pub(crate) name: String,
    /// The demu-shell command word, for example `read`.
    pub(crate) cmd: String,
    /// The command arguments, one whitespace-free token each. The
    /// shared shell dispatcher re-splits on whitespace, so a token
    /// cannot contain spaces.
    #[serde(default)]
    pub(crate) args: Vec<String>,
}

/// The path-slot plan of one file-touching shell command.
///
/// The positions follow the shared shell dispatcher
/// (`demu_shell::text::dispatch`); no other command arm touches the
/// filesystem.
enum PathSlot {
    /// The token at this word position is a path.
    Word(usize),
    /// The first non-flag token is a path (`screenshot`).
    Screenshot,
    /// The token at this position and the value after `-o` are paths.
    WordAndOutput(usize),
    /// Only the value after `-o` is a path.
    OutputOnly,
}

fn path_slot(cmd: &str) -> Option<PathSlot> {
    match cmd {
        "dump" => Some(PathSlot::Word(2)),
        "dump-vram" | "dump-nametable" | "dump-oam" | "dump-palette" | "dump-framebuffer"
        | "save-state" | "load-state" | "script" | "symbols" => Some(PathSlot::Word(0)),
        "screenshot" => Some(PathSlot::Screenshot),
        "mkv-input" | "mkv-render" => Some(PathSlot::WordAndOutput(0)),
        "render-session" => Some(PathSlot::OutputOnly),
        _ => None,
    }
}

/// Confine one user-supplied path token under the session directory.
fn confine_path(confined: &ConfinementDir, token: &str) -> Result<String, CommandError> {
    confined
        .join_confined(token)
        .map(|path| path.to_string_lossy().into_owned())
        .map_err(|error| CommandError::BadArgs(error.to_string()))
}

/// Confine the word-token path at `index`, when the token is present.
///
/// A missing token keeps the shell dispatcher's own usage error.
fn confine_word(
    args: &mut [String],
    index: usize,
    confined: &ConfinementDir,
) -> Result<(), CommandError> {
    let Some(token) = args.get(index).cloned() else {
        return Ok(());
    };
    args[index] = confine_path(confined, &token)?;
    Ok(())
}

/// Confine the value token of the first `-o` flag, when present.
fn confine_output_value(
    args: &mut [String],
    confined: &ConfinementDir,
) -> Result<(), CommandError> {
    let Some(index) = args.iter().position(|token| token == "-o") else {
        return Ok(());
    };
    let Some(token) = args.get(index + 1).cloned() else {
        return Ok(());
    };
    args[index + 1] = confine_path(confined, &token)?;
    Ok(())
}

/// Confine the path tokens of one shell command into a session
/// directory.
///
/// Every token must be whitespace-free: the shared shell dispatcher
/// re-splits the joined command text on whitespace, so a stray space
/// would repartition the command. Each path token becomes its confined
/// absolute form under the session directory; the `dump-*` layer
/// suffixes append after confinement inside the shell service. Escape
/// attempts fail with the shell's bad-args error.
fn confine_command_args(
    cmd: &str,
    args: &[String],
    confined: &ConfinementDir,
) -> Result<Vec<String>, CommandError> {
    for token in args {
        if token.chars().any(char::is_whitespace) {
            return Err(CommandError::BadArgs(format!(
                "command arguments must not contain whitespace: {token:?}"
            )));
        }
    }
    let Some(slot) = path_slot(cmd) else {
        return Ok(args.to_vec());
    };
    let mut confined_args = args.to_vec();
    match slot {
        PathSlot::Word(index) => confine_word(&mut confined_args, index, confined)?,
        PathSlot::WordAndOutput(index) => {
            confine_word(&mut confined_args, index, confined)?;
            confine_output_value(&mut confined_args, confined)?;
        }
        PathSlot::OutputOnly => confine_output_value(&mut confined_args, confined)?,
        PathSlot::Screenshot => {
            let is_flag = |token: &str| token == "--png" || token == "--ppm";
            if let Some(index) = confined_args.iter().position(|token| !is_flag(token)) {
                confine_word(&mut confined_args, index, confined)?;
            }
        }
    }
    Ok(confined_args)
}

/// Decode a user-supplied base64 ROM image and enforce the size cap.
fn decode_rom(text: &str) -> Result<Vec<u8>, ErrorData> {
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD_PAD_INDIFFERENT
        .decode(text.trim())
        .map_err(|error| {
            ErrorData::invalid_params(format!("rom_base64 is not valid base64: {error}"), None)
        })?;
    if bytes.len() > MAX_ROM_BYTES {
        return Err(ErrorData::invalid_params(
            format!(
                "the decoded ROM is {} bytes, over the {} byte cap",
                bytes.len(),
                MAX_ROM_BYTES
            ),
            None,
        ));
    }
    Ok(bytes)
}

/// Build one session spec from validated tool arguments.
fn build_spec(
    args: SessionCreateArgs,
    data_root: &std::path::Path,
) -> Result<SessionSpec, ErrorData> {
    let name = session_name(&args.name)?;
    let config = match (args.config_toml, args.config_triple) {
        (Some(_), Some(_)) | (None, None) => {
            return Err(ErrorData::invalid_params(
                "supply exactly one of config_toml and config_triple",
                None,
            ));
        }
        (Some(text), None) => ConfigSource::Toml(text),
        (None, Some(triple)) => ConfigSource::Triple(triple),
    };
    let rom = match args.rom_base64 {
        Some(text) => Some(decode_rom(&text)?),
        None => None,
    };
    let output_dir = match args.output_dir {
        Some(dir) => PathBuf::from(dir),
        None => data_root.join(name.as_str()),
    };
    if output_dir
        .as_os_str()
        .to_string_lossy()
        .chars()
        .any(char::is_whitespace)
    {
        return Err(ErrorData::invalid_params(
            "the output directory must not contain whitespace: the shell commands \
             address paths as whitespace-free tokens",
            None,
        ));
    }
    Ok(SessionSpec {
        name,
        config,
        rom,
        symbols_text: args.symbols_text,
        output_dir,
    })
}

/// Validate a user-supplied session name.
fn session_name(raw: &str) -> Result<SessionName, ErrorData> {
    SessionName::new(raw).map_err(|error| ErrorData::invalid_params(error.to_string(), None))
}

/// Render one JSON value as the text content of a successful tool call.
fn json_result(value: Value) -> Result<CallToolResult, ErrorData> {
    let text = serde_json::to_string_pretty(&value).map_err(|error| {
        ErrorData::internal_error(
            format!("the tool result failed to serialize: {error}"),
            None,
        )
    })?;
    Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
}

/// Render the configuration lint warnings as JSON.
fn warnings_json(warnings: &[demu_config::LintWarning]) -> Value {
    Value::Array(
        warnings
            .iter()
            .map(|warning| serde_json::json!({"code": warning.code, "message": warning.message}))
            .collect(),
    )
}

#[tool_router]
impl DemuMcp {
    #[tool(
        description = "Create an emulator session. Supply exactly one of config_toml (TOML configuration text) or config_triple (bundled triple such as sm83-nintendo-gameboy). Optionally supply rom_base64 (cartridge ROM, 64 MiB decoded cap), symbols_text (.opl JSON symbol table), and output_dir (defaults to <data-root>/<name>; must not contain whitespace). Returns {name, output_dir, warnings}."
    )]
    async fn session_create(
        &self,
        Parameters(args): Parameters<SessionCreateArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let spec = build_spec(args, &self.data_root)?;
        let registry = Arc::clone(&self.registry);
        let created = tokio::task::spawn_blocking(move || registry.create(spec))
            .await
            .map_err(|error| {
                ErrorData::internal_error(format!("the session create task failed: {error}"), None)
            })?
            .map_err(ErrorData::from)?;
        log::emit(
            "session created",
            &format!("{} at {}", created.name, created.output_dir.display()),
        );
        json_result(serde_json::json!({
            "name": created.name.as_str(),
            "output_dir": created.output_dir.display().to_string(),
            "warnings": warnings_json(&created.warnings),
        }))
    }

    #[tool(description = "List the live emulator sessions with their output directories.")]
    async fn session_list(&self) -> Result<CallToolResult, ErrorData> {
        let value = Value::Array(
            self.registry
                .list()
                .iter()
                .map(|(name, dir)| {
                    serde_json::json!({"name": name.as_str(), "output_dir": dir.display().to_string()})
                })
                .collect(),
        );
        json_result(value)
    }

    #[tool(
        description = "Get the machine info of one session: {name}. The result is the same object the `info` shell command returns."
    )]
    async fn session_info(
        &self,
        Parameters(args): Parameters<SessionNameArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let name = session_name(&args.name)?;
        let registry = Arc::clone(&self.registry);
        let info = tokio::task::spawn_blocking(move || registry.info(&name))
            .await
            .map_err(|error| {
                ErrorData::internal_error(format!("the session info task failed: {error}"), None)
            })?
            .map_err(ErrorData::from)?;
        json_result(info)
    }

    #[tool(
        description = "Destroy a session: {name}. Destroying twice is a clean error, and the name becomes reusable immediately."
    )]
    async fn session_destroy(
        &self,
        Parameters(args): Parameters<SessionNameArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let name = session_name(&args.name)?;
        let destroyed_name = name.as_str().to_owned();
        let registry = Arc::clone(&self.registry);
        tokio::task::spawn_blocking(move || registry.destroy(&name))
            .await
            .map_err(|error| {
                ErrorData::internal_error(format!("the session destroy task failed: {error}"), None)
            })?
            .map_err(ErrorData::from)?;
        log::emit("session destroyed", &destroyed_name);
        json_result(serde_json::json!({"name": destroyed_name}))
    }

    #[tool(
        description = "Run a demu-shell command on a session: {name, cmd, args}. Each args entry must be one whitespace-free token, e.g. cmd \"read\", args [\"0x8000\"]. Path arguments of file commands (dump, screenshot, save-state, script, and so on) confine to the session output directory; files are read from there without an upload path. The result is the demu-shell protocol object {id, ok, data?, error?}. WARNING: an unbounded run, or a run-until that never fires, blocks this session until it returns; prefer bounded commands."
    )]
    async fn command(
        &self,
        Parameters(args): Parameters<CommandArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let name = session_name(&args.name)?;
        let cmd = args.cmd;
        let tokens = args.args;
        let registry = Arc::clone(&self.registry);
        let response = tokio::task::spawn_blocking(move || {
            registry.submit(
                &name,
                Box::new(move |service, confined| {
                    let confined_args = confine_command_args(&cmd, &tokens, confined)?;
                    let request = demu_shell::protocol::Request {
                        id: Value::Null,
                        cmd: cmd.clone(),
                        args: Value::Array(confined_args.into_iter().map(Value::String).collect()),
                    };
                    let response = demu_shell::json::handle(service, &request);
                    serde_json::to_value(response).map_err(|error| {
                        CommandError::Failed(format!(
                            "the command response failed to serialize: {error}"
                        ))
                    })
                }),
            )
        })
        .await
        .map_err(|error| {
            ErrorData::internal_error(format!("the command task failed: {error}"), None)
        })?
        .map_err(ErrorData::from)?;
        json_result(response)
    }
}
