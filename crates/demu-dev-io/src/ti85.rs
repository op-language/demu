//! The TI-85 memory controller.
//!
//! One device carries the 128 KiB ROM, the 32 KiB RAM, and the three
//! paging latches of the machine. The published page mapping: the span
//! `$0000-$3FFF` shows the ROM page that the io port 8 selects, the
//! span `$4000-$7FFF` shows ROM page 0 fixed, the span `$8000-$BFFF`
//! shows the RAM bank that the io port 6 selects, and the span
//! `$C000-$FFFF` shows the RAM bank that the io port 7 selects. A
//! latch value wraps modulo the page or bank count.
//!
//! # Port and memory decode
//!
//! The io ports 6, 7, and 8 sit at the io bus addresses 6 through 8.
//! The io port 5 belongs to the link device of the machine and stays
//! here out of scope. The device answers the port addresses 6 to 8 as
//! the register latches on every bus, and the published memory windows
//! carry the rest. The configuration therefore leaves the span
//! `$0006-$0008` to the io windows, and a plain memory access at those
//! three addresses cannot surface the ROM image bytes. This is the
//! recorded limitation of the one-decode device form: the demu bus
//! model passes the address alone to a device, so the same address on
//! the io bus and on the memory bus cannot answer two different
//! owners.
//!
//! # Boot and save state
//!
//! The boot state fills the ROM the `StaticRam` way: a `Rom` state
//! reads the file and keeps a missing file silent, a `Hex` state
//! copies its prefix, and any unused tail stays zero. The RAM boots
//! zeroed. The save state rides the `TIMM` tag, version 1, and
//! includes the three latches, the whole RAM, and the whole ROM image,
//! so the save state reproduces the machine byte for byte including
//! the boot content.

use demu_config::model::BootState;
use demu_core::device::{AccessWidth, BusAccess};
use demu_core::frequency::{Address, Tick};
use demu_core::signal::Level;
use demu_core::{Device, Snapshot, SnapshotError};

/// The ROM size: eight pages of 16 KiB.
pub const ROM_LEN: usize = 0x20000;
/// The RAM size: two banks of 16 KiB.
pub const RAM_LEN: usize = 0x8000;
/// The save state chunk length: tag, version, three latches, RAM, and
/// the whole ROM image.
const SNAPSHOT_LEN: usize = 4 + 1 + 3 + RAM_LEN + ROM_LEN;

/// The TI-85 memory controller: ROM pages, RAM banks, and the latches
/// of the io ports 6 to 8.
#[derive(Debug)]
pub struct Ti85Mem {
    name: String,
    /// The 128 KiB ROM, eight pages of 16 KiB.
    rom: Box<[u8; ROM_LEN]>,
    /// The 32 KiB RAM, two banks of 16 KiB.
    ram: Box<[u8; RAM_LEN]>,
    /// The io port 8 latch: the ROM page of `$0000-$3FFF` (reset 0).
    pub page8: u8,
    /// The io port 6 latch: the RAM bank of `$8000-$BFFF` (reset 1).
    pub bank6: u8,
    /// The io port 7 latch: the RAM bank of `$C000-$FFFF` (reset 0).
    pub bank7: u8,
}

impl Ti85Mem {
    /// A device from its configuration entry.
    pub fn new(device: &demu_config::model::DeviceInstance) -> Self {
        Self {
            name: device.name.to_string(),
            rom: vec![0; ROM_LEN].try_into().expect("128 KiB ROM"),
            ram: vec![0; RAM_LEN].try_into().expect("32 KiB RAM"),
            page8: 0,
            bank6: 1,
            bank7: 0,
        }
    }

