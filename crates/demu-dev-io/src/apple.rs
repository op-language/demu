//! The Apple II console block: the keyboard, the soft switches, the
//! game-port inputs, and the language-card bank control.
//!
//! One device answers the `$C0xx` console page except the video form
//! switches at `$C050-$C057`, which belong to the video device of the
//! `a2.video` kind. The split keeps the two devices free of shared
//! register state: this device never reads the display form and the
//! video device never reads the keyboard.
//!
//! ## The register map
//!
//! `$C000` carries the keyboard: bit 7 is the key strobe and bits 6-0
//! the last key pressed. `$C010` clears the strobe on a read or a
//! write. The language-card status reads sit at `$C011` (bit 7 = bank
//! 2 selected), `$C012` (bit 7 = card RAM visible), and `$C014` (bit 7
//! = card RAM writable). `$C020` toggles the cassette output on each
//! access, `$C030` toggles the speaker, and `$C060` reads the
//! cassette input bit at bit 7. `$C058-$C05F` set the annunciator
//! outputs AN0-3 off and on, `$C061-$C063` read the game-port buttons,
//! `$C064-$C067` read the analog inputs, and any access to `$C070`
//! re-triggers all four analog one-shots.
//!
//! `$C080-$C08F` is the language-card bank select. The bank follows
//! bit 3 (0 = bank 2, 1 = bank 1); a low nibble of 1 or 3 selects the
//! card RAM read form, and a low nibble of 3 also arms the write
//! enable per the pairs `$C083`/`$C08B`. The device owns the card RAM:
//! 4 KB under `$D000-$DFFF` as bank 1 with its other 4 KB behind
//! `$E000-$EFFF`, and a second 4 KB bank under `$D000-$DFFF` as bank
//! 2. The internal banks answer only while the read form selects the
//! card. In this build the config maps exactly `$D000-$EFFF` here and
//! gives `$F000-$FFFF` to the monitor ROM, so the card's upper half
//! answers at the device level and stays behind the ROM window at the
//! machine level.
//!
//! ## Simplifications
//!
//! The cassette and speaker have no signal engine: each access flips a
//! stored toggle counter, so the software sees a stable side effect
//! with no sound. The analog timers count down once per CPU-cycle
//! visit after the `$C070` trigger and read the active bit at bit 7;
//! the exact resistance-to-duration ramp is a fixed scale of eleven
//! cycles per unit. The unmodeled console and status reads, such as
//! `$C013` and the peripheral-slot pages of `$C090-$C0FF`, return the
//! open-bus value `$FF`. The double-high-resolution forms and the IIe
//! auxiliary-memory banking are not modeled; the machines of this
//! family boot from the main RAM and ROM only. The write-enable form
//! of `$C081` that real hardware arms with a second read is folded
//! into the plain write-enable bit.
//!
//! ## Input grammar
//!
//! `key <byte>` stores one keyboard byte, for example `key 65` or
//! `key A`; a bare ASCII character stores its code. `paddle<n>
//! <0-255>` sets one analog input. `button<n>` presses game-port
//! button n and `button<n>-release` lifts it.

use demu_config::model::BootState;
use demu_core::device::{AccessWidth, BusAccess, InputSink, MachineCtx};
use demu_core::frequency::{Address, Tick};
use demu_core::signal::Level;
use demu_core::{Device, Snapshot, SnapshotError};

/// Language-card card RAM: the 8 KB of `$E000-$FFFF` plus the 4 KB
/// `$D000-$DFFF` half of bank 1.
const LC_BANK1: usize = 12 * 1024;
/// The second 4 KB bank under `$D000-$DFFF`.
const LC_BANK2: usize = 4 * 1024;

/// The cycles one analog timer counts per paddle unit.
const PADDLE_TICKS: u64 = 11;

