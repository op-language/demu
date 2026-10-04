//! The Lynx memory devices: the 512-byte boot ROM of the Mikey chip
//! and the memory-map control register.
//!
//! The Mikey chip's MAPCTL register at $FFF9 wires the address space:
//! a set bit moves its space to the RAM underneath. The CPU core reads
//! the register back at every instruction boundary and drives the
//! overlay lines over the declared connections, so the register stays
//! with the Mikey side and the line state carries to every device.
//! The work RAM itself stays a plain block; the overlay devices gate
//! their own address decodes.

use demu_config::model::BootState;
use demu_core::device::{AccessWidth, BusAccess};
use demu_core::frequency::{Address, Tick};
use demu_core::signal::Level;
use demu_core::{Device, Snapshot, SnapshotError};

/// The 512-byte boot ROM of the Mikey chip.
///
/// The ROM answers $FE00-$FFFF while its space maps it: the ROM slice
/// $FE00-$FFF7 hides when the ROM-space gate is high, the vector slice
/// $FFFA-$FFFF hides when the vector-space gate is high, and the two
/// map-control bytes $FFF8-$FFF9 never come from the ROM. Writes into
/// the window are dropped; the work RAM keeps them from its own
/// window below.
pub struct LynxBootrom {
    name: String,
    data: Vec<u8>,
    /// The ROM slice $FE00-$FFF7 shows its RAM instead.
    rom_off: bool,
    /// The vector slice $FFFA-$FFFF shows its RAM instead.
    vec_off: bool,
}

impl LynxBootrom {
    /// A boot ROM from its configuration entry.
    pub fn new(device: &demu_config::model::DeviceInstance) -> Self {
        Self {
            name: device.name.to_string(),
            data: Vec::new(),
            rom_off: false,
            vec_off: false,
        }
    }
}

impl BusAccess for LynxBootrom {
    fn read(&mut self, _width: AccessWidth, address: Address, _tick: Tick) -> u32 {
        self.peek(address).into()
    }

    fn write(&mut self, _width: AccessWidth, _address: Address, _value: u32, _tick: Tick) {}

    fn peek(&self, address: Address) -> u8 {
        (address.0 as usize)
            .checked_sub(0xFE00)
            .and_then(|o| self.data.get(o).copied())
            .unwrap_or(0xFF)
    }

    fn decodes(&self, address: Address) -> bool {
        match address.0 {
            0xFE00..=0xFFF7 => !self.rom_off,
            0xFFFA..=0xFFFF => !self.vec_off,
            // $FFF8-$FFF9 always belong to the map-control register.
            _ => false,
        }
    }
}

impl Device for LynxBootrom {
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
        self.rom_off = false;
        self.vec_off = false;
    }

    fn boot(&mut self, state: Option<&BootState>) {
        self.reset();
        match state {
            Some(BootState::Rom { path }) => {
                let _ = demu_core::boot::load_rom(&mut self.data, std::path::Path::new(path));
            }
            Some(BootState::Hex { bytes }) => {
                self.data = bytes.clone();
            }
            _ => {}
        }
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(self.data.len() + 6);
        out.extend_from_slice(b"LNBT");
        out.push(1);
        out.push(u8::from(self.rom_off) | (u8::from(self.vec_off) << 1));
        out.extend_from_slice(&self.data);
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        if data.len() < 6 || &data[..4] != b"LNBT" || data[4] != 1 {
            return Err(SnapshotError::Mismatch(
                "Lynx boot ROM chunk too short".into(),
            ));
        }
        self.rom_off = data[5] & 0x01 != 0;
        self.vec_off = data[5] & 0x02 != 0;
        self.data = data[6..].to_vec();
        Ok(())
    }

    fn notify_line(&mut self, line: &str, level: Level, _tick: Tick) {
        let high = level == Level::High;
        match line {
            "rom_ram" => self.rom_off = high,
            "vec_ram" => self.vec_off = high,
            _ => {}
        }
    }

    fn tick(&mut self, _tick: Tick, _ctx: &mut demu_core::MachineCtx) {}
}

/// The memory-map control register of the Mikey chip.
///
/// The register answers $FFF8-$FFF9 with the read priority above the
/// boot ROM slice. A read of $FFF9 returns the stored control byte;
/// $FFF8 is reserved and reads zero. A write to $FFF9 stores the byte;
/// the CPU core reads the byte back at its next instruction boundary
/// and drives the overlay lines from it, so this device holds register
/// state only.
pub struct LynxMapCtl {
    name: String,
    ctl: u8,
}

impl LynxMapCtl {
    /// A map-control register from its configuration entry.
    pub fn new(device: &demu_config::model::DeviceInstance) -> Self {
        Self {
            name: device.name.to_string(),
            ctl: 0,
        }
    }

    /// The stored MAPCTL byte.
    pub fn ctl(&self) -> u8 {
        self.ctl
    }
}

impl BusAccess for LynxMapCtl {
    fn read(&mut self, _width: AccessWidth, address: Address, _tick: Tick) -> u32 {
        u32::from(self.peek(address))
    }

    fn write(&mut self, _width: AccessWidth, address: Address, value: u32, _tick: Tick) {
        if address.0 == 0xFFF9 {
            self.ctl = value as u8;
        }
    }

