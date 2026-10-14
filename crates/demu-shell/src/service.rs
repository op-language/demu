//! The machine service: typed command execution over a loaded machine.
//!
//! The service owns the [`Machine`] plus optional state (symbols, dump
//! and sentinel targets, the input schedule). The text and JSON
//! renderers call it; neither holds device access.

use std::collections::BTreeMap;
use std::sync::Arc;

use demu_config::model::SystemConfig;
use demu_core::expr::EvalError;
use demu_core::machine::Machine;
use demu_core::script::{InputScheduler, InputScript, run_until_with_input};
use demu_core::triggers::TriggerEvent;
use demu_core::{Tick, TriggerRegistry};
use demu_resample::Resampler;
use demu_symbols::{SymbolLookup, SymbolTable};

use crate::protocol::ErrorCode;

/// A machine factory for the media commands that rebuild a machine.
pub type MachineFactory =
    Arc<dyn Fn(&SystemConfig) -> Result<Machine, demu_core::machine::MachineError> + Send + Sync>;

/// A command execution failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum CommandError {
    /// The command arguments do not match the command schema.
    #[error("{0}")]
    BadArgs(String),
    /// The address or symbol does not resolve.
    #[error("bad address: {0}")]
    BadAddress(String),
    /// The trigger text does not parse.
    #[error("bad trigger: {0}")]
    BadTrigger(String),
    /// The input script does not parse or validate against the machine.
    #[error("bad script: {0}")]
    BadScript(String),
    /// The device or register name is unknown.
    #[error("unknown {0}: {1}")]
    Unknown(&'static str, String),
    /// No machine is loaded.
    #[error("no machine is loaded")]
    NoMachine,
    /// The file operation failed.
    #[error("io error: {0}")]
    Io(String),
    /// The command failed for a run-time reason.
    #[error("{0}")]
    Failed(String),
}

impl From<EvalError> for CommandError {
    fn from(e: EvalError) -> Self {
        CommandError::BadTrigger(e.to_string())
    }
}

impl CommandError {
    /// The protocol error code of this failure.
    pub fn error_code(&self) -> ErrorCode {
        match self {
            CommandError::BadArgs(_) => ErrorCode::BadArgs,
            CommandError::BadAddress(_) => ErrorCode::BadAddress,
            CommandError::BadTrigger(_) => ErrorCode::BadTrigger,
            CommandError::BadScript(_) => ErrorCode::BadScript,
            CommandError::Unknown("register", _) => ErrorCode::UnknownRegister,
            CommandError::Unknown(_, _) => ErrorCode::UnknownDevice,
            CommandError::NoMachine => ErrorCode::NoMachine,
            CommandError::Io(_) => ErrorCode::Io,
            CommandError::Failed(_) => ErrorCode::Failed,
        }
    }
}

/// The result of one executed command.
#[derive(Debug, Clone, PartialEq)]
pub enum CommandOutput {
    /// Free-form text plus structured data.
    Text {
        /// The human-readable text.
        text: String,
        /// The structured form for the JSON renderer.
        json: serde_json::Value,
    },
}

impl CommandOutput {
    /// A plain text output with an identical JSON string form.
    pub fn plain(text: impl Into<String>) -> Self {
        let text = text.into();
        Self::Text {
            json: serde_json::Value::String(text.clone()),
            text,
        }
    }
}

/// A video output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShotFormat {
    /// Portable Pixmap, binary P6, written by demu.
    Ppm,
    /// PNG, written through the `png` crate.
    Png,
}

/// The machine service: owns the machine and executes commands.
pub struct MachineService {
    machine: Option<Machine>,
    symbols: Option<SymbolLookup>,
    triggers: TriggerRegistry,
    /// The input event schedule loaded by the `script` command.
    input: InputScheduler,
    /// The machine factory for the media commands that rebuild machines.
    factory: Option<MachineFactory>,
    /// The configuration source text, tagged into media files.
    config_source: Option<String>,
    /// The memory dump path for the CLI contract, if any.
    pub dump_path: Option<std::path::PathBuf>,
    /// The sentinel as `(address, expected value)`, if any.
    pub sentinel: Option<(u32, u8)>,
}

impl Default for MachineService {
    fn default() -> Self {
        Self::new()
    }
}

impl MachineService {
    /// A service without a machine.
    pub fn new() -> Self {
        Self {
            machine: None,
            symbols: None,
            triggers: TriggerRegistry::new(),
            input: InputScheduler::empty(),
            factory: None,
            config_source: None,
            dump_path: None,
            sentinel: None,
        }
    }

    /// Install the machine factory the media commands use to rebuild a
    /// machine from an embedded configuration.
    pub fn set_machine_factory(&mut self, factory: MachineFactory) {
        self.factory = Some(factory);
    }

    /// Record the configuration's source text for media tags.
    pub fn set_config_source(&mut self, text: impl Into<String>) {
        self.config_source = Some(text.into());
    }

    /// Load a machine from a resolved configuration.
    pub fn load_machine(&mut self, machine: Machine) {
        self.machine = Some(machine);
    }

    /// A machine reference.
    pub fn machine(&self) -> Option<&Machine> {
        self.machine.as_ref()
    }

