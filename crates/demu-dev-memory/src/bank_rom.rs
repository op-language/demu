//! The two-page banked ROM of the ZX Spectrum 128 (`z80.bankrom`).
//!
//! The device holds two equal pages and answers one window of the cpu
//! bus; the `select` line switches the page. The `zx.bankram`
//! companion drives the line over the machine connections: its paging
//! port routes the `rom_bank` line into this line, so bit 4 of the
//! paging latch picks the upper page. A reset picks the lower page.
//! The ROM ignores writes.
//!
//! # Parameters and boot
//!
//! The `size` parameter gives the storage in bytes, split into two
//! equal pages; 32768 by default. The constructor rejects a size
//! under two bytes and an odd size. A `Rom` boot state reads an image
//! file, a `Hex` boot state copies its bytes, and both clear the
//! storage and fill from offset 0, so an image shorter than the
//! storage leaves the tail zeroed.
//!
//! # Save state
//!
//! The save state rides the `Z8BR` tag, version 1, with a fixed
//! length: the page flag and the data bytes. A restore validates the
//! length, the tag, and the version.

use demu_config::model::BootState;
use demu_core::device::{AccessWidth, BusAccess};
use demu_core::frequency::{Address, Tick};
use demu_core::machine::MachineError;
use demu_core::signal::Level;
use demu_core::{Device, Snapshot, SnapshotError};

/// The two-page banked ROM.
#[derive(Debug)]
pub struct Z80BankRom {
    name: String,
    /// The ROM image in page order: the lower page first.
    data: Vec<u8>,
    /// The selected page; false is the lower page and true the upper
    /// page.
    page: bool,
}

impl Z80BankRom {
    /// A banked ROM from its configuration entry.
    ///
    /// # Errors
    ///
    /// Returns an error when the `size` parameter is negative, odd, or
    /// under two bytes.
    pub fn new(device: &demu_config::model::DeviceInstance) -> Result<Self, MachineError> {
        let size = device
            .params
            .get("size")
            .and_then(|v| v.as_integer())
            .unwrap_or(32768);
        let size = usize::try_from(size).map_err(|_| {
            MachineError::Assembly(format!(
                "device '{}' has a negative size parameter",
                device.name.as_ref()
            ))
        })?;
        if size < 2 || size % 2 != 0 {
            return Err(MachineError::Assembly(format!(
                "device '{}' of kind '{}' needs an even size of two pages, got {}",
                device.name.as_ref(),
                device.kind.as_str(),
                size
            )));
        }
        Ok(Self {
            name: device.name.to_string(),
            data: vec![0; size],
            page: false,
        })
    }

    /// The byte count of one page.
    fn page_size(&self) -> usize {
        self.data.len() / 2
    }

    /// The storage index of one address: the page offset plus the
    /// selected page. The address wraps inside the page.
    fn map(&self, address: u32) -> usize {
        let page = usize::from(self.page);
        page * self.page_size() + (address as usize % self.page_size())
    }
}

impl BusAccess for Z80BankRom {
    fn read(&mut self, _width: AccessWidth, address: Address, _tick: Tick) -> u32 {
        u32::from(self.data[self.map(address.0)])
    }

    fn write(&mut self, _width: AccessWidth, _address: Address, _value: u32, _tick: Tick) {}

    fn peek(&self, address: Address) -> u8 {
        self.data[self.map(address.0)]
    }
}

