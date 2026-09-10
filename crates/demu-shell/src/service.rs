//! The machine service: typed command execution over a loaded machine.
//!
//! The service owns the [`Machine`] plus optional state (symbols, dump and
//! sentinel targets). The text and JSON renderers call it; neither holds
//! device access.

use std::collections::BTreeMap;

use demu_core::expr::EvalError;
use demu_core::frequency::Address;
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

/// The run command argument: machine units of the primary domain.
pub enum RunTarget {
    /// Run N machine units.
    Units(u64),
    /// Run until a trigger fires.
    Until,
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
        Ok(serde_json::json!({
            "name": config.name,
            "triple": config.triple,
            "description": config.description,
            "devices": config.devices.iter().map(|d| d.name.to_string()).collect::<Vec<_>>(),
            "clocks": config.clocks.keys().cloned().collect::<Vec<_>>(),
            "now": machine.now().0,
        }))
    }

    /// Read `n` bytes from the primary bus windows.
    ///
    /// The read walks every window that covers each address and calls the
    /// device's side-effect-free `peek`.
    pub fn read(&self, addr_text: &str, n: u32) -> Result<CommandOutput, CommandError> {
        let machine = self.machine.as_ref().ok_or(CommandError::NoMachine)?;
        let start = self.resolve_address(addr_text)?;
        let mut bytes = Vec::with_capacity(n as usize);
        for i in 0..n {
            bytes.push(self.peek_byte(machine, Address(start + i)));
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

    /// Write bytes into RAM-backed windows.
    pub fn write(&mut self, addr_text: &str, bytes: &[u8]) -> Result<CommandOutput, CommandError> {
        let start = self.resolve_address(addr_text)?;
        let machine = self.machine.as_mut().ok_or(CommandError::NoMachine)?;
        let mut written = 0usize;
        for (i, b) in bytes.iter().enumerate() {
            written += Self::poke_byte(machine, Address(start + i as u32), *b);
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

    /// Peek one byte through the primary bus windows.
    fn peek_byte(&self, machine: &Machine, address: Address) -> u8 {
        let bus = &machine.buses[0];
        for (window_idx, _) in bus.hits(address.0) {
            let w = &bus.windows[window_idx];
            let idx = (address.0 - w.base) as usize;
            if let Some(ram) = as_smoke_ram(machine, w.device)
                && idx < ram.data().len()
            {
                return ram.data()[idx];
            }
            if let Some(rom) = as_smoke_rom(machine, w.device)
                && idx < rom.data().len()
            {
                return rom.data()[idx];
            }
        }
        0
    }

    /// Poke one byte into a RAM window. Returns 1 when it landed.
    fn poke_byte(machine: &mut Machine, address: Address, value: u8) -> usize {
        let Some((window_idx, _)) = machine.buses[0].hits(address.0).into_iter().next() else {
            return 0;
        };
        let device_slot = machine.buses[0].windows[window_idx].device;
        let idx = (address.0 - machine.buses[0].windows[window_idx].base) as usize;
        let Some(device) = machine.devices.get_mut(device_slot) else {
            return 0;
        };
        let Some(ram) = device
            .as_any_mut()
            .downcast_mut::<demu_core::machine::devices::SmokeRam>()
        else {
            return 0;
        };
        if idx < ram.data_len() {
            ram.poke(idx, value);
            1
        } else {
            0
        }
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

    /// Set a register value on the machine. The smoke CPU exposes `ip`.
    pub fn set_reg(&mut self, name: &str, value: u64) -> Result<CommandOutput, CommandError> {
        if name != "ip" {
            return Err(CommandError::Unknown("register", name.to_string()));
        }
        let machine = self.machine.as_mut().ok_or(CommandError::NoMachine)?;
        for device in &mut machine.devices {
            if let Some(cpu) = as_cpu_mut(device.as_mut()) {
                cpu.ip = value;
                return Ok(CommandOutput::plain(format!("ip = {value:#x}")));
            }
        }
        Err(CommandError::Unknown("device", "cpu".to_string()))
    }

    /// The current register values.
    pub fn regs(&self) -> Result<CommandOutput, CommandError> {
        let machine = self.machine.as_ref().ok_or(CommandError::NoMachine)?;
        let mut map = BTreeMap::new();
        for device in &machine.devices {
            if let Some(cpu) = as_cpu(device.as_ref()) {
                map.insert("ip".to_string(), cpu.ip);
            }
        }
        let text = map
            .iter()
            .map(|(k, v)| format!("{k}={v:#x}"))
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

    /// Register a breakpoint; the address may be a symbol name.
    pub fn break_at(&mut self, token: &str, cond: Option<&str>) -> Result<u64, CommandError> {
        let addr = self.resolve_address(token)?;
        let scope = std::collections::BTreeMap::new();
        self.triggers
            .add(
                demu_core::triggers::TriggerKind::Breakpoint {
                    address: Address(addr),
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
                    address: Address(addr),
                },
                cond,
                std::collections::BTreeMap::new(),
            )
            .map_err(|e| CommandError::BadTrigger(e.to_string()))
    }

    /// Write the memory dump file. The dump is every device snapshot
    /// concatenated in device order; the sentinel byte is read back
    /// through the bus.
    pub fn write_dump(&self) -> Result<Option<(u32, u8)>, CommandError> {
        let Some(machine) = self.machine.as_ref() else {
            return Ok(None);
        };
        let Some(path) = &self.dump_path else {
            return Ok(self
                .sentinel
                .map(|(a, _)| (a, self.peek_byte(machine, Address(a)))));
        };
        // Dump every device snapshot into one file, concatenated in device
        // order. The sentinel address maps through the bus.
        let mut buf = Vec::new();
        for device in &machine.devices {
            buf.extend_from_slice(&device.snapshot());
        }
        std::fs::write(path, &buf).map_err(|e| CommandError::Io(e.to_string()))?;
        Ok(self.sentinel.map(|(a, _)| (a, v_at(&buf, a, machine))))
    }
}

fn v_at(buf: &[u8], addr: u32, machine: &Machine) -> u8 {
    // The byte for the sentinel lives in the RAM device whose window
    // covers the address; find it through the bus.
    let bus = &machine.buses[0];
    for (window_idx, _) in bus.hits(addr) {
        let w = &bus.windows[window_idx];
        let offset_in_window = addr - w.base;
        // Device chunks are concatenated in device order; find the device
        // chunk start by walking the device list.
        let mut start = 0usize;
        for (i, device) in machine.devices.iter().enumerate() {
            let len = device.snapshot().len();
            if i == w.device {
                let idx = start + offset_in_window as usize;
                if idx < buf.len() {
                    return buf[idx];
                }
            }
            let _ = len;
            start += device.snapshot().len();
        }
    }
    0
}

fn as_smoke_ram(machine: &Machine, slot: usize) -> Option<&demu_core::machine::devices::SmokeRam> {
    let device = machine.devices.get(slot)?;
    device.as_any().downcast_ref()
}

fn as_smoke_rom(machine: &Machine, slot: usize) -> Option<&demu_core::machine::devices::SmokeRom> {
    let device = machine.devices.get(slot)?;
    device.as_any().downcast_ref()
}

fn as_cpu(device: &dyn Device) -> Option<&demu_core::machine::devices::SmokeCpu> {
    device.as_any().downcast_ref()
}

fn as_cpu_mut(device: &mut dyn Device) -> Option<&mut demu_core::machine::devices::SmokeCpu> {
    device.as_any_mut().downcast_mut()
}

use demu_core::device::Device;
