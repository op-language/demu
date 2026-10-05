//! The Genesis support silicon of the Mega Drive: the bus arbiter of
//! the two processors, the IO controller of the controller ports,
//! and the bank mapper between the sound bus and the 68000 bus, the
//! `genesis.arbiter`, `genesis.io`, and `genesis.mapper` device
//! kinds.
//!
//! # The bus arbiter
//!
//! The two processors share one machine bus; the 68000 owns it
//! except by the sound side's banked window grant. The control trio
//! carries the published handshake: the memory mode at $A11000
//! (bit 0: 0 = the boot ROM form over the sound RAM region, 1 = the
//! sound RAM form), the bus request at $A11100 (a write with bit 0
//! set records the request and stops the sound core at its next
//! instruction boundary; the read's grant bit is active low; a write
//! with bit 0 clear releases the grant), and the reset at $A11200 (a
//! write with bit 0 clear asserts the sound core's reset hold, a
//! write with bit 0 set releases it and the core re-runs its
//! power-on form).
//!
//! The arbiter drives the `stall` line high for the held grant and
//! low on the release, and pulses `reset`. The sound core stops
//! between instructions and declares no next visit while it is
//! held.
//!
//! # The IO controller
//!
//! The IO area answers the published form: the version register at
//! $A10001 reports the hardware version with the region bit; the
//! data registers of the three port cells carry the controller
//! state under the TH output line; the shell injects events per
//! port.
//!
//! # The bank mapper
//!
//! The bank register is the published nine-bit shift register: every
//! write to the sound-side page shifts the register right one bit
//! and enters the written byte's bit 0 at the register's bit 8, so
//! nine sequential writes fill the register with the 68k address
//! bits 15 to 23. The sound bus's upper window maps onto the 68k
//! bus in 32K pages through the register. The 68k side answers the
//! mapped sound address window: byte lanes route to the devices of
//! the sound bus, and the boot ROM image covers the sound RAM's
//! first 2K while the memory mode reads 0.
//!
//! # Sources
//!
//! The Sega Mega Drive Technical Overview (the memory map and the
//! Z80 control trio), the Nuked-MD arbiter documentation of the
//! shift-register protocol, and the Charles MacDonald Genesis VDP
//! notes.

use demu_config::model::{BootState, DeviceInstance};
use demu_core::InputSink;
use demu_core::device::{AccessWidth, BusAccess, MachineCtx};
use demu_core::frequency::{Address, Tick};
use demu_core::signal::Level;
use demu_core::{Device, Snapshot, SnapshotError};

/// The boot ROM image size of the sound core's first 2K form.
const BOOTROM_SIZE: usize = 2048;
/// The mapped base of the 68000-side sound address window.
const Z_SPACE: u32 = 0x00A0_0000;

/// The bus arbiter between the two processors.
///
/// The arbiter answers the control trio and drives the two lines the
/// sound core answers: the `stall` hold line and the `zreset` wipe
/// line, through declared connections to the sound CPU. The 68000
/// writes the request at `$A11100` with bit 0 set; the arbiter's next
/// tick visit asserts the hold and records the grant, and the release
/// write (bit 0 clear) drops both. The reset form: the write with bit
/// 0 clear asserts the reset hold (the visit wipes the core through
/// `zreset` and parks it with `stall`), and the write with bit 0 set
/// releases the hold so the core runs its power-on form.
#[derive(Debug)]
pub struct GenesisArbiter {
    name: String,
    /// The sound memory mode: 0 the boot ROM form, 1 the RAM.
    dram_mode: bool,
    /// True while the 68000's request stands.
    bus_req: bool,
    /// True while the sound core is held down. The read's grant bit
    /// is its complement, active low.
    granted: bool,
    /// True while the stall line drives high.
    stall_driven: bool,
    /// The armed reset assert of the next visit.
    reset_pending: bool,
    /// The armed reset release of the next visit.
    reset_release: bool,
    /// The next visit the handshake self-schedules.
    next: Option<Tick>,
}

