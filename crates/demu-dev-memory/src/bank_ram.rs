//! The 128K Sinclair banked RAM controller (`zx.bankram`).
//!
//! The device holds the full 128 KiB of banked RAM: eight banks of 16
//! KiB. The configuration maps three windows of this device on the cpu
//! bus and one window on the io bus, and the device routes every
//! access by value, because one device cannot see which bus called.
//!
//! # The memory windows
//!
//! The three windows split `$4000-$FFFF` and decode without
//! exclusion:
//!
//! - `$4000-$7FFF` answers bank 5, the screen bank.
//! - `$8000-$BFFF` answers bank 2.
//! - `$C000-$FFFF` answers the bank that bits 0 to 2 of the paging
//!   latch select. The bank select masks to three bits, so a latch of
//!   8 pages bank 0.
//!
//! # The paging port
//!
//! The io window carries the `$7FFD` family of the published decode:
//! A15 clear, A1 clear, and A14 to A11 set (the mask
//! `(address & 0x8002) == 0 && ((address >> 11) & 0xF) == 0x0F`). The
//! port is write-only and reads `$FF`. A write latches bits 0 to 2
//! into the bank select, bit 3 into the screen select, bit 4 into the
//! ROM select, and bit 5 into the paging lock; a locked latch ignores
//! later writes until a reset or a restore. The mask leaves bit 0
//! open, so the `zx.ula` companion, which answers every even port,
//! meets this decode on the even members of the family; the window
//! order of the machine decides that access, and the software of the
//! 128 machine addresses the port with bit 0 set, at `$7FFD`.
//!
//! Because the dispatch keys on the value, a cpu read at `$7FFD`
//! answers the write-only latch form `$FF` instead of the bank-5 byte
//! under it. The published gate arrays mirror the paging register
//! over the memory span in the same way, so the answer matches the
//! published machine.
//!
//! # The paging lines
//!
//! `BusAccess::write` carries no machine context, so an accepted
//! latch write only records the pending line levels. The scheduler
//! visit that `next_event` announces delivers the three lines and
//! carries the recorded tick:
//!
//! - `contend_page`: high when the bank select is odd. The CPU core
//!   consumes the line. The visit runs after the writing instruction
//!   completes, so the contention state reaches the CPU core one
//!   scheduler pass late; that delay stands for this build.
//! - `rom_bank`: high when bit 4 is set. The `z80.bankrom` companion
//!   consumes the line and swaps its page.
//! - `shadow`: high when bit 3 is set. The `zx.ula` companion
//!   consumes the line and renders the banked screen span.
//!
//! A rapid burst of paging writes delivers the levels of the final
//! latch only.
//!
//! # Boot
//!
//! The storage zero-fills at construction and at reset. A `Ram` boot
//! state fills the storage from the pattern, and a `Rom` or `Hex`
//! boot state copies its image into the banks in linear order; a user
//! configuration preloads a RAM image that way.
//!
//! # Save state
//!
//! The save state rides the `ZXBK` tag, version 1, with a fixed
//! length: the 128 KiB of bank storage, the paging latch, and the
//! pending delivery flags. A restore validates the length, the tag,
//! and the version, reinstates the storage and the latch, and clears
//! the pending deliveries. The lock bit rides the restored latch. The
//! pending deliveries are transient scheduler state, and the machine
//! re-seeds every device after a restore.

use demu_config::model::BootState;
use demu_core::device::{AccessWidth, BusAccess, MachineCtx};
use demu_core::frequency::{Address, Tick};
use demu_core::signal::Level;
use demu_core::{Device, Snapshot, SnapshotError};

/// The size of one bank: 16 KiB.
const BANK_SIZE: usize = 0x4000;
/// The bank count of the controller storage.
const BANK_COUNT: usize = 8;
/// The latch bit of the shadow screen select.
const LATCH_SHADOW: u8 = 0x08;
/// The latch bit of the ROM select.
const LATCH_ROM: u8 = 0x10;
/// The latch bit of the paging lock.
const LATCH_LOCK: u8 = 0x20;
/// The fixed save-state chunk length.
const ZXBK_LEN: usize = 4 + 1 + BANK_SIZE * BANK_COUNT + 1 + 1;

