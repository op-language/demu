//! The session worker: one emulator session on one thread.
//!
//! The worker boots its [`MachineService`] inside the thread, mirrors
//! the CLI bootstrap on submitted data, and serves an ordered queue of
//! command closures until every queue sender is gone.

use std::io::{BufWriter, Write};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{SystemTime, UNIX_EPOCH};

use demu_shell::service::{CommandError, MachineService};
use serde_json::Value;

use super::confinement::{ConfinementDir, ConfinementError};

/// The maximum admitted length of a session name, in characters.
const MAX_NAME_LENGTH: usize = 64;

/// A validated session name.
///
/// The name doubles as a path component under the session data root,
/// so it admits only ASCII letters, digits, hyphen, and underscore, in
/// one to 64 characters.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionName(String);

/// A session name validation failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SessionNameError {
    /// The name carries a character outside the admitted set.
    #[error("session name {raw:?} must contain only ASCII letters, digits, hyphen, and underscore")]
    Character {
        /// The rejected raw text.
        raw: String,
    },
    /// The name is empty or longer than the maximum.
    #[error("session name must be 1 to {max} characters, got {length}")]
    Length {
        /// The rejected length.
        length: usize,
        /// The maximum admitted length.
        max: usize,
    },
}

impl SessionName {
    /// The one canonical constructor: validate the raw text and build
    /// the name.
    pub fn new(raw: &str) -> Result<Self, SessionNameError> {
        let length = raw.chars().count();
        if !(1..=MAX_NAME_LENGTH).contains(&length) {
            return Err(SessionNameError::Length {
                length,
                max: MAX_NAME_LENGTH,
            });
        }
        if !raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(SessionNameError::Character {
                raw: raw.to_owned(),
            });
        }
        Ok(Self(raw.to_owned()))
    }

    /// The name as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SessionName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for SessionName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for SessionName {
    type Error = SessionNameError;

    fn try_from(raw: String) -> Result<Self, Self::Error> {
        Self::new(&raw)
    }
}

/// The configuration source of one session.
#[derive(Debug, Clone)]
pub enum ConfigSource {
    /// Submitted TOML configuration text.
    Toml(String),
    /// An installed or bundled configuration triple.
    Triple(String),
}

/// The requested shape of one emulator session.
#[derive(Debug, Clone)]
pub struct SessionSpec {
    /// The validated session name.
    pub name: SessionName,
    /// The configuration source.
    pub config: ConfigSource,
    /// The cartridge ROM image, sent over the wire.
    pub rom: Option<Vec<u8>>,
    /// The symbol table text, parsed over the wire.
    pub symbols_text: Option<String>,
    /// The output directory for the session's files.
    pub output_dir: std::path::PathBuf,
}

/// The result of one successful session creation.
#[derive(Debug, Clone)]
pub struct SessionCreated {
    /// The session name.
    pub name: SessionName,
    /// The canonical root of the session output directory.
    pub output_dir: std::path::PathBuf,
    /// The configuration lint warnings.
    pub warnings: Vec<demu_config::LintWarning>,
}