    /// A mutable machine reference.
    pub fn machine_mut(&mut self) -> Option<&mut Machine> {
        self.machine.as_mut()
    }

    /// Load a symbol table.
    pub fn load_symbols(&mut self, table: &SymbolTable) {
        self.symbols = Some(SymbolLookup::new(table));
    }

    /// The symbol lookup, if loaded.
    pub fn symbols(&self) -> Option<&SymbolLookup> {
        self.symbols.as_ref()
    }

    /// The trigger registry.
    pub fn triggers(&self) -> &TriggerRegistry {
        &self.triggers
    }

    /// The mutable trigger registry.
    pub fn triggers_mut(&mut self) -> &mut TriggerRegistry {
        &mut self.triggers
    }

    /// The loaded machine name and triple, for `info`.
    pub fn info(&self) -> Result<serde_json::Value, CommandError> {
        let machine = self.machine.as_ref().ok_or(CommandError::NoMachine)?;
        let config = &machine.config;
        let frames = machine
            .devices
            .iter()
            .filter_map(|d| d.video())
            .next()
            .map(|v| v.frames());
        Ok(serde_json::json!({
            "name": config.name,
            "triple": config.triple,
            "description": config.description,
            "devices": config.devices.iter().map(|d| d.name.to_string()).collect::<Vec<_>>(),
            "clocks": config.clocks.keys().cloned().collect::<Vec<_>>(),
            "now": machine.now().0,
            "frames": frames,
        }))
    }

    /// Read `n` bytes through the bus decoder.
    pub fn read(&self, addr_text: &str, n: u32) -> Result<CommandOutput, CommandError> {
        let machine = self.machine.as_ref().ok_or(CommandError::NoMachine)?;
        let start = self.resolve_address(addr_text)?;
        let bus = machine
            .primary_bus()
            .ok_or_else(|| CommandError::Failed("the machine declares no bus".into()))?
            .to_string();
        let mut bytes = Vec::with_capacity(n as usize);
        let mut window: Option<String> = None;
        for i in 0..n {
            let byte = if i == 0 {
                let (byte, answer) =
                    machine.peek_window(&bus, demu_core::Address(start.saturating_add(i)));
                window = answer;
                byte
            } else {
                machine.peek(&bus, demu_core::Address(start.saturating_add(i)))
            };
            bytes.push(byte);
        }
        let hex: Vec<String> = bytes.iter().map(|b| format!("{b:02X}")).collect();
        Ok(CommandOutput::Text {
            text: format!("{:04X}: {}", start, hex.join(" ")),
            json: serde_json::json!({
                "addr": format!("{start:#06x}"),
                "bytes": hex.join(" "),
                "window": window,
            }),
        })
    }

    /// Write bytes through the bus decoder.
    pub fn write(&mut self, addr_text: &str, bytes: &[u8]) -> Result<CommandOutput, CommandError> {
        let start = self.resolve_address(addr_text)?;
        let machine = self.machine.as_mut().ok_or(CommandError::NoMachine)?;
        let bus = machine
            .primary_bus()
            .ok_or_else(|| CommandError::Failed("the machine declares no bus".into()))?
            .to_string();
        let mut written = 0usize;
        for (i, b) in bytes.iter().enumerate() {
            if machine.debug_write(&bus, demu_core::Address(start.saturating_add(i as u32)), *b) {
                written += 1;
            }
        }
        if written < bytes.len() {
            return Err(CommandError::BadAddress(format!(
                "only {written} of {} bytes landed in a writable window",
                bytes.len()
            )));
        }
        Ok(CommandOutput::Text {
            text: format!("wrote {} bytes at {:04X}", bytes.len(), start),
            json: serde_json::json!({"addr": format!("{start:#06x}"), "written": written}),
        })
    }

    /// Resolve an address token through the symbols, then the number
    /// forms.
    fn resolve_address(&self, text: &str) -> Result<u32, CommandError> {
        if let Some(symbols) = &self.symbols
            && let Some(addr) = symbols.resolve(text)
        {
            return Ok(addr);
        }
        match demu_config::model::parse_number(text) {
            Some(v) => u32::try_from(v).map_err(|_| CommandError::BadAddress(text.to_string())),
            None => Err(CommandError::BadAddress(text.to_string())),
        }
    }

    /// Set a register value on the machine's CPU.
    pub fn set_reg(&mut self, name: &str, value: u64) -> Result<CommandOutput, CommandError> {
        let machine = self.machine.as_mut().ok_or(CommandError::NoMachine)?;
        for device in &mut machine.devices {
            if let Some(cpu) = device.cpu_mut()
                && cpu.set_register(name, value)
            {
                return Ok(CommandOutput::plain(format!("{name} = {value:#x}")));
            }
        }
        Err(CommandError::Unknown("register", name.to_string()))
    }

