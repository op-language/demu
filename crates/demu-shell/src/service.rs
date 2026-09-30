//! The machine service: typed command execution over a loaded machine.
//!
//! The service owns the [`Machine`] plus optional state (symbols, dump and
//! sentinel targets). The text and JSON renderers call it; neither holds
//! device access.

use std::collections::BTreeMap;

use demu_core::expr::EvalError;
use demu_core::machine::Machine;
use demu_core::triggers::TriggerRegistry;
use demu_symbols::{SymbolLookup, SymbolTable};

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
            dump_path: None,
            sentinel: None,
        }
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
        for i in 0..n {
            bytes.push(machine.peek(&bus, demu_core::Address(start.saturating_add(i))));
        }
        let hex: Vec<String> = bytes.iter().map(|b| format!("{b:02X}")).collect();
        Ok(CommandOutput::Text {
            text: format!("{:04X}: {}", start, hex.join(" ")),
            json: serde_json::json!({
                "addr": format!("{start:#06x}"),
                "bytes": hex.join(" "),
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

    /// Run the machine until the primary domain reaches `units`.
    pub fn run(&mut self, units: u64) -> Result<CommandOutput, CommandError> {
        let machine = self.machine.as_mut().ok_or(CommandError::NoMachine)?;
        let start = machine.now().0;
        machine.run_until(demu_core::Tick(start + units));
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
        machine.run_hook(
            demu_core::Tick(u64::MAX),
            &mut |event| {
                let hit = !triggers.fire_event(event).is_empty();
                if hit {
                    stopped = true;
                }
                hit
            },
            true,
        );
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
        machine.run_hook(
            demu_core::Tick(u64::MAX),
            &mut |event| {
                let hit = match (want.as_str(), event) {
                    ("vblank", demu_core::TriggerEvent::Vblank) => true,
                    ("irq", demu_core::TriggerEvent::Irq) => true,
                    ("fetch", demu_core::TriggerEvent::Fetch(a)) => target == 0 || a.0 == target,
                    ("read", demu_core::TriggerEvent::Read(a)) => target == 0 || a.0 == target,
                    ("write", demu_core::TriggerEvent::Write(a)) => target == 0 || a.0 == target,
                    _ => false,
                };
                if hit {
                    fired = Some(serde_json::json!(format!("{event:?}")));
                    return true;
                }
                !triggers.fire_event(event).is_empty()
            },
            true,
        );
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

    /// Run `frames` display frames.
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
        machine.run_hook(
            demu_core::Tick(u64::MAX),
            &mut |event| {
                if matches!(event, demu_core::TriggerEvent::Vblank) {
                    count += 1;
                }
                count >= frames
            },
            true,
        );
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
        for i in 0..n {
            bytes.push(machine.peek(&bus, demu_core::Address(start.saturating_add(i))));
        }
        let written = bytes.len();
        std::fs::write(path, bytes).map_err(|e| CommandError::Io(e.to_string()))?;
        Ok(CommandOutput::Text {
            text: format!("dumped {written} bytes at {start:04X} to {path}"),
            json: serde_json::json!({"addr": format!("{start:#06x}"), "written": written}),
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