/// The keyboard, soft-switch, game-port, and language-card block.
#[derive(Debug)]
pub struct AppleIo {
    name: String,
    /// Domain ticks per CPU cycle, from the `cpu` divider entry.
    cpu_div: u64,
    /// The last keyboard byte.
    last_key: u8,
    /// The key strobe: bit 7 of `$C000`.
    strobe: bool,
    /// The speaker toggle counter.
    speaker: u8,
    /// The cassette output toggle counter.
    cass_out: u8,
    /// The cassette input bit.
    tape_in: bool,
    /// The annunciator outputs AN0-3.
    ann: u8,
    /// The game-port buttons, one set bit means pressed.
    buttons: u8,
    /// The four analog input values, 0-255.
    paddles: [u8; 4],
    /// The countdown of each analog timer.
    timers: [u32; 4],
    /// Language-card read form: true while the card RAM answers.
    lc_read: bool,
    /// Language-card write form.
    lc_write: bool,
    /// Language-card bank select under `$D000-$DFFF`.
    lc_bank2: bool,
    /// The 12 KB bank-1 card RAM.
    lc1: Box<[u8]>,
    /// The 4 KB bank-2 card RAM.
    lc2: Box<[u8]>,
    next: Option<Tick>,
    last_tick: Option<Tick>,
}

impl AppleIo {
    /// A device from its configuration entry.
    pub fn new(device: &demu_config::model::DeviceInstance) -> Self {
        Self {
            name: device.name.to_string(),
            cpu_div: u64::from(device.dividers.get("cpu").copied().unwrap_or(1)).max(1),
            last_key: 0,
            strobe: false,
            speaker: 0,
            cass_out: 0,
            tape_in: false,
            ann: 0,
            buttons: 0,
            paddles: [0; 4],
            timers: [0; 4],
            lc_read: false,
            lc_write: false,
            lc_bank2: false,
            lc1: vec![0; LC_BANK1].into_boxed_slice(),
            lc2: vec![0; LC_BANK2].into_boxed_slice(),
            next: None,
            last_tick: None,
        }
    }

    /// The byte of the keyboard register `$C000`.
    fn key_byte(&self) -> u8 {
        self.last_key | if self.strobe { 0x80 } else { 0 }
    }

    /// The byte of one analog timer read.
    fn timer_byte(&self, n: usize) -> u8 {
        if self.timers[n] > 0 { 0x80 } else { 0 }
    }

    /// Re-trigger all four analog timers from the paddle values and
    /// arm the next visit at once; the paddle strobe runs inside a
    /// bus access, so the device must wake the scheduler itself.
    fn paddle_trigger(&mut self, tick: Tick) {
        for n in 0..4 {
            self.timers[n] = u32::from(self.paddles[n]) * PADDLE_TICKS as u32;
        }
        self.next = if self.timers_active() {
            Some(Tick(tick.0 + self.cpu_div))
        } else {
            None
        };
    }

    /// The address selects the language-card RAM windows and the card
    /// read form answers.
    fn lc_visible(&self, address: u32) -> bool {
        self.lc_read && matches!(address, 0xD000..=0xFFFF)
    }

    /// The byte behind one card address in the read form: `$D000-$D7FF`
    /// carries bank 2 or bank 1's lower half per the bank select;
    /// `$D800-$DFFF` and `$E000-$EFFF` always carry bank 1, whose 12
    /// KiB sit in that order.
    fn lc_byte_at(&self, a: u32) -> u8 {
        let idx = (a - 0xD000).try_into().unwrap_or(0);
        if a >= 0xE000 {
            return self.lc1[0x1000 + usize::try_from(a - 0xE000).unwrap_or(0)];
        }
        if a >= 0xD800 {
            return self.lc1[idx];
        }
        if self.lc_bank2 {
            self.lc2[idx]
        } else {
            self.lc1[idx]
        }
    }

    /// Store one byte through the write form at the same place the
    /// read form answers.
    fn lc_put_byte(&mut self, a: u32, value: u8) {
        if a >= 0xE000 {
            let idx = usize::try_from(a - 0xE000).unwrap_or(0);
            self.lc1[0x1000 + idx] = value;
            return;
        }
        let idx = usize::try_from(a - 0xD000).unwrap_or(0);
        if a >= 0xD800 {
            self.lc1[idx] = value;
        } else if self.lc_bank2 {
            self.lc2[idx] = value;
        } else {
            self.lc1[idx] = value;
        }
    }

