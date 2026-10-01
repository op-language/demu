//! The SNES work RAM: 128 KiB spread over the banks `$7E`/`$7F`,
//! mirrored over the first 8 KiB of every bank, with the two-stage
//! port at `$2180-$2183`.
//!
//! The bus model: the device receives absolute addresses. The full
//! 128 KiB answers `$7E0000-$7FFFFF` directly; the 8 KiB mirrors
//! answer `$xxxx0000-$xxxx1FFF` in the low banks and the `$80`-copy
//! banks. The port's low byte's read = the RAM at the long address;
//! the middle bytes' = the address low/mid, the high byte's = write
//! to the long with the bank byte = `$00` when the port writes with
//! the bank = the direct form. The address in the port = a 17-bit
//! form: `(bank & 1) << 16 | short`.

use demu_config::model::BootState;
use demu_core::device::{AccessWidth, BusAccess, MachineCtx};
use demu_core::frequency::{Address, Tick};
use demu_core::signal::Level;
use demu_core::{Device, Snapshot, SnapshotError};

/// The device RAM of 128 KiB.
const SIZE: usize = 0x20000;

/// The port's page offset: $2180 & 0x3FF.
const PORT_BASE: u32 = 0x180;

/// 8 KiB.
const MIRROR_SIZE: u32 = 0x2000;

/// The work RAM with the port address in the 17-bit internal form.
#[derive(Debug)]
pub struct SnesWram {
    name: String,
    data: Vec<u8>,
    /// The long address of the port: bank-bit and lower 16 bits, in
    /// the 17-bit internal form (bit 16 = bank 7F's, else 7E's).
    port_addr: u32,
    next: Option<Tick>,
    last_tick: Option<Tick>,
}

impl SnesWram {
    /// A device from its configuration entry.
    pub fn new(device: &demu_config::model::DeviceInstance) -> Self {
        Self {
            name: device.name.to_string(),
            data: vec![0; SIZE],
            port_addr: 0,
            next: None,
            last_tick: None,
        }
    }

    /// Map one mapped address to a RAM slot, or None when the window
    /// does not answer.
    fn map(&self, a: u32) -> Option<usize> {
        // The main bank copy: $7E0000-$7FFFFF and $7F0000.
        if a & 0xFF0000 == 0x7E0000 {
            return usize::try_from(a & 0x1FFFF).ok();
        }
        // The low mirror's 8K at $xxxx0000-$xxxx1FFF, the same for
        // the $80-banks copy.
        let low = a & 0x1FFF;
        if low < MIRROR_SIZE {
            // The bank = the 8K mirror copy's page: slot = the bank's
            // low 8K of the RAM (the real form: the first 8K of WRAM
            // repeats in every bank).
            return usize::try_from(low).ok();
        }
        None
    }
}

impl BusAccess for SnesWram {
    fn read(&mut self, _width: AccessWidth, address: Address, _tick: Tick) -> u32 {
        // The port's read path: $2180-3.
        let page_off = address.0 & 0x3FF;
        if page_off == PORT_BASE {
            let slot = usize::try_from(self.port_addr & 0x1FFFF).unwrap_or(0);
            let b = self.data[slot];
            self.port_addr = (self.port_addr + 1) & 0x1FFFF;
            return u32::from(b);
        }
        u32::from(
            self.map(address.0)
                .and_then(|i| self.data.get(i))
                .copied()
                .unwrap_or(0),
        )
    }

    fn write(&mut self, _width: AccessWidth, address: Address, value: u32, _tick: Tick) {
        let page_off = address.0 & 0x3FF;
        let v = value as u8;
        if (0x180..=0x183).contains(&page_off) {
            // $2180 = data, $2181/2 = the address's lo/mid, $2183 =
            // the bank byte (only 7E/7F: the bit-16 = the bank 7F's).
            match page_off {
                0x180 => {
                    let slot = usize::try_from(self.port_addr & 0x1FFFF).unwrap_or(0);
                    self.data[slot] = v;
                    self.port_addr = (self.port_addr + 1) & 0x1FFFF;
                }
                _ => {
                    // $2181/2/3 = the low, middle, and bank bytes of
                    // the 17-bit long address; the bank byte writes
                    // only its bit 0 (the bank 7E/7F pick).
                    let shift = (page_off - 0x181) * 8;
                    let mask = !(0xFFu32 << shift) & 0x1FFFF;
                    self.port_addr = (self.port_addr & mask) | ((u32::from(v) << shift) & 0x1FFFF);
                }
            }
            return;
        }
        if let Some(slot) = self.map(address.0) {
            self.data[slot] = v;
        }
    }

    fn peek(&self, address: Address) -> u8 {
        self.map(address.0)
            .and_then(|i| self.data.get(i))
            .copied()
            .unwrap_or(0)
    }
}

