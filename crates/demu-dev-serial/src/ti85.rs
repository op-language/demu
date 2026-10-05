//! The TI-85 link port, the `ti85.link` device kind.
//!
//! The published link port carries the two cable lines: the white
//! (ring) line and the red (tip) line. The port byte keeps four bits.
//! Bits 0-1 carry the local drive levels of the white and red lines,
//! and bits 2-3 carry the output enables of the white and red lines.
//! An enabled line drives its own level; a disabled line floats. The
//! demu machine maps the port window over the Z80 I/O page addresses
//! `$05`, the window the demu TI-85 machine family declares.
//!
//! # The line levels
//!
//! The write stores the drive levels and the enables. The read
//! returns the effective line levels in the low two bits: bit 0 the
//! white line, bit 1 the red line. An enabled line reads its local
//! drive; a floating line reads the remote level, the far end of the
//! cable over the link protocol. The remote level defaults high, the
//! idle cable form: both undriven lines read 1. The read answers the
//! same byte at every address of the window.
//!
//! # The remote side
//!
//! The remote levels arrive through the input sink. The event form is
//! `link <white> <red>` with the level words `0` and `1`; the event
//! sets both remote levels. A real two-device cable is a machine
//! concern; the single device answers the injected remote side.
//!
//! The device snapshots under the `TILK` tag, version 1: the drive
//! levels, the enables, and the remote levels in three packed bytes
//! and two reserved bytes.

use demu_config::model::BootState;
use demu_core::device::{AccessWidth, BusAccess, InputSink, MachineCtx};
use demu_core::frequency::{Address, Tick};
use demu_core::signal::Level;
use demu_core::{Device, Snapshot, SnapshotError};

/// The link snapshot byte length: tag and version, the three packed
/// state bytes, and the two reserved bytes.
const LINK_SNAPSHOT_LEN: usize = 10;

/// The remote line levels at reset: the idle cable floats both lines
/// high.
const REMOTE_IDLE: u8 = 0x03;

/// The TI-85 link port.
#[derive(Debug)]
pub struct Ti85Link {
    name: String,
    /// The local drive levels: bit 0 the white line, bit 1 the red
    /// line.
    drive: u8,
    /// The output enables: bit 0 the white line, bit 1 the red line.
    drive_enable: u8,
    /// The remote line levels: bit 0 the white line, bit 1 the red
    /// line.
    remote: u8,
}

impl Ti85Link {
    /// A link port from its configuration entry.
    pub fn new(device: &demu_config::model::DeviceInstance) -> Self {
        Self {
            name: device.name.to_string(),
            drive: 0,
            drive_enable: 0,
            remote: REMOTE_IDLE,
        }
    }

    /// The effective line levels: the enabled lines read their local
    /// drive, the floating lines read the remote level.
    fn levels(&self) -> u8 {
        let white = if self.drive_enable & 0x01 != 0 {
            self.drive & 0x01
        } else {
            self.remote & 0x01
        };
        let red = if self.drive_enable & 0x02 != 0 {
            self.drive & 0x02
        } else {
            self.remote & 0x02
        };
        white | red
    }
}

impl BusAccess for Ti85Link {
    fn read(&mut self, _width: AccessWidth, _address: Address, _tick: Tick) -> u32 {
        u32::from(self.levels())
    }

    fn write(&mut self, _width: AccessWidth, _address: Address, value: u32, _tick: Tick) {
        let v = value as u8;
        // Bits 0-1 carry the drive levels and bits 2-3 the enables.
        self.drive = v & 0x03;
        self.drive_enable = (v >> 2) & 0x03;
    }

    fn peek(&self, _address: Address) -> u8 {
        self.levels()
    }

    fn decodes(&self, _address: Address) -> bool {
        true
    }
}

impl InputSink for Ti85Link {
    fn inject(&mut self, event: &str) -> bool {
        // The event form `link <white> <red>`: the level words 0 and
        // 1 set the far end of the cable.
        let mut parts = event.split_whitespace();
        if parts.next() != Some("link") {
            return false;
        }
        let white = match parts.next() {
            Some("0") => 0,
            Some("1") => 1,
            _ => return false,
        };
        let red = match parts.next() {
            Some("0") => 0,
            Some("1") => 1,
            _ => return false,
        };
        self.remote = (red << 1) | white;
        true
    }
}