    /// One CPU-cycle visit: the analog timers count down.
    fn timer_visit(&mut self) {
        for t in &mut self.timers {
            *t = t.saturating_sub(1);
        }
    }

    /// True while any analog timer still counts.
    fn timers_active(&self) -> bool {
        self.timers.iter().any(|t| *t > 0)
    }
}

impl BusAccess for AppleIo {
    fn read(&mut self, _width: AccessWidth, address: Address, tick: Tick) -> u32 {
        let a = address.0;
        if self.lc_visible(a) {
            return u32::from(self.lc_byte_at(a));
        }
        match a {
            0xC000 => u32::from(self.key_byte()),
            0xC010 => {
                self.strobe = false;
                u32::from(self.last_key & 0x7F)
            }
            0xC020 => {
                self.cass_out = self.cass_out.wrapping_add(1);
                u32::from(self.cass_out & 1)
            }
            0xC030 => {
                self.speaker = self.speaker.wrapping_add(1);
                u32::from(self.speaker & 1)
            }
            0xC060 => u32::from(if self.tape_in { 0x80u8 } else { 0u8 }),
            0xC061..=0xC063 => {
                let n = (a - 0xC061) as usize;
                u32::from(if self.buttons & (1 << n.min(7)) != 0 {
                    0x80u8
                } else {
                    0u8
                })
            }
            0xC064..=0xC067 => u32::from(self.timer_byte((a - 0xC064) as usize)),
            // The paddle strobe fires on a read or a write; the
            // hardware uses it to seed the four analog timers.
            0xC070 => {
                self.paddle_trigger(tick);
                0
            }
            _ => 0xFF,
        }
    }

    fn write(&mut self, _width: AccessWidth, address: Address, value: u32, tick: Tick) {
        let a = address.0;
        let v = value as u8;
        if self.lc_visible(a) && self.lc_write {
            self.lc_put_byte(a, v);
            return;
        }
        match a {
            0xC010 => self.strobe = false,
            0xC020 => self.cass_out = self.cass_out.wrapping_add(1),
            0xC030 => self.speaker = self.speaker.wrapping_add(1),
            0xC058..=0xC05F => {
                let n = ((a - 0xC058) / 2) as u8;
                let mask = 1u8 << n;
                if (a - 0xC058) & 1 == 1 {
                    self.ann |= mask;
                } else {
                    self.ann &= !mask;
                }
            }
            0xC070 => self.paddle_trigger(tick),
            0xC080..=0xC08F => {
                let n = (a & 0x0F) as u8;
                self.lc_bank2 = n & 0x08 == 0;
                self.lc_read = n & 0x03 == 1 || n & 0x03 == 3;
                self.lc_write = n & 0x03 == 3;
            }
            _ => {}
        }
    }

    fn peek(&self, address: Address) -> u8 {
        let a = address.0;
        if self.lc_visible(a) {
            return self.lc_byte_at(a);
        }
        match a {
            0xC000 => self.key_byte(),
            0xC011 => {
                if self.lc_bank2 {
                    0x80
                } else {
                    0
                }
            }
            0xC012 => {
                if self.lc_read {
                    0x80
                } else {
                    0
                }
            }
            0xC014 => {
                if self.lc_write {
                    0x80
                } else {
                    0
                }
            }
            0xC060 => {
                if self.tape_in {
                    0x80
                } else {
                    0
                }
            }
            0xC061..=0xC063 => {
                let n = (a - 0xC061) as usize;
                if self.buttons & (1 << n.min(7)) != 0 {
                    0x80
                } else {
                    0
                }
            }
            0xC064..=0xC067 => self.timer_byte((a - 0xC064) as usize),
            0xC010 => self.last_key & 0x7F,
            // The toggle switches read their toggled bit on probe
            // without flipping them again.
            0xC020 => self.cass_out & 1,
            0xC030 => self.speaker & 1,
            _ => 0xFF,
        }
    }

    /// The language-card RAM window answers only while the card read
    /// form is on; the console switches always answer.
    fn decodes(&self, address: Address) -> bool {
        if self.lc_visible(address.0) {
            return true;
        }
        !(0xD000..=0xFFFF).contains(&address.0)
    }
}