    /// The current register values of the machine's CPU.
    pub fn regs(&self) -> Result<CommandOutput, CommandError> {
        let machine = self.machine.as_ref().ok_or(CommandError::NoMachine)?;
        let mut map = BTreeMap::new();
        for device in &machine.devices {
            if let Some(cpu) = device.cpu() {
                for (k, v) in cpu.registers() {
                    map.insert(k, v);
                }
                break;
            }
        }
        if map.is_empty() {
            return Err(CommandError::Unknown("device", "cpu".to_string()));
        }
        let text = map
            .iter()
            .map(|(k, v)| match k.as_str() {
                "A" | "B" | "C" | "D" | "E" | "H" | "L" | "F" => format!("{k}={v:02X}"),
                _ => format!("{k}={v:#06X}"),
            })
            .collect::<Vec<_>>()
            .join(" ");
        let json: serde_json::Map<String, serde_json::Value> = map
            .into_iter()
            .map(|(k, v)| (k, serde_json::json!(v)))
            .collect();
        Ok(CommandOutput::Text {
            text,
            json: serde_json::Value::Object(json),
        })
    }

    /// Run the machine until the primary domain reaches `units`,
    /// applying scheduled script events on the way.
    pub fn run(&mut self, units: u64) -> Result<CommandOutput, CommandError> {
        let start = {
            let machine = self.machine.as_ref().ok_or(CommandError::NoMachine)?;
            machine.now().0
        };
        let machine = self.machine.as_mut().ok_or(CommandError::NoMachine)?;
        Self::run_segments(
            machine,
            &mut self.input,
            Tick(start.saturating_add(units)),
            &mut |_event| false,
            false,
        )?;
        let machine = self.machine.as_ref().ok_or(CommandError::NoMachine)?;
        let now = machine.now().0;
        Ok(CommandOutput::Text {
            text: format!("ran to {now}"),
            json: serde_json::json!({"now": now, "start": start}),
        })
    }

    /// Step N machine units.
    pub fn step(&mut self, units: u64) -> Result<CommandOutput, CommandError> {
        self.run(units)
    }

    /// Continue until a registered trigger hits.
    pub fn continue_run(&mut self) -> Result<CommandOutput, CommandError> {
        let machine = self.machine.as_mut().ok_or(CommandError::NoMachine)?;
        let start = machine.now().0;
        let triggers = &mut self.triggers;
        let mut stopped = false;
        let mut hook = |event: &TriggerEvent| {
            let hit = !triggers.fire_event(event).is_empty();
            if hit {
                stopped = true;
            }
            hit
        };
        let input = &mut self.input;
        Self::run_segments(machine, input, Tick(u64::MAX), &mut hook, true)?;
        let now = machine.now().0;
        Ok(CommandOutput::Text {
            text: format!("ran to {now}"),
            json: serde_json::json!({"now": now, "start": start, "trigger_hit": stopped}),
        })
    }

    /// Run until one runtime event fires, or a registered trigger hits.
    ///
    /// The event grammar: `vblank`, `irq`, `fetch <addr>`, `read
    /// <addr>`, or `write <addr>`. An omitted address matches every
    /// event of that kind.
    pub fn run_until(&mut self, spec: &str) -> Result<CommandOutput, CommandError> {
        let words: Vec<&str> = spec.split_whitespace().collect();
        let want = words.first().copied().unwrap_or("vblank").to_string();
        let target = words
            .get(1)
            .and_then(|t| demu_config::model::parse_number(t))
            .and_then(|v| u32::try_from(v).ok())
            .unwrap_or(0);
        let machine = self.machine.as_mut().ok_or(CommandError::NoMachine)?;
        let start = machine.now().0;
        let triggers = &mut self.triggers;
        let mut fired: Option<serde_json::Value> = None;
        let mut hook = |event: &TriggerEvent| {
            let hit = match (want.as_str(), event) {
                ("vblank", TriggerEvent::Vblank) => true,
                ("irq", TriggerEvent::Irq) => true,
                ("fetch", TriggerEvent::Fetch(a)) => target == 0 || a.0 == target,
                ("read", TriggerEvent::Read(a)) => target == 0 || a.0 == target,
                ("write", TriggerEvent::Write(a)) => target == 0 || a.0 == target,
                _ => false,
            };
            if hit {
                fired = Some(serde_json::json!(format!("{event:?}")));
                return true;
            }
            !triggers.fire_event(event).is_empty()
        };
        Self::run_segments(machine, &mut self.input, Tick(u64::MAX), &mut hook, true)?;
        let now = machine.now().0;
        Ok(CommandOutput::Text {
            text: format!("ran to {now}"),
            json: serde_json::json!({
                "now": now,
                "start": start,
                "event": fired,
            }),
        })
    }

    /// Run `frames` display frames, applying scheduled script events on
    /// the way.
    pub fn run_frame(&mut self, frames: u64) -> Result<CommandOutput, CommandError> {
        let machine = self.machine.as_mut().ok_or(CommandError::NoMachine)?;
        let start = machine.now().0;
        let video_frames = |m: &Machine| {
            m.devices
                .iter()
                .filter_map(|d| d.video())
                .next()
                .map(|v| v.frames())
        };
        let start_frames = video_frames(machine);
        let mut count = 0u64;
        let mut hook = |event: &TriggerEvent| {
            if matches!(event, TriggerEvent::Vblank) {
                count += 1;
            }
            count >= frames
        };
        Self::run_segments(machine, &mut self.input, Tick(u64::MAX), &mut hook, true)?;
        let now = machine.now().0;
        let end_frames = video_frames(machine);
        Ok(CommandOutput::Text {
            text: format!("ran to tick {now}"),
            json: serde_json::json!({
                "now": now,
                "start": start,
                "frames": [start_frames, end_frames],
            }),
        })
    }

