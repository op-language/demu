//! The DMA device family: the SM83 OAM DMA unit and CGB VRAM DMA
//! unit, the NES OAM DMA unit, and the SNES 5A22 support die.
//!
//! The OAM DMA unit copies 160 bytes from any bus source into object
//! memory at one byte per M-cycle. The CGB VRAM DMA unit moves 16-byte
//! blocks: a general-purpose transfer runs to completion and holds the
//! CPU, and the HBlank transfer moves one block per HBlank period. Both
//! stamp every read and write with its exact tick.

pub mod nes;
pub mod snessys;

pub use nes::NesOamDma;
pub use snessys::SnesSys;

use demu_config::model::BootState;
use demu_core::device::{AccessWidth, BusAccess, MachineCtx};
use demu_core::frequency::{Address, Tick};
use demu_core::signal::Level;
use demu_core::triggers::TriggerEvent;
use demu_core::{Device, Snapshot, SnapshotError};

/// The OAM DMA unit.
///
/// A write to `0xFF46` starts a 160-byte copy from the page the value
/// names. One byte moves per M-cycle: the source read lands on the
/// M-cycle start and the OAM write two dots later.
pub struct OamDma {
    name: String,
    /// The high byte of the source page.
    source: u8,
    /// The byte index within the transfer.
    byte: u8,
    /// The tick of the next byte.
    next: Option<Tick>,
    /// Domain dots per T-cycle.
    ticks_per_t: u32,
    last_tick: Option<Tick>,
}

impl OamDma {
    /// An OAM DMA unit from its configuration entry.
    pub fn new(device: &demu_config::model::DeviceInstance) -> Self {
        Self {
            name: device.name.to_string(),
            source: 0xFF,
            byte: 0,
            next: None,
            ticks_per_t: 1,
            last_tick: None,
        }
    }
}

impl Device for OamDma {
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
        self.source = 0xFF;
        self.byte = 0;
        self.next = None;
        self.last_tick = None;
    }

    fn boot(&mut self, state: Option<&BootState>) {
        let _ = state;
        self.reset();
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(16);
        out.extend_from_slice(b"ODMA");
        out.push(self.source);
        out.push(self.byte);
        out.extend_from_slice(&self.ticks_per_t.to_le_bytes());
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        if data.len() != 4 + 2 + 4 || &data[..4] != b"ODMA" {
            return Err(SnapshotError::Mismatch("bad oam dma chunk".into()));
        }
        self.source = data[4];
        self.byte = data[5];
        self.ticks_per_t = u32::from_le_bytes(data[6..10].try_into().expect("4 bytes")).clamp(1, 2);
        self.next = None;
        self.last_tick = None;
        Ok(())
    }

    fn notify_line(&mut self, line: &str, _level: Level, _tick: Tick) {
        // The transfer runs twice as fast in double speed.
        if line == "speed" {
            self.ticks_per_t = if self.ticks_per_t == 1 { 2 } else { 1 };
        }
    }

    fn next_event(&self) -> Option<Tick> {
        self.next
    }

    fn tick(&mut self, tick: Tick, ctx: &mut MachineCtx) {
        if self.last_tick.is_some_and(|t| t.0 >= tick.0) {
            return;
        }
        self.last_tick = Some(tick);
        if self.byte == 0 {
            ctx.emit(TriggerEvent::Dma(demu_core::triggers::DmaPhase::Start));
        }
        let source_addr = (u16::from(self.source) << 8) | u16::from(self.byte);
        let value = ctx.read(
            "cpu",
            demu_core::device::AccessWidth::Byte,
            demu_core::Address(u32::from(source_addr)),
            tick,
        ) as u8;
        let write_at = Tick(tick.0 + (4 * u64::from(self.ticks_per_t)) / 2);
        ctx.write(
            "cpu",
            demu_core::device::AccessWidth::Byte,
            demu_core::Address(0xFE00 + u32::from(self.byte)),
            u32::from(value),
            write_at,
        );
        self.byte += 1;
        if self.byte == 160 {
            ctx.emit(TriggerEvent::Dma(demu_core::triggers::DmaPhase::Complete));
            self.next = None;
        } else {
            let dots_per_byte = 4 * u64::from(self.ticks_per_t);
            self.next = Some(Tick(tick.0 + dots_per_byte));
        }
    }
}

impl BusAccess for OamDma {
    fn read(&mut self, _width: AccessWidth, address: Address, _tick: Tick) -> u32 {
        if address.0 == 0xFF46 {
            u32::from(self.source)
        } else {
            0xFF
        }
    }

    fn write(&mut self, _width: AccessWidth, address: Address, value: u32, tick: Tick) {
        if address.0 != 0xFF46 {
            return;
        }
        self.source = value as u8;
        self.byte = 0;
        // The transfer starts on the M-cycle after the write.
        let dots_per_m = 4 * u64::from(self.ticks_per_t);
        self.next = Some(Tick(tick.0 + dots_per_m));
        self.last_tick = None;
    }

    fn peek(&self, address: Address) -> u8 {
        if address.0 == 0xFF46 {
            self.source
        } else {
            0xFF
        }
    }
}

/// The CGB VRAM DMA unit.
pub struct Hdma {
    name: String,
    /// The source address, low four bits forced to zero.
    src: u16,
    /// The destination address inside VRAM.
    dst: u16,
    /// The blocks of 16 bytes remaining.
    remaining: u8,
    /// The HBlank mode is active.
    hblank_mode: bool,
    /// A block is in flight.
    block: u8,
    next: Option<Tick>,
    ticks_per_t: u32,
    last_tick: Option<Tick>,
}