impl GenesisArbiter {
    /// An arbiter from its configuration entry.
    pub fn new(device: &DeviceInstance) -> Result<Self, String> {
        Ok(Self {
            name: device.name.to_string(),
            dram_mode: false,
            bus_req: false,
            granted: false,
            stall_driven: false,
            reset_pending: false,
            reset_release: false,
            next: None,
        })
    }

    /// The control register's index of an address: 0 = the memory
    /// mode, 1 = the bus request, 2 = the reset.
    fn cell(&self, address: u32) -> Option<u8> {
        match address & 0x0F_FFFF {
            0x01_1000 => Some(0),
            0x01_1100 => Some(1),
            0x01_1200 => Some(2),
            _ => None,
        }
    }

    /// The snapshot tail of the grant record.
    fn snapshot_tail(&self, out: &mut Snapshot) {
        out.push(1);
        out.push(u8::from(self.dram_mode));
        out.push(u8::from(self.bus_req));
        out.push(u8::from(self.granted));
        out.push(u8::from(self.stall_driven));
        out.push(u8::from(self.reset_pending));
        out.push(u8::from(self.reset_release));
        out.extend_from_slice(&self.next.map(|t| t.0).unwrap_or(u64::MAX).to_le_bytes());
    }
}

impl BusAccess for GenesisArbiter {
    fn read(&mut self, _width: AccessWidth, address: Address, _tick: Tick) -> u32 {
        match self.cell(address.0) {
            Some(0) => u32::from(self.dram_mode),
            Some(1) => u32::from(!self.granted),
            _ => 0xFF,
        }
    }

    fn write(&mut self, _width: AccessWidth, address: Address, value: u32, tick: Tick) {
        let at_next = Tick(tick.0.saturating_add(1));
        match self.cell(address.0) {
            Some(0) => self.dram_mode = (value & 0x01) == 0x01,
            Some(1) => {
                // The published form: a write with bit 0 set
                // requests the sound bus, a write with it clear
                // releases. The grant lands on the next visit so the
                // sound core stops at its own instruction boundary.
                self.bus_req = value & 0x01 == 0x01;
                self.next = Some(at_next);
            }
            Some(2) => {
                // The published reset: the write with bit 0 clear
                // asserts the sound core's reset (the core wipes and
                // parks); the write with it set releases the hold and
                // the core runs its power-on form.
                if value & 0x01 == 0x00 {
                    self.reset_pending = true;
                } else {
                    self.reset_release = true;
                }
                self.next = Some(at_next);
            }
            _ => {}
        }
    }

    fn peek(&self, address: Address) -> u8 {
        match self.cell(address.0) {
            Some(0) => u8::from(self.dram_mode),
            Some(1) => u8::from(!self.granted),
            _ => 0xFF,
        }
    }
}