impl Device for Ti85Link {
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
        // The published reset form: the drive is low, no line drives,
        // and the idle cable floats both remote levels high.
        self.drive = 0;
        self.drive_enable = 0;
        self.remote = REMOTE_IDLE;
    }

    fn boot(&mut self, _state: Option<&BootState>) {
        self.reset();
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(LINK_SNAPSHOT_LEN);
        out.extend_from_slice(b"TILK");
        out.push(1);
        out.push(self.drive & 0x03);
        out.push(self.drive_enable & 0x03);
        out.push(self.remote & 0x03);
        out.push(0);
        out.push(0);
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        if data.len() != LINK_SNAPSHOT_LEN || &data[..4] != b"TILK" || data[4] != 1 {
            return Err(SnapshotError::Mismatch(
                "ti85 link chunk must be 10 bytes".into(),
            ));
        }
        self.drive = data[5] & 0x03;
        self.drive_enable = data[6] & 0x03;
        self.remote = data[7] & 0x03;
        Ok(())
    }

    fn notify_line(&mut self, _line: &str, _level: Level, _tick: Tick) {}

    fn tick(&mut self, _tick: Tick, _ctx: &mut MachineCtx) {}
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
            name: DeviceName::new("link0").expect("name"),
            kind: KindName::new("ti85.link").expect("kind"),
            domain: "main".to_string(),
            params: BTreeMap::new(),
            dividers: BTreeMap::new(),
        }
    }

    fn link() -> Ti85Link {
        Ti85Link::new(&instance())
    }

    fn wr(s: &mut Ti85Link, b: u8) {
        s.write(AccessWidth::Byte, Address(0x05), u32::from(b), Tick(0));
    }

    fn rd(s: &mut Ti85Link) -> u8 {
        s.read(AccessWidth::Byte, Address(0x06), Tick(0)) as u8
    }

    #[test]
    fn enabled_lines_read_the_local_drives() {
        let mut s = link();
        // Both lines enabled at level 1.
        wr(&mut s, 0x0F);
        assert_eq!(rd(&mut s), 0x03);
        // The white line enabled at 1, the red enabled at 0.
        wr(&mut s, 0x0D);
        assert_eq!(rd(&mut s), 0x01);
        // The white line disabled, the red enabled at 0: the floating
        // white line reads the idle remote 1.
        wr(&mut s, 0x08);
        assert_eq!(rd(&mut s), 0x01);
    }

    #[test]
    fn disabled_lines_read_the_remote_levels() {
        let mut s = link();
        // The idle cable floats both lines high.
        wr(&mut s, 0x00);
        assert_eq!(rd(&mut s), 0x03);
        assert!(s.inject("link 1 0"));
        // The remote white level rides the floating white line, the
        // remote red level the floating red line.
        assert_eq!(rd(&mut s), 0x01);
        assert!(s.inject("link 0 0"));
        assert_eq!(rd(&mut s), 0x00);
    }

    #[test]
    fn enabled_lines_override_the_remote_levels() {
        let mut s = link();
        assert!(s.inject("link 1 1"));
        wr(&mut s, 0x0F);
        assert_eq!(rd(&mut s), 0x03);
        // The red line enabled at 0 reads locally; the floating white
        // line reads the remote 1.
        wr(&mut s, 0x08);
        assert_eq!(rd(&mut s), 0x01);
    }

    #[test]
    fn event_grammar_takes_the_link_form_only() {
        let mut s = link();
        assert!(s.inject("link 0 1"));
        assert!(s.inject("link 1 1"));
        assert!(!s.inject("link 2 0"));
        assert!(!s.inject("link 0"));
        assert!(!s.inject("link"));
        assert!(!s.inject("press a"));
        assert!(!s.inject(""));
    }

    #[test]
    fn event_words_map_to_the_remote_bits() {
        let mut s = link();
        // `link <white> <red>`: white first.
        assert!(s.inject("link 0 1"));
        assert_eq!(s.remote, 0x02);
        assert!(s.inject("link 1 0"));
        assert_eq!(s.remote, 0x01);
    }

    #[test]
    fn reset_returns_the_idle_float() {
        let mut s = link();
        wr(&mut s, 0x0F);
        assert!(s.inject("link 0 0"));
        s.reset();
        assert_eq!(s.drive, 0);
        assert_eq!(s.drive_enable, 0);
        assert_eq!(s.remote, REMOTE_IDLE);
        assert_eq!(rd(&mut s), 0x03);
        // The boot form matches reset.
        s.boot(None);
        assert_eq!(s.drive_enable, 0);
    }

    #[test]
    fn every_window_address_answers_the_same_byte() {
        let mut s = link();
        wr(&mut s, 0x0E);
        assert_eq!(s.peek(Address(0x05)), 0x02);
        assert_eq!(s.peek(Address(0x06)), 0x02);
        assert_eq!(rd(&mut s), 0x02);
        assert!(s.decodes(Address(0x05)));
        assert!(s.decodes(Address(0x06)));
    }

    #[test]
    fn snapshot_round_trips_and_rejects_lengths() {
        let mut s = link();
        wr(&mut s, 0x0F);
        assert!(s.inject("link 0 1"));
        let snap = s.snapshot();
        assert_eq!(snap.len(), LINK_SNAPSHOT_LEN);
        assert_eq!(&snap[..4], b"TILK");
        let mut twin = link();
        twin.restore(&snap).expect("restore");
        assert_eq!(twin.snapshot(), snap);
        assert_eq!(twin.drive, 0x03);
        assert_eq!(twin.drive_enable, 0x03);
        assert_eq!(twin.remote, 0x02);
        assert_eq!(rd(&mut twin), rd(&mut s));
        // The length and tag guards.
        assert!(twin.restore(&snap[..snap.len() - 1]).is_err());
        let mut bad = snap.clone();
        bad[0] = b'X';
        assert!(twin.restore(&bad).is_err());
    }

    /// The mini machine: the link window at `$05-$06` on the Z80 I/O
    /// page form.
    fn link_config() -> demu_config::model::SystemConfig {
        let link = instance();
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
        by_name.insert(link.name.clone(), 0usize);
        demu_config::model::SystemConfig {
            name: "ti85-test".to_string(),
            triple: "ti/z80".to_string(),
            description: "the link test machine".to_string(),
            clocks,
            devices: vec![link],
            devices_by_name: by_name,
            buses: vec![CBus {
                name: "cpu".to_string(),
                address_bits: 16,
                owner: None,
                parent: None,
            }],
            windows: vec![CWindow {
                name: "link".to_string(),
                bus: "cpu".to_string(),
                device: DeviceName::new("link0").expect("name"),
                base: 0x0005,
                size: 0x0002,
                kind: None,
                mirror: None,
                overlay: None,
                overlay_read_priority: None,
                overlay_write_priority: None,
                conflict: None,
            }],
            connections: Vec::new(),
            signals: Vec::new(),
            boot: BTreeMap::new(),
            quirks: BTreeMap::new(),
        }
    }

    #[test]
    fn machine_writes_and_injects_through_the_window() {
        let mut ctors = demu_core::smoke_ctors();
        ctors.register("ti85.link", Box::new(|d, _| Ok(Box::new(Ti85Link::new(d)))));
        let mut machine = Machine::assemble(link_config(), &ctors).expect("assemble");
        machine.boot();
        // Both lines enabled, both drive levels high.
        assert!(machine.debug_write("cpu", Address(0x05), 0x0F));
        assert!(
            machine
                .device_by_name_mut("link0")
                .expect("device")
                .input_mut()
                .expect("sink")
                .inject("link 1 0")
        );
        // The enabled drives read back locally.
        assert_eq!(machine.peek("cpu", Address(0x05)), 0x03);
        // With the drive off, the floating lines read the remote
        // levels: white 1, red 0.
        machine.debug_write("cpu", Address(0x05), 0x00);
        assert_eq!(machine.peek("cpu", Address(0x05)), 0x01);
        assert_eq!(machine.peek("cpu", Address(0x06)), 0x01);
    }
}
