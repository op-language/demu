//! The named MCP debug tools of the daemon.
//!
//! The debug tools are the ergonomic hot path over the demu command
//! set: run, step, run-until, run-frame, memory read and write,
//! registers, the machine triggers, and the screenshot. Every tool
//! submits its command over the session worker queue as one closure,
//! in the same way the session tools do, so machine work never joins
//! the HTTP runtime. Each call carries a [`ToolCall`] descriptor, so
//! the session command log records the call around its execution.
//!
//! Defaults follow the demu-shell commands: `run` without units
//! continues until a trigger fires, `step` and `run_frame` default to
//! one, and `read_memory` defaults to 16 bytes. Address arguments stay
//! strings, so the shell symbol resolution applies.

use std::sync::Arc;

use demu_shell::service::{CommandError, CommandOutput, MachineService, ShotFormat};
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
    registry::Registry,
    session::{SessionJob, ToolCall},
};

use super::{
    DemuMcp,
    tools::{SessionNameArgs, json_result, session_name},
};
use base64::Engine as _;

/// The default byte count of `read_memory`, as in the shell `read`.
const DEFAULT_READ_BYTES: u32 = 16;

/// The maximum byte count of one `read_memory` request.
const MAX_READ_BYTES: u32 = 65536;

/// The events `run_until` accepts, in the shell event grammar. A
/// rejected event word stops the tool before it queues a command that
/// can only run free.
const RUN_UNTIL_EVENTS: [&str; 5] = ["vblank", "irq", "fetch", "read", "write"];

/// The maximum collision-suffix attempts of one screenshot name before
/// the tool reports the session directory exhausted.
const MAX_SCREENSHOT_TRIES: u32 = 4096;

/// The arguments of `run` and `step`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct UnitsArgs {
    /// The session name.
    pub(crate) name: String,
    /// The units to run: clock units of the primary domain. Omitted in
    /// `run` means a free run until a trigger fires, and in `step`
    /// means one unit.
    #[serde(default)]
    pub(crate) units: Option<u64>,
}

/// The arguments of `run_frame`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct FramesArgs {
    /// The session name.
    pub(crate) name: String,
    /// The display frames to run. Defaults to one.
    #[serde(default)]
    pub(crate) frames: Option<u64>,
}

/// The arguments of `run_until`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct RunUntilArgs {
    /// The session name.
    pub(crate) name: String,
    /// The runtime event: `vblank`, `irq`, or `fetch`, `read`, `write`
    /// with the optional address. The shared shell grammar applies.
    pub(crate) event: String,
    /// The event address, one number token such as `0x8000`. The
    /// address is not symbol-resolved, matching the shell form.
    #[serde(default)]
    pub(crate) addr: Option<String>,
}

/// The arguments of `read_memory`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct ReadMemoryArgs {
    /// The session name.
    pub(crate) name: String,
    /// The address: a symbol name or a number token such as `0xFF40`.
    pub(crate) addr: String,
    /// The byte count. Defaults to 16; the cap is 65536.
    #[serde(default)]
    pub(crate) count: Option<u32>,
}

/// The arguments of `write_memory`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct WriteMemoryArgs {
    /// The session name.
    pub(crate) name: String,
    /// The address: a symbol name or a number token.
    pub(crate) addr: String,
    /// The bytes as hex digit pairs. Internal whitespace is ignored;
    /// an empty or odd-digit text is an argument error.
    pub(crate) bytes_hex: String,
}

/// A register value: a JSON integer, or the shell's number text in
/// decimal or `0x` hexadecimal form.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub(crate) enum RegValue {
    /// A plain JSON integer.
    Number(u64),
    /// A number token, decimal or `0x` hexadecimal.
    Token(String),
}

/// The arguments of `set_reg`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct SetRegArgs {
    /// The session name.
    pub(crate) name: String,
    /// The register name, as the CPU names it, for example `A`.
    pub(crate) reg: String,
    /// The value to set.
    pub(crate) value: RegValue,
}

/// The arguments of `break` and `watch`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct TriggerArgs {
    /// The session name.
    pub(crate) name: String,
    /// The address: a symbol name or a number token.
    pub(crate) addr: String,
}

/// The JSON data form of one command output: the same object the
/// JSON-lines protocol returns for the command.
fn command_json(output: CommandOutput) -> Value {
    match output {
        CommandOutput::Text { json, .. } => json,
    }
}

