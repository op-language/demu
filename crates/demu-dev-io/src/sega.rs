//! The Sega 8-bit I/O controller of the SG-1000, the Master System,
//! and the Game Gear.
//!
//! One device carries the two joypad ports and the console keys. The
//! published port layout: `$DC` carries player 1 and `$DD` player 2,
//! with the directions up, down, left, right at bits 0 to 3, the two
//! fire lines at bits 4 and 5, and the TH (the pad select) input line
//! at bit 6. A pressed control reads 0; a released control reads 1.
//! The Game Gear form adds the console key port `$00`: bit 7 carries
//! the START key (0 when pressed) and bit 6 reads the parallel line,
//! which stays 0 when the machine maps no parallel data.
//!
//! # The TH swap rule
//!
//! The published pad form: while the TH line reads low, bit 4 reports
//! fire line 1 and bit 5 reports fire line 2; while TH reads high, the
//! two fire lines swap their bit positions. The TH state comes from
//! the injected `th-0` and `th-1` events and stays low otherwise; bit
//! 6 always reads the stored TH level.
//!
//! # Event grammar
//!
//! The input sink accepts `press <key>` and `release <key>` events (a
//! bare event name presses, and the `-release` suffix releases). The
//! key names carry the optional player prefix `p1-` or `p2-` (a bare
//! name aliases player 1) and one of the controls `up`, `down`,
//! `left`, `right`, `1`, `2`, `start`, `th-0`, `th-1`. The TH events
//! set the named TH level for their player by either verb.
//!
//! The console keys: a `pause` press on the sms form latches an NMI
//! pulse, the form the published PAUSE button drives over the NMI
//! line: High at the next visit, Low at the visit after. A release
//! does nothing, and a press during the pulse restarts it. On the game
//! gear form the START key stores its state for the `$00` read; the
//! quirk `sega.gg_start` makes a START press also latch the same NMI
//! pulse, which published Game Gear hardware does not drive. The other
//! forms answer pause with no action.
//!
//! # Save state
//!
//! The save state rides the `SGIO` tag, version 1: the pressed bits of
//! both players, the TH levels, the START states, and the pulse stage.
//! The pulse stage rides the state but the pending stage visit drops at
//! a restore; the stage machinery re-arms on the next latch.
//!
//! # Recorded approximations
//!
//! - The light-pen lines of the published `$DD` port stay out of the
//!   model; the spare bits read high.
//! - The pulse stages run at device visits; the stage width is one
//!   visit and the pulse width is therefore not the published frame
//!   length.

use demu_config::model::BootState;
use demu_core::device::{AccessWidth, BusAccess, InputSink, MachineCtx};
use demu_core::frequency::{Address, Tick};
use demu_core::signal::Level;
use demu_core::{Device, Snapshot, SnapshotError};

/// The pressed-state masks, one bit per control. A pressed control
/// sets its bit.
const MASK_UP: u8 = 0x01;
const MASK_DOWN: u8 = 0x02;
const MASK_LEFT: u8 = 0x04;
const MASK_RIGHT: u8 = 0x08;
const MASK_FIRE1: u8 = 0x10;
const MASK_FIRE2: u8 = 0x20;
const MASK_DIRS: u8 = MASK_UP | MASK_DOWN | MASK_LEFT | MASK_RIGHT;

/// The NMI pulse stages: idle, the pending High visit, the pending Low
/// visit.
const PULSE_IDLE: u8 = 0;
const PULSE_HIGH: u8 = 1;
const PULSE_LOW: u8 = 2;

/// The save state chunk length.
const SNAPSHOT_LEN: usize = 10;

/// One injected console key.
enum SegKey {
    /// A pad control with its pressed-state mask.
    Pad(u8),
    /// The TH level a `th-0` or `th-1` event sets.
    Th(bool),
    /// The START key.
    Start,
    /// The console pause key.
    Pause,
}

/// The Sega console form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SegaForm {
    /// The SG-1000.
    Sg1000,
    /// The Master System.
    Sms,
    /// The Game Gear.
    Gg,
}

