//! The plain RAM and mask-ROM devices of the Atari machine families.
//!
//! The `atari.ram` kind is read-write storage: the main RAM of the
//! 5200 and the 7800. The `atari.rom` kind is read-only storage: the
//! BIOS of the 5200 and the 7800. Both take the storage size from the
//! `size` parameter in bytes and wrap bus accesses into that range,
//! so the window geometry stays in the configuration.

use demu_config::model::BootState;
use demu_core::device::{AccessWidth, BusAccess};
use demu_core::frequency::{Address, Tick};
use demu_core::signal::Level;
use demu_core::{Device, Snapshot, SnapshotError};

/// A static RAM or mask-ROM device with a flat storage range.
pub struct StaticRam {
    name: String,
    data: Vec<u8>,
    readonly: bool,
}

impl StaticRam {
    /// A device from its configuration entry.
    ///
    /// The `size` parameter gives the storage in bytes and defaults to
    /// `0x0800`. The `readonly` parameter makes the chip a mask ROM,
    /// which is what the `atari.rom` kind selects.
    pub fn new(
        device: &demu_config::model::DeviceInstance,
    ) -> Result<Self, demu_core::machine::MachineError> {
        let size = device
            .params
            .get("size")
            .and_then(|v| v.as_integer())
            .ok_or_else(|| {
                demu_core::machine::MachineError::Assembly(format!(
                    "device '{}' of kind '{}' needs a size parameter",
                    device.name.as_ref(),
                    device.kind.as_str()
                ))
            })?;
        let size = usize::try_from(size).map_err(|_| {
            demu_core::machine::MachineError::Assembly(format!(
                "device '{}' has a negative size parameter",
                device.name.as_ref()
            ))
        })?;
        let readonly = device
            .params
            .get("readonly")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        Ok(Self {
            name: device.name.to_string(),
            data: vec![0; size],
            readonly,
        })
    }

    /// Force the read-only state for a kind-level constructor.
    pub fn set_readonly(&mut self, readonly: bool) {
        self.readonly = readonly;
    }

    /// The storage index of a bus address.
    fn map(&self, address: u32) -> usize {
        address as usize % self.data.len().max(1)
    }
}

impl BusAccess for StaticRam {
    fn read(&mut self, _width: AccessWidth, address: Address, _tick: Tick) -> u32 {
        u32::from(self.data[self.map(address.0)])
    }

    fn write(&mut self, _width: AccessWidth, address: Address, value: u32, _tick: Tick) {
        if self.readonly {
            return;
        }
        let index = self.map(address.0);
        self.data[index] = value as u8;
    }

    fn peek(&self, address: Address) -> u8 {
        self.data[self.map(address.0)]
    }
}

impl Device for StaticRam {
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
        let mut out = Vec::with_capacity(self.data.len() + 5);
        out.extend_from_slice(b"SRAM");
        out.push(1);
        out.push(u8::from(self.readonly));
        out.extend_from_slice(&self.data);
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        let expected = 4 + 1 + 1 + self.data.len();
        if data.len() != expected || &data[..4] != b"SRAM" || data[4] != 1 {
            return Err(SnapshotError::Mismatch(format!(
                "static RAM chunk must be {expected} bytes"
            )));
        }
        self.readonly = data[5] != 0;
        self.data.copy_from_slice(&data[6..]);
        Ok(())
    }

    fn notify_line(&mut self, _line: &str, _level: Level, _tick: Tick) {}

    fn tick(&mut self, _tick: Tick, _ctx: &mut demu_core::MachineCtx) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instance(kind: &str, size: i64) -> demu_config::model::DeviceInstance {
        let mut params = std::collections::BTreeMap::new();
        params.insert("size".to_string(), toml::Value::Integer(size));
        if kind == "atari.rom" {
            params.insert("readonly".to_string(), toml::Value::Boolean(true));
        }
        demu_config::model::DeviceInstance {
            name: demu_config::newtypes::DeviceName::new("ram0").expect("name"),
            kind: demu_config::newtypes::KindName::new(kind).expect("kind"),
            domain: "main".to_string(),
            params,
            dividers: std::collections::BTreeMap::new(),
        }
    }

    #[test]
    fn reads_and_writes_within_size() {
        let device = instance("atari.ram", 0x800);
        let mut ram = StaticRam::new(&device).expect("ram");
        ram.write(AccessWidth::Byte, Address(0x123), 0x42, Tick(0));
        assert_eq!(ram.read(AccessWidth::Byte, Address(0x123), Tick(0)), 0x42);
        // The access wraps into the storage range.
        assert_eq!(ram.read(AccessWidth::Byte, Address(0x923), Tick(0)), 0x42);
    }

    #[test]
    fn rom_kind_ignores_writes() {
        let device = instance("atari.rom", 0x800);
        let mut rom = StaticRam::new(&device).expect("rom");
        assert!(rom.readonly);
        rom.write(AccessWidth::Byte, Address(0x123), 0x42, Tick(0));
        assert_eq!(rom.read(AccessWidth::Byte, Address(0x123), Tick(0)), 0);
    }

    #[test]
    fn ram_pattern_boot_fills() {
        let device = instance("atari.ram", 0x400);
        let mut ram = StaticRam::new(&device).expect("ram");
        ram.boot(Some(&BootState::Ram {
            pattern: demu_config::model::RamPattern::Random { seed: 7 },
        }));
        assert!(ram.data.iter().any(|b| *b != 0));
    }

    #[test]
    fn hex_boot_copies_prefix() {
        let device = instance("atari.rom", 0x800);
        let mut rom = StaticRam::new(&device).expect("rom");
        rom.boot(Some(&BootState::Hex {
            bytes: vec![1, 2, 3, 4],
        }));
        assert_eq!(rom.peek(Address(0)), 1);
        assert_eq!(rom.peek(Address(3)), 4);
        assert_eq!(rom.peek(Address(4)), 0);
    }

    #[test]
    fn snapshot_round_trip() {
        let device = instance("atari.ram", 0x400);
        let mut ram = StaticRam::new(&device).expect("ram");
        ram.write(AccessWidth::Byte, Address(0x10), 0x5A, Tick(0));
        let snapshot = ram.snapshot();
        let mut twin = StaticRam::new(&instance("atari.ram", 0x400)).expect("ram");
        twin.restore(&snapshot).expect("restore");
        assert_eq!(twin.snapshot(), snapshot);
    }
}
