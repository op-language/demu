//! The NES OAM DMA unit.
//!
//! A write to $4014 copies 256 bytes from the named page over the
//! object attribute memory through the data port, one byte per two
//! CPU cycles. The unit holds the CPU's ready line through the copy
//! and releases it at the end, so the core idles for the exact
//! transfer length of 513 cycles plus the odd-cycle alignment.

use demu_config::model::BootState;
use demu_core::device::{AccessWidth, BusAccess, MachineCtx};
use demu_core::frequency::{Address, Tick};
use demu_core::signal::Level;
use demu_core::triggers::TriggerEvent;
use demu_core::{Device, Snapshot, SnapshotError};

/// The OAM DMA unit of the RP2A03.
#[derive(Debug)]
pub struct NesOamDma {
    name: String,
    /// The bus the transfer reads from and writes to.
    bus_name: String,
    /// The high byte of the source page.
    page: u8,
    /// The byte index within the transfer.
    byte: u16,
    /// Domain ticks per CPU cycle.
    cpu_div: u64,
    /// The OAM data write the unit still owes.
    pending_write: Option<(u8, u8)>,
    /// True while the ready line is low.
    holding: bool,
    /// The tick of the next visit.
    next: Option<Tick>,
    last_tick: Option<Tick>,
}

impl NesOamDma {
    /// An OAM DMA unit from its configuration entry.
    pub fn new(device: &demu_config::model::DeviceInstance) -> Self {
        let cpu_div = u64::from(*device.dividers.get("cpu").unwrap_or(&12));
        let bus_name = device
            .params
            .get("bus")
            .and_then(|v| v.as_str())
            .unwrap_or("cpu")
            .to_string();
        Self {
            name: device.name.to_string(),
            bus_name,
            page: 0xFF,
            byte: 0,
            cpu_div,
            pending_write: None,
            holding: false,
            next: None,
            last_tick: None,
        }
    }
}

impl Device for NesOamDma {
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
        self.page = 0xFF;
        self.byte = 0;
        self.pending_write = None;
        self.holding = false;
        self.next = None;
        self.last_tick = None;
    }

    fn boot(&mut self, state: Option<&BootState>) {
        let _ = state;
        self.reset();
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(16);
        out.extend_from_slice(b"NODA");
        out.push(self.page);
        out.extend_from_slice(&self.byte.to_le_bytes());
        out.push(u8::from(self.holding));
        out.extend_from_slice(&self.cpu_div.to_le_bytes());
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        if data.len() != 16 || &data[..4] != b"NODA" {
            return Err(SnapshotError::Mismatch("bad nes oam dma chunk".into()));
        }
        self.page = data[4];
        self.byte = u16::from_le_bytes(data[5..7].try_into().expect("2 bytes"));
        self.holding = data[7] != 0;
        self.cpu_div = u64::from_le_bytes(data[8..16].try_into().expect("8 bytes")).clamp(1, 64);
        self.next = None;
        self.last_tick = None;
        Ok(())
    }

    fn notify_line(&mut self, _line: &str, _level: Level, _tick: Tick) {}

    fn next_event(&self) -> Option<Tick> {
        self.next
    }

    fn tick(&mut self, tick: Tick, ctx: &mut MachineCtx) {
        if self.last_tick.is_some_and(|t| t.0 >= tick.0) {
            return;
        }
        self.last_tick = Some(tick);
        if let Some((_byte, value)) = self.pending_write.take() {
            // The object data write lands one CPU cycle after the
            // source read, which closes the two-cycle byte period.
            ctx.write(
                &self.bus_name,
                AccessWidth::Byte,
                Address(0x2004),
                u32::from(value),
                Tick(tick.0 + self.cpu_div),
            );
            if self.byte >= 256 {
                ctx.emit(TriggerEvent::Dma(demu_core::triggers::DmaPhase::Complete));
                ctx.notify("oam_halt", Level::Low, tick);
                self.holding = false;
                self.next = None;
                return;
            }
            self.next = Some(Tick(tick.0 + 2 * self.cpu_div));
            return;
        }
        if self.byte >= 256 {
            self.next = None;
            return;
        }
        if !self.holding {
            // The ready line drops one alignment cycle after the
            // register write, before the first read.
            ctx.notify("oam_halt", Level::High, tick);
            self.holding = true;
            self.next = Some(Tick(tick.0 + self.cpu_div));
            return;
        }
        let source = (u16::from(self.page) << 8) | self.byte;
        let value = ctx.read(
            &self.bus_name,
            AccessWidth::Byte,
            Address(u32::from(source)),
            tick,
        ) as u8;
        self.pending_write = Some((self.byte as u8, value));
        self.byte += 1;
        self.next = Some(Tick(tick.0 + self.cpu_div));
    }
}

impl BusAccess for NesOamDma {
    fn read(&mut self, _width: AccessWidth, address: Address, _tick: Tick) -> u32 {
        if address.0 == 0x4014 {
            u32::from(self.page)
        } else {
            0xFF
        }
    }

    fn write(&mut self, _width: AccessWidth, address: Address, value: u32, tick: Tick) {
        if address.0 != 0x4014 {
            return;
        }
        self.page = value as u8;
        self.byte = 0;
        self.holding = false;
        // The transfer starts after the dummy read cycle, with the
        // extra alignment cycle on an odd CPU cycle.
        let cycle_parity = tick.0 / self.cpu_div % 2;
        self.next = Some(Tick(tick.0 + self.cpu_div * (1 + cycle_parity)));
        self.last_tick = None;
    }

    fn peek(&self, address: Address) -> u8 {
        if address.0 == 0x4014 { self.page } else { 0xFF }
    }

    fn decodes(&self, address: Address) -> bool {
        address.0 == 0x4014
    }
}