/// The Sega 8-bit I/O controller: the joypad ports and the console
/// keys of one form.
#[derive(Debug)]
pub struct SegaIo {
    name: String,
    /// The console form; it selects the extra key port.
    form: SegaForm,
    /// True when a game gear START press also latches the NMI pulse,
    /// from the `sega.gg_start` quirk key of the `sega` family.
    gg_start_nmi: bool,
    /// Player 1 pressed bits.
    p1: u8,
    /// Player 1 TH line state.
    p1_th: bool,
    /// Player 2 pressed bits.
    p2: u8,
    /// Player 2 TH line state.
    p2_th: bool,
    /// Player 1 START key state.
    p1_start: bool,
    /// Player 2 START key state.
    p2_start: bool,
    /// The NMI pulse stage.
    pulse: u8,
    next: Option<Tick>,
    last_tick: Option<Tick>,
}

impl SegaIo {
    /// A device from its configuration entry.
    ///
    /// # Errors
    ///
    /// Returns an error when the `form` parameter is missing or names
    /// no known form.
    pub fn new(
        device: &demu_config::model::DeviceInstance,
        config: &demu_config::model::SystemConfig,
    ) -> Result<Self, demu_core::machine::MachineError> {
        let form = match device
            .params
            .get("form")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
        {
            "sg1000" => SegaForm::Sg1000,
            "sms" => SegaForm::Sms,
            "gg" => SegaForm::Gg,
            form => {
                return Err(demu_core::machine::MachineError::Assembly(format!(
                    "device '{}' of kind '{}' needs a known form, got '{}'",
                    device.name.as_ref(),
                    device.kind.as_str(),
                    form
                )));
            }
        };
        // The quirk key applies when its first segment names the device
        // kind family.
        let gg_start_nmi = (device.kind.family() == "sega")
            && config
                .quirks
                .get("sega.gg_start")
                .is_some_and(|v| v.as_flag());
        Ok(Self {
            name: device.name.to_string(),
            form,
            gg_start_nmi,
            p1: 0,
            p1_th: false,
            p2: 0,
            p2_th: false,
            p1_start: false,
            p2_start: false,
            pulse: PULSE_IDLE,
            next: None,
            last_tick: None,
        })
    }

    /// Latch the NMI pulse: the next visit raises the line and the
    /// visit after drops it. A press during the pulse restarts it.
    fn latch_pulse(&mut self) {
        self.pulse = PULSE_HIGH;
        self.next = Some(Tick(0));
        self.last_tick = None;
    }

    /// The joypad byte of one player: the active-low directions and
    /// fire lines under the TH swap rule, the TH level at bit 6, and
    /// the spare pull-high bit 7.
    fn pad_byte(&self, state: u8, th: bool) -> u8 {
        let dirs = !(state & MASK_DIRS) & MASK_DIRS;
        let fire1 = state & MASK_FIRE1 != 0;
        let fire2 = state & MASK_FIRE2 != 0;
        // The TH low form reports fire 1 at bit 4 and fire 2 at bit 5;
        // the TH high form swaps the two lines.
        let (bit4, bit5) = if th { (fire2, fire1) } else { (fire1, fire2) };
        dirs | (u8::from(!bit4) << 4) | (u8::from(!bit5) << 5) | (u8::from(th) << 6) | 0x80
    }

    /// The Game Gear console key byte: bit 7 low when START is
    /// pressed, and the parallel line and the low bits read low.
    fn key_byte(&self, start: bool) -> u8 {
        if start { 0x00 } else { 0x80 }
    }

    /// Set the pad state of one player.
    fn player_mut(&mut self, player: usize) -> &mut u8 {
        if player == 1 {
            &mut self.p2
        } else {
            &mut self.p1
        }
    }
}

/// The player pad that one event key prefix selects, or `None`. A bare
/// name aliases player 1.
fn pad_of(prefix: &str) -> Option<usize> {
    match prefix {
        "p1" => Some(0),
        "p2" => Some(1),
        _ => None,
    }
}

/// Split one event name into its player pad and its key.
fn key_of(name: &str) -> Option<(usize, SegKey)> {
    let (player, ctrl) = match name.split_once('-') {
        Some((prefix, rest)) => (pad_of(prefix)?, rest),
        None => (0, name),
    };
    let key = match ctrl {
        "th-1" => SegKey::Th(true),
        "th-0" => SegKey::Th(false),
        "up" => SegKey::Pad(MASK_UP),
        "down" => SegKey::Pad(MASK_DOWN),
        "left" => SegKey::Pad(MASK_LEFT),
        "right" => SegKey::Pad(MASK_RIGHT),
        "1" => SegKey::Pad(MASK_FIRE1),
        "2" => SegKey::Pad(MASK_FIRE2),
        "start" => SegKey::Start,
        "pause" => SegKey::Pause,
        _ => return None,
    };
    Some((player, key))
}