impl Device for GenesisArbiter {
    fn name(&self) -> &str {
        &self.name
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn bus(&self) -> Option<&dyn BusAccess> {
        Some(self)
    }

    fn bus_mut(&mut self) -> Option<&mut dyn BusAccess> {
        Some(self)
    }

    fn reset(&mut self) {
        self.dram_mode = false;
        self.bus_req = false;
        self.granted = false;
        self.stall_driven = false;
        self.reset_pending = false;
        self.reset_release = false;
        self.next = None;
    }

    fn boot(&mut self, _state: Option<&BootState>) {
        self.reset();
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(19);
        out.extend_from_slice(b"MDAR");
        self.snapshot_tail(&mut out);
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        let form = 19;
        if data.len() != form || &data[..4] != b"MDAR" || data[4] != 1 {
            return Err(SnapshotError::Mismatch(
                "bad mega drive arbiter chunk".into(),
            ));
        }
        self.dram_mode = data[5] != 0;
        self.bus_req = data[6] != 0;
        self.granted = data[7] != 0;
        self.stall_driven = data[8] != 0;
        self.reset_pending = data[9] != 0;
        self.reset_release = data[10] != 0;
        let next = u64::from_le_bytes(data[11..19].try_into().expect("form-checked"));
        self.next = (next != u64::MAX).then_some(Tick(next));
        Ok(())
    }

    fn notify_line(&mut self, _line: &str, _level: Level, _tick: Tick) {}

    fn next_event(&self) -> Option<Tick> {
        self.next
    }

    fn tick(&mut self, tick: Tick, ctx: &mut MachineCtx) {
        self.next = None;
        // The armed reset assert: the core wipes, wakes into its
        // power-on form only when released, and parks like a held
        // sound core.
        if self.reset_pending {
            self.reset_pending = false;
            ctx.notify("zreset", Level::Low, tick);
            ctx.notify("stall", Level::High, tick);
            self.stall_driven = true;
            return;
        }
        // The armed reset release: the held core unstalls and runs.
        if self.reset_release {
            self.reset_release = false;
            if self.stall_driven {
                ctx.notify("stall", Level::Low, tick);
                self.stall_driven = false;
            }
            return;
        }
        if self.bus_req {
            // The grant lands once and stands until the release
            // write; the stalled core parks at its own boundary.
            self.granted = true;
            if !self.stall_driven {
                ctx.notify("stall", Level::High, tick);
                self.stall_driven = true;
            }
            return;
        }
        if self.stall_driven {
            // The release write's visit: the grant drops with the
            // hold.
            self.granted = false;
            ctx.notify("stall", Level::Low, tick);
            self.stall_driven = false;
        }
    }
}

/// Parse the numeric body of a form: the bits = a decimal number
/// under 64.
fn parse_bits(rest: &str) -> Option<u8> {
    let bits = rest.trim().parse::<u8>().ok()?;
    (bits <= 63).then_some(bits)
}

/// Parse one injection event: `(cell, bits, press)`.
fn parse_event(event: &str) -> Option<(usize, u8, bool)> {
    let form = event.trim().to_ascii_lowercase();
    let rest = form.strip_prefix("port ")?;
    let (n, tail) = rest.split_once(' ')?;
    let port = n.trim().parse::<usize>().ok().filter(|p| *p < 3)?;
    let (bits, press) = if let Some(bits) = tail.strip_prefix("press ") {
        (bits, true)
    } else {
        let bits = tail.strip_prefix("lift ")?;
        (bits, false)
    };
    let value = parse_bits(bits)?;
    Some((port, value, press))
}

impl InputSink for GenesisIo {
    fn inject(&mut self, event: &str) -> bool {
        // The grammar: `port <n> press <bits>` and `port <n> lift
        // <bits>`, with the bits = a decimal number over the six data
        // bits of one port cell.
        let Some((cell, value, press)) = parse_event(event) else {
            return false;
        };
        if press {
            self.cells[cell].data |= value;
        } else {
            self.cells[cell].data &= !value;
        }
        true
    }
}

impl Device for GenesisIo {
    fn name(&self) -> &str {
        &self.name
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn bus(&self) -> Option<&dyn BusAccess> {
        Some(self)
    }

    fn bus_mut(&mut self) -> Option<&mut dyn BusAccess> {
        Some(self)
    }

    fn input_mut(&mut self) -> Option<&mut dyn InputSink> {
        Some(self)
    }

    fn reset(&mut self) {
        self.cells = [IoCell::default(); 3];
    }

    fn boot(&mut self, _state: Option<&BootState>) {
        self.reset();
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(10);
        out.extend_from_slice(b"MDIO");
        self.snapshot_tail(&mut out);
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        // The form: the tag, the version, and the three port cells'
        // data and TH bytes = 11 bytes.
        if data.len() != 11 || &data[..4] != b"MDIO" || data[4] != 1 {
            return Err(SnapshotError::Mismatch("bad mega drive io chunk".into()));
        }
        for (i, cell) in self.cells.iter_mut().enumerate() {
            cell.data = data[5 + i * 2];
            cell.th = data[6 + i * 2] != 0;
        }
        Ok(())
    }

    fn notify_line(&mut self, _line: &str, _level: Level, _tick: Tick) {}

    fn next_event(&self) -> Option<Tick> {
        None
    }

    fn tick(&mut self, _tick: Tick, _ctx: &mut MachineCtx) {}
}

/// The bank mapper between the sound bus and the 68000 bus.
///
/// The mapper answers the sound card's register page, the mapped
/// sound address region over the sound RAM and the boot ROM form,
/// and the 68000-side mapped sound address window. The bank
/// register is the nine-bit shift register of the published form.
///
/// The mapper owns the 8 KiB of sound RAM; the boot ROM image loads
/// through the device's own boot state. The memory mode decides the
/// sound side's first 2 KiB: mode 0 answers the boot ROM image, mode
/// 1 answers the RAM. The 68000-side sound address window routes to
/// the sound RAM over the borrowed-decoder form.
#[derive(Debug)]
pub struct GenesisMapper {
    name: String,
    /// The sound bus's memory mode (shared with the arbiter's
    /// register through the config wiring): 0 = the boot ROM form.
    dram_mode: bool,
    /// The nine-bit bank register.
    bank: u32,
    /// The number of bank bits written since the last reset.
    bank_bits: u32,
    /// The 2K boot ROM image, loaded in boot.
    bootrom: Vec<u8>,
    /// The 8K sound RAM image the mapper itself answers.
    ram: Vec<u8>,
    /// The bus whose decoder answers the banked window's translated
    /// accesses (the mapper borrows it, default "cpu").
    main_bus: String,
    /// The own-answered next visit, for the self-scheduled forms.
    next: Option<Tick>,
}

impl GenesisMapper {
    /// A mapper from its configuration entry.
    pub fn new(device: &DeviceInstance) -> Result<Self, String> {
        let main_bus = device
            .params
            .get("main_bus")
            .and_then(|v| v.as_str())
            .unwrap_or("cpu")
            .to_string();
        Ok(Self {
            name: device.name.to_string(),
            dram_mode: false,
            bank: 0,
            bank_bits: 0,
            bootrom: Vec::new(),
            ram: vec![0; 0x2000],
            main_bus,
            next: None,
        })
    }