impl Device for Z80BankRom {
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
        // The reset picks the lower page; the image stays.
        self.page = false;
    }

    fn boot(&mut self, state: Option<&BootState>) {
        self.reset();
        match state {
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
        let mut out = Vec::with_capacity(4 + 1 + 1 + self.data.len());
        out.extend_from_slice(b"Z8BR");
        out.push(1);
        out.push(u8::from(self.page));
        out.extend_from_slice(&self.data);
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        let expected = 4 + 1 + 1 + self.data.len();
        if data.len() != expected || &data[..4] != b"Z8BR" || data[4] != 1 {
            return Err(SnapshotError::Mismatch(format!(
                "z80 bank ROM chunk must be {expected} bytes"
            )));
        }
        self.page = data[5] != 0;
        self.data.copy_from_slice(&data[6..]);
        Ok(())
    }

    fn notify_line(&mut self, line: &str, level: Level, _tick: Tick) {
        if line == "select" {
            // High selects the upper page and low the lower page.
            self.page = level == Level::High;
        }
    }

    fn tick(&mut self, _tick: Tick, _ctx: &mut demu_core::MachineCtx) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use demu_config::model::{Bus as CBus, ClockDomain, Window as CWindow};
    use demu_config::newtypes::{DeviceName, KindName};
    use demu_core::machine::Machine;
    use std::collections::BTreeMap;

    fn instance(size: i64) -> demu_config::model::DeviceInstance {
        let mut params = BTreeMap::new();
        params.insert("size".to_string(), toml::Value::Integer(size));
        demu_config::model::DeviceInstance {
            name: DeviceName::new("rom0").expect("name"),
            kind: KindName::new("z80.bankrom").expect("kind"),
            domain: "main".to_string(),
            params,
            dividers: BTreeMap::new(),
        }
    }

    fn filled_rom() -> Z80BankRom {
        let mut rom = Z80BankRom::new(&instance(4)).expect("rom");
        rom.boot(Some(&BootState::Hex {
            bytes: vec![0x11, 0x22, 0x33, 0x44],
        }));
        rom
    }

    fn entry_name(name: &str) -> DeviceName {
        DeviceName::new(name).expect("name")
    }

    fn window(name: &str, bus: &str, device: &str, base: u32, size: u32) -> CWindow {
        CWindow {
            name: name.to_string(),
            bus: bus.to_string(),
            device: entry_name(device),
            base,
            size,
            kind: None,
            mirror: None,
            overlay: None,
            overlay_read_priority: None,
            overlay_write_priority: None,
            conflict: None,
        }
    }

    /// The bank ROM test machine: the four-byte ROM behind a one-page
    /// window at `$0000` and the CPU shim. The select line picks the
    /// page that answers the window.
    fn bank_rom_machine() -> Machine {
        let rom = instance(4);
        let cp = demu_config::model::DeviceInstance {
            name: entry_name("cp"),
            kind: KindName::new("smoke.cpu").expect("kind"),
            domain: "main".to_string(),
            params: BTreeMap::new(),
            dividers: BTreeMap::new(),
        };
        let mut clocks = BTreeMap::new();
        clocks.insert(
            "main".to_string(),
            ClockDomain {
                name: "main".to_string(),
                frequency: demu_config::newtypes::Frequency::new(3_500_000, 1).expect("freq"),
                origin: "the machine master".to_string(),
            },
        );
        let mut by_name = BTreeMap::new();
        by_name.insert(rom.name.clone(), 0usize);
        by_name.insert(cp.name.clone(), 1usize);
        let mut boot = BTreeMap::new();
        boot.insert(
            rom.name.clone(),
            BootState::Hex {
                bytes: vec![0x11, 0x22, 0x33, 0x44],
            },
        );
        let config = demu_config::model::SystemConfig {
            name: "bankrom-test".to_string(),
            triple: "test".to_string(),
            description: "the ZX bank ROM test machine".to_string(),
            clocks,
            devices: vec![rom, cp],
            devices_by_name: by_name,
            buses: vec![CBus {
                name: "cpu".to_string(),
                address_bits: 16,
                owner: None,
                parent: None,
            }],
            windows: vec![window("romw", "cpu", "rom0", 0x0000, 0x0002)],
            connections: Vec::new(),
            signals: Vec::new(),
            boot,
            quirks: BTreeMap::new(),
        };
        let mut ctors = demu_core::smoke_ctors();
        ctors.register(
            "z80.bankrom",
            Box::new(|d, _| Ok(Box::new(Z80BankRom::new(d)?))),
        );
        let mut machine = Machine::assemble(config, &ctors).expect("assembly");
        machine.boot();
        machine
    }

    fn rom_of(machine: &mut Machine) -> &mut Z80BankRom {
        machine
            .device_by_name_mut("rom0")
            .expect("device")
            .as_any_mut()
            .downcast_mut::<Z80BankRom>()
            .expect("the bank ROM")
    }

    #[test]
    fn pages_read_by_the_select_line() {
        let mut rom = filled_rom();
        assert_eq!(rom.peek(Address(0)), 0x11, "the lower page first");
        assert_eq!(rom.peek(Address(1)), 0x22);
        rom.notify_line("select", Level::High, Tick(0));
        assert_eq!(rom.peek(Address(0)), 0x33, "the upper page");
        assert_eq!(rom.peek(Address(1)), 0x44);
        rom.notify_line("select", Level::Low, Tick(0));
        assert_eq!(rom.peek(Address(0)), 0x11, "back on the lower page");
    }

    #[test]
    fn reads_wrap_inside_the_page() {
        let mut rom = filled_rom();
        assert_eq!(rom.peek(Address(0)), 0x11);
        assert_eq!(rom.peek(Address(2)), 0x11, "the page offset wraps");
        assert_eq!(rom.peek(Address(5)), 0x22, "5 wraps onto the page tail");
        rom.notify_line("select", Level::High, Tick(0));
        assert_eq!(rom.peek(Address(0)), 0x33, "the upper page");
        assert_eq!(rom.peek(Address(3)), 0x44, "3 wraps onto the page-1 tail");
    }

    #[test]
    fn reset_selects_page_0() {
        let mut rom = filled_rom();
        rom.notify_line("select", Level::High, Tick(0));
        assert_eq!(rom.peek(Address(0)), 0x33);
        rom.reset();
        assert_eq!(rom.peek(Address(0)), 0x11, "the reset picks the lower page");
        assert_eq!(
            rom.peek(Address(1)),
            0x22,
            "the image stays across the reset"
        );
    }

    #[test]
    fn writes_are_ignored() {
        let mut rom = filled_rom();
        rom.write(AccessWidth::Byte, Address(0x0001), 0xAA, Tick(0));
        assert_eq!(rom.peek(Address(1)), 0x22);
        assert!(
            rom.bus().expect("bus").decodes(Address(0x0001)),
            "the chip answers"
        );
    }

    #[test]
    fn hex_boot_fills_from_offset_0() {
        let mut rom = Z80BankRom::new(&instance(4)).expect("rom");
        rom.boot(Some(&BootState::Hex { bytes: vec![9, 8] }));
        assert_eq!(rom.peek(Address(0)), 9, "the fill starts at the lower page");
        assert_eq!(rom.peek(Address(1)), 8);
        rom.notify_line("select", Level::High, Tick(0));
        assert_eq!(rom.peek(Address(0)), 0, "the upper page stays zeroed");
        assert_eq!(rom.peek(Address(1)), 0);
    }

    #[test]
    fn the_size_needs_two_pages() {
        assert!(Z80BankRom::new(&instance(-1)).is_err(), "a negative size");
        assert!(
            Z80BankRom::new(&instance(0)).is_err(),
            "a size under two bytes"
        );
        assert!(Z80BankRom::new(&instance(3)).is_err(), "an odd size");
        assert!(Z80BankRom::new(&instance(16)).is_ok(), "any even byte size");
    }

    #[test]
    fn snapshot_round_trips() {
        let mut rom = filled_rom();
        rom.notify_line("select", Level::High, Tick(0));
        let snapshot = rom.snapshot();
        assert_eq!(snapshot.len(), 4 + 1 + 1 + 4);
        let mut twin = Z80BankRom::new(&instance(4)).expect("rom");
        twin.restore(&snapshot).expect("restore");
        assert_eq!(twin.snapshot(), snapshot);
        assert_eq!(twin.peek(Address(0)), 0x33, "the upper page stays");
    }

    #[test]
    fn restore_rejects_malformed_chunks() {
        let rom = filled_rom();
        let snapshot = rom.snapshot();
        let mut twin = Z80BankRom::new(&instance(4)).expect("rom");
        assert!(
            twin.restore(&snapshot[..snapshot.len() - 1]).is_err(),
            "a short body"
        );
        assert!(twin.restore(&[0; 7]).is_err(), "a long body");
        let mut retag = snapshot.clone();
        retag[2] = b'X';
        assert!(twin.restore(&retag).is_err(), "a wrong tag");
        let mut reversion = snapshot.clone();
        reversion[4] = 2;
        assert!(twin.restore(&reversion).is_err(), "a wrong version");
    }

    #[test]
    fn the_machine_window_reads_through_the_page() {
        let mut m = bank_rom_machine();
        assert_eq!(m.peek("cpu", Address(0x0000)), 0x11, "page 0 at reset");
        assert_eq!(m.peek("cpu", Address(0x0001)), 0x22);
        rom_of(&mut m).notify_line("select", Level::High, Tick(0));
        assert_eq!(
            m.peek("cpu", Address(0x0000)),
            0x33,
            "page 1 after the line"
        );
        assert_eq!(m.peek("cpu", Address(0x0001)), 0x44);
        rom_of(&mut m).notify_line("select", Level::Low, Tick(0));
        assert_eq!(m.peek("cpu", Address(0x0000)), 0x11, "page 0 again");
    }
}
