//! The PC Engine turbo-tap pad of the HuC6280 machine.
//!
//! One device carries the controller port register of the HuC6280 I/O
//! page and the turbo model of the demu machine family. The machine
//! maps the I/O window over `$1000-$13FF` and the device answers the
//! offsets of that window: the pad register at offset 0, the turbo
//! rate byte at offset 5, and the turbo enable bits at offset 6.
//!
//! # Pad register
//!
//! The write to offset 0 carries the pad select byte: bit 0 selects
//! the multiplexer row and bit 1 is the published CLR form (the
//! multitap counter reset, which this single-port build keeps out of
//! the model); bit 7 stores the six-button protocol bit.
//! The read of offset 0 returns the published pad form:
//!
//! - bit 7 reports the CD addon line, present when the `cd_addon`
//!   parameter is set;
//! - bit 6 reports the country line: set for the PC Engine (Japan)
//!   region and clear for the TurboGrafx-16 (USA) region, per the
//!   MagicKit hardware text and the Mednafen readback, the `japan`
//!   parameter;
//! - bits 5-4 pull high;
//! - bits 3-0 carry the selected row's buttons, active low: a pressed
//!   button reads clear.
//!
//! The published row layout: a select of 1 returns the directions row
//! (up at bit 0, right at bit 1, down at bit 2, left at bit 3) and a
//! select of 0 returns the buttons row (button A/I at bit 0, B/II at
//! bit 1, Start/Run at bit 2, Select at bit 3); the published CLR bit
//! of the select byte does not change the row. Selects that read the
//! six-button extension rows (the protocol bit's second states)
//! return the idle row form in this build and the extension is
//! recorded as an approximation. The read of a held enabled button
//! during the pulse off sub-period reports the released form.
//!
//! # Turbo registers
//!
//! The turbo rate byte (offset 5) carries the visit cadence of the
//! autofire form in domain ticks, and the turbo enable bits (offset 6)
//! select the buttons the form covers: bit 0 covers button A/I and
//! bit 1 covers button B/II; the other bits store without effect. The
//! scheduled visit at the rate cadence toggles the pulse, so a held
//! enabled button reports pressed for one rate period and released for
//! the next. A rate byte of zero stops the cadence. The published
//! turbo pads autofire by a mechanical switch; these two registers are
//! the demu model surface, and the visit cadence form is recorded.
//!
//! # Event grammar
//!
//! The input sink accepts `press <button>` and `release <button>`
//! events for `up`, `down`, `left`, `right`, `a`, `b`, `select`,
//! `start`, and their published native names `i`, `ii`, and `run` (the
//! pairs are aliases: A carries I, B carries II, and Start carries
//! Run). The event `press vi` stores the sixth button state; the rows
//! that would report it are the recorded idle forms of the six-button
//! extension.
//!
//! The device snapshots under the `PJOY` tag, version 1: the select,
//! protocol, and turbo state bytes. It schedules no visit while the
//! turbo cadence is off.

use demu_config::model::BootState;
use demu_core::device::{AccessWidth, BusAccess, InputSink, MachineCtx};
use demu_core::frequency::{Address, Tick};
use demu_core::signal::Level;
use demu_core::{Device, Snapshot, SnapshotError};

/// The window coordinate mask of the pad page: the machine maps the
/// I/O window over `$1000-$13FF` and the device answers its offsets.
const PAGE_MASK: u32 = 0x03FF;

/// The offset of the pad select and data register.
const OFF_PAD: u32 = 0x0000;
/// The offset of the turbo rate byte.
const OFF_TURBO_RATE: u32 = 0x0005;
/// The offset of the turbo enable bits.
const OFF_TURBO_ENABLE: u32 = 0x0006;

/// The button masks of the buttons row, one bit per button.
const ACT_A: u8 = 0x01;
const ACT_B: u8 = 0x02;
const ACT_START: u8 = 0x04;
const ACT_SELECT: u8 = 0x08;

/// The turbo-pair mapping of the enable bits: bit n of the enable byte
/// covers the button of the same act bit.
const TURBO_ACTS: u8 = ACT_A | ACT_B;