/// The banked RAM and paging controller of the ZX Spectrum 128.
#[derive(Debug)]
pub struct ZxBankRam {
    name: String,
    /// The full 128 KiB of RAM in bank order.
    data: Vec<u8>,
    /// The paging latch: bits 0 to 2 the home bank select, bit 3 the
    /// shadow screen, bit 4 the ROM select, and bit 5 the paging
    /// lock.
    latch: u8,
    /// The pending `contend_page` delivery level.
    pending_contend: Option<bool>,
    /// The pending `rom_bank` delivery level.
    pending_rom: Option<bool>,
    /// The pending `shadow` delivery level.
    pending_shadow: Option<bool>,
    /// The tick the pending deliveries carry.
    pending_tick: Option<Tick>,
}

impl ZxBankRam {
    /// A device from its configuration entry.
    pub fn new(device: &demu_config::model::DeviceInstance) -> Self {
        Self {
            name: device.name.to_string(),
            data: vec![0; BANK_SIZE * BANK_COUNT],
            latch: 0,
            pending_contend: None,
            pending_rom: None,
            pending_shadow: None,
            pending_tick: None,
        }
    }

    /// The storage index of one memory address, or `None` below the
    /// `$4000` window base: the screen window routes bank 5, the
    /// middle window bank 2, and the home window the bank of the
    /// paging latch.
    fn map(&self, a: u32) -> Option<usize> {
        let bank = match a {
            0x4000..=0x7FFF => 5,
            0x8000..=0xBFFF => 2,
            0xC000..=0xFFFF => (self.latch & 0x07) as usize,
            _ => return None,
        };
        Some(bank * BANK_SIZE + (a as usize & (BANK_SIZE - 1)))
    }

    /// True when the address sits in the `$7FFD` family: A15 clear, A1
    /// clear, and A14 to A11 set.
    fn paging_matches(a: u32) -> bool {
        (a & 0x8002) == 0 && ((a >> 11) & 0xF) == 0x0F
    }

    /// Latch one paging write under the lock rule and record the
    /// pending line levels of the new latch value.
    fn latch_write(&mut self, value: u8, tick: Tick) {
        if self.latch & LATCH_LOCK != 0 {
            // The locked latch ignores further writes until a reset or
            // a restore.
            return;
        }
        self.latch = value;
        let bank = self.latch & 0x07;
        self.pending_contend = Some(bank & 0x01 != 0);
        self.pending_rom = Some(self.latch & LATCH_ROM != 0);
        self.pending_shadow = Some(self.latch & LATCH_SHADOW != 0);
        self.pending_tick = Some(tick);
    }

    /// The save-state byte of the pending delivery flags.
    fn pending_flags(&self) -> u8 {
        let mut flags = 0u8;
        if self.pending_contend.is_some() {
            flags |= 0x01;
        }
        if self.pending_rom.is_some() {
            flags |= 0x02;
        }
        if self.pending_shadow.is_some() {
            flags |= 0x04;
        }
        flags
    }
}

impl BusAccess for ZxBankRam {
    fn read(&mut self, _width: AccessWidth, address: Address, _tick: Tick) -> u32 {
        let a = address.0 & 0xFFFF;
        if Self::paging_matches(a) {
            // The paging port is write-only.
            return 0xFF;
        }
        self.map(a).map_or(0xFF, |i| u32::from(self.data[i]))
    }

    fn write(&mut self, _width: AccessWidth, address: Address, value: u32, tick: Tick) {
        let a = address.0 & 0xFFFF;
        if Self::paging_matches(a) {
            self.latch_write(value as u8, tick);
            return;
        }
        if let Some(i) = self.map(a) {
            self.data[i] = value as u8;
        }
    }

    fn peek(&self, address: Address) -> u8 {
        let a = address.0 & 0xFFFF;
        if Self::paging_matches(a) {
            return 0xFF;
        }
        self.map(a).map_or(0xFF, |i| self.data[i])
    }