/// A session management failure.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SessionError {
    /// A session with the same name exists already.
    #[error("session {0} already exists")]
    Duplicate(SessionName),

    /// The session limit is reached.
    #[error("the session limit of {limit} is reached")]
    LimitReached {
        /// The configured session limit.
        limit: usize,
    },

    /// The configuration text does not parse or resolve.
    #[error(transparent)]
    Config(#[from] demu_config::ConfigError),

    /// The configuration does not validate.
    #[error("configuration validation failed")]
    Validation(demu_config::ValidationReport),

    /// The output directory could not be created or anchored.
    #[error(transparent)]
    OutputDir(#[from] ConfinementError),

    /// The machine failed to assemble or boot.
    #[error(transparent)]
    Machine(#[from] demu_core::machine::MachineError),

    /// The configuration declares no cartridge device for the ROM bytes.
    #[error("the configuration declares no cartridge device")]
    NoCartridge,

    /// The symbol table text does not parse.
    #[error(transparent)]
    Symbols(#[from] demu_symbols::SymbolsError),

    /// No session runs under that name.
    #[error("no session named {0}")]
    Unknown(SessionName),

    /// The session worker died before or while serving a command.
    #[error("the session worker failed: {0}")]
    Worker(String),

    /// The daemon engine hit an internal fault.
    #[error("session engine failure: {0}")]
    Internal(String),

    /// A command executed on the machine failed.
    #[error(transparent)]
    Command(#[from] demu_shell::service::CommandError),
}

/// One queued command for a session worker.
///
/// The closure runs on the worker thread against the session's machine
/// service and its confinement directory, and replies once over the
/// paired one-shot channel. One closure transports one command across
/// the thread boundary; the engine itself keeps plain data types.
pub type SessionJob =
    Box<dyn FnOnce(&mut MachineService, &ConfinementDir) -> Result<Value, CommandError> + Send>;

/// One MCP-driven tool call, recorded around its execution in the
/// session command log.
#[derive(Debug, Clone)]
pub(crate) struct ToolCall {
    /// The MCP tool name, as the client names it, such as `run` or
    /// `command`.
    pub(crate) tool: String,
    /// The key arguments of the call, without the session name.
    pub(crate) args: Value,
}

impl ToolCall {
    /// Build one call descriptor from its tool name and arguments.
    pub(crate) fn new(tool: impl Into<String>, args: Value) -> Self {
        Self {
            tool: tool.into(),
            args,
        }
    }
}

/// The per-session command log: `<output-dir>/session.jsonl`.
///
/// One JSON object per MCP-driven command (direction `request`) and
/// one per response (direction `response`). The file opens with the
/// first entry, in create-plus-append mode, and entries buffer through
/// a [`BufWriter`] with an explicit flush per entry. One write failure
/// disables the log for the session's lifetime and records one daemon
/// event on stderr; a log failure never fails a command. The log grows
/// append-only, so a reused session name keeps the earlier eras'
/// entries and a reader sees an era boundary where the tick starts
/// again.
pub(crate) struct SessionLog {
    /// The log file path, built from the session's confinement root.
    path: std::path::PathBuf,
    /// The buffered writer, opened with the first entry.
    writer: Option<BufWriter<std::fs::File>>,
    /// Set after the first write failure; the log then stays disabled.
    failed: bool,
}

impl SessionLog {
    /// A fresh log for the session output directory; nothing touches
    /// the file system until the first entry.
    pub(crate) fn new(confined: &ConfinementDir) -> Self {
        Self {
            path: confined.root().join("session.jsonl"),
            writer: None,
            failed: false,
        }
    }

    /// Record the request entry for one MCP-driven call: the `request`
    /// direction, the tool name, the key arguments, and the wall clock.
    pub(crate) fn record_request(&mut self, call: &ToolCall) {
        self.write_entry(serde_json::json!({
            "direction": "request",
            "tool": call.tool,
            "args": call.args,
            "wall_ms": wall_ms(),
        }));
    }

    /// Record the response entry for one MCP-driven call: the
    /// `response` direction, the success flag, the protocol error code
    /// string on failure, and the post-execution machine tick.
    pub(crate) fn record_response(
        &mut self,
        call: &ToolCall,
        ok: bool,
        error: Option<&str>,
        tick: u64,
    ) {
        let mut entry = serde_json::json!({
            "direction": "response",
            "tool": call.tool,
            "ok": ok,
            "tick": tick,
            "wall_ms": wall_ms(),
        });
        if let Some(code) = error {
            entry["error"] = serde_json::Value::String(code.to_owned());
        }
        self.write_entry(entry);
    }

    /// Write one entry. The first entry opens the file in
    /// create-plus-append mode; any open, write, or flush failure
    /// disables the log and the session keeps serving commands.
    fn write_entry(&mut self, entry: Value) {
        if self.failed {
            return;
        }
        if self.writer.is_none() {
            match std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)
            {
                Ok(file) => self.writer = Some(BufWriter::new(file)),
                Err(error) => {
                    self.disable(&error);
                    return;
                }
            }
        }
        let Some(writer) = self.writer.as_mut() else {
            return;
        };
        let line = entry.to_string();
        let written = writeln!(writer, "{line}").and_then(|()| writer.flush());
        if let Err(error) = written {
            self.disable(&error);
        }
    }

    /// Disable the log after one write failure: drop the writer,
    /// report one daemon event, and stay inert for the session's
    /// lifetime.
    fn disable(&mut self, error: &std::io::Error) {
        self.writer = None;
        self.failed = true;
        super::log::emit(
            "session log disabled",
            &format!("{}: {error}", self.path.display()),
        );
    }
}

/// Milliseconds since the Unix epoch.
///
/// The wall clock is log metadata only; it never touches machine state.
fn wall_ms() -> u64 {
    let since_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    u64::try_from(since_epoch.as_millis()).unwrap_or(u64::MAX)
}

/// One queued job with its one-shot reply channel.
pub(crate) struct WorkerJob {
    /// The command closure.
    pub(crate) job: SessionJob,
    /// The reply channel; the worker sends exactly one result.
    pub(crate) reply: mpsc::Sender<Result<Value, CommandError>>,
    /// The MCP-driven call descriptor, when the caller logs the job.
    pub(crate) logged: Option<ToolCall>,
}

/// The boot instructions for one worker thread.
pub(crate) struct WorkerInstruction {
    /// The validated configuration.
    config: demu_config::SystemConfig,
    /// The configuration text tagged into media files.
    source_text: String,
    /// The over-the-wire cartridge image, if any.
    rom: Option<Vec<u8>>,
    /// The over-the-wire symbol table text, if any.
    symbols_text: Option<String>,
    /// The confinement root of the session output directory.
    confined: ConfinementDir,
}

impl WorkerInstruction {
    /// Assemble the boot instructions of one session worker.
    pub(crate) fn new(
        config: demu_config::SystemConfig,
        source_text: String,
        rom: Option<Vec<u8>>,
        symbols_text: Option<String>,
        confined: ConfinementDir,
    ) -> Self {
        Self {
            config,
            source_text,
            rom,
            symbols_text,
            confined,
        }
    }
}

/// Resolve a session configuration source to its raw form and its
/// text.
///
/// A submitted TOML text parses as-is and applies verbatim as the
/// session's media tag; a bundled triple resolves to the bundled text
/// form.
pub(crate) fn resolve_source(
    source: &ConfigSource,
) -> Result<(demu_config::RawConfig, String), SessionError> {
    match source {
        ConfigSource::Toml(text) => Ok((demu_config::RawConfig::parse(text)?, text.clone())),
        ConfigSource::Triple(triple) => {
            let raw = demu_config::bundled::resolve_source(triple)?;
            let text = raw.to_toml_text()?;
            Ok((raw, text))
        }
    }
}

/// Spawn one session worker thread.
///
/// The worker assembles and boots its machine inside the thread, and
/// reports the boot result once over `boot_reply`. The thread exits
/// when the boot fails or when every queue sender is gone.
pub(crate) fn spawn_worker(
    instruction: WorkerInstruction,
    boot_reply: mpsc::Sender<Result<(), SessionError>>,
    queue: mpsc::Receiver<WorkerJob>,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("demu-session".to_owned())
        .spawn(move || run_worker(instruction, boot_reply, queue))
}

/// The worker loop: bootstrap, then serve queued jobs until every
/// queue sender is gone.
fn run_worker(
    instruction: WorkerInstruction,
    boot_reply: mpsc::Sender<Result<(), SessionError>>,
    queue: mpsc::Receiver<WorkerJob>,
) {
    let mut service = MachineService::new();
    let boot = bootstrap(&mut service, &instruction);
    let booted = boot.is_ok();
    if boot_reply.send(boot).is_err() || !booted {
        // A failed boot, or a vanished creator, registers nothing.
        return;
    }
    let mut engine_log = SessionLog::new(&instruction.confined);
    while let Ok(WorkerJob { job, reply, logged }) = queue.recv() {
        if let Some(call) = &logged {
            engine_log.record_request(call);
        }
        let result = job(&mut service, &instruction.confined);
        let tick = service.machine().map(|m| m.now().0).unwrap_or(0);
        if let Some(call) = logged {
            // A job result may itself carry the demu-shell protocol
            // response (the passthrough tool), whose `ok` boolean is
            // the truthful command outcome and whose `error` object
            // carries the protocol code; a result without an `ok` field
            // is a plain tool result and counts as ok. A failed job
            // reports through the command error channel instead.
            let (ok, error) = match &result {
                Ok(value) => {
                    let ok = value.get("ok").and_then(Value::as_bool).unwrap_or(true);
                    let error = if ok {
                        None
                    } else {
                        value
                            .get("error")
                            .and_then(|field| field.get("code"))
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    };
                    (ok, error)
                }
                Err(error) => (false, Some(error.error_code().as_str().to_owned())),
            };
            engine_log.record_response(&call, ok, error.as_deref(), tick);
        }
        // A gone caller leaves nobody to reply to; the worker keeps
        // serving the session.
        let _ = reply.send(result);
    }
}

/// Build the session machine inside the worker thread.
///
/// The flow mirrors the CLI session bootstrap: the configuration text,
/// then the machine factory for the media commands, then the machine
/// with any over-the-wire cartridge image, then the over-the-wire
/// symbol table.
fn bootstrap(
    service: &mut MachineService,
    instruction: &WorkerInstruction,
) -> Result<(), SessionError> {
    service.set_config_source(instruction.source_text.clone());
    service.set_machine_factory(Arc::new(|config| {
        let mut machine =
            demu_core::Machine::assemble(config.clone(), &crate::assemble::demu_ctors())?;
        machine.boot();
        crate::assemble::apply_nes_cartridge_wiring(&mut machine);
        Ok(machine)
    }));
    let mut machine =
        demu_core::Machine::assemble(instruction.config.clone(), &crate::assemble::demu_ctors())?;
    if let Some(bytes) = &instruction.rom {
        // A ROM payload overrides the cartridge boot state before the
        // boot applies every boot entry.
        let cart = machine
            .config
            .devices
            .iter()
            .find(|d| crate::assemble::is_cart_kind(d.kind.as_str()))
            .map(|d| d.name.to_string())
            .ok_or(SessionError::NoCartridge)?;
        machine.override_boot(
            &cart,
            demu_config::model::BootState::Hex {
                bytes: bytes.clone(),
            },
        )?;
    }
    machine.boot();
    crate::assemble::apply_nes_cartridge_wiring(&mut machine);
    service.load_machine(machine);
    if let Some(text) = &instruction.symbols_text {
        let table = demu_symbols::SymbolTable::parse(text)?;
        service.load_symbols(&table);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_accept_the_documented_charset() {
        for raw in ["a", "gb-dev_01", "A9_zZ"] {
            let name = SessionName::new(raw).expect("the name validates");
            assert_eq!(name.as_str(), raw);
            assert_eq!(name.to_string(), raw);
        }
        SessionName::new(&"x".repeat(64)).expect("64 characters pass");
        let name = SessionName::try_from("moved-name".to_owned()).expect("try_from delegates");
        assert_eq!(name.as_str(), "moved-name");
    }

    #[test]
    fn names_reject_bad_charset_and_length() {
        assert!(matches!(
            SessionName::new(""),
            Err(SessionNameError::Length { .. })
        ));
        assert!(matches!(
            SessionName::new(&"x".repeat(65)),
            Err(SessionNameError::Length { .. })
        ));
        assert!(matches!(
            SessionName::new("has space"),
            Err(SessionNameError::Character { .. })
        ));
        assert!(matches!(
            SessionName::new("sláinte"),
            Err(SessionNameError::Character { .. })
        ));
        assert!(matches!(
            SessionName::new("../etc"),
            Err(SessionNameError::Character { .. })
        ));
    }

    /// A temp directory cleared of any prior test artifact.
    fn temp_dir(case: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("demu-daemon-session-{case}"));
        std::fs::remove_dir_all(&path).ok();
        path
    }

    /// The JSON objects of one written log, one per line.
    fn log_entries(path: &std::path::Path) -> Vec<serde_json::Value> {
        let text = std::fs::read_to_string(path).expect("the log file reads back");
        text.lines()
            .map(|line| serde_json::from_str(line).expect("each log line is JSON"))
            .collect()
    }

    #[test]
    fn the_session_log_records_request_and_response_pairs() {
        let dir = temp_dir("log-pairs");
        let confined = ConfinementDir::create(&dir).expect("the session dir creates");
        let mut log = SessionLog::new(&confined);
        let call = ToolCall::new("read_memory", serde_json::json!({"addr": "0x8000"}));
        log.record_request(&call);
        log.record_response(&call, true, None, 60);
        log.record_response(&call, false, Some("bad_address"), 60);
        drop(log);
        let entries = log_entries(&confined.root().join("session.jsonl"));
        assert_eq!(entries.len(), 3, "one request plus two responses");
        assert_eq!(entries[0]["direction"], "request");
        assert_eq!(entries[0]["tool"], "read_memory");
        assert_eq!(entries[0]["args"], serde_json::json!({"addr": "0x8000"}));
        assert!(
            entries[0]["wall_ms"].as_u64().is_some(),
            "the request logs a wall-clock field"
        );
        assert_eq!(entries[1]["direction"], "response");
        assert_eq!(entries[1]["tool"], "read_memory");
        assert_eq!(entries[1]["ok"], true);
        assert_eq!(entries[1]["tick"], 60);
        assert!(
            entries[1].get("error").is_none(),
            "the ok entry carries no error key"
        );
        assert_eq!(entries[2]["ok"], false);
        assert_eq!(entries[2]["error"], "bad_address");
        assert_eq!(entries[2]["tick"], 60);
    }

    #[test]
    fn a_reused_session_log_appends_past_its_earlier_era() {
        let dir = temp_dir("log-append");
        let confined = ConfinementDir::create(&dir).expect("the session dir creates");
        let call = ToolCall::new("run", serde_json::json!({"units": 50}));
        let mut first = SessionLog::new(&confined);
        first.record_request(&call);
        first.record_response(&call, true, None, 10);
        drop(first);
        let log_path = confined.root().join("session.jsonl");
        let first_era = log_entries(&log_path);
        assert_eq!(first_era.len(), 2);
        let mut second = SessionLog::new(&confined);
        second.record_request(&call);
        second.record_response(&call, true, None, 20);
        drop(second);
        let whole_era = log_entries(&log_path);
        assert!(
            whole_era.len() > first_era.len(),
            "the second era appends after the first"
        );
        assert_eq!(
            whole_era.first(),
            first_era.first(),
            "the earlier era's first entry survives"
        );
        assert_eq!(
            whole_era[3]["tick"], 20,
            "the era boundary shows the tick restarting"
        );
    }

    #[test]
    fn a_log_failure_disables_the_log_and_never_fails_a_command() {
        let dir = temp_dir("log-failure");
        let confined = ConfinementDir::create(&dir).expect("the session dir creates");
        // A directory in the log's place: every open fails.
        std::fs::create_dir(confined.root().join("session.jsonl"))
            .expect("the log blocker creates");
        let mut log = SessionLog::new(&confined);
        let call = ToolCall::new("read_memory", serde_json::json!({}));
        log.record_request(&call);
        log.record_response(&call, true, None, 0);
        assert!(
            confined.root().join("session.jsonl").is_dir()
                && !confined.root().join("session.jsonl").is_file(),
            "the failing log writes nothing"
        );
        // Later records stay inert too: nothing panics, nothing changes.
        log.record_request(&call);
        assert!(confined.root().join("session.jsonl").is_dir());
    }
}