/// The PC Engine turbo-tap pad: the controller port register of the
/// I/O page with the turbo autofire model.
#[derive(Debug)]
pub struct PceTurbotap {
    name: String,
    /// True when the read reports a Japanese console.
    japan: bool,
    /// True when the read reports the CD addon present.
    cd_addon: bool,
    /// The stored select bits of the pad select byte.
    select: u8,
    /// The stored six-button protocol bit of the pad select byte.
    protocol: bool,
    /// The held directions: up, right, down, left at bits 3 down to 0.
    dirs: u8,
    /// The held buttons: A/I, B/II, Start/Run, Select at bits 3 down
    /// to 0.
    acts: u8,
    /// The stored sixth button state of the six-button extension.
    vi: bool,
    /// The turbo pulse cadence in domain ticks; zero stops it.
    turbo_rate: u8,
    /// The turbo enable bits, one per autofire pair.
    turbo_enable: u8,
    /// True during the pulse off sub-period: the held enabled buttons
    /// report released.
    turbo_off: bool,
    next: Option<Tick>,
    last_tick: Option<Tick>,
}

impl PceTurbotap {
    /// A device from its configuration entry.
    pub fn new(device: &demu_config::model::DeviceInstance) -> Self {
        Self {
            name: device.name.to_string(),
            japan: device
                .params
                .get("japan")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            cd_addon: device
                .params
                .get("cd_addon")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            select: 0,
            protocol: false,
            dirs: 0,
            acts: 0,
            vi: false,
            turbo_rate: 0,
            turbo_enable: 0,
            turbo_off: false,
            next: None,
            last_tick: None,
        }
    }

    /// True while the turbo cadence runs: the rate byte is set and an
    /// enable bit covers a modelled button.
    fn turbo_runs(&self) -> bool {
        self.turbo_rate != 0 && self.turbo_enable & TURBO_ACTS != 0
    }

    /// Schedule the turbo visit or drop the schedule.
    fn schedule(&mut self, tick: Tick) {
        self.next = if self.turbo_runs() {
            Some(Tick(tick.0 + u64::from(self.turbo_rate)))
        } else {
            None
        };
    }

    /// The button form the read reports: a held button whose enable
    /// bit is set reads released during the pulse off sub-period.
    fn reported_acts(&self) -> u8 {
        if self.turbo_off {
            self.acts & !(self.turbo_enable & TURBO_ACTS)
        } else {
            self.acts
        }
    }

    /// The pad data byte: the published country and CD addon lines,
    /// the spare bits pulled high, and the selected row's buttons
    /// active low.
    fn pad_byte(&self) -> u8 {
        // The country line: set for the PC Engine (Japan) region and
        // clear for the TurboGrafx-16 (USA) region.
        let flags = (u8::from(self.cd_addon) << 7) | if self.japan { 0x40 } else { 0x00 };
        // The select byte stores masked to the row bit, so the row is
        // the bit itself.
        let row = if self.select & 0x01 != 0 {
            !self.dirs & 0x0F
        } else {
            !self.reported_acts() & 0x0F
        };
        flags | 0x30 | row
    }
}

/// The mask of one button event name in the buttons row state, or zero.
fn acts_mask(name: &str) -> u8 {
    match name {
        "a" | "i" => ACT_A,
        "b" | "ii" => ACT_B,
        "start" | "run" => ACT_START,
        "select" => ACT_SELECT,
        _ => 0,
    }
}

/// The mask of one direction event name, or zero.
fn dirs_mask(name: &str) -> u8 {
    match name {
        "up" => 0x01,
        "right" => 0x02,
        "down" => 0x04,
        "left" => 0x08,
        _ => 0,
    }
}

impl InputSink for PceTurbotap {
    fn inject(&mut self, event: &str) -> bool {
        let (verb, button) = event.split_once(' ').unwrap_or((event, ""));
        let pressed = match verb.trim().to_ascii_lowercase().as_str() {
            "press" => true,
            "release" => false,
            _ => return false,
        };
        let name = button.trim().to_ascii_lowercase();
        if name == "vi" {
            self.vi = pressed;
            return true;
        }
        let mask = acts_mask(&name) | dirs_mask(&name);
        if mask == 0 {
            return false;
        }
        if acts_mask(&name) != 0 {
            self.acts = if pressed {
                self.acts | mask
            } else {
                self.acts & !mask
            };
        } else {
            self.dirs = if pressed {
                self.dirs | mask
            } else {
                self.dirs & !mask
            };
        }
        true
    }
}