    fn peek(&self, address: Address) -> u8 {
        match address.0 {
            0xFFF9 => self.ctl,
            _ => 0,
        }
    }
}

impl Device for LynxMapCtl {
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
        self.ctl = 0;
    }

    fn boot(&mut self, _state: Option<&BootState>) {
        // The register resets to the boot form: every space maps to
        // its hardware or ROM.
        self.ctl = 0;
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(6);
        out.extend_from_slice(b"LNMP");
        out.push(1);
        out.push(self.ctl);
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        if data.len() != 6 || &data[..4] != b"LNMP" || data[4] != 1 {
            return Err(SnapshotError::Mismatch(
                "Lynx map-control chunk must be 6 bytes".into(),
            ));
        }
        self.ctl = data[5];
        Ok(())
    }

    fn notify_line(&mut self, _line: &str, _level: Level, _tick: Tick) {}

    fn tick(&mut self, _tick: Tick, _ctx: &mut demu_core::MachineCtx) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instance(kind: &str) -> demu_config::model::DeviceInstance {
        use demu_config::newtypes::{DeviceName, KindName};
        demu_config::model::DeviceInstance {
            name: DeviceName::new("mem0").expect("name"),
            kind: KindName::new(kind).expect("kind"),
            domain: "main".to_string(),
            params: std::collections::BTreeMap::new(),
            dividers: std::collections::BTreeMap::new(),
        }
    }

    #[test]
    fn bootrom_gates_per_space() {
        let mut rom = LynxBootrom::new(&instance("lynx.bootrom"));
        rom.boot(Some(&BootState::Hex {
            bytes: (0..512).map(|i| (i % 251) as u8).collect(),
        }));
        assert!(rom.decodes(Address(0xFE00)), "the ROM space maps at reset");
        assert!(
            rom.decodes(Address(0xFFFC)),
            "the vectors come from the ROM"
        );
        assert_eq!(
            rom.peek(Address(0xFFF9)),
            3,
            "the image byte stays, but the decode excludes $FFF9"
        );
        assert!(!rom.decodes(Address(0xFFF9)), "$FFF9 never decodes");
        assert_eq!(rom.peek(Address(0xFE00)), 0);
        assert!(
            !rom.decodes(Address(0xFFF8)),
            "the map bytes stay with Mikey"
        );
        rom.notify_line("rom_ram", Level::High, Tick(0));
        assert!(!rom.decodes(Address(0xFE00)), "the ROM space moved to RAM");
        assert!(
            rom.decodes(Address(0xFFFC)),
            "the vector slice keeps its gate"
        );
        rom.notify_line("vec_ram", Level::High, Tick(0));
        assert!(!rom.decodes(Address(0xFFFC)), "the vector RAM visible");
        assert_eq!(rom.peek(Address(0xFE00)), 0, "the bytes stay for peek");
        assert_eq!(rom.read(AccessWidth::Byte, Address(0xFE00), Tick(0)), 0);
    }

    #[test]
    fn bootrom_writes_are_dropped() {
        let mut rom = LynxBootrom::new(&instance("lynx.bootrom"));
        rom.boot(Some(&BootState::Hex {
            bytes: vec![0x42; 512],
        }));
        rom.write(AccessWidth::Byte, Address(0xFE10), 0xFF, Tick(0));
        assert_eq!(rom.peek(Address(0xFE10)), 0x42);
        assert!(
            rom.decodes(Address(0xFE10)),
            "the mapped ROM keeps answering"
        );
    }

    #[test]
    fn mapctl_register_round_trip() {
        let mut map = LynxMapCtl::new(&instance("lynx.mapctl"));
        assert_eq!(map.peek(Address(0xFFF9)), 0);
        map.write(AccessWidth::Byte, Address(0xFFF9), 0x0F, Tick(0));
        assert_eq!(map.peek(Address(0xFFF9)), 0x0F);
        assert_eq!(map.peek(Address(0xFFF8)), 0, "the reserved byte reads zero");
        map.write(AccessWidth::Byte, Address(0xFFF8), 0xFF, Tick(0));
        assert_eq!(map.peek(Address(0xFFF8)), 0);
        let saved = map.snapshot();
        let mut twin = LynxMapCtl::new(&instance("lynx.mapctl"));
        twin.restore(&saved).expect("restore");
        assert_eq!(twin.ctl(), 0x0F);
    }

    #[test]
    fn snapshots_round_trip() {
        let rom = LynxBootrom::new(&instance("lynx.bootrom"));
        let saved = rom.snapshot();
        let mut twin = LynxBootrom::new(&instance("lynx.bootrom"));
        twin.restore(&saved).expect("restore");
        assert_eq!(twin.snapshot(), saved);
        assert!(twin.restore(&[0; 5]).is_err());
        let mut rom = LynxBootrom::new(&instance("lynx.bootrom"));
        rom.boot(Some(&BootState::Hex {
            bytes: (0..512u32).map(|i| (i % 251) as u8).collect(),
        }));
        rom.notify_line("vec_ram", Level::High, Tick(0));
        let saved = rom.snapshot();
        let mut twin = LynxBootrom::new(&instance("lynx.bootrom"));
        twin.restore(&saved).expect("restore");
        assert_eq!(twin.snapshot(), saved);
        assert!(!twin.decodes(Address(0xFFFC)));
        assert!(twin.decodes(Address(0xFE00)));
    }
}