impl Device for SnesWram {
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
        self.data.iter_mut().for_each(|b| *b = 0);
        self.port_addr = 0;
        self.next = None;
        self.last_tick = None;
    }

    fn boot(&mut self, state: Option<&BootState>) {
        self.reset();
        match state {
            Some(BootState::Ram { pattern }) => demu_core::boot::fill_ram(&mut self.data, *pattern),
            Some(BootState::Rom { path }) => {
                let bytes = std::fs::read(path).unwrap_or_default();
                let n = bytes.len().min(self.data.len());
                self.data[..n].copy_from_slice(&bytes[..n]);
            }
            Some(BootState::Hex { bytes }) => {
                let n = bytes.len().min(self.data.len());
                self.data[..n].copy_from_slice(&bytes[..n]);
            }
            _ => {}
        }
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(SIZE + 20);
        out.extend_from_slice(b"SWRM");
        out.push(1);
        out.extend_from_slice(&self.port_addr.to_le_bytes());
        out.extend_from_slice(&self.data);
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        let head = 4 + 1 + 4;
        if data.len() != head + SIZE || &data[..4] != b"SWRM" || data[4] != 1 {
            return Err(SnapshotError::Mismatch("bad snes wram chunk".into()));
        }
        self.port_addr = u32::from_le_bytes(data[5..9].try_into().expect("4 bytes"));
        self.data.copy_from_slice(&data[head..]);
        self.next = None;
        self.last_tick = None;
        Ok(())
    }

    fn notify_line(&mut self, _line: &str, _level: Level, _tick: Tick) {}

    fn next_event(&self) -> Option<Tick> {
        self.next
    }

    fn tick(&mut self, _tick: Tick, _ctx: &mut MachineCtx) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use demu_config::newtypes::{DeviceName, KindName};

    fn device() -> demu_config::model::DeviceInstance {
        demu_config::model::DeviceInstance {
            name: DeviceName::new("wram").expect("name"),
            kind: KindName::new("snes.wram").expect("kind"),
            domain: "main".to_string(),
            params: std::collections::BTreeMap::new(),
            dividers: [("cpu".to_string(), 6)].into_iter().collect(),
        }
    }

    fn wram() -> SnesWram {
        SnesWram::new(&device())
    }

    #[test]
    fn banks_answer_through_the_map() {
        let mut w = wram();
        w.write(AccessWidth::Byte, Address(0x7E0000), 0x11, Tick(0));
        w.write(AccessWidth::Byte, Address(0x7E0023), 0x22, Tick(0));
        w.write(AccessWidth::Byte, Address(0x7F1234), 0x33, Tick(0));
        assert_eq!(w.peek(Address(0x7E0000)), 0x11);
        assert_eq!(w.peek(Address(0x7E0023)), 0x22);
        assert_eq!(w.peek(Address(0x7F1234)), 0x33);
    }

    #[test]
    fn low_mirrors_repeat_the_first_8k() {
        let mut w = wram();
        w.write(AccessWidth::Byte, Address(0x000002), 0x44, Tick(0));
        assert_eq!(w.peek(Address(0x000002)), 0x44);
        assert_eq!(
            w.peek(Address(0x7E0002)),
            0x44,
            "the mirror is the same RAM"
        );
        // The bank mirror repeats at $800002.
        assert_eq!(w.peek(Address(0x800002)), 0x44);
    }

    #[test]
    fn port_write_writes_and_increments() {
        let mut w = wram();
        // Set the address $7E:0100 via the port bytes.
        w.write(AccessWidth::Byte, Address(0x002181), 0x00, Tick(0));
        w.write(AccessWidth::Byte, Address(0x002182), 0x01, Tick(0));
        w.write(AccessWidth::Byte, Address(0x002183), 0x00, Tick(0));
        w.write(AccessWidth::Byte, Address(0x002180), 0x55, Tick(0));
        assert_eq!(w.peek(Address(0x7E0100)), 0x55);
        // The next write lands at $0101.
        w.write(AccessWidth::Byte, Address(0x002180), 0x66, Tick(0));
        assert_eq!(w.peek(Address(0x7E0101)), 0x66);
        // The read path walks the same pointer: after the two writes
        // the pointer sits at $0102, so the port's first read answers
        // that slot, then the pointer advances.
        w.write(AccessWidth::Byte, Address(0x7E0102), 0x77, Tick(0));
        assert_eq!(w.read(AccessWidth::Byte, Address(0x002180), Tick(0)), 0x77);
        w.write(AccessWidth::Byte, Address(0x7E0103), 0x88, Tick(0));
        assert_eq!(w.read(AccessWidth::Byte, Address(0x002180), Tick(0)), 0x88);
    }

    #[test]
    fn snapshot_round_trip() {
        let mut w = wram();
        w.write(AccessWidth::Byte, Address(0x7E0100), 0x77, Tick(0));
        let snap = w.snapshot();
        let mut twin = wram();
        twin.restore(&snap).expect("restore");
        assert_eq!(twin.snapshot(), snap);
    }
}