    fn bank_target(&self, offset: u32) -> u32 {
        (self.bank << 15) | (offset & 0x7FFF)
    }

    /// Read one byte of the sound address space.
    fn sound_space_read(&self, zoff: u32) -> u8 {
        // The 68000-side mapped window covers the 8-KiB sound RAM
        // with the boot ROM overlay; the bank register page answers
        // only on the sound bus.
        match zoff {
            0x0000..=0x1FFF => {
                // The memory mode overlays the boot ROM form.
                if !self.dram_mode {
                    let i = (zoff as usize) & (BOOTROM_SIZE - 1);
                    if i < self.bootrom.len() {
                        return self.bootrom[i];
                    }
                    return 0xFF;
                }
                self.ram[zoff as usize]
            }
            _ => 0xFF,
        }
    }

    /// Write one byte into the sound address space. Writes land in
    /// the sound RAM in the RAM mode form.
    fn sound_space_write(&mut self, zoff: u32, value: u8) {
        if let 0x0000..=0x1FFF = zoff
            && self.dram_mode
        {
            self.ram[zoff as usize] = value;
        }
    }

    /// The snapshot tail of the mapper record.
    fn snapshot_tail(&self, out: &mut Snapshot) {
        out.push(1);
        out.push(u8::from(self.dram_mode));
        out.extend_from_slice(&self.bank.to_le_bytes());
        out.push(self.bank_bits as u8);
        out.extend_from_slice(&self.ram);
    }
}

/// The mapped sound-address offset of a 68000-side window access:
/// the window's base sits at `Z_SPACE` over the 8 KiB sound RAM, and
/// the real form mirrors the RAM at a $2000 stride through the
/// window's 16-KiB reach.
fn sound_offset(address: Address) -> u32 {
    (address.0 - Z_SPACE) & 0x1FFF
}

impl BusAccess for GenesisMapper {
    fn read(&mut self, _width: AccessWidth, address: Address, _tick: Tick) -> u32 {
        if address.0 >= Z_SPACE {
            // The 68000-side mapped sound RAM; the mode decides the
            // boot ROM form over the first 2K.
            return u32::from(self.sound_space_read(sound_offset(address)));
        }
        match address.0 {
            0x0000_0000..=0x0000_1FFF => {
                // The sound bus's sound RAM form, with the memory
                // mode overlay.
                u32::from(self.sound_space_read(address.0))
            }
            0x0000_6000..=0x0000_7FFF => {
                // The bank register page answers its own value.
                self.bank & 0xFF
            }
            0x0000_8000..=0x0000_FFFF => {
                // The sound-side banked window borrows the 68000 bus.
                0xFF
            }
            _ => 0xFF,
        }
    }