/// Parse one register value token into the plain number the CPU
/// accepts.
fn reg_value(value: RegValue) -> Result<u64, ErrorData> {
    match value {
        RegValue::Number(value) => Ok(value),
        RegValue::Token(token) => demu_config::model::parse_number(&token).ok_or_else(|| {
            ErrorData::invalid_params(format!("set_reg value is not a number: {token}"), None)
        }),
    }
}

/// Decode hex byte pairs into bytes, in the shell token form.
fn parse_hex_bytes(text: &str) -> Result<Vec<u8>, ErrorData> {
    let tokens: Vec<&str> = text.split_whitespace().collect();
    if tokens.is_empty() {
        return Err(ErrorData::invalid_params(
            "bytes_hex must carry at least one byte".to_owned(),
            None,
        ));
    }
    demu_shell::text::hex_bytes(&tokens).ok_or_else(|| {
        ErrorData::invalid_params(
            "bytes_hex must be an even count of hex digit pairs".to_owned(),
            None,
        )
    })
}

/// Capture one PNG screenshot on the session worker and return its
/// result fields plus the base64 image.
///
/// The name is deterministic in the machine tick:
/// `screenshot-<tick>.png`, with a `-<n>` counter suffix when a same
/// tick has already written its file. The tool calls the service with
/// the PNG format only.
fn screenshot_job(
    service: &mut MachineService,
    confined: &ConfinementDir,
) -> Result<Value, CommandError> {
    let now = service
        .machine()
        .map(|machine| machine.now().0)
        .ok_or(CommandError::NoMachine)?;
    let mut attempt: u32 = 0;
    let path = loop {
        let suffix = if attempt == 0 {
            String::new()
        } else {
            format!("-{attempt}")
        };
        let candidate = confined
            .join_confined(&format!("screenshot-{now}{suffix}.png"))
            .map_err(|error| CommandError::BadArgs(error.to_string()))?;
        if !candidate.exists() {
            break candidate;
        }
        attempt = attempt
            .checked_add(1)
            .filter(|attempt| *attempt <= MAX_SCREENSHOT_TRIES)
            .ok_or_else(|| {
                CommandError::Failed(format!(
                    "every screenshot name of tick {now} is taken: cap {MAX_SCREENSHOT_TRIES}"
                ))
            })?;
    };
    let text = path.to_string_lossy().into_owned();
    let output = service.screenshot(&text, Some(ShotFormat::Png))?;
    let bytes = std::fs::read(&path).map_err(|error| CommandError::Io(error.to_string()))?;
    let size = bytes.len();
    let png = base64::engine::general_purpose::STANDARD.encode(bytes);
    let Value::Object(fields) = command_json(output) else {
        return Err(CommandError::Failed(
            "the screenshot result carries no fields".into(),
        ));
    };
    let field = |name: &str| -> Result<Value, CommandError> {
        fields
            .get(name)
            .cloned()
            .ok_or_else(|| CommandError::Failed(format!("the screenshot data carries no {name}")))
    };
    let path = field("path")?;
    let width = field("width")?;
    let height = field("height")?;
    Ok(serde_json::json!({
        "path": path,
        "width": width,
        "height": height,
        "size": size,
        "png_base64": png,
    }))
}

/// The shared flow of the named debug tools: validate the session
/// name, submit the command job with its log descriptor on a blocking
/// task, and return the command's JSON data.
async fn submit_tool_value(
    registry: Arc<Registry>,
    raw_name: &str,
    call: ToolCall,
    job: SessionJob,
) -> Result<Value, ErrorData> {
    let name = session_name(raw_name)?;
    tokio::task::spawn_blocking(move || registry.submit_logged(&name, call, job))
        .await
        .map_err(|error| {
            ErrorData::internal_error(format!("the command task failed: {error}"), None)
        })?
        .map_err(ErrorData::from)
}