impl Device for AppleIo {
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
        self.last_key = 0;
        self.strobe = false;
        self.speaker = 0;
        self.cass_out = 0;
        self.tape_in = false;
        self.ann = 0;
        self.buttons = 0;
        self.paddles = [0; 4];
        self.timers = [0; 4];
        self.lc_read = false;
        self.lc_write = false;
        self.lc_bank2 = false;
        self.lc1.iter_mut().for_each(|b| *b = 0);
        self.lc2.iter_mut().for_each(|b| *b = 0);
        self.next = None;
        self.last_tick = None;
    }

    fn boot(&mut self, state: Option<&BootState>) {
        self.reset();
        if let Some(BootState::Ram { pattern }) = state {
            demu_core::boot::fill_ram(self.lc1.as_mut(), *pattern);
            demu_core::boot::fill_ram(self.lc2.as_mut(), *pattern);
        }
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(5 + 11 + 16 + LC_BANK1 + LC_BANK2);
        out.extend_from_slice(b"A2IO");
        out.push(1);
        out.push(self.last_key);
        out.push(u8::from(self.strobe));
        out.push(self.speaker);
        out.push(self.cass_out);
        out.push(u8::from(self.tape_in));
        out.push(self.ann);
        out.push(self.buttons);
        out.extend_from_slice(&self.paddles);
        for t in &self.timers {
            out.extend_from_slice(&t.to_le_bytes());
        }
        out.push(
            u8::from(self.lc_read)
                | (u8::from(self.lc_write) << 1)
                | (u8::from(self.lc_bank2) << 2),
        );
        out.extend_from_slice(&self.lc1);
        out.extend_from_slice(&self.lc2);
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        let expected = 4
            + 1
            + 7 // key, strobe, speaker, cassette out, tape in, ann, buttons
            + 4 // the paddle values
            + 16 // the timers
            + 1
            + LC_BANK1
            + LC_BANK2;
        if data.len() != expected || &data[..4] != b"A2IO" || data[4] != 1 {
            return Err(SnapshotError::Mismatch(format!(
                "apple io chunk must be {expected} bytes"
            )));
        }
        let mut pos = 5usize;
        self.last_key = data[pos];
        pos += 1;
        self.strobe = data[pos] != 0;
        pos += 1;
        self.speaker = data[pos];
        pos += 1;
        self.cass_out = data[pos];
        pos += 1;
        self.tape_in = data[pos] != 0;
        pos += 1;
        self.ann = data[pos];
        pos += 1;
        self.buttons = data[pos];
        pos += 1;
        self.paddles.copy_from_slice(&data[pos..pos + 4]);
        pos += 4;
        for t in self.timers.iter_mut() {
            *t = u32::from_le_bytes(data[pos..pos + 4].try_into().expect("4 bytes"));
            pos += 4;
        }
        let lc = data[pos];
        pos += 1;
        self.lc_read = lc & 0x01 != 0;
        self.lc_write = lc & 0x02 != 0;
        self.lc_bank2 = lc & 0x04 != 0;
        self.lc1.copy_from_slice(&data[pos..pos + LC_BANK1]);
        pos += LC_BANK1;
        self.lc2.copy_from_slice(&data[pos..pos + LC_BANK2]);
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
        self.timer_visit();
        self.next = if self.timers_active() {
            Some(Tick(tick.0 + self.cpu_div))
        } else {
            None
        };
    }
}

impl InputSink for AppleIo {
    fn inject(&mut self, event: &str) -> bool {
        // The keyboard: `key <byte>` stores the byte, a single ASCII
        // character its code with its original case. The analog
        // values: `paddle<n> <0-255>`. The game buttons: `button<n>`
        // presses, `button<n>-release` lifts.
        let trimmed = event.trim();
        if let Some(rest) = trimmed.strip_prefix("key ") {
            return self.inject_key(rest.trim());
        }
        let form = trimmed.to_ascii_lowercase();
        let (name, release) = match form.strip_suffix("-release") {
            Some(n) => (n, true),
            None => (form.as_str(), false),
        };
        if let Some(rest) = name.strip_prefix("paddle") {
            let Some((n, v)) = rest.split_once(' ') else {
                return false;
            };
            if let (Ok(idx), Ok(val)) = (n.parse::<usize>(), v.trim().parse::<u8>())
                && idx < 4
            {
                self.paddles[idx] = val;
                return true;
            }
            return false;
        }
        if let Some(n) = name.strip_prefix("button")
            && let Ok(idx) = n.parse::<usize>()
            && idx < 3
        {
            let bit = 1u8 << idx;
            if release {
                self.buttons &= !bit;
            } else {
                self.buttons |= bit;
            }
            return true;
        }
        false
    }
}