impl Hdma {
    /// A VRAM DMA unit from its configuration entry.
    pub fn new(device: &demu_config::model::DeviceInstance) -> Self {
        Self {
            name: device.name.to_string(),
            src: 0xFFFF,
            dst: 0xFFFF,
            remaining: 0,
            hblank_mode: false,
            block: 0,
            next: None,
            ticks_per_t: 1,
            last_tick: None,
        }
    }

    /// The dots per byte: 2 at normal speed, 1 at double speed.
    fn dots_per_byte(&self) -> u64 {
        (2 / u64::from(self.ticks_per_t)).max(1)
    }

    /// The tick one block takes from its start.
    fn block_ticks(&self) -> u64 {
        16 * self.dots_per_byte()
    }
}

impl Device for Hdma {
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
        self.src = 0xFFFF;
        self.dst = 0xFFFF;
        self.remaining = 0;
        self.hblank_mode = false;
        self.block = 0;
        self.next = None;
        self.last_tick = None;
    }

    fn boot(&mut self, state: Option<&BootState>) {
        let _ = state;
        self.reset();
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(20);
        out.extend_from_slice(b"HDMA");
        out.extend_from_slice(&self.src.to_le_bytes());
        out.extend_from_slice(&self.dst.to_le_bytes());
        out.push(self.remaining);
        out.push(u8::from(self.hblank_mode));
        out.push(self.block);
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        if data.len() != 4 + 2 + 2 + 3 || &data[..4] != b"HDMA" {
            return Err(SnapshotError::Mismatch("bad hdma chunk".into()));
        }
        self.src = u16::from_le_bytes(data[4..6].try_into().expect("2 bytes"));
        self.dst = u16::from_le_bytes(data[6..8].try_into().expect("2 bytes"));
        self.remaining = data[8];
        self.hblank_mode = data[9] != 0;
        self.block = data[10];
        self.next = None;
        self.last_tick = None;
        Ok(())
    }

    fn notify_line(&mut self, line: &str, level: Level, tick: Tick) {
        match line {
            "speed" => {
                self.ticks_per_t = if self.ticks_per_t == 1 { 2 } else { 1 };
            }
            "hblank" if level == Level::High && self.hblank_mode && self.remaining > 0 => {
                // One block of 16 bytes moves during this HBlank.
                self.block = 0;
                self.next = Some(Tick(tick.0 + self.block_ticks()));
                self.last_tick = None;
            }
            _ => {}
        }
    }

    fn next_event(&self) -> Option<Tick> {
        self.next
    }

    fn tick(&mut self, tick: Tick, ctx: &mut MachineCtx) {
        if self.last_tick.is_some_and(|t| t.0 >= tick.0) {
            return;
        }
        self.last_tick = Some(tick);
        if self.block == 0 {
            ctx.notify("busy", Level::High, tick);
        }
        // One byte moves per visit.
        let value = ctx.read(
            "cpu",
            demu_core::device::AccessWidth::Byte,
            demu_core::Address(u32::from(self.src)),
            tick,
        ) as u8;
        ctx.write(
            "cpu",
            demu_core::device::AccessWidth::Byte,
            demu_core::Address(u32::from(self.dst)),
            u32::from(value),
            Tick(tick.0 + self.dots_per_byte().saturating_sub(1)),
        );
        self.src = self.src.wrapping_add(1);
        self.dst = self.dst.wrapping_add(1);
        if self.dst > 0x9FFF {
            self.dst = 0x8000;
        }
        self.block += 1;
        if self.block < 16 {
            self.next = Some(Tick(tick.0 + self.dots_per_byte()));
            return;
        }
        // The block is complete; release the bus.
        ctx.notify("busy", Level::Low, tick);
        self.block = 0;
        if self.remaining == 0 {
            self.next = None;
            return;
        }
        self.remaining -= 1;
        if self.remaining == 0 {
            self.next = None;
            return;
        }
        if self.hblank_mode {
            // The next block waits for the next HBlank.
            self.next = None;
        } else {
            self.next = Some(Tick(tick.0 + self.block_ticks()));
        }
    }
}

impl BusAccess for Hdma {
    fn read(&mut self, _width: AccessWidth, address: Address, _tick: Tick) -> u32 {
        match address.0 {
            0xFF55 => {
                let active = self.remaining > 0 && (self.hblank_mode || self.block < 16);
                if active {
                    u32::from(0x7F & (self.remaining - 1) & 0x7F)
                } else {
                    0xFF
                }
            }
            _ => 0xFF,
        }
    }

    fn write(&mut self, _width: AccessWidth, address: Address, value: u32, tick: Tick) {
        let v = value as u8;
        match address.0 {
            0xFF51 => self.src = (self.src & 0x00FF) | (u16::from(v) << 8),
            0xFF52 => self.src = (self.src & 0xFF00) | u16::from(v & 0xF0),
            0xFF53 => self.dst = 0x8000 | (u16::from(v & 0x1F) << 8) | (self.dst & 0x00F0),
            0xFF54 => self.dst = (self.dst & 0xFF00) | u16::from(v & 0xF0),
            0xFF55 => {
                if self.hblank_mode && self.remaining > 0 && v & 0x80 != 0 {
                    // A write with bit 7 set while an HBlank transfer is
                    // active restarts nothing.
                    return;
                }
                self.hblank_mode = v & 0x80 != 0;
                self.remaining = (v & 0x7F) + 1;
                self.block = 0;
                if self.hblank_mode {
                    // The first block waits for the next HBlank.
                    self.next = None;
                } else {
                    self.next = Some(Tick(tick.0 + self.block_ticks()));
                }
            }
            _ => {}
        }
    }

    fn peek(&self, address: Address) -> u8 {
        match address.0 {
            0xFF55 if self.remaining > 0 => 0x80 | (self.remaining - 1),
            0xFF55 => 0xFF,
            _ => 0xFF,
        }
    }
}