impl InputSink for SegaIo {
    fn inject(&mut self, event: &str) -> bool {
        let text = event.trim().to_ascii_lowercase();
        let (pressed, name) = if let Some((verb, rest)) = text.split_once(' ') {
            let pressed = match verb {
                "press" => true,
                "release" => false,
                _ => return false,
            };
            (pressed, rest.trim())
        } else if let Some(base) = text.strip_suffix("-release") {
            (false, base)
        } else {
            (true, text.as_str())
        };
        let Some((player, key)) = key_of(name) else {
            return false;
        };
        match key {
            SegKey::Pad(mask) => {
                let state = self.player_mut(player);
                if pressed {
                    *state |= mask;
                } else {
                    *state &= !mask;
                }
                true
            }
            SegKey::Th(level) => {
                if player == 0 {
                    self.p1_th = level;
                } else {
                    self.p2_th = level;
                }
                true
            }
            SegKey::Start => {
                if player == 0 {
                    self.p1_start = pressed;
                } else {
                    self.p2_start = pressed;
                }
                if pressed && player == 0 && self.form == SegaForm::Gg && self.gg_start_nmi {
                    self.latch_pulse();
                }
                true
            }
            SegKey::Pause => {
                if self.form != SegaForm::Sms {
                    return false;
                }
                if pressed {
                    self.latch_pulse();
                }
                // The release does nothing.
                true
            }
        }
    }
}

impl BusAccess for SegaIo {
    fn read(&mut self, _width: AccessWidth, address: Address, _tick: Tick) -> u32 {
        u32::from(self.peek(address))
    }

    fn write(&mut self, _width: AccessWidth, _address: Address, _value: u32, _tick: Tick) {
        // The ports carry no writable registers.
    }

    fn peek(&self, address: Address) -> u8 {
        match address.0 & 0xFFFF {
            0x00 if self.form == SegaForm::Gg => self.key_byte(self.p1_start),
            0xDC => self.pad_byte(self.p1, self.p1_th),
            0xDD => self.pad_byte(self.p2, self.p2_th),
            _ => 0xFF,
        }
    }

    fn decodes(&self, address: Address) -> bool {
        let a = address.0 & 0xFFFF;
        // The published low-byte decode: the pad and pause lines
        // answer ports $DC-$DD through the full port value; the Game
        // Gear start port answers the port plane address $10000.
        match a & 0xFF {
            0x00 if (a & 0xFFFF) >> 8 == 0 && self.form == SegaForm::Gg => true,
            0xDC | 0xDD => true,
            _ => false,
        }
    }
}