    /// Run the machine toward `target`; pending script events apply at
    /// their exact instants, other events go to the hook. A free
    /// function so the callers can borrow the machine, the schedule,
    /// and the trigger table as disjoint fields.
    fn run_segments(
        machine: &mut Machine,
        input: &mut InputScheduler,
        target: Tick,
        hook: &mut dyn FnMut(&TriggerEvent) -> bool,
        emit: bool,
    ) -> Result<(), CommandError> {
        if input.pending() == 0 {
            machine.run_hook(target, hook, emit);
            return Ok(());
        }
        run_until_with_input(machine, input, target, hook, emit)
            .map_err(|e| CommandError::Failed(e.to_string()))
    }

    /// Write `n` bytes from the bus into a file.
    pub fn dump_file(
        &self,
        addr_text: &str,
        n: u32,
        path: &str,
    ) -> Result<CommandOutput, CommandError> {
        let machine = self.machine.as_ref().ok_or(CommandError::NoMachine)?;
        let start = self.resolve_address(addr_text)?;
        let bus = machine
            .primary_bus()
            .ok_or_else(|| CommandError::Failed("the machine declares no bus".into()))?
            .to_string();
        let mut bytes = Vec::with_capacity(n as usize);
        let mut window: Option<String> = None;
        for i in 0..n {
            let byte = if i == 0 {
                let (byte, answer) =
                    machine.peek_window(&bus, demu_core::Address(start.saturating_add(i)));
                window = answer;
                byte
            } else {
                machine.peek(&bus, demu_core::Address(start.saturating_add(i)))
            };
            bytes.push(byte);
        }
        let written = bytes.len();
        std::fs::write(path, bytes).map_err(|e| CommandError::Io(e.to_string()))?;
        Ok(CommandOutput::Text {
            text: format!("dumped {written} bytes at {start:04X} to {path}"),
            json: serde_json::json!({"addr": format!("{start:#06x}"), "written": written, "window": window}),
        })
    }

    /// Fill `n` bytes with one byte value.
    pub fn fill(
        &mut self,
        addr_text: &str,
        n: u32,
        byte: u8,
    ) -> Result<CommandOutput, CommandError> {
        let start = self.resolve_address(addr_text)?;
        let machine = self.machine.as_mut().ok_or(CommandError::NoMachine)?;
        let bus = machine
            .primary_bus()
            .ok_or_else(|| CommandError::Failed("the machine declares no bus".into()))?
            .to_string();
        let mut written = 0usize;
        for i in 0..n {
            if machine.debug_write(&bus, demu_core::Address(start.saturating_add(i)), byte) {
                written += 1;
            }
        }
        Ok(CommandOutput::Text {
            text: format!("filled {} bytes at {:04X}", written, start),
            json: serde_json::json!({"addr": format!("{start:#06x}"), "written": written}),
        })
    }

    /// Inject one input event into the named device.
    pub fn inject(&mut self, device: &str, event: &str) -> Result<CommandOutput, CommandError> {
        let machine = self.machine.as_mut().ok_or(CommandError::NoMachine)?;
        let Some(target) = machine.device_by_name_mut(device) else {
            return Err(CommandError::Unknown("device", device.to_string()));
        };
        match target.input_mut() {
            Some(input) => {
                if input.inject(event) {
                    Ok(CommandOutput::plain(format!("injected {event}")))
                } else {
                    Err(CommandError::BadArgs(format!("unknown event: {event}")))
                }
            }
            None => Err(CommandError::Unknown("device", device.to_string())),
        }
    }

    /// Load an input script: parse, validate against the machine, and
    /// install it as the run schedule.
    ///
    /// # Errors
    ///
    /// Returns [`CommandError::BadScript`] when the script does not
    /// parse or validate, and [`CommandError::NoMachine`] without a
    /// machine.
    pub fn load_script(&mut self, text: &str) -> Result<CommandOutput, CommandError> {
        let machine = self.machine.as_ref().ok_or(CommandError::NoMachine)?;
        let script = InputScript::parse(text, &machine.config)
            .map_err(|e| CommandError::BadScript(e.to_string()))?;
        let count = script.events().len();
        self.input = InputScheduler::new(script);
        Ok(CommandOutput::Text {
            text: format!("loaded {count} input events"),
            json: serde_json::json!({"events": count}),
        })
    }