impl BusAccess for PceTurbotap {
    fn read(&mut self, _width: AccessWidth, address: Address, _tick: Tick) -> u32 {
        match address.0 & PAGE_MASK {
            OFF_PAD => u32::from(self.pad_byte()),
            OFF_TURBO_RATE => u32::from(self.turbo_rate),
            OFF_TURBO_ENABLE => u32::from(self.turbo_enable),
            // The rest of the page reads the open-bus high form.
            _ => 0xFF,
        }
    }

    fn write(&mut self, _width: AccessWidth, address: Address, value: u32, tick: Tick) {
        let v = value as u8;
        match address.0 & PAGE_MASK {
            OFF_PAD => {
                // Bit 0 selects the multiplexer row of the published
                // 2-button form; bit 1 is the CLR form (the multitap
                // counter reset of the published port, kept out of
                // this single-port model) and bit 7 stores the
                // six-button protocol bit of the published pad
                // protocol, recorded here as stored state.
                self.select = v & 0x01;
                self.protocol = v & 0x80 != 0;
            }
            OFF_TURBO_RATE => {
                self.turbo_rate = v;
                self.schedule(tick);
            }
            OFF_TURBO_ENABLE => {
                self.turbo_enable = v;
                self.schedule(tick);
            }
            _ => {}
        }
    }

    fn peek(&self, address: Address) -> u8 {
        match address.0 & PAGE_MASK {
            OFF_PAD => self.pad_byte(),
            OFF_TURBO_RATE => self.turbo_rate,
            OFF_TURBO_ENABLE => self.turbo_enable,
            _ => 0xFF,
        }
    }
}