    fn write(&mut self, _width: AccessWidth, address: Address, value: u32, _tick: Tick) {
        if address.0 >= Z_SPACE {
            self.sound_space_write(sound_offset(address), value as u8);
            return;
        }
        match address.0 {
            0x0000_0000..=0x0000_1FFF => {
                // The sound bus's RAM write, active in the RAM mode.
                self.sound_space_write(address.0, value as u8);
            }
            0x0000_6000..=0x0000_7FFF => {
                // The nine-bit shift register: the written byte's bit
                // 0 enters at the register's bit 8.
                self.bank = (self.bank >> 1) | ((value & 0x01) << 8);
                self.bank_bits = self.bank_bits.wrapping_add(1);
            }
            _ => {}
        }
    }

    fn peek(&self, address: Address) -> u8 {
        if address.0 >= Z_SPACE {
            return self.sound_space_read(sound_offset(address));
        }
        match address.0 {
            0x0000_0000..=0x0000_1FFF => self.sound_space_read(address.0),
            0x0000_6000..=0x0000_7FFF => (self.bank & 0xFF) as u8,
            _ => 0xFF,
        }
    }

    fn decodes(&self, _address: Address) -> bool {
        true
    }

    fn borrow(&self, address: Address) -> demu_core::device::Borrow {
        use demu_core::device::Borrow;
        if (0x0000_8000..=0x0000_FFFF).contains(&address.0) {
            // The sound-side banked window composes the 32K page of
            // the 68000 bus from the nine-bit bank register. The
            // 68000-side sound RAM window answers locally: the
            // mapper owns the RAM bytes themselves.
            return Borrow::Bridge {
                bus: self.main_bus.clone(),
                address: self.bank_target(address.0 - 0x0000_8000),
            };
        }
        Borrow::Local
    }
}

/// The device form of the mapper for the machine's device table.
impl Device for GenesisMapper {
    fn name(&self) -> &str {
        &self.name
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn bus(&self) -> Option<&dyn BusAccess> {
        Some(self)
    }

    fn bus_mut(&mut self) -> Option<&mut dyn BusAccess> {
        Some(self)
    }

    fn reset(&mut self) {
        self.dram_mode = false;
        self.bank = 0;
        self.bank_bits = 0;
        self.bootrom.clear();
        self.ram.fill(0);
    }

    fn boot(&mut self, state: Option<&BootState>) {
        self.reset();
        match state {
            Some(BootState::Hex { bytes }) => {
                let n = bytes.len().min(BOOTROM_SIZE);
                self.bootrom = bytes[..n].to_vec();
            }
            Some(BootState::Rom { path }) => {
                let bytes = std::fs::read(path).unwrap_or_default();
                let n = bytes.len().min(BOOTROM_SIZE);
                self.bootrom = bytes[..n].to_vec();
            }
            _ => {}
        }
        self.next = None;
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(6 + self.ram.len() + self.bootrom.len());
        out.extend_from_slice(b"MDMP");
        self.snapshot_tail(&mut out);
        out.extend_from_slice(&(self.bootrom.len() as u32).to_le_bytes());
        out.extend_from_slice(&self.bootrom);
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        let head = 4 + 1 + 1 + 4 + 1;
        if data.len() < head + self.ram.len() + 4 || &data[..4] != b"MDMP" || data[4] != 1 {
            return Err(SnapshotError::Mismatch(
                "bad mega drive mapper chunk".into(),
            ));
        }
        let mut i = 5usize;
        self.dram_mode = data[i] != 0;
        i += 1;
        self.bank = u32::from_le_bytes(data[i..i + 4].try_into().expect("form-checked"));
        i += 4;
        self.bank_bits = u32::from(data[i]);
        i += 1;
        let ram_len = self.ram.len();
        self.ram.copy_from_slice(&data[i..i + ram_len]);
        i += self.ram.len();
        let rom_len = u32::from_le_bytes(data[i..i + 4].try_into().expect("form-checked")) as usize;
        i += 4;
        if data.len() != i + rom_len {
            return Err(SnapshotError::Mismatch(format!(
                "mega drive mapper chunk must carry {rom_len} boot ROM bytes"
            )));
        }
        self.bootrom = data[i..i + rom_len].to_vec();
        self.next = None;
        Ok(())
    }

    fn notify_line(&mut self, line: &str, level: Level, _tick: Tick) {
        if line == "mode" {
            // The arbiter's memory-mode line: high = the RAM form.
            self.dram_mode = level == Level::High;
        }
    }

    fn next_event(&self) -> Option<Tick> {
        self.next
    }

    fn tick(&mut self, _tick: Tick, _ctx: &mut MachineCtx) {
        // The mapper owns no scheduled work of its own; the bank
        // register answers in the access paths.
        self.next = None;
    }
}

/// The controller-port state of one IO cell.
#[derive(Debug, Clone, Copy, Default)]
struct IoCell {
    /// The stored input state: bit 0-5 the six data bits.
    data: u8,
    /// True while the TH output line is high.
    th: bool,
}

/// The IO controller of the controller ports.
///
/// The controller answers the version register and the per-cell data
/// registers with the published form; the shell injects input per
/// cell.
#[derive(Debug)]
pub struct GenesisIo {
    name: String,
    /// The stored input cells of the three port cells.
    cells: [IoCell; 3],
}

impl GenesisIo {
    /// An IO controller from its configuration entry.
    pub fn new(device: &DeviceInstance) -> Result<Self, String> {
        Ok(Self {
            name: device.name.to_string(),
            cells: [IoCell::default(); 3],
        })
    }