    /// Convert an input script file into the input form of a Matroska
    /// file: one event track plus the configuration tags and attachment.
    ///
    /// # Errors
    ///
    /// Returns the command error for the caller to report.
    pub fn mkv_input(
        &mut self,
        script_path: &str,
        out_path: &str,
    ) -> Result<CommandOutput, CommandError> {
        let machine = self.machine.as_ref().ok_or(CommandError::NoMachine)?;
        let config_text = self
            .config_source
            .clone()
            .ok_or_else(|| CommandError::Failed("no configuration source is known".to_string()))?;
        let text =
            std::fs::read_to_string(script_path).map_err(|e| CommandError::Io(e.to_string()))?;
        let script = InputScript::parse(&text, &machine.config)
            .map_err(|e| CommandError::BadScript(e.to_string()))?;
        let count = script.events().len();
        demu_mkv::write_input_file(
            std::path::Path::new(out_path),
            &machine.config,
            &config_text,
            script.events(),
        )
        .map_err(|e| CommandError::Failed(e.to_string()))?;
        Ok(CommandOutput::Text {
            text: format!("wrote {count} event blocks to {out_path}"),
            json: serde_json::json!({"events": count, "path": out_path}),
        })
    }

    /// Render an input-form Matroska file: rebuild the machine from the
    /// file's embedded configuration, replay its events over `frames`
    /// display frames, and record the output form with FFV1 video and
    /// both PCM tracks.
    ///
    /// # Errors
    ///
    /// Returns the command error when the file, the rebuild, or the
    /// recording fails.
    pub fn mkv_render(
        &mut self,
        input_path: &str,
        frames: u64,
        out_path: &str,
    ) -> Result<CommandOutput, CommandError> {
        let source = demu_mkv::read_input_file(std::path::Path::new(input_path))
            .map_err(|e| CommandError::Failed(e.to_string()))?;
        let config_text = source.config_text.clone().ok_or(CommandError::Failed(
            "the file carries no system configuration".to_string(),
        ))?;
        self.rebuild_machine(&config_text)?;
        self.input = InputScheduler::new(InputScript::from_events(source.events));
        // The render tags the file with the machine it actually ran:
        // the file's embedded configuration text, not the CLI
        // session's.
        self.config_source = Some(config_text);
        self.record_pipeline(out_path, frames)
    }

    /// Record the current session for `frames` display frames, with any
    /// scheduled script events applied, into the output form.
    ///
    /// # Errors
    ///
    /// Returns the command error when the machine lacks a video device,
    /// or when the recording fails.
    pub fn render_session(
        &mut self,
        frames: u64,
        out_path: &str,
    ) -> Result<CommandOutput, CommandError> {
        {
            let machine = self.machine.as_ref().ok_or(CommandError::NoMachine)?;
            if machine.devices.iter().all(|d| d.video().is_none()) {
                return Err(CommandError::Failed(
                    "the machine declares no video device".to_string(),
                ));
            }
        }
        self.record_pipeline(out_path, frames)
    }

    /// The shared media recording loop.
    ///
    /// The loop runs one display frame per round through the vblank
    /// hook with the script events interleaved, captures the frame,
    /// drains every audio device, resamples the drained samples, and
    /// writes the three blocks of the emulated display frame. The
    /// writer builds at the first completed frame, when the frame
    /// geometry and the native sample rate are known.
    ///
    /// # Errors
    ///
    /// Returns the command error when a run, an event, or the writer
    /// fails, or when no display frame ever completes.
    fn record_pipeline(
        &mut self,
        out_path: &str,
        frames: u64,
    ) -> Result<CommandOutput, CommandError> {
        let config_text = self
            .config_source
            .clone()
            .ok_or_else(|| CommandError::Failed("no configuration source is known".to_string()))?;
        if frames == 0 {
            return Ok(CommandOutput::Text {
                text: "recorded 0 frames".to_string(),
                json: serde_json::json!({"frames": 0, "path": out_path}),
            });
        }
        let (triple, name) = {
            let machine = self.machine.as_ref().ok_or(CommandError::NoMachine)?;
            (machine.config.triple.clone(), machine.config.name.clone())
        };
        let mut writer: Option<demu_mkv::RenderWriter> = None;
        let mut resampler: Option<Resampler> = None;
        let mut native_rate = 0u32;
        let mut made = 0u64;
        let mut rounds = 0u64;
        let max_rounds = frames.saturating_add(1024);
        while made < frames {
            if rounds >= max_rounds {
                return Err(CommandError::Failed(
                    "no display frame completed for the recording".to_string(),
                ));
            }
            rounds += 1;
            let machine = self.machine.as_mut().ok_or(CommandError::NoMachine)?;
            let input_slot = &mut self.input;
            let primary = machine.scheduler.primary;
            let before = machine.now().0;
            let mut vblanks = 0u64;
            let mut hook = |event: &TriggerEvent| {
                if matches!(event, TriggerEvent::Vblank) {
                    vblanks += 1;
                }
                vblanks >= 1
            };
            Self::run_segments(machine, input_slot, Tick(u64::MAX), &mut hook, true)
                .map_err(|e| CommandError::Failed(e.to_string()))?;
            if vblanks == 0 && machine.now().0 == before {
                return Err(CommandError::Failed(
                    "the machine cannot reach the next display frame".to_string(),
                ));
            }
            if vblanks == 0 {
                continue;
            }
            let ts = machine.scheduler.domains[primary]
                .frequency
                .ticks_to_ns(machine.now().0);
            let frame = machine
                .devices
                .iter()
                .find_map(|d| d.video())
                .and_then(|v| v.frame())
                .cloned()
                .ok_or_else(|| CommandError::Failed("no frame is complete yet".to_string()))?;
            // The track layout assumes one sample rate: the first
            // audio device's. A later rate difference would misstamp
            // both PCM tracks, so the recording fails loudly instead.
            let mut native = Vec::new();
            for device in machine.devices.iter_mut() {
                if let Some(audio) = device.audio_mut() {
                    native.extend(audio.take_samples());
                    if native_rate == 0 {
                        native_rate = audio.sample_rate();
                    } else if audio.sample_rate() != native_rate {
                        return Err(CommandError::Failed(
                            "the machine declares audio devices with different sample rates"
                                .to_string(),
                        ));
                    }
                }
            }
            if resampler.is_none() && native_rate > 0 {
                resampler =
                    Some(Resampler::new(native_rate).map_err(|e| {
                        CommandError::Failed(format!("resampler setup failed: {e}"))
                    })?);
            }
            let resampled = match resampler.as_mut() {
                Some(resampler) => {
                    resampler.push(&native);
                    resampler.take_output()
                }
                None => Vec::new(),
            };
            if writer.is_none() {
                writer = Some(
                    demu_mkv::RenderWriter::create(
                        std::path::Path::new(out_path),
                        frame.width,
                        frame.height,
                        (native_rate > 0).then_some(native_rate),
                        &config_text,
                        &triple,
                        &name,
                    )
                    .map_err(|e| CommandError::Failed(e.to_string()))?,
                );
            }
            writer
                .as_mut()
                .map(|writer| writer.add_frame(ts, &frame.pixels, &native, &resampled))
                .transpose()
                .map_err(|e| CommandError::Failed(e.to_string()))?;
            made += 1;
        }
        if let Some(writer) = writer.as_mut() {
            writer
                .finish()
                .map_err(|e| CommandError::Failed(e.to_string()))?;
        }
        Ok(CommandOutput::Text {
            text: format!("recorded {made} frames to {out_path}"),
            json: serde_json::json!({"frames": made, "path": out_path}),
        })
    }