    fn decodes(&self, address: Address) -> bool {
        let a = address.0 & 0xFFFF;
        // The paging family answers before the memory spans; the three
        // memory spans then decode without exclusion.
        if Self::paging_matches(a) {
            return true;
        }
        (0x4000..=0xFFFF).contains(&a)
    }
}

impl Device for ZxBankRam {
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
        self.latch = 0;
        self.pending_contend = None;
        self.pending_rom = None;
        self.pending_shadow = None;
        self.pending_tick = None;
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
        let mut out = Vec::with_capacity(ZXBK_LEN);
        out.extend_from_slice(b"ZXBK");
        out.push(1);
        out.extend_from_slice(&self.data);
        out.push(self.latch);
        out.push(self.pending_flags());
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        if data.len() != ZXBK_LEN || &data[..4] != b"ZXBK" || data[4] != 1 {
            return Err(SnapshotError::Mismatch(format!(
                "zx bank RAM chunk must be {ZXBK_LEN} bytes"
            )));
        }
        let storage = BANK_SIZE * BANK_COUNT;
        self.data.copy_from_slice(&data[5..5 + storage]);
        self.latch = data[5 + storage];
        // One pending-flag byte follows the latch; the restore drops
        // the pending deliveries, because the machine re-seeds every
        // device after a restore.
        self.pending_contend = None;
        self.pending_rom = None;
        self.pending_shadow = None;
        self.pending_tick = None;
        Ok(())
    }

    fn notify_line(&mut self, _line: &str, _level: Level, _tick: Tick) {}

    fn next_event(&self) -> Option<Tick> {
        self.pending_tick
    }

    fn tick(&mut self, tick: Tick, ctx: &mut MachineCtx) {
        // The accepted latch write recorded its tick; the first visit
        // at or after that tick delivers the lines with it.
        if let Some(due) = self.pending_tick
            && due.0 <= tick.0
        {
            self.pending_tick = None;
            if let Some(high) = self.pending_contend.take() {
                ctx.notify("contend_page", level_of(high), due);
            }
            if let Some(high) = self.pending_rom.take() {
                ctx.notify("rom_bank", level_of(high), due);
            }
            if let Some(high) = self.pending_shadow.take() {
                ctx.notify("shadow", level_of(high), due);
            }
        }
    }
}

/// The level form of one driven flag.
fn level_of(high: bool) -> Level {
    if high { Level::High } else { Level::Low }
}

#[cfg(test)]
mod tests {
    use super::*;
    use demu_config::model::{
        Bus as CBus, ClockDomain, Connection, ConnectionDirection, ConnectionSemantics,
        Window as CWindow,
    };
    use demu_config::newtypes::{DeviceName, KindName, LinePath};
    use demu_core::machine::Machine;
    use std::collections::BTreeMap;

    fn instance() -> demu_config::model::DeviceInstance {
        demu_config::model::DeviceInstance {
            name: DeviceName::new("door0").expect("name"),
            kind: KindName::new("zx.bankram").expect("kind"),
            domain: "main".to_string(),
            params: BTreeMap::new(),
            dividers: BTreeMap::new(),
        }
    }