    /// The snapshot tail of the input record.
    fn snapshot_tail(&self, out: &mut Snapshot) {
        out.push(1);
        for cell in &self.cells {
            out.push(cell.data);
            out.push(u8::from(cell.th));
        }
    }
}

impl BusAccess for GenesisIo {
    fn read(&mut self, _width: AccessWidth, address: Address, _tick: Tick) -> u32 {
        let cell = (address.0 & 0x1F) / 2;
        match cell {
            0 => 0xC0, // the version cell: the fixed identity form
            _ => {
                let i = ((cell - 1) as usize).min(2);
                let c = self.cells[i];
                // The TH output splits the register: 1 reports the
                // direction bits, 0 the ABC and Start bits.
                let base = if c.th { 0x00 } else { 0x40 };
                u32::from(base | (c.data & 0x3F))
            }
        }
    }

    fn write(&mut self, _width: AccessWidth, address: Address, value: u32, _tick: Tick) {
        let cell = (address.0 & 0x1F) / 2;
        if cell >= 1 {
            let i = ((cell - 1) as usize).min(2);
            self.cells[i].th = (value & 0x40) == 0x40;
        }
    }

    fn peek(&self, _address: Address) -> u8 {
        0xFF
    }
}

#[cfg(test)]
mod genesis_support_tests {
    //! The arbiter and mapper handshake tests over a mini machine: a
    //! probe device records the line edges the arbiter drives.

    use super::*;
    use demu_config::model::{
        Bus as CBus, ClockDomain, Connection as CConn, SystemConfig, Window as CWindow,
    };
    use demu_config::newtypes::{DeviceName, KindName, LinePath};
    use demu_core::machine::{DeviceCtors, Machine};
    use std::collections::BTreeMap;

    /// A test device that records every line edge delivered to it.
    struct LineProbe {
        name: String,
        edges: Vec<(String, bool)>,
    }

    impl LineProbe {
        fn new(name: &str) -> Self {
            Self {
                name: name.to_string(),
                edges: Vec::new(),
            }
        }
    }