    /// Rebuild the session machine from a configuration text through
    /// the installed factory. The rebuilt machine replaces the current
    /// one; the script schedule stays.
    fn rebuild_machine(&mut self, config_text: &str) -> Result<(), CommandError> {
        let factory = self
            .factory
            .clone()
            .ok_or_else(|| CommandError::Failed("no machine factory is installed".to_string()))?;
        let raw = demu_config::RawConfig::parse(config_text)
            .map_err(|e| CommandError::Failed(format!("configuration parse failed: {e}")))?;
        let (config, warnings) = demu_config::validate(&raw).map_err(|report| {
            CommandError::Failed(format!("configuration validation failed: {report:?}"))
        })?;
        let _ = warnings;
        let machine = factory(&config).map_err(|e| CommandError::Failed(e.to_string()))?;
        self.machine = Some(machine);
        Ok(())
    }

    /// The machine's video source, when one device produces a display.
    fn video(&self) -> Result<&dyn demu_core::VideoSource, CommandError> {
        let machine = self.machine.as_ref().ok_or(CommandError::NoMachine)?;
        machine
            .devices
            .iter()
            .filter_map(|d| d.video())
            .next()
            .ok_or_else(|| CommandError::Failed("the machine declares no video device".into()))
    }

    /// Capture the composed frame as an image file.
    pub fn screenshot(
        &mut self,
        path: &str,
        format: Option<ShotFormat>,
    ) -> Result<CommandOutput, CommandError> {
        let video = self.video()?;
        let frame = video
            .frame()
            .ok_or_else(|| CommandError::Failed("no frame is complete yet".into()))?;
        let format = format.unwrap_or(ShotFormat::Ppm);
        let bytes = match format {
            ShotFormat::Ppm => encode_ppm(frame),
            ShotFormat::Png => encode_png(frame)
                .map_err(|e| CommandError::Failed(format!("png encode failed: {e}")))?,
        };
        std::fs::write(path, bytes).map_err(|e| CommandError::Io(e.to_string()))?;
        Ok(CommandOutput::Text {
            text: format!("wrote {path} ({}x{})", frame.width, frame.height),
            json: serde_json::json!({
                "path": path,
                "width": frame.width,
                "height": frame.height,
                "format": match format { ShotFormat::Ppm => "ppm", ShotFormat::Png => "png" },
            }),
        })
    }

    /// Dump one video layer to a file.
    pub fn dump_layer(
        &mut self,
        layer: demu_core::VideoLayer,
        path: &str,
    ) -> Result<CommandOutput, CommandError> {
        let video = self.video()?;
        let data = video
            .dump_layer(layer)
            .ok_or_else(|| CommandError::Failed("the layer dump is not available".into()))?;
        let name = match layer {
            demu_core::VideoLayer::Vram => "vram",
            demu_core::VideoLayer::NameTable => "nametable",
            demu_core::VideoLayer::Oam => "oam",
            demu_core::VideoLayer::Palette => "palette",
            demu_core::VideoLayer::Framebuffer => "framebuffer",
        };
        let file = format!("{path}.{name}");
        std::fs::write(&file, data).map_err(|e| CommandError::Io(e.to_string()))?;
        Ok(CommandOutput::plain(format!("dumped {name} to {file}")))
    }