    /// The storage byte of one memory address under the current latches.
    ///
    /// The latch values wrap modulo the page and bank counts: the ROM
    /// page wraps at 8 and the RAM banks wrap at 2.
    fn mem_byte(&self, address: Address) -> u8 {
        let a = address.0 & 0xFFFF;
        match a {
            0x0000..=0x3FFF => self.rom[(usize::from(self.page8) % 8) * 0x4000 + a as usize],
            0x4000..=0x7FFF => self.rom[(a - 0x4000) as usize],
            0x8000..=0xBFFF => {
                self.ram[(usize::from(self.bank6) % 2) * 0x4000 + (a - 0x8000) as usize]
            }
            0xC000..=0xFFFF => {
                self.ram[(usize::from(self.bank7) % 2) * 0x4000 + (a - 0xC000) as usize]
            }
            _ => 0xFF,
        }
    }

    /// Store one memory write under the current latches. The ROM
    /// windows ignore writes.
    fn mem_write(&mut self, address: Address, value: u8) {
        let a = address.0 & 0xFFFF;
        match a {
            0x0000..=0x7FFF => {} // the ROM spans ignore writes
            0x8000..=0xBFFF => {
                self.ram[(usize::from(self.bank6) % 2) * 0x4000 + (a - 0x8000) as usize] = value;
            }
            0xC000..=0xFFFF => {
                self.ram[(usize::from(self.bank7) % 2) * 0x4000 + (a - 0xC000) as usize] = value;
            }
            _ => {}
        }
    }
}

impl BusAccess for Ti85Mem {
    fn read(&mut self, _width: AccessWidth, address: Address, _tick: Tick) -> u32 {
        u32::from(self.peek(address))
    }

    fn write(&mut self, _width: AccessWidth, address: Address, value: u32, _tick: Tick) {
        let v = value as u8;
        let a = address.0 & 0xFFFF;
        match a {
            // The io ports 6 to 8 carry the latches. The published
            // memory image cannot surface at those three addresses;
            // the configuration leaves the span to the io windows.
            6 => self.bank6 = v & 0x03,
            7 => self.bank7 = v & 0x03,
            8 => self.page8 = v & 0x07,
            _ => self.mem_write(address, v),
        }
    }

    fn peek(&self, address: Address) -> u8 {
        // The side-effect-free read: this device holds no read effects.
        let a = address.0 & 0xFFFF;
        match a {
            6 => self.bank6,
            7 => self.bank7,
            8 => self.page8,
            _ => self.mem_byte(address),
        }
    }
}