impl Device for SegaIo {
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
        self.p1 = 0;
        self.p1_th = false;
        self.p2 = 0;
        self.p2_th = false;
        self.p1_start = false;
        self.p2_start = false;
        self.pulse = PULSE_IDLE;
        self.next = None;
        self.last_tick = None;
    }

    fn boot(&mut self, state: Option<&BootState>) {
        let _ = state;
        self.reset();
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(SNAPSHOT_LEN);
        out.extend_from_slice(b"SGIO");
        out.push(1);
        out.push(self.p1);
        out.push(self.p2);
        out.push(u8::from(self.p1_th) | (u8::from(self.p2_th) << 1));
        out.push(u8::from(self.p1_start) | (u8::from(self.p2_start) << 1));
        out.push(self.pulse);
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        if data.len() != SNAPSHOT_LEN || &data[..4] != b"SGIO" || data[4] != 1 {
            return Err(SnapshotError::Mismatch(format!(
                "sega io chunk must be {SNAPSHOT_LEN} bytes"
            )));
        }
        self.p1 = data[5];
        self.p2 = data[6];
        self.p1_th = data[7] & 0x01 != 0;
        self.p2_th = data[7] & 0x02 != 0;
        self.p1_start = data[8] & 0x01 != 0;
        self.p2_start = data[8] & 0x02 != 0;
        self.pulse = data[9];
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
        match self.pulse {
            PULSE_HIGH => {
                ctx.notify("nmi", Level::High, tick);
                self.pulse = PULSE_LOW;
                self.next = Some(Tick(tick.0 + 1));
            }
            PULSE_LOW => {
                ctx.notify("nmi", Level::Low, tick);
                self.pulse = PULSE_IDLE;
                self.next = None;
            }
            _ => self.next = None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use demu_config::model::{Bus as CBus, ClockDomain, QuirkValue, Window as CWindow};
    use demu_config::newtypes::{DeviceName, KindName};
    use demu_core::machine::Machine;
    use std::collections::BTreeMap;

    fn instance(kind: &str, form: &str) -> demu_config::model::DeviceInstance {
        let mut params = BTreeMap::new();
        params.insert("form".to_string(), toml::Value::String(form.to_string()));
        demu_config::model::DeviceInstance {
            name: DeviceName::new("seg").expect("name"),
            kind: KindName::new(kind).expect("kind"),
            domain: "main".to_string(),
            params,
            dividers: BTreeMap::new(),
        }
    }

    fn config() -> demu_config::model::SystemConfig {
        let mut quirks = BTreeMap::new();
        quirks.insert("sega.gg_start".to_string(), QuirkValue::Bool(true));
        config_with_quirks(quirks)
    }

    fn config_with_quirks(
        quirks: std::collections::BTreeMap<String, QuirkValue>,
    ) -> demu_config::model::SystemConfig {
        demu_config::model::SystemConfig {
            name: "test".to_string(),
            triple: "test".to_string(),
            description: String::new(),
            clocks: BTreeMap::new(),
            devices: Vec::new(),
            devices_by_name: BTreeMap::new(),
            buses: Vec::new(),
            windows: Vec::new(),
            connections: Vec::new(),
            signals: Vec::new(),
            boot: BTreeMap::new(),
            quirks,
        }
    }

    fn device(form: &str) -> SegaIo {
        SegaIo::new(&instance("sega.io", form), &config()).expect("device")
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

    /// A test machine: the pad on the io bus with a recorder on the
    /// NMI line and a smoke CPU for the run loop.
    fn machine(form: &str, quirk: bool) -> Machine {
        let seg = demu_config::model::DeviceInstance {
            name: DeviceName::new("seg").expect("name"),
            kind: KindName::new("sega.io").expect("kind"),
            domain: "main".to_string(),
            params: [("form".to_string(), toml::Value::String(form.to_string()))]
                .into_iter()
                .collect(),
            dividers: BTreeMap::new(),
        };
        let lines = demu_config::model::DeviceInstance {
            name: DeviceName::new("lines").expect("name"),
            kind: KindName::new("test.lines").expect("kind"),
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
                frequency: demu_config::newtypes::Frequency::new(3_579_545, 1).expect("freq"),
                origin: "the machine master".to_string(),
            },
        );
        let mut by_name = BTreeMap::new();
        by_name.insert(seg.name.clone(), 0usize);
        by_name.insert(lines.name.clone(), 1usize);
        by_name.insert(cpu.name.clone(), 2usize);
        let mut quirks = BTreeMap::new();
        if quirk {
            quirks.insert("sega.gg_start".to_string(), QuirkValue::Bool(true));
        }
        let config = demu_config::model::SystemConfig {
            name: "sega-test".to_string(),
            triple: "test".to_string(),
            description: "the Sega io test machine".to_string(),
            clocks,
            devices: vec![seg, lines, cpu],
            devices_by_name: by_name,
            buses: vec![CBus {
                name: "io".to_string(),
                address_bits: 16,
                owner: None,
                parent: None,
            }],
            windows: vec![
                CWindow {
                    name: "key0".to_string(),
                    bus: "io".to_string(),
                    device: DeviceName::new("seg").expect("name"),
                    base: 0x0000,
                    size: 1,
                    kind: None,
                    mirror: None,
                    overlay: None,
                    overlay_read_priority: None,
                    overlay_write_priority: None,
                    conflict: None,
                },
                CWindow {
                    name: "pads".to_string(),
                    bus: "io".to_string(),
                    device: DeviceName::new("seg").expect("name"),
                    base: 0x00DC,
                    size: 2,
                    kind: None,
                    mirror: None,
                    overlay: None,
                    overlay_read_priority: None,
                    overlay_write_priority: None,
                    conflict: None,
                },
            ],
            connections: vec![demu_config::model::Connection {
                from: demu_config::newtypes::LinePath::parse("seg.nmi").expect("route"),
                to: demu_config::newtypes::LinePath::parse("lines.nmi").expect("route"),
                semantics: demu_config::model::ConnectionSemantics::Irq,
                direction: demu_config::model::ConnectionDirection::Forward,
            }],
            signals: Vec::new(),
            boot: BTreeMap::new(),
            quirks,
        };
        let mut ctors = demu_core::smoke_ctors();
        ctors.register("sega.io", Box::new(|d, c| Ok(Box::new(SegaIo::new(d, c)?))));
        ctors.register(
            "test.lines",
            Box::new(|d, _| Ok(Box::new(LineRecorder::new(d.name.as_ref())))),
        );
        Machine::assemble(config, &ctors).expect("assembly")
    }

    fn recorder(machine: &mut Machine) -> &mut LineRecorder {
        machine.devices[1]
            .as_any_mut()
            .downcast_mut::<LineRecorder>()
            .expect("recorder")
    }

    #[test]
    fn unknown_or_missing_form_rejects_the_assembly() {
        assert!(SegaIo::new(&instance("sega.io", "gg"), &config()).is_ok());
        let mut plain = instance("sega.io", "gg");
        plain.params.clear();
        assert!(SegaIo::new(&plain, &config()).is_err());
        assert!(SegaIo::new(&instance("sega.io", "nope"), &config()).is_err());
    }

    #[test]
    fn pad_bits_are_active_low_under_the_th_swap_rule() {
        let mut seg = device("sms");
        // The idle form: the spare bit 7 pulls high, the TH line reads
        // low at bit 6, and four directions pull high.
        assert_eq!(seg.peek(Address(0xDC)), 0xBF);
        assert!(seg.inject("press up"));
        assert_eq!(seg.peek(Address(0xDC)), 0xBE, "bit 0 carries up, low");
        assert!(seg.inject("press 1"));
        // TH low: bit 4 carries fire 1, bit 5 fire 2.
        assert_eq!(seg.peek(Address(0xDC)), 0xAE);
        // TH high swaps the fire lines: fire 2 moves to bit 4 and fire
        // 1 drops to bit 5.
        assert!(seg.inject("press p1-th-1"));
        assert_eq!(seg.peek(Address(0xDC)), 0xDE, "the TH bit lifts");
        assert!(seg.inject("press 2"));
        assert_eq!(seg.peek(Address(0xDC)), 0xCE, "fire 2 sits at bit 4");
        assert!(seg.inject("release 1"));
        assert_eq!(seg.peek(Address(0xDC)), 0xEE, "fire 1 lifted at bit 5");
        assert!(seg.inject("press p1-th-0"));
        assert_eq!(seg.peek(Address(0xDC)), 0x9E, "TH low swaps the lines");
    }

    #[test]
    fn player_two_reads_through_dd() {
        let mut seg = device("sms");
        assert!(seg.inject("press p2-left"));
        assert_eq!(seg.peek(Address(0xDD)) & MASK_LEFT, 0, "left pressed");
        assert_eq!(seg.peek(Address(0xDC)) & 0x0F, 0x0F, "player 1 idle");
        assert!(seg.inject("release p2-left"));
        assert_eq!(seg.peek(Address(0xDD)), 0xBF, "the idle pull-high form");
    }

    #[test]
    fn the_gg_form_answers_the_start_port_only() {
        let mut gg = device("gg");
        assert_eq!(gg.peek(Address(0x0000)), 0x80, "the START idle form");
        assert!(gg.inject("press start"));
        assert_eq!(gg.peek(Address(0x0000)), 0x00, "bit 7 low when pressed");
        assert!(gg.inject("release start"));
        assert_eq!(gg.peek(Address(0x0000)), 0x80);
        // The key port decodes only on the game gear form.
        assert!(gg.bus().expect("bus").decodes(Address(0x0000)));
        let sms = device("sms");
        assert!(!sms.bus().expect("bus").decodes(Address(0x0000)));
        assert_eq!(sms.peek(Address(0x0000)), 0xFF);
    }

    #[test]
    fn a_pause_press_latches_the_nmi_pulse_on_the_sms_form() {
        let mut m = machine("sms", false);
        m.boot();
        assert!(
            m.device_by_name_mut("seg")
                .expect("device")
                .input_mut()
                .expect("sink")
                .inject("press pause")
        );
        m.run_until(Tick(2));
        assert_eq!(
            recorder(&mut m).lines,
            vec![
                ("nmi".to_string(), Level::High, 0),
                ("nmi".to_string(), Level::Low, 1)
            ]
        );
        // The release does nothing and the line stays low.
        assert!(
            m.device_by_name_mut("seg")
                .expect("device")
                .input_mut()
                .expect("sink")
                .inject("release pause")
        );
        m.run_until(Tick(10));
        assert_eq!(
            recorder(&mut m).lines.len(),
            2,
            "the release raised no edge"
        );
    }

    #[test]
    fn a_press_during_the_pulse_restarts_it() {
        let mut m = machine("sms", false);
        m.boot();
        let inject = |m: &mut Machine| {
            m.device_by_name_mut("seg")
                .expect("device")
                .input_mut()
                .expect("sink")
                .inject("press pause")
        };
        assert!(inject(&mut m));
        m.run_until(Tick(1));
        assert!(inject(&mut m), "the restart press");
        m.run_until(Tick(3));
        let lines = &recorder(&mut m).lines;
        assert_eq!(lines.len(), 4, "the restart ran a fresh pulse");
        assert_eq!(lines[2], ("nmi".to_string(), Level::High, 0));
        assert_eq!(lines[3], ("nmi".to_string(), Level::Low, 1));
    }

    #[test]
    fn the_pause_press_stays_outside_the_sms_form() {
        let mut seg = device("gg");
        assert!(!seg.inject("press pause"), "the key port form has no pause");
        let mut seg = device("sg1000");
        assert!(!seg.inject("press pause"));
        // The TH events stay in the grammar on every form.
        assert!(seg.inject("press p1-th-1"));
        assert_eq!(seg.peek(Address(0xDC)) & 0x40, 0x40);
    }

    #[test]
    fn the_gg_start_quirk_latches_the_pulse() {
        let mut m = machine("gg", true);
        m.boot();
        m.device_by_name_mut("seg")
            .expect("device")
            .input_mut()
            .expect("sink")
            .inject("press start");
        m.run_until(Tick(2));
        assert_eq!(
            recorder(&mut m).lines,
            vec![
                ("nmi".to_string(), Level::High, 0),
                ("nmi".to_string(), Level::Low, 1)
            ]
        );
    }

    #[test]
    fn the_gg_start_without_the_quirk_drives_no_nmi() {
        let mut m = machine("gg", false);
        m.boot();
        m.device_by_name_mut("seg")
            .expect("device")
            .input_mut()
            .expect("sink")
            .inject("press start");
        m.run_until(Tick(10));
        assert!(recorder(&mut m).lines.is_empty(), "no NMI pulse");
    }

    #[test]
    fn the_event_grammar_accepts_the_aliases() {
        let mut seg = device("sms");
        assert!(seg.inject("up"), "a bare event presses");
        assert_eq!(seg.p1, MASK_UP);
        assert!(seg.inject("p2-down"));
        assert_eq!(seg.p2, MASK_DOWN);
        assert!(seg.inject("up-release"), "the suffix releases");
        assert_eq!(seg.p1, 0);
        assert!(!seg.inject("toggle up"));
        assert!(!seg.inject("press zzz"));
        assert!(!seg.inject("press p3-up"));
        // The bare TH names and the TH verbs both set the level.
        assert!(seg.inject("p1-th-1"));
        assert!(seg.p1_th);
        assert!(seg.inject("press p2-th-0"));
        assert!(!seg.p2_th);
    }

    #[test]
    fn decodes_gates_the_published_ports() {
        let seg = device("sg1000");
        let bus = seg.bus().expect("bus");
        assert!(bus.decodes(Address(0x00DC)));
        assert!(bus.decodes(Address(0x00DD)));
        // The published low-byte decode: the port high byte rides in
        // from the full Z80 port value, so the A8-set port answers
        // through the mask like the real chips do.
        assert!(
            bus.decodes(Address(0x01DC)),
            "the low byte decodes through the full port value"
        );
        assert!(!bus.decodes(Address(0x00DE)), "the next port stays open");
        assert_eq!(seg.peek(Address(0x00DE)), 0xFF);
    }

    #[test]
    fn snapshot_round_trips_and_rejects_the_wrong_length() {
        let mut seg = device("gg");
        assert!(seg.inject("press up"));
        assert!(seg.inject("press p2-start"));
        assert!(seg.inject("p1-th-1"));
        let snap = seg.snapshot();
        assert_eq!(snap.len(), SNAPSHOT_LEN);
        let mut twin = device("gg");
        twin.restore(&snap).expect("restore");
        assert_eq!(twin.snapshot(), snap);
        assert!(twin.p1_th);
        assert!(twin.p2_start);
        assert_eq!(
            twin.peek(Address(0xDC)) & 0x40,
            0x40,
            "player 1 TH rides bit 6"
        );
        assert!(twin.restore(&snap[..snap.len() - 1]).is_err());
        assert!(twin.restore(&[0u8; SNAPSHOT_LEN]).is_err());
    }
}