    /// Write the cartridge-pattern table bytes to a file.
    ///
    /// The byte count is the CHR store of the first cartridge device
    /// that carries one. The file name gains the `.pattern` suffix,
    /// like the video layer dumps gain theirs.
    pub fn dump_pattern(&self, path: &str) -> Result<CommandOutput, CommandError> {
        let machine = self.machine.as_ref().ok_or(CommandError::NoMachine)?;
        let bytes = machine
            .devices
            .iter()
            .filter_map(|d| d.pattern())
            .find_map(|source| source.pattern_table())
            .ok_or_else(|| CommandError::Failed("no device carries a pattern table".into()))?;
        let file = format!("{path}.pattern");
        std::fs::write(&file, bytes).map_err(|e| CommandError::Io(e.to_string()))?;
        Ok(CommandOutput::Text {
            text: format!("dumped {} pattern bytes to {file}", bytes.len()),
            json: serde_json::json!({"path": file, "bytes": bytes.len()}),
        })
    }

    /// Register a breakpoint; the address may be a symbol name.
    pub fn break_at(&mut self, token: &str, cond: Option<&str>) -> Result<u64, CommandError> {
        let addr = self.resolve_address(token)?;
        let scope = std::collections::BTreeMap::new();
        self.triggers
            .add(
                demu_core::triggers::TriggerKind::Breakpoint {
                    address: demu_core::Address(addr),
                },
                cond,
                scope,
            )
            .map_err(|e| CommandError::BadTrigger(e.to_string()))
    }

    /// Register a write watchpoint.
    pub fn watch(&mut self, token: &str, cond: Option<&str>) -> Result<u64, CommandError> {
        let addr = self.resolve_address(token)?;
        self.triggers
            .add(
                demu_core::triggers::TriggerKind::WriteWatch {
                    address: demu_core::Address(addr),
                },
                cond,
                std::collections::BTreeMap::new(),
            )
            .map_err(|e| CommandError::BadTrigger(e.to_string()))
    }

    /// Write the memory dump file and evaluate the sentinel.
    ///
    /// The dump is the machine's address-indexed image: the byte at file
    /// offset `A` is the bus byte at address `A`, which matches the
    /// existing `cart test` contract.
    pub fn write_dump(&self) -> Result<Option<(u32, u8)>, CommandError> {
        let Some(machine) = self.machine.as_ref() else {
            return Ok(None);
        };
        let Some(bus_name) = machine.primary_bus() else {
            return Ok(None);
        };
        let mut image = Vec::with_capacity(0x10000);
        for addr in 0..0x10000u32 {
            image.push(machine.peek(bus_name, demu_core::Address(addr)));
        }
        if let Some(path) = &self.dump_path {
            std::fs::write(path, &image).map_err(|e| CommandError::Io(e.to_string()))?;
        }
        Ok(self
            .sentinel
            .map(|(a, _)| (a, image.get(a as usize).copied().unwrap_or(0))))
    }
}

/// Encode a frame as a binary PPM (P6).
fn encode_ppm(frame: &demu_core::Frame) -> Vec<u8> {
    let mut out = Vec::with_capacity(15 + frame.pixels.len());
    out.extend_from_slice(format!("P6\n{} {}\n255\n", frame.width, frame.height).as_bytes());
    out.extend_from_slice(&frame.pixels);
    out
}