    /// Run one device visit under an empty context: the notifications
    /// find no routes.
    fn visit(d: &mut ZxBankRam, tick: Tick) {
        let mut before: Vec<Box<dyn Device>> = Vec::new();
        let mut after: Vec<Box<dyn Device>> = Vec::new();
        let mut buses: Vec<demu_core::Bus> = Vec::new();
        let mut scratch: Vec<demu_core::DecodeHit> = Vec::new();
        let mut events: Vec<demu_core::TriggerEvent> = Vec::new();
        let mut dirty: Vec<usize> = Vec::new();
        let index = Default::default();
        let mut ctx = MachineCtx::new(
            demu_core::device::DeviceSlices {
                before: before.as_mut_slice(),
                after: after.as_mut_slice(),
                caller_slot: 0,
            },
            demu_core::device::BusBank {
                buses: buses.as_mut_slice(),
                index: &index,
            },
            &[],
            &mut scratch,
            demu_core::device::EventBuffers {
                events: &mut events,
                dirty: &mut dirty,
            },
            false,
        );
        d.tick(tick, &mut ctx);
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

    fn connection(from: &str, to: &str) -> Connection {
        Connection {
            from: LinePath::parse(from).expect("route"),
            to: LinePath::parse(to).expect("route"),
            semantics: ConnectionSemantics::Irq,
            direction: ConnectionDirection::Forward,
        }
    }

    /// A recorder device: it collects the notified line edges of the
    /// machine's connections.
    struct LineRecorder {
        name: String,
        lines: Vec<(String, Level, u64)>,
    }

    impl LineRecorder {
        fn new(name: &str) -> Self {
            Self {
                name: name.to_string(),
                lines: Vec::new(),
            }
        }
    }

    impl Device for LineRecorder {
        fn name(&self) -> &str {
            &self.name
        }

        fn as_any(&self) -> &dyn std::any::Any {
            self
        }

        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }

        fn reset(&mut self) {
            self.lines.clear();
        }

        fn boot(&mut self, _state: Option<&BootState>) {
            self.lines.clear();
        }

        fn snapshot(&self) -> Snapshot {
            Vec::new()
        }

        fn restore(&mut self, _data: &[u8]) -> Result<(), SnapshotError> {
            Ok(())
        }

        fn notify_line(&mut self, line: &str, level: Level, tick: Tick) {
            self.lines.push((line.to_string(), level, tick.0));
        }

        fn tick(&mut self, _tick: Tick, _ctx: &mut MachineCtx) {}
    }