impl Device for Ti85Mem {
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
        self.rom.iter_mut().for_each(|b| *b = 0);
        self.ram.iter_mut().for_each(|b| *b = 0);
        self.page8 = 0;
        self.bank6 = 1;
        self.bank7 = 0;
    }

    fn boot(&mut self, state: Option<&BootState>) {
        // The StaticRam boot convention: the reset zeroes both stores,
        // the file read keeps a missing file silent, and a short image
        // pads the tail with zeros.
        self.reset();
        match state {
            Some(BootState::Rom { path }) => {
                let bytes = std::fs::read(path).unwrap_or_default();
                let n = bytes.len().min(ROM_LEN);
                self.rom[..n].copy_from_slice(&bytes[..n]);
            }
            Some(BootState::Hex { bytes }) => {
                let n = bytes.len().min(ROM_LEN);
                self.rom[..n].copy_from_slice(&bytes[..n]);
            }
            Some(BootState::Ram { pattern }) => {
                demu_core::boot::fill_ram(&mut self.rom[..], *pattern)
            }
            _ => {}
        }
    }

    fn snapshot(&self) -> Snapshot {
        // The save state carries the whole ROM image too: the boot
        // content belongs to the byte-exact machine state.
        let mut out = Vec::with_capacity(SNAPSHOT_LEN);
        out.extend_from_slice(b"TIMM");
        out.push(1);
        out.push(self.page8);
        out.push(self.bank6);
        out.push(self.bank7);
        out.extend_from_slice(&self.ram[..]);
        out.extend_from_slice(&self.rom[..]);
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        if data.len() != SNAPSHOT_LEN || &data[..4] != b"TIMM" || data[4] != 1 {
            return Err(SnapshotError::Mismatch(format!(
                "TI-85 memory chunk must be {SNAPSHOT_LEN} bytes"
            )));
        }
        self.page8 = data[5];
        self.bank6 = data[6];
        self.bank7 = data[7];
        self.ram.copy_from_slice(&data[8..8 + RAM_LEN]);
        self.rom.copy_from_slice(&data[8 + RAM_LEN..]);
        Ok(())
    }

    fn notify_line(&mut self, _line: &str, _level: Level, _tick: Tick) {}

    fn next_event(&self) -> Option<Tick> {
        None
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

    fn instance() -> demu_config::model::DeviceInstance {
        demu_config::model::DeviceInstance {
            name: DeviceName::new("mem").expect("name"),
            kind: KindName::new("ti85.mem").expect("kind"),
            domain: "main".to_string(),
            params: BTreeMap::new(),
            dividers: BTreeMap::new(),
        }
    }

    fn device() -> Ti85Mem {
        let mut mem = Ti85Mem::new(&instance());
        mem.boot(Some(&BootState::Hex {
            bytes: vec![0x21, 0x34, 0x56, 0x78],
        }));
        mem
    }

    fn window(name: &str, base: u32, size: u32) -> CWindow {
        CWindow {
            name: name.to_string(),
            bus: "cpu".to_string(),
            device: DeviceName::new("mem").expect("name"),
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

    fn io_window(name: &str, base: u32, size: u32) -> CWindow {
        CWindow {
            bus: "io".to_string(),
            ..window(name, base, size)
        }
    }

    fn machine() -> Machine {
        let mem = demu_config::model::DeviceInstance {
            name: DeviceName::new("mem").expect("name"),
            kind: KindName::new("ti85.mem").expect("kind"),
            domain: "main".to_string(),
            params: BTreeMap::new(),
            dividers: BTreeMap::new(),
        };
        let cpu = demu_config::model::DeviceInstance {
            name: DeviceName::new("cp").expect("name"),
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
                frequency: demu_config::newtypes::Frequency::new(6_000_000, 1).expect("freq"),
                origin: "the machine master".to_string(),
            },
        );
        let mut by_name = BTreeMap::new();
        by_name.insert(mem.name.clone(), 0usize);
        by_name.insert(cpu.name.clone(), 1usize);
        let config = demu_config::model::SystemConfig {
            name: "ti85-test".to_string(),
            triple: "ti/ti85".to_string(),
            description: "the TI-85 memory test machine".to_string(),
            clocks,
            devices: vec![mem, cpu],
            devices_by_name: by_name,
            buses: vec![
                CBus {
                    name: "cpu".to_string(),
                    address_bits: 16,
                    owner: None,
                    parent: None,
                },
                CBus {
                    name: "io".to_string(),
                    address_bits: 8,
                    owner: None,
                    parent: None,
                },
            ],
            windows: vec![
                window("mem0", 0x0000, 0x4000),
                window("mem4", 0x4000, 0x4000),
                window("mem8", 0x8000, 0x4000),
                window("memC", 0xC000, 0x4000),
                io_window("io678", 0x0006, 3),
            ],
            connections: Vec::new(),
            signals: Vec::new(),
            boot: BTreeMap::new(),
            quirks: BTreeMap::new(),
        };
        let mut ctors = demu_core::smoke_ctors();
        ctors.register("ti85.mem", Box::new(|d, _| Ok(Box::new(Ti85Mem::new(d)))));
        let mut machine = Machine::assemble(config, &ctors).expect("assembly");
        machine.boot();
        machine
    }

    #[test]
    fn windows_route_to_the_published_pages() {
        let mut d = device();
        // Five distinct storage bytes: the boot hex, ROM page 1, RAM
        // bank 0, RAM bank 1.
        d.rom[0x4000] = 0xA1;
        d.ram[0x0000] = 0xC3;
        d.ram[0x4000] = 0xD4;
        // The reset page 0 shows the boot hex through the paged span.
        assert_eq!(d.peek(Address(0x0002)), 0x56);
        d.page8 = 1;
        d.bank6 = 0;
        d.bank7 = 1;
        assert_eq!(d.peek(Address(0x0000)), 0xA1, "ROM page 1 via port 8");
        assert_eq!(d.peek(Address(0x4000)), 0x21, "ROM page 0 fixed at $4000");
        assert_eq!(
            d.peek(Address(0x4001)),
            0x34,
            "the next boot byte of the fixed page"
        );
        assert_eq!(d.peek(Address(0x8000)), 0xC3, "RAM bank 0 via port 6");
        assert_eq!(d.peek(Address(0xC000)), 0xD4, "RAM bank 1 via port 7");
    }

    #[test]
    fn latch_writes_change_the_routing() {
        let mut d = device();
        d.ram[0x4001] = 0x5A;
        d.ram[0x0001] = 0x33;
        d.write(AccessWidth::Byte, Address(0x8001), 0x5A, Tick(0));
        assert_eq!(d.peek(Address(0x8001)), 0x5A, "the bank 1 cell holds");
        d.write(AccessWidth::Byte, Address(6), 0x00, Tick(0));
        assert_eq!(d.peek(Address(0x8001)), 0x33, "bank 0 switched in");
        d.write(AccessWidth::Byte, Address(6), 0x01, Tick(0));
        assert_eq!(d.peek(Address(0x8001)), 0x5A, "bank 1 switched back");
        // The port 7 latch routes the second 16 KiB bank pair.
        d.write(AccessWidth::Byte, Address(0xC000), 0x77, Tick(0));
        d.write(AccessWidth::Byte, Address(7), 0x01, Tick(0));
        assert_eq!(d.peek(Address(0xC000)), 0x00, "bank 1 answers $C000");
        d.write(AccessWidth::Byte, Address(7), 0x00, Tick(0));
        assert_eq!(d.peek(Address(0xC000)), 0x77, "bank 0 answers $C000");
    }

    #[test]
    fn latch_values_wrap_modulo_the_page_count() {
        let mut d = device();
        d.rom[0x4005] = 0x11;
        d.rom[0x14005] = 0x22;
        d.write(AccessWidth::Byte, Address(8), 0xFF, Tick(0));
        assert_eq!(d.page8, 0x07, "the write masks to three bits");
        // The mapping itself wraps a latched value modulo the page
        // count: a direct field set past the count answers page 1.
        d.page8 = 9;
        assert_eq!(d.peek(Address(0x0005)), 0x11, "wrapped page 1 answers");
        d.write(AccessWidth::Byte, Address(8), 0x09, Tick(0));
        assert_eq!(d.page8, 0x01, "the write itself wraps page 9 to 1");
        assert_eq!(d.peek(Address(0x0005)), 0x11, "wrapped page 1 answers");
        d.write(AccessWidth::Byte, Address(8), 0x1D, Tick(0));
        assert_eq!(d.page8, 0x05, "page 29 wraps to page 5");
        assert_eq!(d.peek(Address(0x0005)), 0x22, "wrapped page 5 answers");
        d.write(AccessWidth::Byte, Address(6), 0x05, Tick(0));
        assert_eq!(d.bank6, 0x01, "the RAM latch wraps at 2");
    }

    #[test]
    fn rom_spans_ignore_writes() {
        let mut d = device();
        d.write(AccessWidth::Byte, Address(0x0002), 0xFF, Tick(0));
        d.write(AccessWidth::Byte, Address(0x4002), 0xFF, Tick(0));
        assert_eq!(d.peek(Address(0x0002)), 0x56, "the boot page holds");
        assert_eq!(d.peek(Address(0x4002)), 0x56, "the fixed page holds");
    }

    #[test]
    fn io_ports_store_and_read_back_the_latches() {
        let mut d = device();
        assert_eq!(d.peek(Address(6)), 0x01, "the port 6 reset value");
        assert_eq!(d.peek(Address(7)), 0x00, "the port 7 reset value");
        assert_eq!(d.peek(Address(8)), 0x00, "the port 8 reset value");
        // The read of a port returns the stored latch through the io
        // window form.
        d.write(AccessWidth::Byte, Address(6), 0x02, Tick(0));
        d.write(AccessWidth::Byte, Address(8), 0x06, Tick(0));
        assert_eq!(d.read(AccessWidth::Byte, Address(6), Tick(0)), 0x02);
        assert_eq!(d.read(AccessWidth::Byte, Address(8), Tick(0)), 0x06);
    }

    #[test]
    fn boot_hex_copies_prefix_and_pads_the_tail() {
        let d = device();
        assert_eq!(d.peek(Address(0x0000)), 0x21);
        assert_eq!(d.peek(Address(0x0003)), 0x78);
        assert_eq!(d.peek(Address(0x0004)), 0x00, "the padded tail");
        assert_eq!(d.peek(Address(ROM_LEN as u32 - 1)), 0x00, "the last byte");
    }

    #[test]
    fn boot_rom_file_fills_the_page_zero_prefix() {
        let path = std::env::temp_dir().join("demu-ti85-boot-test.bin");
        std::fs::write(&path, [0x11, 0x22, 0x33]).expect("write file");
        let mut d = Ti85Mem::new(&instance());
        d.boot(Some(&BootState::Rom {
            path: path.to_string_lossy().to_string(),
        }));
        assert_eq!(d.peek(Address(0)), 0x11);
        assert_eq!(d.peek(Address(2)), 0x33);
        assert_eq!(d.peek(Address(3)), 0x00, "the padded tail");
        std::fs::remove_file(&path).expect("cleanup");
    }

    #[test]
    fn snapshot_round_trip_carries_the_latches_and_cells() {
        let mut d = device();
        d.write(AccessWidth::Byte, Address(6), 0x00, Tick(0));
        d.write(AccessWidth::Byte, Address(0x8100), 0x9C, Tick(0));
        let snap = d.snapshot();
        assert_eq!(snap.len(), SNAPSHOT_LEN);
        let mut twin = Ti85Mem::new(&instance());
        twin.restore(&snap).expect("restore");
        assert_eq!(twin.snapshot(), snap);
        assert_eq!(twin.page8, d.page8);
        assert_eq!(twin.bank6, d.bank6);
        assert_eq!(twin.bank7, d.bank7);
        assert_eq!(twin.peek(Address(0x8100)), 0x9C, "the RAM cell rides");
        assert_eq!(twin.peek(Address(0x0000)), 0x21, "the boot image rides");
    }

    #[test]
    fn restore_rejects_a_wrong_length_and_tag() {
        let d = device();
        let snap = d.snapshot();
        let mut twin = Ti85Mem::new(&instance());
        assert!(twin.restore(&snap[..snap.len() - 1]).is_err());
        assert!(twin.restore(&[0u8; SNAPSHOT_LEN]).is_err());
        let mut retagged = snap.clone();
        retagged[3] = b'X';
        assert!(twin.restore(&retagged).is_err());
    }

    #[test]
    fn the_bank_switch_round_trips_through_the_machine() {
        let mut m = machine();
        assert!(m.debug_write("io", Address(6), 0x00));
        assert!(m.debug_write("cpu", Address(0x8100), 0x42), "bank 0 write");
        assert_eq!(m.peek("cpu", Address(0x8100)), 0x42);
        assert!(m.debug_write("io", Address(6), 0x01), "switch bank 1");
        assert_eq!(m.peek("cpu", Address(0x8100)), 0x00, "bank 1 reads zero");
        assert!(m.debug_write("io", Address(6), 0x00), "switch back");
        assert_eq!(m.peek("cpu", Address(0x8100)), 0x42, "the cell held");
        // The fixed ROM window and the paged ROM window route apart.
        assert_eq!(m.peek("cpu", Address(0x0000)), 0x00);
        assert_eq!(m.peek("cpu", Address(0x4000)), 0x00);
    }
}