/// Encode a frame as PNG.
///
/// # Errors
///
/// Returns the encoder error.
fn encode_png(frame: &demu_core::Frame) -> Result<Vec<u8>, std::io::Error> {
    let mut out = Vec::new();
    {
        let mut encoder =
            png::Encoder::new(std::io::Cursor::new(&mut out), frame.width, frame.height);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        writer
            .write_image_data(&frame.pixels)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::CommandOutputText as _;

    #[test]
    fn error_code_maps_every_command_failure() {
        let cases = [
            (
                CommandError::BadArgs("bad args".to_owned()),
                ErrorCode::BadArgs,
            ),
            (
                CommandError::BadAddress("bad address".to_owned()),
                ErrorCode::BadAddress,
            ),
            (
                CommandError::BadTrigger("bad trigger".to_owned()),
                ErrorCode::BadTrigger,
            ),
            (
                CommandError::BadScript("bad script".to_owned()),
                ErrorCode::BadScript,
            ),
            (
                CommandError::Unknown("register", "r0".to_owned()),
                ErrorCode::UnknownRegister,
            ),
            (
                CommandError::Unknown("device", "cpu".to_owned()),
                ErrorCode::UnknownDevice,
            ),
            (CommandError::NoMachine, ErrorCode::NoMachine),
            (CommandError::Io("io error".to_owned()), ErrorCode::Io),
            (CommandError::Failed("failed".to_owned()), ErrorCode::Failed),
        ];
        for (error, code) in cases {
            assert_eq!(error.error_code(), code, "{error}");
        }
    }

    fn service() -> MachineService {
        let mut service = MachineService::new();
        service.load_machine(
            Machine::new(demu_config::smoke_config().expect("smoke config is valid"))
                .expect("smoke machine assembles"),
        );
        service.machine_mut().expect("loaded").boot();
        service
    }

    #[test]
    fn script_loads_and_events_apply_during_run() {
        let mut service = service();
        let out = service
            .load_script("#main:2 cpu0 left\n#main:4 cpu0 right\n")
            .expect("script loads");
        assert_eq!(out.text(), "loaded 2 input events");

        // One machine unit steps past both event instants on the 4 MHz
        // smoke domain; the events inject in script order.
        service.step(10).expect("run");
        let machine = service.machine().expect("machine");
        let cpu = machine
            .device_by_name("cpu0")
            .and_then(|d| {
                d.as_any()
                    .downcast_ref::<demu_core::machine::devices::SmokeCpu>()
            })
            .expect("cpu0 exists");
        assert_eq!(cpu.input_log(), &["left".to_string(), "right".to_string()]);
    }

    #[test]
    fn script_rejects_unknown_devices() {
        let mut service = service();
        assert!(matches!(
            service.load_script("#main:2 ghost nope\n"),
            Err(CommandError::BadScript(_))
        ));
    }

    #[test]
    fn script_requires_a_machine() {
        let mut service = MachineService::new();
        assert!(matches!(
            service.load_script("#main:2 cpu0 left\n"),
            Err(CommandError::NoMachine)
        ));
    }

    #[test]
    fn render_session_needs_a_video_device() {
        let mut service = service();
        let path = std::env::temp_dir().join("demu-shell-render-session.mkv");
        assert!(matches!(
            service.render_session(1, &path.to_string_lossy()),
            Err(CommandError::Failed(message)) if message.contains("no video device")
        ));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn dump_pattern_writes_the_cartridge_bytes() {
        struct PatternStub {
            name: String,
            chr: Vec<u8>,
        }
        impl demu_core::device::PatternSource for PatternStub {
            fn pattern_table(&self) -> Option<&[u8]> {
                Some(&self.chr)
            }
        }
        impl demu_core::Device for PatternStub {
            fn name(&self) -> &str {
                &self.name
            }

            fn as_any(&self) -> &dyn std::any::Any {
                self
            }

            fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
                self
            }

            fn pattern(&self) -> Option<&dyn demu_core::device::PatternSource> {
                Some(self)
            }

            fn reset(&mut self) {}

            fn boot(&mut self, _state: Option<&demu_config::model::BootState>) {}

            fn snapshot(&self) -> demu_core::Snapshot {
                self.chr.clone()
            }

            fn restore(&mut self, _data: &[u8]) -> Result<(), demu_core::SnapshotError> {
                Ok(())
            }

            fn notify_line(&mut self, _line: &str, _level: demu_core::Level, _tick: Tick) {}

            fn tick(&mut self, _tick: Tick, _ctx: &mut demu_core::MachineCtx) {}
        }

        let source = vec![0x42, 0x21, 0x07, 0xFF];
        let expected = source.clone();
        let mut config = demu_config::smoke_config().expect("smoke config is valid");
        let domain = config.devices[0].domain.clone();
        let slot = config.devices.len();
        config.devices_by_name.insert(
            demu_config::newtypes::DeviceName::new("pattern0").expect("the device name validates"),
            slot,
        );
        config.devices.push(demu_config::model::DeviceInstance {
            name: demu_config::newtypes::DeviceName::new("pattern0")
                .expect("the device name validates"),
            kind: demu_config::newtypes::KindName::new("test.pattern")
                .expect("the kind name validates"),
            domain,
            params: std::collections::BTreeMap::new(),
            dividers: std::collections::BTreeMap::new(),
        });
        let mut ctors = demu_core::smoke_ctors();
        ctors.register(
            "test.pattern",
            Box::new(move |device: &_, _config: &_| {
                Ok(Box::new(PatternStub {
                    name: device.name.to_string(),
                    chr: source.clone(),
                }) as Box<dyn demu_core::Device>)
            }),
        );
        let mut loaded = MachineService::new();
        loaded
            .load_machine(demu_core::Machine::assemble(config, &ctors).expect("machine assembles"));
        loaded.machine_mut().expect("loaded").boot();

        let path = std::env::temp_dir().join("demu-shell-dump-pattern");
        let file = format!("{}.pattern", path.to_string_lossy());
        std::fs::remove_file(&file).ok();
        let out = loaded
            .dump_pattern(&path.to_string_lossy())
            .expect("the pattern table dumps");
        assert_eq!(
            std::fs::read(&file).expect("the pattern file reads back"),
            expected
        );
        let CommandOutput::Text { text, json } = out;
        assert_eq!(text, format!("dumped 4 pattern bytes to {file}"));
        assert_eq!(json["bytes"], 4);
        std::fs::remove_file(&file).ok();

        // The smoke machine carries no pattern-table device.
        let bare = service();
        assert!(matches!(
            bare.dump_pattern(&path.to_string_lossy()),
            Err(CommandError::Failed(_))
        ));
    }
}