#[tool_router(router = tool_router_debug, vis = "pub(crate)")]
impl DemuMcp {
    #[tool(
        name = "run",
        description = "Run the session's machine for `units` clock units of the primary domain and return {now, start}. Without units, continue until a registered trigger fires and return {now, start, trigger_hit}. WARNING: a free run with no trigger set blocks this session until a trigger hits."
    )]
    async fn run_tool(
        &self,
        Parameters(args): Parameters<UnitsArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let call = ToolCall::new(
            "run",
            match args.units {
                Some(units) => serde_json::json!({"units": units}),
                None => serde_json::json!({}),
            },
        );
        let units = args.units;
        let value = submit_tool_value(
            Arc::clone(&self.registry),
            &args.name,
            call,
            Box::new(move |service, _confined| {
                let output = match units {
                    Some(units) => service.run(units)?,
                    None => service.continue_run()?,
                };
                Ok(command_json(output))
            }),
        )
        .await?;
        json_result(value)
    }

    #[tool(
        name = "run_frame",
        description = "Run `frames` display frames of the session's machine (default 1) and return {now, start, frames} with the [start, end] display-frame counts. A frames value of zero waits for the first completed display frame."
    )]
    async fn run_frame_tool(
        &self,
        Parameters(args): Parameters<FramesArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let frames = args.frames.unwrap_or(1);
        let call = ToolCall::new("run_frame", serde_json::json!({"frames": frames}));
        let value = submit_tool_value(
            Arc::clone(&self.registry),
            &args.name,
            call,
            Box::new(move |service, _confined| Ok(command_json(service.run_frame(frames)?))),
        )
        .await?;
        json_result(value)
    }

    #[tool(
        name = "run_until",
        description = "Run the session's machine until one runtime event fires and return {now, start, event}. The event is `vblank` or `irq`, or one of `fetch`, `read`, `write` with the optional number token `addr`. The `event` field is null when a registered trigger stopped the run instead. WARNING: an event that never fires blocks this session until the machine makes it happen."
    )]
    async fn run_until_tool(
        &self,
        Parameters(args): Parameters<RunUntilArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        if !RUN_UNTIL_EVENTS.contains(&args.event.as_str()) {
            return Err(ErrorData::invalid_params(
                format!(
                    "unknown run_until event {:?}; expected one of the shell event forms: {}",
                    args.event,
                    RUN_UNTIL_EVENTS.join(", ")
                ),
                None,
            ));
        }
        let call = ToolCall::new(
            "run_until",
            match &args.addr {
                Some(addr) => serde_json::json!({"event": &args.event, "addr": addr}),
                None => serde_json::json!({"event": &args.event}),
            },
        );
        let event = args.event;
        let addr = args.addr;
        let value = submit_tool_value(
            Arc::clone(&self.registry),
            &args.name,
            call,
            Box::new(move |service, _confined| {
                let spec = match addr {
                    Some(addr) => format!("{event} {addr}"),
                    None => event,
                };
                Ok(command_json(service.run_until(&spec)?))
            }),
        )
        .await?;
        json_result(value)
    }

    #[tool(
        name = "step",
        description = "Step the session's machine by `units` clock units (default 1) and return {now, start}."
    )]
    async fn step_tool(
        &self,
        Parameters(args): Parameters<UnitsArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let units = args.units.unwrap_or(1);
        let call = ToolCall::new("step", serde_json::json!({"units": units}));
        let value = submit_tool_value(
            Arc::clone(&self.registry),
            &args.name,
            call,
            Box::new(move |service, _confined| Ok(command_json(service.step(units)?))),
        )
        .await?;
        json_result(value)
    }

    #[tool(
        name = "read_memory",
        description = "Read memory of the session's machine at `addr` through the primary bus; `addr` is a symbol name or a number token such as 0xFF40. The optional `count` reads more bytes: 16 by default, capped at 65536. Returns {addr, bytes} as a hex text."
    )]
    async fn read_memory_tool(
        &self,
        Parameters(args): Parameters<ReadMemoryArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let count = args.count.unwrap_or(DEFAULT_READ_BYTES);
        if count > MAX_READ_BYTES {
            return Err(ErrorData::invalid_params(
                format!("read_memory count {count} is over the {MAX_READ_BYTES} byte cap"),
                None,
            ));
        }
        let call = ToolCall::new(
            "read_memory",
            serde_json::json!({"addr": &args.addr, "count": count}),
        );
        let addr = args.addr;
        let value = submit_tool_value(
            Arc::clone(&self.registry),
            &args.name,
            call,
            Box::new(move |service, _confined| Ok(command_json(service.read(&addr, count)?))),
        )
        .await?;
        json_result(value)
    }

    #[tool(
        name = "write_memory",
        description = "Write `bytes_hex` (hex digit pairs; internal whitespace is ignored) at `addr` of the session's machine (a symbol name or number token). Returns {addr, written} with the byte count that landed in a writable window."
    )]
    async fn write_memory_tool(
        &self,
        Parameters(args): Parameters<WriteMemoryArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let bytes = parse_hex_bytes(&args.bytes_hex)?;
        let call = ToolCall::new(
            "write_memory",
            serde_json::json!({"addr": &args.addr, "bytes_hex": &args.bytes_hex}),
        );
        let addr = args.addr;
        let value = submit_tool_value(
            Arc::clone(&self.registry),
            &args.name,
            call,
            Box::new(move |service, _confined| Ok(command_json(service.write(&addr, &bytes)?))),
        )
        .await?;
        json_result(value)
    }

    #[tool(
        name = "regs",
        description = "Read the CPU register values of the session's machine and return the register map object."
    )]
    async fn regs_tool(
        &self,
        Parameters(args): Parameters<SessionNameArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let call = ToolCall::new("regs", serde_json::json!({}));
        let value = submit_tool_value(
            Arc::clone(&self.registry),
            &args.name,
            call,
            Box::new(move |service, _confined| Ok(command_json(service.regs()?))),
        )
        .await?;
        json_result(value)
    }

    #[tool(
        name = "set_reg",
        description = "Set the CPU register `reg` of the session's machine to `value` (a JSON integer, or a number token such as 0xFF) and return the text form, for example \"A = 0x42\". An unknown register name fails cleanly."
    )]
    async fn set_reg_tool(
        &self,
        Parameters(args): Parameters<SetRegArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let value_token = reg_value(args.value)?;
        let call = ToolCall::new(
            "set_reg",
            serde_json::json!({"reg": &args.reg, "value": value_token}),
        );
        let reg = args.reg;
        let value = submit_tool_value(
            Arc::clone(&self.registry),
            &args.name,
            call,
            Box::new(move |service, _confined| {
                Ok(command_json(service.set_reg(&reg, value_token)?))
            }),
        )
        .await?;
        json_result(value)
    }

    #[tool(
        name = "break",
        description = "Set a breakpoint that fires when the machine fetches `addr` (a symbol name or number token) and return the trigger text, e.g. \"trigger 1 set\". A registered trigger makes a free `run` stop and report trigger_hit."
    )]
    async fn break_tool(
        &self,
        Parameters(args): Parameters<TriggerArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let call = ToolCall::new("break", serde_json::json!({"addr": &args.addr}));
        let addr = args.addr;
        let value = submit_tool_value(
            Arc::clone(&self.registry),
            &args.name,
            call,
            Box::new(move |service, _confined| {
                let id = service.break_at(&addr, None)?;
                Ok(Value::String(format!("trigger {id} set")))
            }),
        )
        .await?;
        json_result(value)
    }

    #[tool(
        name = "watch",
        description = "Set a write watchpoint on `addr` (a symbol name or number token) and return the trigger text, e.g. \"trigger 2 set\". Trigger management stays with the command passthrough (triggers, clear <id>)."
    )]
    async fn watch_tool(
        &self,
        Parameters(args): Parameters<TriggerArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let call = ToolCall::new("watch", serde_json::json!({"addr": &args.addr}));
        let addr = args.addr;
        let value = submit_tool_value(
            Arc::clone(&self.registry),
            &args.name,
            call,
            Box::new(move |service, _confined| {
                let id = service.watch(&addr, None)?;
                Ok(Value::String(format!("trigger {id} set")))
            }),
        )
        .await?;
        json_result(value)
    }

    #[tool(
        name = "screenshot",
        description = "Capture the composed frame of the session's machine as a PNG into the session output directory: the deterministic name screenshot-<tick>.png carries the machine tick, and a -1, -2, ... counter suffix resolves a same-tick collision. The result carries the PNG as an image content block plus text {path, width, height, size}. A machine with no video device, or a machine whose video reports no frame, fails cleanly."
    )]
    async fn screenshot_tool(
        &self,
        Parameters(args): Parameters<SessionNameArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let value = submit_tool_value(
            Arc::clone(&self.registry),
            &args.name,
            ToolCall::new("screenshot", serde_json::json!({})),
            Box::new(screenshot_job),
        )
        .await?;
        let Value::Object(mut payload) = value else {
            return Err(ErrorData::internal_error(
                "the screenshot result is not an object",
                None,
            ));
        };
        let Some(data) = payload
            .get("png_base64")
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            return Err(ErrorData::internal_error(
                "the screenshot result carries no image",
                None,
            ));
        };
        payload.remove("png_base64");
        let text = serde_json::to_string_pretty(&payload).map_err(|error| {
            ErrorData::internal_error(
                format!("the screenshot result failed to serialize: {error}"),
                None,
            )
        })?;
        Ok(CallToolResult::success(vec![
            ContentBlock::image(data, "image/png"),
            ContentBlock::text(text),
        ]))
    }
}