impl AppleIo {
    /// Store one keyboard event and raise the strobe.
    fn inject_key(&mut self, payload: &str) -> bool {
        if let Ok(b) = payload.parse::<u8>() {
            self.last_key = b;
            self.strobe = true;
            return true;
        }
        if payload.chars().count() == 1 {
            let code = u32::from(payload.chars().next().unwrap_or('\0'));
            return match u8::try_from(code) {
                Ok(b) => {
                    self.last_key = b & 0x7F;
                    self.strobe = true;
                    true
                }
                Err(_) => false,
            };
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instance() -> demu_config::model::DeviceInstance {
        demu_config::model::DeviceInstance {
            name: demu_config::newtypes::DeviceName::new("a2io").expect("name"),
            kind: demu_config::newtypes::KindName::new("a2.io").expect("kind"),
            domain: "main".to_string(),
            params: std::collections::BTreeMap::new(),
            dividers: [("cpu".to_string(), 1)].into_iter().collect(),
        }
    }

    #[test]
    fn keyboard_strobe_clears_on_c010() {
        let mut io = AppleIo::new(&instance());
        assert!(io.inject("key 65"));
        assert_eq!(io.read(AccessWidth::Byte, Address(0xC000), Tick(0)), 0xC1);
        // The C010 read returns the key and clears the strobe.
        assert_eq!(io.read(AccessWidth::Byte, Address(0xC010), Tick(0)), 0x41);
        assert_eq!(io.read(AccessWidth::Byte, Address(0xC000), Tick(0)), 0x41);
        assert!(io.inject("key A"));
        assert_eq!(io.last_key, 0x41);
    }

    #[test]
    fn annunciators_decode_off_on_pairs() {
        let mut io = AppleIo::new(&instance());
        // $C058-$C05F carry AN0-AN3: the even address turns the output
        // off and the odd address turns it on.
        io.write(AccessWidth::Byte, Address(0xC059), 0, Tick(0));
        assert_eq!(io.ann & 0x01, 0x01, "C059 turns AN0 on");
        io.write(AccessWidth::Byte, Address(0xC058), 0, Tick(0));
        assert_eq!(io.ann & 0x01, 0, "C058 turns AN0 off");
        io.write(AccessWidth::Byte, Address(0xC05A), 0, Tick(0));
        assert_eq!(io.ann & 0x02, 0, "C05A turns AN1 off");
        io.write(AccessWidth::Byte, Address(0xC05B), 0, Tick(0));
        assert_eq!(io.ann & 0x02, 0x02, "C05B turns AN1 on");
        io.write(AccessWidth::Byte, Address(0xC05F), 0, Tick(0));
        assert_eq!(io.ann & 0x08, 0x08, "C05F turns AN3 on");
    }

    #[test]
    fn language_card_selects_banks_and_forms() {
        let mut io = AppleIo::new(&instance());
        // Bank 2, RAM read, write enabled.
        io.write(AccessWidth::Byte, Address(0xC083), 0, Tick(0));
        assert!(io.lc_bank2);
        assert!(io.lc_read);
        assert!(io.lc_write);
        assert_eq!(io.peek(Address(0xC011)), 0x80);
        assert_eq!(io.peek(Address(0xC012)), 0x80);
        assert_eq!(io.peek(Address(0xC014)), 0x80);
        // The write lands in bank 2.
        io.write(AccessWidth::Byte, Address(0xD000), 0x42, Tick(0));
        assert_eq!(io.lc2[0], 0x42);
        // Bank 1 answers the same window after the select.
        io.write(AccessWidth::Byte, Address(0xC08B), 0, Tick(0));
        assert!(!io.lc_bank2);
        assert_eq!(io.peek(Address(0xD000)), 0);
        io.write(AccessWidth::Byte, Address(0xD000), 0x7E, Tick(0));
        assert_eq!(io.lc1[0], 0x7E);
        // The upper halves always map to bank 1.
        io.write(AccessWidth::Byte, Address(0xE000), 0x11, Tick(0));
        assert_eq!(io.lc1[0x1000], 0x11);
        io.write(AccessWidth::Byte, Address(0xD900), 0x22, Tick(0));
        assert_eq!(io.lc1[0x900], 0x22, "the mid half stays bank 1");
    }

    #[test]
    fn language_card_window_falls_through_in_rom_form() {
        let mut io = AppleIo::new(&instance());
        assert!(!io.decodes(Address(0xD123)), "reset leaves the card hidden");
        io.write(AccessWidth::Byte, Address(0xC083), 0, Tick(0));
        assert!(io.decodes(Address(0xD123)));
        // The console page always answers.
        assert!(io.decodes(Address(0xC000)));
    }

    #[test]
    fn paddle_trigger_counts_down_once_per_visit() {
        let mut io = AppleIo::new(&instance());
        assert!(io.inject("paddle0 2"));
        io.paddle_trigger(Tick(0));
        assert!(io.timers_active());
        assert_eq!(io.peek(Address(0xC064)), 0x80);
        for _ in 0..22 {
            io.timer_visit();
        }
        assert_eq!(io.timers[0], 0);
        assert_eq!(io.peek(Address(0xC064)), 0);
        assert!(!io.timers_active());
    }

    #[test]
    fn game_buttons_read_bit_seven() {
        let mut io = AppleIo::new(&instance());
        assert!(io.inject("button0"));
        assert_eq!(io.peek(Address(0xC061)), 0x80);
        io.inject("button1");
        assert_eq!(io.peek(Address(0xC062)), 0x80);
        // Both pressed together still read one bit each.
        assert_eq!(io.peek(Address(0xC061)), 0x80);
        io.inject("button0-release");
        assert_eq!(io.peek(Address(0xC061)), 0);
    }

    #[test]
    fn cassette_and_speaker_toggle_per_access() {
        let mut io = AppleIo::new(&instance());
        assert_eq!(io.read(AccessWidth::Byte, Address(0xC020), Tick(0)), 1);
        assert_eq!(io.read(AccessWidth::Byte, Address(0xC020), Tick(0)), 0);
        assert_eq!(io.read(AccessWidth::Byte, Address(0xC030), Tick(0)), 1);
        io.write(AccessWidth::Byte, Address(0xC030), 0, Tick(0));
        assert_eq!(io.speaker, 2);
    }

    #[test]
    fn unknown_console_reads_return_open_bus() {
        let mut io = AppleIo::new(&instance());
        assert_eq!(io.read(AccessWidth::Byte, Address(0xC0B3), Tick(0)), 0xFF);
        assert_eq!(io.read(AccessWidth::Byte, Address(0xC045), Tick(0)), 0xFF);
    }

    #[test]
    fn snapshot_round_trip() {
        let mut io = AppleIo::new(&instance());
        io.boot(None);
        io.inject("key 65");
        io.inject("button2");
        io.inject("paddle3 200");
        io.write(AccessWidth::Byte, Address(0xC083), 0, Tick(0));
        io.write(AccessWidth::Byte, Address(0xD010), 0x5A, Tick(0));
        io.ann = 0x0F;
        let snapshot = io.snapshot();
        let mut twin = AppleIo::new(&instance());
        twin.restore(&snapshot).expect("restore");
        assert_eq!(twin.snapshot(), snapshot);
        assert_eq!(twin.peek(Address(0xD010)), 0x5A);
        assert_eq!(twin.ann, 0x0F);
    }

    #[test]
    fn restore_rejects_a_bad_chunk() {
        let mut io = AppleIo::new(&instance());
        io.lc_read = true;
        let snapshot = io.snapshot();
        let mut broken = snapshot.clone();
        broken.push(0);
        assert!(io.restore(&broken).is_err());
        assert!(io.restore(&[0; 8]).is_err());
    }
}