impl Device for PceTurbotap {
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
        self.select = 0;
        self.protocol = false;
        self.dirs = 0;
        self.acts = 0;
        self.vi = false;
        self.turbo_rate = 0;
        self.turbo_enable = 0;
        self.turbo_off = false;
        self.next = None;
        self.last_tick = None;
    }

    fn boot(&mut self, state: Option<&BootState>) {
        let _ = state;
        self.reset();
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(13);
        out.extend_from_slice(b"PJOY");
        out.push(1);
        out.push(self.select);
        out.push(self.acts);
        out.push(self.dirs);
        out.push(u8::from(self.protocol) | (u8::from(self.vi) << 1));
        out.push(self.turbo_rate);
        out.push(self.turbo_enable);
        out.push(u8::from(self.turbo_off));
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        let expected = 12;
        if data.len() != expected || &data[..4] != b"PJOY" || data[4] != 1 {
            return Err(SnapshotError::Mismatch(format!(
                "turbo-tap chunk must be {expected} bytes"
            )));
        }
        self.select = data[5];
        self.acts = data[6];
        self.dirs = data[7];
        self.protocol = data[8] & 0x01 != 0;
        self.vi = data[8] & 0x02 != 0;
        self.turbo_rate = data[9];
        self.turbo_enable = data[10];
        self.turbo_off = data[11] != 0;
        self.next = None;
        self.last_tick = None;
        Ok(())
    }

    fn notify_line(&mut self, _line: &str, _level: Level, _tick: Tick) {}

    fn next_event(&self) -> Option<Tick> {
        self.next
    }

    fn tick(&mut self, tick: Tick, _ctx: &mut MachineCtx) {
        if self.last_tick.is_some_and(|t| t.0 >= tick.0) {
            return;
        }
        self.last_tick = Some(tick);
        if self.turbo_runs() {
            // The pulse flips at the visit cadence of the rate byte.
            self.turbo_off = !self.turbo_off;
            self.next = Some(Tick(tick.0 + u64::from(self.turbo_rate)));
        } else {
            self.next = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use demu_core::device::{BusBank, DeviceSlices, EventBuffers};

    fn instance() -> demu_config::model::DeviceInstance {
        demu_config::model::DeviceInstance {
            name: demu_config::newtypes::DeviceName::new("pad0").expect("name"),
            kind: demu_config::newtypes::KindName::new("pce.joypad").expect("kind"),
            domain: "main".to_string(),
            params: std::collections::BTreeMap::new(),
            dividers: std::collections::BTreeMap::new(),
        }
    }

    fn pad() -> PceTurbotap {
        PceTurbotap::new(&instance())
    }

    /// Run one device visit under an empty context. The turbo visit
    /// carries no notification, so the context needs no devices.
    fn visit(pad: &mut PceTurbotap, tick: Tick) {
        let mut before: Vec<Box<dyn demu_core::Device>> = Vec::new();
        let mut after: Vec<Box<dyn demu_core::Device>> = Vec::new();
        let mut buses: Vec<demu_core::Bus> = Vec::new();
        let mut scratch: Vec<demu_core::DecodeHit> = Vec::new();
        let mut events: Vec<demu_core::TriggerEvent> = Vec::new();
        let mut dirty: Vec<usize> = Vec::new();
        let index = Default::default();
        let mut ctx = MachineCtx::new(
            DeviceSlices {
                before: before.as_mut_slice(),
                after: after.as_mut_slice(),
                caller_slot: 0,
            },
            BusBank {
                buses: buses.as_mut_slice(),
                index: &index,
            },
            &[],
            &mut scratch,
            EventBuffers {
                events: &mut events,
                dirty: &mut dirty,
            },
            false,
        );
        pad.tick(tick, &mut ctx);
    }

    #[test]
    fn rows_return_published_button_nibbles() {
        let mut p = pad();
        assert!(p.inject("press a"));
        assert!(p.inject("press left"));
        // Row 0 carries the buttons row (A at bit 0, pressed low).
        assert_eq!(p.peek(Address(0x1000)), 0x3E);
        // Row 1 carries the directions row (left at bit 3, pressed).
        p.write(AccessWidth::Byte, Address(0x1000), 0x01, Tick(0));
        assert_eq!(p.read(AccessWidth::Byte, Address(0x1000), Tick(0)), 0x37);
        // Row 0 again through the clear select: the release form of
        // the buttons row holds.
        p.write(AccessWidth::Byte, Address(0x1000), 0x00, Tick(0));
        assert_eq!(p.read(AccessWidth::Byte, Address(0x1000), Tick(0)), 0x3E);
    }

    #[test]
    fn the_clr_bit_keeps_the_select_row_on_a_mid_scan_write() {
        // The published port form: bit 0 selects the row, and the CLR
        // bit of a mid-scan $03 write resets the multitap counter
        // without touching the row.
        let mut p = pad();
        assert!(p.inject("press a"));
        p.write(AccessWidth::Byte, Address(0x1000), 0x01, Tick(0));
        assert_eq!(p.peek(Address(0x1000)), 0x3F);
        p.write(AccessWidth::Byte, Address(0x1000), 0x03, Tick(0));
        assert_eq!(
            p.peek(Address(0x1000)),
            0x3F,
            "the row held through the CLR write"
        );
        // The SEL clear returns the buttons row.
        p.write(AccessWidth::Byte, Address(0x1000), 0x00, Tick(0));
        assert_eq!(
            p.peek(Address(0x1000)),
            0x3E,
            "the pressed A shows on the buttons row"
        );
    }

    #[test]
    fn the_country_line_follows_the_japan_parameter() {
        let japan = {
            let mut inst = instance();
            inst.params
                .insert("japan".to_string(), toml::Value::Boolean(true));
            PceTurbotap::new(&inst)
        };
        // The PC Engine form: the country line sets bit 6.
        assert_eq!(japan.peek(Address(0x1000)) & 0x40, 0x40);
        // The TurboGrafx-16 form: the line sits clear.
        let us = pad();
        assert_eq!(us.peek(Address(0x1000)) & 0x40, 0x00);
        // The idle buttons row keeps its pull-high nibble.
        assert_eq!(japan.peek(Address(0x1000)), 0x7F);
        assert_eq!(us.peek(Address(0x1000)), 0x3F);
    }

    #[test]
    fn idle_and_extension_rows_pull_high() {
        let mut p = pad();
        assert_eq!(p.peek(Address(0x1000)), 0x3F);
        // Selects 2 and 3 answer the idle form of the six-button
        // extension; the protocol bit stores without effect.
        p.write(AccessWidth::Byte, Address(0x1000), 0x82, Tick(0));
        assert!(p.protocol);
        assert_eq!(p.peek(Address(0x1000)), 0x3F);
        assert!(p.inject("press vi"));
        assert_eq!(p.peek(Address(0x1000)), 0x3F);
    }

    #[test]
    fn turbo_pulses_at_rate_cadence() {
        let mut p = pad();
        assert!(p.inject("press a"));
        p.write(AccessWidth::Byte, Address(0x1005), 10, Tick(0));
        p.write(AccessWidth::Byte, Address(0x1006), 0x01, Tick(0));
        assert_eq!(p.next_event(), Some(Tick(10)));
        // The pulse off sub-period masks the held enabled button and
        // the visit at the cadence flips the form.
        visit(&mut p, Tick(10));
        assert!(p.turbo_off);
        assert_eq!(p.peek(Address(0x1000)), 0x3F);
        assert_eq!(p.next_event(), Some(Tick(20)));
        // The second form reports the pressed button again.
        visit(&mut p, Tick(20));
        assert!(!p.turbo_off);
        // The rate of zero stops the cadence.
        p.write(AccessWidth::Byte, Address(0x1005), 0, Tick(30));
        assert_eq!(p.next_event(), None);
    }

    #[test]
    fn turbo_enable_bits_cover_the_button_pairs() {
        let mut p = pad();
        assert!(p.inject("press b"));
        p.write(AccessWidth::Byte, Address(0x1005), 4, Tick(0));
        p.write(AccessWidth::Byte, Address(0x1006), 0x02, Tick(0));
        assert_eq!(p.next_event(), Some(Tick(4)));
        assert_eq!(p.peek(Address(0x1000)), 0x3D);
        p.turbo_off = true;
        assert_eq!(p.peek(Address(0x1000)), 0x3F);
        p.turbo_off = false;
        assert_eq!(p.peek(Address(0x1000)), 0x3D);
    }

    #[test]
    fn turbo_off_leaves_buttons_intact() {
        let mut p = pad();
        assert!(p.inject("press a"));
        // The enable byte clear reads the held form regardless of the
        // rate byte.
        p.write(AccessWidth::Byte, Address(0x1005), 8, Tick(0));
        p.turbo_off = true;
        assert_eq!(p.peek(Address(0x1000)), 0x3E);
    }

    #[test]
    fn unmodeled_offsets_read_open_bus() {
        let mut p = pad();
        p.write(AccessWidth::Byte, Address(0x1002), 0x55, Tick(0));
        assert_eq!(p.peek(Address(0x1002)), 0xFF);
        assert_eq!(p.peek(Address(0x1001)), 0xFF);
        assert_eq!(p.peek(Address(0x1005)), 0);
    }

    #[test]
    fn event_grammar_accepts_aliases_and_rejects_unknown() {
        let mut p = pad();
        assert!(p.inject("press i"));
        assert!(p.inject("press run"));
        assert_eq!(p.acts, ACT_A | ACT_START);
        assert!(p.inject("release ii"));
        assert!(!p.inject("press zzz"));
        assert!(!p.inject("toggle a"));
    }

    #[test]
    fn deterministic_state_and_schedule() {
        let mut a = pad();
        let mut b = pad();
        for p in [&mut a, &mut b] {
            assert!(p.inject("press a"));
            p.write(AccessWidth::Byte, Address(0x1005), 7, Tick(0));
            p.write(AccessWidth::Byte, Address(0x1006), 0x01, Tick(0));
            visit(p, Tick(7));
        }
        assert_eq!(a.snapshot(), b.snapshot());
        assert_eq!(a.next_event(), b.next_event());
        visit(&mut a, Tick(14));
        visit(&mut b, Tick(14));
        assert_eq!(a.snapshot(), b.snapshot());
    }

    #[test]
    fn snapshot_round_trip_rejects_wrong_length() {
        let mut p = pad();
        assert!(p.inject("press a"));
        p.write(AccessWidth::Byte, Address(0x1005), 7, Tick(0));
        p.write(AccessWidth::Byte, Address(0x1006), 0x01, Tick(0));
        visit(&mut p, Tick(7));
        let snap = p.snapshot();
        let mut twin = pad();
        twin.restore(&snap).expect("restore");
        assert_eq!(twin.snapshot(), snap);
        // The restored twin continues the pulse at the same instants
        // (the savestate machinery re-arms the queue event).
        visit(&mut twin, Tick(14));
        visit(&mut p, Tick(14));
        assert_eq!(twin.snapshot(), p.snapshot());
        assert!(twin.restore(&snap[..snap.len() - 1]).is_err());
        assert!(twin.restore(&[0u8; 12]).is_err());
    }
}
