//! The Mega II glue of the Apple IIgs: the console soft switches,
//! the keyboard interface, the status readbacks, and the IIgs memory
//! and control registers.
//!
//! One device answers the `$C000-$C0FF` console window (the sound
//! GLU slice at `$C03C-$C03F` and the disk port at `$C0E0-$C0EF`
//! overlay it), plus the rest of the IIgs glue. The register set
//! follows the published IIgs I/O page:
//!
//! - `$C000` = the keyboard data with the strobe at bit 7, and the
//!   `$C010` access clears the strobe.
//! - `$C001`-$C00F` = the IIe soft-switch writes: the 80-store form,
//!   the main/aux read and write forms, the slot and internal CX ROM
//!   forms, the zero-page and stack form, the 80-column form, and
//!   the alternate character set. The stored flags answer the status
//!   readbacks; the display side of the forms belongs to the video
//!   device.
//! - `$C011`-$C01F` = the readbacks of those flags at bit 7, with
//!   `$C019` reading the vertical-blank state in the IIgs form (0 =
//!   the beam draws, 1 = the blank period).
//! - `$C021` = the mono/color switch, `$C022` = the text and border
//!   color byte, `$C029` = the new-video register (bit 7 = the Super
//!   Hi-Res select, bit 6 = the linearize bit, bit 5 = the
//!   black-and-white bit), `$C034` = the clock control byte, `$C035`
//!   = the shadow inhibit byte, `$C036` = the CYA register (bit 7 =
//!   the fast-speed select, bit 4 = the shadow-all-banks bit, bits
//!   0-3 = the disk detect bits).
//! - `$C041` = the Mega II interrupt-enable byte (bit 3 = the
//!   vertical-blank enable), `$C046` = the interrupt flag readback
//!   (bit 0 = the armed IRQ), `$C047` = the vertical-blank interrupt
//!   clear, `$C058`-$C05F` = the annunciators AN0-3, `$C060` = game
//!   button three, `$C061`-$C063` = the game-port buttons, `$C064`-
//!   `$C067` = the analog paddles, `$C070` = the paddle trigger, and
//!   `$C068` = the state register that composes the bank and form
//!   flags.
//!
//! ## Simplifications
//!
//! The language-card bank windows, the auxiliary memory forms, and
//! the shadow copying are not modeled in this build: the config maps
//! the RAM and ROM windows statically and the boot walk touches the
//! switch and register set only. The vertical-blank state stands
//! still until a config connects a source line to the `vbl` line;
//! the one-second and quarter-second interrupt timers and the ADB
//! registers (`$C024`-$C027`) are not modeled and read the open-bus
//! form. The video-form readbacks (`$C01A`-$C01D`) sit with the
//! video device and read the open-bus form here. The write-armed
//! interrupt sources reduce to the interrupt-enable byte and the
//! vertical-blank flag.

use demu_config::model::BootState;
use demu_core::device::{AccessWidth, BusAccess, InputSink, MachineCtx};
use demu_core::frequency::{Address, Tick};
use demu_core::signal::Level;
use demu_core::{Device, Snapshot, SnapshotError};

/// The cycles one analog timer counts per paddle unit, the same
/// scale the `a2.io` device uses.
const PADDLE_TICKS: u64 = 11;

/// The stored IIe soft-switch flags: bit 7 = ALTZP, bit 6 = 80STORE,
/// bit 5 = the aux read form, bit 4 = the aux write form, bit 3 =
/// the internal CX ROM form, bit 2 = the internal slot 3 ROM form,
/// bit 1 = 80VID, and bit 0 = the alternate character set.
const SW_ALTZP: u8 = 0x80;
const SW_80STORE: u8 = 0x40;
const SW_RAMRD: u8 = 0x20;
const SW_RAMWRT: u8 = 0x10;
const SW_INTCXROM: u8 = 0x08;
const SW_C3ROM: u8 = 0x04;
const SW_80VID: u8 = 0x02;
const SW_ALTCHAR: u8 = 0x01;