    /// The bank RAM test machine: the controller with its three memory
    /// windows and the paging io window, and the recorder on the lines.
    fn bank_ram_machine() -> Machine {
        let door = instance();
        let lines = demu_config::model::DeviceInstance {
            name: entry_name("lines"),
            kind: KindName::new("test.lines").expect("kind"),
            domain: "main".to_string(),
            params: BTreeMap::new(),
            dividers: BTreeMap::new(),
        };
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
        by_name.insert(door.name.clone(), 0usize);
        by_name.insert(lines.name.clone(), 1usize);
        by_name.insert(cp.name.clone(), 2usize);
        let config = demu_config::model::SystemConfig {
            name: "bankram-test".to_string(),
            triple: "test".to_string(),
            description: "the ZX bank RAM test machine".to_string(),
            clocks,
            devices: vec![door, lines, cp],
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
                    address_bits: 16,
                    owner: None,
                    parent: None,
                },
            ],
            windows: vec![
                window("screen", "cpu", "door0", 0x4000, 0x4000),
                window("middle", "cpu", "door0", 0x8000, 0x4000),
                window("home", "cpu", "door0", 0xC000, 0x4000),
                window("paging", "io", "door0", 0x7800, 0x0800),
            ],
            connections: vec![
                connection("door0.contend_page", "lines.contend"),
                connection("door0.rom_bank", "lines.rom"),
                connection("door0.shadow", "lines.shadow"),
            ],
            signals: Vec::new(),
            boot: BTreeMap::new(),
            quirks: BTreeMap::new(),
        };
        let mut ctors = demu_core::smoke_ctors();
        ctors.register(
            "zx.bankram",
            Box::new(|d, _| Ok(Box::new(ZxBankRam::new(d)))),
        );
        ctors.register(
            "test.lines",
            Box::new(|d, _| Ok(Box::new(LineRecorder::new(d.name.as_ref())))),
        );
        let mut machine = Machine::assemble(config, &ctors).expect("assembly");
        machine.boot();
        machine
    }

    fn recorder(machine: &mut Machine, slot: usize) -> &mut LineRecorder {
        machine.devices[slot]
            .as_any_mut()
            .downcast_mut::<LineRecorder>()
            .expect("recorder")
    }

    /// The port write of the paging latch through the device handle.
    fn latch_write(machine: &mut Machine, value: u8, tick: u64) {
        let door = machine.device_by_name_mut("door0").expect("device");
        door.bus_mut().expect("bus").write(
            AccessWidth::Byte,
            Address(0x7FFD),
            u32::from(value),
            Tick(tick),
        );
    }

    // ---------------------------------------------------------------
    // Memory windows and bank routing

    #[test]
    fn the_three_windows_route_the_banks() {
        let mut d = ZxBankRam::new(&instance());
        d.write(AccessWidth::Byte, Address(0x4000), 0x11, Tick(0));
        d.write(AccessWidth::Byte, Address(0x8000), 0x22, Tick(0));
        d.write(AccessWidth::Byte, Address(0xC000), 0x33, Tick(0));
        assert_eq!(d.peek(Address(0x4000)), 0x11, "bank 5 at the screen window");
        assert_eq!(d.peek(Address(0x8000)), 0x22, "bank 2 at the middle window");
        assert_eq!(d.peek(Address(0xC000)), 0x33, "bank 0 at home after reset");
        // The banks stay separate.
        d.write(AccessWidth::Byte, Address(0xC0FF), 0x44, Tick(0));
        assert_eq!(
            d.peek(Address(0x40FF)),
            0,
            "the home write stayed in bank 0"
        );
        d.write(AccessWidth::Byte, Address(0xBFFF), 0x55, Tick(0));
        assert_eq!(
            d.peek(Address(0x7FFF)),
            0,
            "the middle write stayed in bank 2"
        );
    }

    #[test]
    fn the_latch_routes_the_home_bank_with_wrap() {
        let mut d = ZxBankRam::new(&instance());
        d.write(AccessWidth::Byte, Address(0x7FFD), 0x03, Tick(0));
        d.write(AccessWidth::Byte, Address(0xC000), 0x55, Tick(0));
        assert_eq!(d.peek(Address(0xC000)), 0x55, "bank 3 routes home");
        // A latch of 8 masks to bank 0.
        d.write(AccessWidth::Byte, Address(0x7FFD), 0x08, Tick(0));
        assert_eq!(d.peek(Address(0xC000)), 0, "the latch of 8 wraps to bank 0");
        d.write(AccessWidth::Byte, Address(0x7FFD), 0x03, Tick(0));
        assert_eq!(d.peek(Address(0xC000)), 0x55, "the bank-3 cell stays");
        // The borrow windows never follow the home bank.
        d.write(AccessWidth::Byte, Address(0x4001), 0x66, Tick(0));
        d.write(AccessWidth::Byte, Address(0x7FFD), 0x07, Tick(0));
        assert_eq!(d.peek(Address(0x4001)), 0x66, "bank 5 stays");
        assert_eq!(d.peek(Address(0x8100)), 0, "bank 2 stays");
    }

    #[test]
    fn the_lock_bit_ignores_writes_until_reset() {
        let mut d = ZxBankRam::new(&instance());
        d.write(AccessWidth::Byte, Address(0x7FFD), 0x27, Tick(0));
        assert_eq!(d.latch, 0x27);
        assert_eq!(d.pending_tick, Some(Tick(0)), "the accepted write recorded");
        d.write(AccessWidth::Byte, Address(0x7FFD), 0x01, Tick(4));
        assert_eq!(d.latch, 0x27, "the locked write stored nothing");
        assert_eq!(
            d.pending_tick,
            Some(Tick(0)),
            "the locked write recorded nothing"
        );
        d.reset();
        assert_eq!(d.latch, 0, "the reset unlocked the latch");
        d.write(AccessWidth::Byte, Address(0x7FFD), 0x01, Tick(8));
        assert_eq!(d.latch, 0x01);
    }

    // ---------------------------------------------------------------
    // Paging port decode and dispatch

    #[test]
    fn the_paging_mask_decode_matrix() {
        let d = ZxBankRam::new(&instance());
        // The published mask itself.
        assert!(ZxBankRam::paging_matches(0x7FFD), "the port value matches");
        assert!(
            ZxBankRam::paging_matches(0x7FF5),
            "the family member matches"
        );
        assert!(ZxBankRam::paging_matches(0x7FFC), "the even member matches");
        assert!(
            !ZxBankRam::paging_matches(0x7FFF),
            "bit 1 set stays out of the mask"
        );
        assert!(
            !ZxBankRam::paging_matches(0x3FFD),
            "bit 14 clear stays out of the mask"
        );
        // The device decode: the family first, then the memory spans
        // without exclusion.
        let dec = |a: u32| d.bus().expect("bus").decodes(Address(a));
        assert!(dec(0x7FFD), "the port decodes over the family");
        assert!(dec(0x7FFC), "the even member decodes over the family");
        assert!(
            dec(0x7FFF),
            "the bank-5 span covers the same value in memory"
        );
        assert!(dec(0x4000), "the screen span decodes");
        assert!(dec(0xBFFF), "the middle span decodes");
        assert!(dec(0xFFFF), "the home span decodes");
        assert!(!dec(0x0000), "below the memory base stays out");
        assert!(!dec(0x3FFF), "below the memory base stays out");
    }

    #[test]
    fn the_paging_port_read_returns_the_open_bus_form() {
        let mut d = ZxBankRam::new(&instance());
        assert_eq!(d.peek(Address(0x7FFD)), 0xFF, "the port is write-only");
        assert_eq!(
            d.read(AccessWidth::Byte, Address(0x7FFD), Tick(0)),
            0xFF,
            "the read takes the same form"
        );
    }

    #[test]
    fn the_memory_span_answers_the_latch_form_at_the_port() {
        let mut d = ZxBankRam::new(&instance());
        // A memory read at the port value answers the write-only latch
        // form instead of the bank-5 byte under it.
        d.write(AccessWidth::Byte, Address(0x7FFC), 0x22, Tick(0));
        assert_eq!(
            d.peek(Address(0x7FFC)),
            0xFF,
            "the family member reads the latch form"
        );
        assert_eq!(
            d.peek(Address(0x7FFB)),
            0,
            "the non-family cell answers the bank"
        );
        // The same value acts as the latch whichever window reached the
        // device, so a window write at `$7FFD` pages the home bank.
        d.write(AccessWidth::Byte, Address(0x7FFD), 0x03, Tick(0));
        d.write(AccessWidth::Byte, Address(0xC000), 0x5A, Tick(0));
        assert_eq!(d.peek(Address(0xC000)), 0x5A, "the write stored the latch");
    }

    // ---------------------------------------------------------------
    // Pending line deliveries

    #[test]
    fn accepted_writes_record_the_pending_levels() {
        let mut d = ZxBankRam::new(&instance());
        d.write(AccessWidth::Byte, Address(0x7FFD), 0x0F, Tick(7));
        assert_eq!(d.pending_contend, Some(true), "bank 7 is odd");
        assert_eq!(d.pending_rom, Some(false));
        assert_eq!(d.pending_shadow, Some(true));
        assert_eq!(d.pending_tick, Some(Tick(7)));
        assert_eq!(d.next_event(), Some(Tick(7)), "the visit is pending");
        // The burst overwrites the recorded levels.
        d.write(AccessWidth::Byte, Address(0x7FFD), 0x04, Tick(9));
        assert_eq!(d.pending_contend, Some(false), "bank 4 is even");
        assert_eq!(d.pending_rom, Some(false));
        assert_eq!(d.pending_shadow, Some(false));
        assert_eq!(d.next_event(), Some(Tick(9)));
        assert_eq!(d.latch, 0x04, "the latch took the final write");
    }

    #[test]
    fn the_flush_clears_the_pending_state() {
        let mut d = ZxBankRam::new(&instance());
        d.write(AccessWidth::Byte, Address(0x7FFD), 0x1B, Tick(2));
        visit(&mut d, Tick(2));
        assert_eq!(d.pending_tick, None);
        assert_eq!(d.pending_contend, None);
        assert_eq!(d.pending_rom, None);
        assert_eq!(d.pending_shadow, None);
        assert_eq!(d.next_event(), None);
        // A visit before the recorded tick leaves the delivery armed.
        d.write(AccessWidth::Byte, Address(0x7FFD), 0x1B, Tick(6));
        visit(&mut d, Tick(4));
        assert_eq!(
            d.pending_tick,
            Some(Tick(6)),
            "the early visit armed nothing"
        );
        visit(&mut d, Tick(6));
        assert_eq!(d.pending_tick, None);
    }

    // ---------------------------------------------------------------
    // Boot and snapshots

    #[test]
    fn boot_states_fill_the_storage() {
        let mut d = ZxBankRam::new(&instance());
        d.boot(Some(&BootState::Hex {
            bytes: vec![1, 2, 3],
        }));
        assert_eq!(d.peek(Address(0xC000)), 1, "the image starts at bank 0");
        assert_eq!(d.peek(Address(0xC002)), 3);
        assert_eq!(d.peek(Address(0xC003)), 0, "the tail stays zeroed");
        let mut d = ZxBankRam::new(&instance());
        d.boot(Some(&BootState::Ram {
            pattern: demu_config::model::RamPattern::Random { seed: 7 },
        }));
        assert!(d.data.iter().any(|b| *b != 0));
    }

    #[test]
    fn snapshot_round_trips_the_storage_and_latch() {
        let mut d = ZxBankRam::new(&instance());
        d.write(AccessWidth::Byte, Address(0x4000), 0x11, Tick(0));
        d.write(AccessWidth::Byte, Address(0x8000), 0x22, Tick(0));
        d.write(AccessWidth::Byte, Address(0xC000), 0x33, Tick(0));
        d.write(AccessWidth::Byte, Address(0x7FFD), 0x37, Tick(0));
        visit(&mut d, Tick(0));
        let snapshot = d.snapshot();
        assert_eq!(snapshot.len(), ZXBK_LEN);
        let mut twin = ZxBankRam::new(&instance());
        twin.restore(&snapshot).expect("restore");
        assert_eq!(twin.snapshot(), snapshot);
        assert_eq!(twin.peek(Address(0x4000)), 0x11, "bank 5 rides");
        assert_eq!(twin.peek(Address(0x8000)), 0x22, "bank 2 rides");
        assert_eq!(twin.peek(Address(0xC000)), 0, "bank 7 answers home");
        assert_eq!(twin.latch, 0x37);
        // The lock rides the restored latch.
        twin.write(AccessWidth::Byte, Address(0x7FFD), 0x01, Tick(0));
        assert_eq!(twin.latch, 0x37, "the restored lock holds");
    }

    #[test]
    fn restore_drops_the_pending_deliveries() {
        let mut d = ZxBankRam::new(&instance());
        d.write(AccessWidth::Byte, Address(0x7FFD), 0x0F, Tick(5));
        assert_eq!(d.next_event(), Some(Tick(5)));
        let snapshot = d.snapshot();
        let mut twin = ZxBankRam::new(&instance());
        twin.restore(&snapshot).expect("restore");
        assert_eq!(twin.next_event(), None, "the pending delivery dropped");
        assert!(twin.pending_contend.is_none());
        assert!(twin.pending_rom.is_none());
        assert!(twin.pending_shadow.is_none());
        assert_eq!(twin.latch, 0x0F, "the latch rode the save state");
        // A save taken after the flush round trips byte for byte.
        visit(&mut d, Tick(5));
        let flushed = d.snapshot();
        let mut another = ZxBankRam::new(&instance());
        another.restore(&flushed).expect("restore");
        assert_eq!(another.snapshot(), flushed);
    }

    #[test]
    fn restore_rejects_malformed_chunks() {
        let d = ZxBankRam::new(&instance());
        let snapshot = d.snapshot();
        let mut twin = ZxBankRam::new(&instance());
        assert!(
            twin.restore(&snapshot[..snapshot.len() - 1]).is_err(),
            "a short body"
        );
        assert!(twin.restore(&[0; ZXBK_LEN + 1]).is_err(), "a long body");
        let mut retag = snapshot.clone();
        retag[2] = b'X';
        assert!(twin.restore(&retag).is_err(), "a wrong tag");
        let mut reversion = snapshot.clone();
        reversion[4] = 2;
        assert!(twin.restore(&reversion).is_err(), "a wrong version");
    }

    // ---------------------------------------------------------------
    // The machine visits and line routes

    #[test]
    fn the_machine_delivers_the_paging_lines() {
        let mut m = bank_ram_machine();
        latch_write(&mut m, 0x0F, 4);
        assert_eq!(
            m.device_by_name("door0").expect("device").next_event(),
            Some(Tick(4)),
            "the accepted write announces the visit"
        );
        m.run_until(Tick(4));
        assert_eq!(
            recorder(&mut m, 1).lines,
            vec![
                ("contend".to_string(), Level::High, 4),
                ("rom".to_string(), Level::Low, 4),
                ("shadow".to_string(), Level::High, 4),
            ],
        );
        assert_eq!(
            m.device_by_name("door0").expect("device").next_event(),
            None,
            "the flush disarms the visit"
        );
    }

    #[test]
    fn the_machine_flushes_the_final_latch_of_a_burst() {
        let mut m = bank_ram_machine();
        latch_write(&mut m, 0x03, 1);
        latch_write(&mut m, 0x1F, 3);
        m.run_until(Tick(3));
        assert_eq!(
            recorder(&mut m, 1).lines,
            vec![
                ("contend".to_string(), Level::High, 3),
                ("rom".to_string(), Level::High, 3),
                ("shadow".to_string(), Level::High, 3),
            ],
            "the burst leaves the levels of the final latch only"
        );
    }

    #[test]
    fn the_machine_flushes_latches_in_sequence() {
        let mut m = bank_ram_machine();
        latch_write(&mut m, 0x0A, 1);
        m.run_until(Tick(1));
        latch_write(&mut m, 0x04, 5);
        m.run_until(Tick(5));
        assert_eq!(
            recorder(&mut m, 1).lines,
            vec![
                ("contend".to_string(), Level::Low, 1),
                ("rom".to_string(), Level::Low, 1),
                ("shadow".to_string(), Level::High, 1),
                ("contend".to_string(), Level::Low, 5),
                ("rom".to_string(), Level::Low, 5),
                ("shadow".to_string(), Level::Low, 5),
            ],
            "each accepted write delivers once, at its own tick"
        );
    }

    #[test]
    fn the_machine_routes_the_home_bank_through_the_latch() {
        let mut m = bank_ram_machine();
        assert!(m.debug_write("cpu", Address(0x4000), 0x11));
        assert!(m.debug_write("cpu", Address(0x8000), 0x22));
        latch_write(&mut m, 0x00, 1);
        m.run_until(Tick(1));
        assert!(m.debug_write("cpu", Address(0xC000), 0x33));
        assert_eq!(
            m.peek("cpu", Address(0xC000)),
            0x33,
            "bank 0 holds the home write"
        );
        latch_write(&mut m, 0x03, 7);
        m.run_until(Tick(7));
        assert_eq!(
            m.peek("cpu", Address(0xC000)),
            0,
            "bank 3 replaced the view"
        );
        assert!(m.debug_write("cpu", Address(0xC001), 0x44));
        latch_write(&mut m, 0x08, 11);
        m.run_until(Tick(11));
        assert_eq!(
            m.peek("cpu", Address(0xC000)),
            0x33,
            "a latch of 8 wraps to bank 0"
        );
        assert_eq!(
            m.peek("cpu", Address(0x4000)),
            0x11,
            "the screen bank stays"
        );
        assert_eq!(
            m.peek("cpu", Address(0x8000)),
            0x22,
            "the middle bank stays"
        );
        latch_write(&mut m, 0x03, 15);
        m.run_until(Tick(15));
        assert_eq!(
            m.peek("cpu", Address(0xC001)),
            0x44,
            "the bank-3 cell stays"
        );
    }

    #[test]
    fn the_machine_paging_port_reads_the_open_bus_form() {
        let mut m = bank_ram_machine();
        assert_eq!(
            m.peek("io", Address(0x7FFD)),
            0xFF,
            "the port is write-only"
        );
        latch_write(&mut m, 0x0F, 1);
        m.run_until(Tick(1));
        assert_eq!(
            m.peek("io", Address(0x7FFD)),
            0xFF,
            "the write stored the latch"
        );
    }
}