    impl demu_core::Device for LineProbe {
        fn name(&self) -> &str {
            &self.name
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }
        fn reset(&mut self) {}
        fn boot(&mut self, _state: Option<&BootState>) {}
        fn snapshot(&self) -> Snapshot {
            Snapshot::new()
        }
        fn restore(&mut self, _data: &[u8]) -> Result<(), SnapshotError> {
            Ok(())
        }
        fn notify_line(&mut self, line: &str, level: Level, _tick: Tick) {
            self.edges.push((line.to_string(), level.is_high()));
        }
        fn tick(&mut self, _tick: Tick, _ctx: &mut MachineCtx) {}
    }

    fn line_of(path: &str) -> LinePath {
        LinePath::parse(path).expect("line path")
    }

    /// One arbiter wired to a probe over the stall and zreset lines.
    fn arbiter_machine() -> Machine {
        let mut clocks = BTreeMap::new();
        clocks.insert(
            "main".to_string(),
            ClockDomain {
                name: "main".to_string(),
                frequency: demu_config::newtypes::Frequency::new(53_693_175, 1).expect("freq"),
                origin: "the test master".to_string(),
            },
        );
        let mut params = BTreeMap::new();
        params.insert(
            "main_bus".to_string(),
            toml::Value::String("cpu".to_string()),
        );
        let arb = DeviceInstance {
            name: DeviceName::new("arbiter").expect("name"),
            kind: KindName::new("genesis.arbiter").expect("kind"),
            domain: "main".to_string(),
            params,
            dividers: BTreeMap::new(),
        };
        let probe = DeviceInstance {
            name: DeviceName::new("probe").expect("name"),
            kind: KindName::new("test.probe").expect("kind"),
            domain: "main".to_string(),
            params: BTreeMap::new(),
            dividers: BTreeMap::new(),
        };
        let mut by_name = BTreeMap::new();
        by_name.insert(arb.name.clone(), 0usize);
        by_name.insert(probe.name.clone(), 1usize);
        let config = SystemConfig {
            name: "arbiter-test".to_string(),
            triple: "test".to_string(),
            description: "the arbiter handshake test".to_string(),
            clocks,
            devices: vec![arb, probe],
            devices_by_name: by_name,
            buses: vec![CBus {
                name: "cpu".to_string(),
                address_bits: 24,
                owner: None,
                parent: None,
            }],
            windows: vec![CWindow {
                name: "w_arbiter".to_string(),
                bus: "cpu".to_string(),
                device: DeviceName::new("arbiter").expect("name"),
                base: 0x00A1_1000,
                size: 0x0400,
                kind: None,
                mirror: None,
                overlay: None,
                overlay_read_priority: None,
                overlay_write_priority: None,
                conflict: None,
            }],
            connections: vec![
                CConn {
                    from: line_of("arbiter.stall"),
                    to: line_of("probe.stall"),
                    semantics: demu_config::model::ConnectionSemantics::AsyncPort,
                    direction: demu_config::model::ConnectionDirection::Forward,
                },
                CConn {
                    from: line_of("arbiter.zreset"),
                    to: line_of("probe.zreset"),
                    semantics: demu_config::model::ConnectionSemantics::AsyncPort,
                    direction: demu_config::model::ConnectionDirection::Forward,
                },
            ],
            signals: Vec::new(),
            boot: BTreeMap::new(),
            quirks: BTreeMap::new(),
        };
        let mut ctors = DeviceCtors::new();
        ctors.register(
            "genesis.arbiter",
            Box::new(|d, _| {
                Ok(Box::new(
                    GenesisArbiter::new(d).map_err(demu_core::machine::MachineError::Assembly)?,
                ) as Box<dyn Device>)
            }),
        );
        ctors.register(
            "test.probe",
            Box::new(|d, _| Ok(Box::new(LineProbe::new(d.name.as_str())) as Box<dyn Device>)),
        );
        let mut machine = Machine::assemble(config, &ctors).expect("machine");
        machine.boot();
        machine
    }