/// The Mega II glue: the keyboard, the switches, and the registers.
#[derive(Debug)]
pub struct Mega2 {
    name: String,
    cpu_div: u64,
    /// The last keyboard byte.
    last_key: u8,
    /// The key strobe: bit 7 of `$C000`.
    strobe: bool,
    /// The stored soft-switch flag set.
    switches: u8,
    /// `$C021` the mono/color form: bit 7 = mono.
    mono: bool,
    /// `$C022` the text and border color byte.
    tb_color: u8,
    /// `$C029` the new-video register byte.
    newvideo: u8,
    /// `$C034` the clock control byte.
    clock_ctl: u8,
    /// `$C035` the shadow inhibit byte.
    shadow: u8,
    /// `$C036` the CYA register byte.
    cya: u8,
    /// `$C041` the Mega II interrupt enable byte.
    inten: u8,
    /// The vertical-blank interrupt flag.
    vbl_irq: bool,
    /// The vertical-blank line state: high while the beam is blank.
    vbl_line: Option<Level>,
    /// The speaker toggle counter.
    speaker: u8,
    /// The annunciator outputs AN0-3.
    ann: u8,
    /// The game-port buttons, one set bit per pressed button.
    buttons: u8,
    /// The four analog input values, 0-255.
    paddles: [u8; 4],
    /// The countdown of each analog timer.
    timers: [u32; 4],
    next: Option<Tick>,
    last_tick: Option<Tick>,
}

impl Mega2 {
    /// A device from its configuration entry.
    pub fn new(device: &demu_config::model::DeviceInstance) -> Self {
        Self {
            name: device.name.to_string(),
            cpu_div: u64::from(device.dividers.get("cpu").copied().unwrap_or(1)).max(1),
            last_key: 0,
            strobe: false,
            switches: 0,
            mono: false,
            tb_color: 0,
            newvideo: 0,
            clock_ctl: 0,
            shadow: 0,
            cya: 0,
            inten: 0,
            vbl_irq: false,
            vbl_line: None,
            speaker: 0,
            ann: 0,
            buttons: 0,
            paddles: [0; 4],
            timers: [0; 4],
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

    /// True while any analog timer still counts.
    fn timers_active(&self) -> bool {
        self.timers.iter().any(|t| *t > 0)
    }

    /// One CPU-cycle visit: the analog timers count down.
    fn timer_visit(&mut self) {
        for t in &mut self.timers {
            *t = t.saturating_sub(1);
        }
    }

    /// True while the beam draws: the stored line state or the reset
    /// form reads as drawing.
    fn beam_draws(&self) -> bool {
        self.vbl_line.is_none_or(|l| l == Level::Low)
    }

    /// The switch-flag readbacks at bit 7, in the `$C011`-$C01F`
    /// order. None leaves the address to the open-bus form.
    fn status_bit(&self, a: u32) -> Option<u8> {
        let set = match a {
            0xC011 => false, // the LC bank two state: unmodeled banks.
            0xC012 => false, // the LC RAM form: unmodeled banks.
            0xC013 => self.switches & SW_RAMRD != 0,
            0xC014 => self.switches & SW_RAMWRT != 0,
            0xC015 => self.switches & SW_INTCXROM != 0,
            0xC016 => self.switches & SW_ALTZP != 0,
            0xC017 => self.switches & SW_C3ROM != 0,
            0xC018 => self.switches & SW_80STORE != 0,
            // The IIgs form: 0 = drawing, 1 = the blank period.
            0xC019 => !self.beam_draws(),
            0xC01E => self.switches & SW_ALTCHAR != 0,
            0xC01F => self.switches & SW_80VID != 0,
            _ => return None,
        };
        Some(u8::from(set) << 7)
    }

    /// The composed `$C068` state register: bit 7 = ALTZP, bit 5 =
    /// the aux read form, bit 4 = the aux write form, bit 0 =
    /// INTCXROM. The bit placement is this model's own convention
    /// for the unbanked form; the unmodeled bank bits (ROMBANK,
    /// LCBNK2, RDROM, PAGE2) read zero.
    fn state_byte(&self) -> u8 {
        let mut v = 0u8;
        if self.switches & SW_ALTZP != 0 {
            v |= 0x80;
        }
        if self.switches & SW_RAMRD != 0 {
            v |= 0x20;
        }
        if self.switches & SW_RAMWRT != 0 {
            v |= 0x10;
        }
        if self.switches & SW_INTCXROM != 0 {
            v |= 0x01;
        }
        v
    }

    /// The composed interrupt flag byte of `$C046`: bit 0 = the IRQ
    /// state, armed by the interrupt-enable byte.
    fn intflag_byte(&self) -> u8 {
        if self.inten & 0x08 != 0 && self.vbl_irq {
            0x01
        } else {
            0
        }
    }
}

impl BusAccess for Mega2 {
    fn read(&mut self, _width: AccessWidth, address: Address, tick: Tick) -> u32 {
        let a = address.0 & 0xFFFF;
        match a {
            0xC000 => u32::from(self.key_byte()),
            0xC010 => {
                self.strobe = false;
                u32::from(self.last_key & 0x7F)
            }
            0xC022 => u32::from(self.tb_color),
            0xC029 => u32::from(self.newvideo),
            0xC034 => u32::from(self.clock_ctl),
            0xC035 => u32::from(self.shadow),
            0xC036 => u32::from(self.cya),
            0xC041 => u32::from(self.inten),
            0xC046 => u32::from(self.intflag_byte()),
            0xC047 => {
                self.vbl_irq = false;
                0
            }
            0xC030 => {
                self.speaker = self.speaker.wrapping_add(1);
                u32::from(self.speaker & 1)
            }
            0xC060 => u32::from(if self.buttons & 0x08 != 0 {
                0x80u8
            } else {
                0u8
            }),
            0xC061..=0xC063 => {
                let n = (a - 0xC061) as usize;
                u32::from(if self.buttons & (1 << n.min(7)) != 0 {
                    0x80u8
                } else {
                    0u8
                })
            }
            0xC064..=0xC067 => u32::from(self.timer_byte((a - 0xC064) as usize)),
            0xC068 => u32::from(self.state_byte()),
            // The paddle strobe fires on a read or a write; the
            // hardware uses it to seed the four analog timers.
            0xC070 => {
                self.paddle_trigger(tick);
                0
            }
            _ => u32::from(self.status_bit(a).unwrap_or(0xFF)),
        }
    }

    fn write(&mut self, _width: AccessWidth, address: Address, value: u32, tick: Tick) {
        let a = address.0 & 0xFFFF;
        match a {
            0xC000 => self.switches &= !SW_80STORE,
            0xC001 => self.switches |= SW_80STORE,
            0xC002 => self.switches &= !SW_RAMRD,
            0xC003 => self.switches |= SW_RAMRD,
            0xC004 => self.switches &= !SW_RAMWRT,
            0xC005 => self.switches |= SW_RAMWRT,
            // The slot CX ROM form = peripheral; the internal form
            // = the Mega II's own CX area.
            0xC006 => self.switches &= !SW_INTCXROM,
            0xC007 => self.switches |= SW_INTCXROM,
            0xC008 => self.switches &= !SW_ALTZP,
            0xC009 => self.switches |= SW_ALTZP,
            0xC00A => self.switches &= !SW_C3ROM,
            0xC00B => self.switches |= SW_C3ROM,
            0xC00C => self.switches &= !SW_80VID,
            0xC00D => self.switches |= SW_80VID,
            0xC00E => self.switches &= !SW_ALTCHAR,
            0xC00F => self.switches |= SW_ALTCHAR,
            0xC010 => self.strobe = false,
            0xC021 => self.mono = value & 0x80 != 0,
            0xC022 => self.tb_color = value as u8,
            0xC029 => self.newvideo = value as u8,
            0xC034 => self.clock_ctl = value as u8,
            0xC035 => self.shadow = value as u8,
            0xC036 => self.cya = value as u8,
            0xC041 => self.inten = value as u8,
            0xC047 => self.vbl_irq = false,
            0xC058..=0xC05F => {
                let n = ((a - 0xC058) / 2) as u8;
                let mask = 1u8 << n;
                if (a - 0xC058) & 1 == 1 {
                    self.ann |= mask;
                } else {
                    self.ann &= !mask;
                }
            }
            // The paddle strobe re-seeds the timers on any access
            // here.
            0xC070 => self.paddle_trigger(tick),
            _ => {}
        }
    }

    fn peek(&self, address: Address) -> u8 {
        let a = address.0 & 0xFFFF;
        match a {
            0xC000 => self.key_byte(),
            // The strobe-clear access shows the key without
            // clearing on probe.
            0xC010 => self.last_key & 0x7F,
            0xC022 => self.tb_color,
            0xC029 => self.newvideo,
            0xC034 => self.clock_ctl,
            0xC035 => self.shadow,
            0xC036 => self.cya,
            0xC041 => self.inten,
            0xC046 => self.intflag_byte(),
            0xC060 => u8::from(self.buttons & 0x08 != 0) << 7,
            0xC061..=0xC063 => {
                let n = (a - 0xC061) as usize;
                if self.buttons & (1 << n.min(7)) != 0 {
                    0x80
                } else {
                    0
                }
            }
            0xC064..=0xC067 => self.timer_byte((a - 0xC064) as usize),
            0xC068 => self.state_byte(),
            // The speaker toggle reads its toggled bit on probe
            // without flipping it again.
            0xC030 => self.speaker & 1,
            _ => self.status_bit(a).unwrap_or(0xFF),
        }
    }
}

impl InputSink for Mega2 {
    fn inject(&mut self, event: &str) -> bool {
        // `key <byte>` stores the byte, a single ASCII character its
        // code with its original case. `paddle<n> <0-255>` sets one
        // analog input. `button<n>` presses game-port button n and
        // `button<n>-release` lifts it.
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
            && idx < 4
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

impl Mega2 {
    /// Store one keyboard event and raise the strobe.
    fn inject_key(&mut self, payload: &str) -> bool {
        if let Ok(b) = payload.parse::<u8>() {
            self.last_key = b;
            self.strobe = true;
            return true;
        }
        if payload.chars().count() == 1 {
            let code = u32::from(payload.chars().next().unwrap_or('\0'));
            return u8::try_from(code)
                .map(|b| {
                    self.last_key = b & 0x7F;
                    self.strobe = true;
                    true
                })
                .unwrap_or(false);
        }
        false
    }
}

impl Device for Mega2 {
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
        self.switches = 0;
        self.mono = false;
        self.tb_color = 0;
        self.newvideo = 0;
        self.clock_ctl = 0;
        self.shadow = 0;
        self.cya = 0;
        self.inten = 0;
        self.vbl_irq = false;
        self.vbl_line = None;
        self.speaker = 0;
        self.ann = 0;
        self.buttons = 0;
        self.paddles = [0; 4];
        self.timers = [0; 4];
        self.next = None;
        self.last_tick = None;
    }

    fn boot(&mut self, _state: Option<&BootState>) {
        self.reset();
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(48);
        out.extend_from_slice(b"IGMS");
        out.push(1);
        out.push(self.last_key);
        out.push(u8::from(self.strobe));
        out.push(self.switches);
        out.push(u8::from(self.mono));
        out.push(self.tb_color);
        out.push(self.newvideo);
        out.push(self.clock_ctl);
        out.push(self.shadow);
        out.push(self.cya);
        out.push(self.inten);
        out.push(u8::from(self.vbl_irq));
        out.push(u8::from(self.vbl_line == Some(Level::High)));
        out.push(u8::from(self.vbl_line == Some(Level::Low)));
        out.push(self.speaker);
        out.push(self.ann);
        out.push(self.buttons);
        out.extend_from_slice(&self.paddles);
        for t in &self.timers {
            out.extend_from_slice(&t.to_le_bytes());
        }
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        let expected = 4 + 1 + 16 + 4 + 16;
        if data.len() != expected || &data[..4] != b"IGMS" || data[4] != 1 {
            return Err(SnapshotError::Mismatch(format!(
                "mega2 chunk must be {expected} bytes"
            )));
        }
        let mut pos = 5usize;
        self.last_key = data[pos];
        pos += 1;
        self.strobe = data[pos] != 0;
        pos += 1;
        self.switches = data[pos];
        pos += 1;
        self.mono = data[pos] != 0;
        pos += 1;
        self.tb_color = data[pos];
        pos += 1;
        self.newvideo = data[pos];
        pos += 1;
        self.clock_ctl = data[pos];
        pos += 1;
        self.shadow = data[pos];
        pos += 1;
        self.cya = data[pos];
        pos += 1;
        self.inten = data[pos];
        pos += 1;
        self.vbl_irq = data[pos] != 0;
        pos += 1;
        self.vbl_line = match (data[pos] != 0, data[pos + 1] != 0) {
            (true, _) => Some(Level::High),
            (false, true) => Some(Level::Low),
            (false, false) => None,
        };
        pos += 2;
        self.speaker = data[pos];
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
        self.next = None;
        self.last_tick = None;
        Ok(())
    }

    fn notify_line(&mut self, line: &str, level: Level, _tick: Tick) {
        if line == "vbl" {
            self.vbl_line = Some(level);
            if level == Level::High {
                self.vbl_irq = true;
            }
        }
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use demu_config::newtypes::{DeviceName, KindName};

    fn device() -> demu_config::model::DeviceInstance {
        demu_config::model::DeviceInstance {
            name: DeviceName::new("mega2").expect("name"),
            kind: KindName::new("iigs.mega2").expect("kind"),
            domain: "main".to_string(),
            params: std::collections::BTreeMap::new(),
            dividers: [("cpu".to_string(), 1)].into_iter().collect(),
        }
    }

    fn mega2() -> Mega2 {
        Mega2::new(&device())
    }

    #[test]
    fn keyboard_strobe_clears_on_c010() {
        let mut m = mega2();
        assert!(m.inject("key 65"));
        assert_eq!(m.read(AccessWidth::Byte, Address(0xC000), Tick(0)), 0xC1);
        // The C010 read returns the key and clears the strobe.
        assert_eq!(m.read(AccessWidth::Byte, Address(0xC010), Tick(0)), 0x41);
        assert_eq!(m.read(AccessWidth::Byte, Address(0xC000), Tick(0)), 0x41);
        assert!(m.inject("key A"));
        assert_eq!(m.last_key, 0x41);
    }

    #[test]
    fn the_switch_flags_read_back_per_group() {
        let mut m = mega2();
        // The aux read/write forms and the internal CX ROM form.
        m.write(AccessWidth::Byte, Address(0xC003), 0, Tick(0));
        m.write(AccessWidth::Byte, Address(0xC005), 0, Tick(0));
        m.write(AccessWidth::Byte, Address(0xC007), 0, Tick(0));
        assert_eq!(m.peek(Address(0xC013)), 0x80);
        assert_eq!(m.peek(Address(0xC014)), 0x80);
        assert_eq!(m.peek(Address(0xC015)), 0x80);
        assert_eq!(m.peek(Address(0xC012)), 0, "the LC banks read not-selected");
        // The composed state register carries the three set bits.
        let state = m.peek(Address(0xC068));
        assert_eq!(state & 0x30, 0x30, "the aux read and write forms");
        assert_eq!(state & 0x01, 0x01, "the internal CX ROM form");
        assert_eq!(state & 0x80, 0, "the main stack form");
        // The main-form writes clear the flags again.
        m.write(AccessWidth::Byte, Address(0xC002), 0, Tick(0));
        assert_eq!(m.peek(Address(0xC013)), 0);
    }

    #[test]
    fn the_iigs_registers_store_and_read_back() {
        let mut m = mega2();
        m.write(AccessWidth::Byte, Address(0xC029), 0x80, Tick(0));
        assert_eq!(m.read(AccessWidth::Byte, Address(0xC029), Tick(0)), 0x80);
        m.write(AccessWidth::Byte, Address(0xC035), 0x3F, Tick(0));
        assert_eq!(m.peek(Address(0xC035)), 0x3F);
        m.write(AccessWidth::Byte, Address(0xC036), 0x80, Tick(0));
        assert_eq!(m.peek(Address(0xC036)), 0x80);
        m.write(AccessWidth::Byte, Address(0xC022), 0x7C, Tick(0));
        assert_eq!(m.read(AccessWidth::Byte, Address(0xC022), Tick(0)), 0x7C);
        // The write-only mono switch stays an open-bus read.
        m.write(AccessWidth::Byte, Address(0xC021), 0x80, Tick(0));
        assert_eq!(m.read(AccessWidth::Byte, Address(0xC021), Tick(0)), 0xFF);
    }

    #[test]
    fn the_vbl_flag_arms_through_the_line_and_the_enable() {
        let mut m = mega2();
        // The interrupt flag reads zero while the enable is off.
        m.notify_line("vbl", Level::High, Tick(0));
        assert_eq!(m.peek(Address(0xC046)), 0);
        // The enable gates the flag.
        m.write(AccessWidth::Byte, Address(0xC041), 0x08, Tick(0));
        assert_eq!(m.peek(Address(0xC046)), 0x01);
        // The clear register drops it.
        m.read(AccessWidth::Byte, Address(0xC047), Tick(0));
        assert_eq!(m.peek(Address(0xC046)), 0);
        // The IIgs readback form: 0 = drawing, 1 = the blank period.
        m.notify_line("vbl", Level::Low, Tick(0));
        assert_eq!(m.peek(Address(0xC019)), 0);
        m.notify_line("vbl", Level::High, Tick(0));
        assert_eq!(m.peek(Address(0xC019)), 0x80);
    }

    #[test]
    fn game_buttons_cover_button_three() {
        let mut m = mega2();
        assert!(m.inject("button3"));
        assert_eq!(m.peek(Address(0xC060)), 0x80);
        assert!(m.inject("button0"));
        assert_eq!(m.peek(Address(0xC061)), 0x80);
        assert!(m.inject("button3-release"));
        assert_eq!(m.peek(Address(0xC060)), 0);
    }

    #[test]
    fn paddle_trigger_counts_down_once_per_visit() {
        let mut m = mega2();
        assert!(m.inject("paddle0 2"));
        m.paddle_trigger(Tick(0));
        assert!(m.timers_active());
        assert_eq!(m.peek(Address(0xC064)), 0x80);
        for _ in 0..22 {
            m.timer_visit();
        }
        assert_eq!(m.timers[0], 0);
        assert_eq!(m.peek(Address(0xC064)), 0);
    }

    #[test]
    fn unknown_console_access_reads_open_bus() {
        let mut m = mega2();
        assert_eq!(m.read(AccessWidth::Byte, Address(0xC044), Tick(0)), 0xFF);
        assert_eq!(m.read(AccessWidth::Byte, Address(0xC01B), Tick(0)), 0xFF);
        assert_eq!(m.read(AccessWidth::Byte, Address(0xC075), Tick(0)), 0xFF);
    }

    #[test]
    fn snapshot_round_trip() {
        let mut m = mega2();
        m.inject("key 65");
        m.inject("button2");
        m.inject("paddle3 200");
        m.write(AccessWidth::Byte, Address(0xC009), 0, Tick(0));
        m.write(AccessWidth::Byte, Address(0xC035), 0x2A, Tick(0));
        m.notify_line("vbl", Level::High, Tick(0));
        m.write(AccessWidth::Byte, Address(0xC041), 0x08, Tick(0));
        m.ann = 0x0F;
        let snapshot = m.snapshot();
        let mut twin = mega2();
        twin.restore(&snapshot).expect("restore");
        assert_eq!(twin.snapshot(), snapshot);
        assert_eq!(twin.peek(Address(0xC035)), 0x2A);
        assert_eq!(twin.peek(Address(0xC046)), 0x01);
        assert_eq!(twin.ann, 0x0F);
    }

    #[test]
    fn restore_rejects_a_bad_chunk() {
        let mut m = mega2();
        let snapshot = m.snapshot();
        let mut broken = snapshot.clone();
        broken.push(0);
        assert!(m.restore(&broken).is_err());
        assert!(m.restore(&[0; 8]).is_err());
    }
}