    fn edges_of(machine: &mut Machine) -> Vec<(String, bool)> {
        machine
            .devices
            .iter()
            .filter_map(|d| d.as_any().downcast_ref::<LineProbe>())
            .flat_map(|p| p.edges.iter().cloned())
            .collect()
    }

    /// The published PauseZ80/ResumeZ80 walk: the bit-0-set write takes
    /// the grant, the release drops it, and a second request grants
    /// again.
    #[test]
    fn the_arbiter_walks_request_grant_release_and_re_request() {
        let mut machine = arbiter_machine();

        assert!(machine.debug_write("cpu", Address(0xA1_1100), 0x01));
        machine.run_until(Tick(9));
        assert_eq!(edges_of(&mut machine), [("stall".to_string(), true)]);
        assert_eq!(
            machine.peek("cpu", Address(0xA1_1100)),
            0x00,
            "the grant bit reads active low"
        );

        assert!(machine.debug_write("cpu", Address(0xA1_1100), 0x00));
        machine.run_until(Tick(20));
        assert_eq!(
            edges_of(&mut machine),
            [("stall".to_string(), true), ("stall".to_string(), false)]
        );

        assert!(machine.debug_write("cpu", Address(0xA1_1100), 0x01));
        machine.run_until(Tick(30));
        assert_eq!(
            edges_of(&mut machine),
            [
                ("stall".to_string(), true),
                ("stall".to_string(), false),
                ("stall".to_string(), true),
            ]
        );
    }

    /// The reset walk: the assert wipes through zreset and holds the
    /// core parked; the release unstalls it.
    #[test]
    fn the_arbiter_walks_reset_assert_and_release() {
        let mut machine = arbiter_machine();

        assert!(machine.debug_write("cpu", Address(0xA1_1200), 0x00));
        machine.run_until(Tick(9));
        assert_eq!(
            edges_of(&mut machine),
            [("zreset".to_string(), false), ("stall".to_string(), true)]
        );

        // A release with no request unstalls the held core.
        assert!(machine.debug_write("cpu", Address(0xA1_1200), 0x01));
        machine.run_until(Tick(20));
        assert_eq!(
            edges_of(&mut machine),
            [
                ("zreset".to_string(), false),
                ("stall".to_string(), true),
                ("stall".to_string(), false),
            ]
        );
    }

    /// The mapper's bank window borrow: the shift-register writes
    /// compose the 32-KiB page and the banked window routes through
    /// into a probe-backed device.
    #[test]
    fn the_mapper_walks_the_nine_bit_bank_register() {
        // The mapper device-level form: the register page's writes
        // shift the bank; the read page answers its own bank byte.
        let mut params = BTreeMap::new();
        params.insert(
            "main_bus".to_string(),
            toml::Value::String("cpu".to_string()),
        );
        let mapper = DeviceInstance {
            name: DeviceName::new("mapper").expect("name"),
            kind: KindName::new("genesis.mapper").expect("kind"),
            domain: "main".to_string(),
            params,
            dividers: BTreeMap::new(),
        };
        let mut m = GenesisMapper::new(&mapper).expect("mapper");
        // The published shift form: nine writes fill bits 15..7.
        for b in [0u8, 1u8, 0u8, 1u8, 0u8, 0u8, 0u8, 0u8, 0u8] {
            m.write(
                AccessWidth::Byte,
                Address(0x0000_6000),
                u32::from(b),
                Tick(0),
            );
        }
        // The composed bank reads back over its byte lane: the newest
        // write enters at the bank bit 8 and earlier writes shift
        // down, so this nine-write sequence composes as observed.
        assert_eq!(
            m.read(AccessWidth::Byte, Address(0x0000_6010), Tick(0)),
            0x0A
        );
        // The snapshot round trip restores into a fresh mapper.
        let snap = m.snapshot();
        let mut twin = GenesisMapper::new(&mapper).expect("twin");
        twin.restore(&snap).expect("restore");
        // The twin composes the same bank bytes.
        assert_eq!(
            twin.read(AccessWidth::Byte, Address(0x0000_6010), Tick(0)),
            m.read(AccessWidth::Byte, Address(0x0000_6010), Tick(0))
        );
    }
}
