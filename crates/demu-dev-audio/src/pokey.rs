//! The POKEY of the 5200.
//!
//! The device carries the four audio channels, the seventeen-bit and
//! nine-bit and five-bit and four-bit poly counters, the keyboard
//! code register, the serial-input stubs, and the interrupt system.
//! Channel dividers follow AUDCTL: the joined pairs form sixteen-bit
//! divisors, channels one and three can run at the high clock, and
//! the base clock is 64 kHz or 15 kHz exact rational divisions of the
//! machine clock.
//!
//! Register decode: the offset is the low four bits. The writes carry
//! the channel volume and control, AUDCTL, STIMER, SKRES, POTGO,
//! SEROUT, IRQEN, and SKCTL; the reads carry the pot inputs, ALLPOT,
//! KBCODE, RANDOM, SERIN, IRQST, and SKSTAT.
//!
//! Audio advances on machine-cycle ticks, so the output sample rate
//! equals the CPU cycle rate of the domain. Each channel stops its
//! square or noise waveform at its divided rate, the distortion field
//! picks the poly source, and the volume-only bit forces the channel
//! output to its volume regardless of the waveform. The high-pass
//! filters XOR channel one against three and channel two against four.

use demu_config::model::BootState;
use demu_core::device::{AccessWidth, BusAccess, InputSink, MachineCtx};
use demu_core::frequency::{Address, Tick};
use demu_core::signal::Level;
use demu_core::{Device, Snapshot, SnapshotError};

/// One POKEY audio channel.
#[derive(Debug)]
struct PokeyChannel {
    audf: u8,
    audc: u8,
    /// The machine-cycle countdown of the waveform divider.
    div: u32,
    /// The divisor period in machine cycles.
    div_max: u32,
    /// The channel's four-bit poly position (a bit index into a fixed
    /// 15-step pattern).
    p4: u8,
    /// The channel's five-bit poly position.
    p5: u8,
    /// The output wave state.
    wave: bool,
}

impl PokeyChannel {
    fn new() -> Self {
        Self {
            audf: 0,
            audc: 0xB0,
            div: 0,
            div_max: 1,
            p4: 0,
            p5: 0,
            wave: false,
        }
    }

    /// The volume-only bit: D4 of AUDC.
    fn volume_only(&self) -> bool {
        self.audc & 0x10 != 0
    }

    /// The 4-bit volume level of D3-0.
    fn volume(&self) -> i32 {
        i32::from(self.audc & 0x0F)
    }
}

/// The 4-bit poly pattern of the chip: a 15-step sequence.
const POLY4: [u8; 15] = [1, 1, 0, 1, 1, 1, 0, 0, 0, 0, 1, 0, 1, 0, 0];

/// The 5-bit poly pattern: 31 steps.
const POLY5: [u8; 31] = [
    1, 1, 1, 1, 1, 0, 0, 1, 1, 0, 1, 0, 0, 1, 0, 1, 1, 0, 0, 0, 1, 0, 1, 0, 1, 1, 1, 0, 0, 0, 1,
];

/// The poly bit at a position, wrapping over the table.
fn poly_bit(table: &[u8], pos: u8) -> bool {
    table[pos as usize % table.len()] == 1
}

/// The waveform decision of one division tick, per the distortion
/// field of AUDC bits 7-5. Selects zero and two gate the noise polys
/// with the five-bit poly, four reads the seventeen-bit poly, six the
/// four-bit poly; the odd selects are the pure tone, which toggles.
/// The `wave_state` carries the current toggle for the pure forms.
fn wave_decision(
    audc: u8,
    poly17_word: u8,
    poly4_bit: bool,
    poly5_bit: bool,
    wave_state: bool,
) -> (bool, bool) {
    let select = (audc >> 5) & 0x07;
    let wide_bit = u32::from(audc & 0x03);
    match select {
        0 => ((poly17_word & (1 << wide_bit) != 0) && poly5_bit, false),
        2 => (poly4_bit && poly5_bit, false),
        4 => (poly17_word & (1 << wide_bit) != 0, false),
        6 => (poly4_bit, false),
        _ => (!wave_state, true), // The pure tone toggles.
    }
}

/// The POKEY sound chip.
#[derive(Debug)]
pub struct Pokey {
    name: String,
    /// Domain ticks per machine cycle, from the `cpu` divider entry.
    cpu_div: u64,
    channels: [PokeyChannel; 4],
    audctl: u8,
    /// The global seventeen-bit poly state.
    p17: u32,
    /// The global nine-bit poly state.
    p9: u16,
    irqen: u8,
    irqst: u8,
    kbcode: u8,
    skctl: u8,
    skstat: u8,
    /// The pot inputs 0-7.
    pot: [u8; 8],
    samples: Vec<i16>,
    next: Option<Tick>,
    last_tick: Option<Tick>,
}

impl Pokey {
    /// The IRQ output level: asserted while any enabled source is
    /// still active in IRQST.
    pub fn irq_level(&self) -> bool {
        (!self.irqst) & self.irqen != 0
    }

    /// A device from its configuration entry.
    pub fn new(device: &demu_config::model::DeviceInstance) -> Self {
        let cpu_div = u64::from(device.dividers.get("cpu").copied().unwrap_or(1)).max(1);
        let mut pokey = Self {
            name: device.name.to_string(),
            cpu_div,
            channels: [
                PokeyChannel::new(),
                PokeyChannel::new(),
                PokeyChannel::new(),
                PokeyChannel::new(),
            ],
            audctl: 0,
            p17: 0x1_FFFF,
            p9: 0x1_FF,
            irqen: 0,
            irqst: 0xFF,
            kbcode: 0x7F,
            skctl: 0,
            skstat: 0xFF,
            pot: [0; 8],
            samples: Vec::new(),
            next: None,
            last_tick: None,
        };
        pokey.reset_dividers();
        pokey
    }

    /// The base clock divisor of a channel per AUDCTL.
    fn base_divisor(&self, ch: usize) -> u32 {
        let _ = ch;
        if self.audctl & 0x01 != 0 { 114 } else { 28 }
    }

    /// Recompute the channel divider periods.
    fn reset_dividers(&mut self) {
        let audfs = [
            self.channels[0].audf,
            self.channels[1].audf,
            self.channels[2].audf,
            self.channels[3].audf,
        ];
        let base = self.base_divisor(0);
        for (i, ch) in self.channels.iter_mut().enumerate() {
            // Joined pairs form sixteen-bit divisors; the low byte is
            // channel one or three and the high byte its partner.
            if self.audctl & 0x18 != 0 {
                if i == 1 || i == 3 {
                    ch.div_max = 0; // The slave channel carries no timer.
                    continue;
                }
                let high = audfs[if i == 0 { 1 } else { 3 }] as u32;
                let word = (high << 8) | audfs[i] as u32;
                ch.div_max = (word + 7).max(1) * 2;
                continue;
            }
            let high_bit = if i == 0 { 0x40 } else { 0x20 };
            let high_clock = (i == 0 || i == 2) && self.audctl & high_bit != 0;
            let n = if high_clock {
                u32::from(audfs[i]) + 4
            } else {
                u32::from(audfs[i]) + 1
            };
            ch.div_max = if high_clock { n * 2 } else { n * base * 2 };
        }
    }

    /// The top eight bits of the seventeen-bit poly.
    fn poly17_word(&self) -> u8 {
        (self.p17 >> 9) as u8
    }

    /// One machine-cycle tick of the audio engine.
    fn audio_tick(&mut self) {
        // The global polys clock at the machine rate. The seventeen-bit
        // form shifts right, feeds bit seven from bits eight and
        // thirteen, and carries the old bit zero to the top; the
        // nine-bit form feeds from bits zero and five. Random reads
        // the top eight bits.
        let old = self.p17 & 0x1_FFFF;
        let in8 = ((old >> 8) ^ (old >> 13)) & 1;
        let top = old & 1;
        self.p17 = (((old >> 1) & !(1u32 << 7)) | (in8 << 7) | (top << 16)) & 0x1_FFFF;
        let old9 = u32::from(self.p9);
        let in9 = (old9 ^ (old9 >> 5)) & 1;
        self.p9 = (((old9 >> 1) | (in9 << 8)) & 0x1_FF) as u16;

        // The poly sources read before the per-channel updates so the
        // borrow of the channel list stays free.
        let word17 = self.poly17_word();
        let bit9 = self.p9 & 1 != 0;
        let mut waves = [true; 4];
        for (i, ch) in self.channels.iter_mut().enumerate() {
            if ch.div_max == 0 {
                // The slave channel of a joined pair stays silent.
                waves[i] = false;
                continue;
            }
            if ch.div > 1 {
                ch.div -= 1;
                waves[i] = ch.wave;
                continue;
            }
            ch.div = ch.div_max.max(1);
            // Advance the channel's poly taps.
            ch.p4 = ch.p4.wrapping_add(1);
            ch.p5 = ch.p5.wrapping_add(1);
            let (audc, p4, p5, voice_only, state) =
                (ch.audc, ch.p4, ch.p5, ch.volume_only(), ch.wave);
            let (wave, _toggled) = if voice_only {
                (true, false)
            } else {
                wave_decision(
                    audc,
                    word17,
                    poly_bit(&POLY4, p4),
                    poly_bit(&POLY5, p5),
                    state,
                )
            };
            ch.wave = wave;
            waves[i] = wave;
        }
        let _ = bit9;
        // The high-pass filters: one against three, two against four.
        let mut samples = 0i32;
        for (i, ch) in self.channels.iter().enumerate() {
            let mut on = ch.wave || ch.volume_only();
            if i == 0 && self.audctl & 0x02 != 0 {
                on = on && !waves[2];
            }
            if i == 1 && self.audctl & 0x01 != 0 {
                on = on && !waves[3];
            }
            samples += if on { ch.volume() } else { -ch.volume() };
        }
        self.samples.push((samples * 2048) as i16);
    }
}

impl BusAccess for Pokey {
    fn read(&mut self, _width: AccessWidth, address: Address, _tick: Tick) -> u32 {
        let r = address.0 & 0x0F;
        match r {
            0..=7 => u32::from(self.pot[r as usize]),
            0x08 => {
                // ALLPOT: a bit per pot, zero = the pot connected low.
                let mut all = 0xFFu8;
                for (i, p) in self.pot.iter().enumerate() {
                    if *p == 0 {
                        all &= !(1 << i);
                    }
                }
                u32::from(all)
            }
            0x09 => u32::from(self.kbcode),
            0x0A => u32::from(self.poly17_word()),
            0x0C => 0, // SERIN: the serial input waits for the media pipeline.
            0x0E => u32::from(self.irqst),
            0x0F => u32::from(self.skstat),
            _ => 0xFF,
        }
    }

    fn write(&mut self, _width: AccessWidth, address: Address, value: u32, _tick: Tick) {
        let r = address.0 & 0x0F;
        let v = value as u8;
        match r {
            0 => {
                self.channels[0].audf = v & 0x0F;
                self.reset_dividers();
            }
            1 => self.channels[0].audc = v,
            2 => {
                self.channels[1].audf = v & 0x0F;
                self.reset_dividers();
            }
            3 => self.channels[1].audc = v,
            4 => {
                self.channels[2].audf = v & 0x0F;
                self.reset_dividers();
            }
            5 => self.channels[2].audc = v,
            6 => {
                self.channels[3].audf = v & 0x0F;
                self.reset_dividers();
            }
            7 => self.channels[3].audc = v,
            8 => {
                self.audctl = v;
                self.reset_dividers();
            }
            // STIMER and SKRES share the address with the reads; SKRES
            // clears IRQST and SKSTAT bits.
            0x09 => {
                for ch in &mut self.channels {
                    ch.div = ch.div_max.max(1);
                }
                self.irqst |= 0x09; // Timer one and timer four hold.
            }
            0x0A => self.irqst |= 0xFF,
            0x0B => {} // POTGO: the pot scan starts.
            0x0D => {} // SEROUT.
            0x0E => {
                // The IRQEN write acknowledges the matching sources.
                self.irqen = v;
                self.irqst |= v;
            }
            0x0F => self.skctl = v,
            _ => {}
        }
    }

    fn peek(&self, address: Address) -> u8 {
        let r = address.0 & 0x0F;
        match r {
            0..=7 => self.pot[r as usize],
            0x09 => self.kbcode,
            0x0A => self.poly17_word(),
            0x0E => self.irqst,
            0x0F => self.skstat,
            _ => 0,
        }
    }
}

impl Device for Pokey {
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

    fn audio(&self) -> Option<&dyn demu_core::AudioSource> {
        Some(self)
    }

    fn audio_mut(&mut self) -> Option<&mut dyn demu_core::AudioSource> {
        Some(self)
    }

    fn input_mut(&mut self) -> Option<&mut dyn InputSink> {
        Some(self)
    }

    fn reset(&mut self) {
        self.channels = [
            PokeyChannel::new(),
            PokeyChannel::new(),
            PokeyChannel::new(),
            PokeyChannel::new(),
        ];
        for ch in &mut self.channels {
            ch.audc = 0;
        }
        self.audctl = 0;
        self.p17 = 0x1_FFFF;
        self.p9 = 0x1_FF;
        self.irqen = 0;
        self.irqst = 0xFF;
        self.kbcode = 0x7F;
        self.skctl = 0;
        self.skstat = 0xFF;
        self.pot = [0; 8];
        self.samples.clear();
        self.next = None;
        self.last_tick = None;
        self.reset_dividers();
    }

    fn boot(&mut self, _state: Option<&BootState>) {
        self.reset();
        self.next = Some(Tick(0));
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(64);
        out.extend_from_slice(b"POKY");
        out.push(1);
        for ch in &self.channels {
            out.push(ch.audf);
            out.push(ch.audc);
            out.extend_from_slice(&ch.div.to_le_bytes());
            out.extend_from_slice(&ch.div_max.to_le_bytes());
            out.push(ch.p4);
            out.push(ch.p5);
            out.push(u8::from(ch.wave));
        }
        out.push(self.audctl);
        out.extend_from_slice(&self.p17.to_le_bytes());
        out.extend_from_slice(&self.p9.to_le_bytes());
        out.extend_from_slice(&self.irqen.to_le_bytes());
        out.extend_from_slice(&self.irqst.to_le_bytes());
        out.extend_from_slice(&self.kbcode.to_le_bytes());
        out.extend_from_slice(&self.skctl.to_le_bytes());
        out.extend_from_slice(&self.skstat.to_le_bytes());
        out.extend_from_slice(&self.pot[..]);
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        let expected = 5 + 4 * 13 + 1 + 4 + 2 + 1 + 1 + 1 + 1 + 1 + 8;
        if data.len() != expected || &data[..4] != b"POKY" || data[4] != 1 {
            return Err(SnapshotError::Mismatch(format!(
                "pokey chunk must be {expected} bytes"
            )));
        }
        let mut pos = 5usize;
        for ch in self.channels.iter_mut() {
            ch.audf = data[pos];
            pos += 1;
            ch.audc = data[pos];
            pos += 1;
            ch.div = u32::from_le_bytes(data[pos..pos + 4].try_into().expect("4 bytes"));
            pos += 4;
            ch.div_max = u32::from_le_bytes(data[pos..pos + 4].try_into().expect("4 bytes"));
            pos += 4;
            ch.p4 = data[pos];
            pos += 1;
            ch.p5 = data[pos];
            pos += 1;
            ch.wave = data[pos] != 0;
            pos += 1;
        }
        self.audctl = data[pos];
        pos += 1;
        self.p17 = u32::from_le_bytes(data[pos..pos + 4].try_into().expect("4 bytes")) & 0x1_FFFF;
        pos += 4;
        self.p9 = u16::from_le_bytes(data[pos..pos + 2].try_into().expect("2 bytes")) & 0x1_FF;
        pos += 2;
        self.irqen = data[pos];
        pos += 1;
        self.irqst = data[pos];
        pos += 1;
        self.kbcode = data[pos];
        pos += 1;
        self.skctl = data[pos];
        pos += 1;
        self.skstat = data[pos];
        pos += 1;
        self.pot.copy_from_slice(&data[pos..pos + 8]);
        self.samples.clear();
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
        ctx.notify(
            "irq",
            if self.irq_level() {
                Level::High
            } else {
                Level::Low
            },
            tick,
        );
        self.audio_tick();
        self.next = Some(Tick(tick.0 + self.cpu_div));
    }
}

impl InputSink for Pokey {
    fn inject(&mut self, event: &str) -> bool {
        // Grammar: `key <n>` presses the key code and raises the key
        // IRQ; `key <n>-release` releases; `pot<n> <value>` sets a pot.
        let form = event.trim().to_ascii_lowercase();
        if let Some(code) = form
            .strip_prefix("key ")
            .and_then(|v| v.strip_suffix("-release"))
            .and_then(|v| v.parse::<u8>().ok())
        {
            self.kbcode = code | 0x40;
            self.skstat |= 0x04;
            self.irqst |= 0x40;
            return true;
        }
        if let Some(code) = form.strip_prefix("key ").and_then(|v| v.parse::<u8>().ok()) {
            self.kbcode = code;
            self.skstat &= !0x04;
            self.irqst &= !0x40;
            return true;
        }
        let pot = form.strip_prefix("pot").and_then(|v| v.split_once(' '));
        match pot.and_then(|(n, v)| n.parse::<usize>().ok().zip(v.trim().parse::<u8>().ok())) {
            Some((n, v)) if n < 8 => {
                self.pot[n] = v;
                true
            }
            _ => false,
        }
    }
}

impl demu_core::AudioSource for Pokey {
    fn sample_rate(&self) -> u32 {
        1_789_773
    }

    fn take_samples(&mut self) -> Vec<i16> {
        std::mem::take(&mut self.samples)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use demu_core::AudioSource;

    fn pokey() -> Pokey {
        Pokey {
            name: "pokey".to_string(),
            cpu_div: 4,
            channels: [
                PokeyChannel::new(),
                PokeyChannel::new(),
                PokeyChannel::new(),
                PokeyChannel::new(),
            ],
            audctl: 0,
            p17: 0x1_FFFF,
            p9: 0x1_FF,
            irqen: 0,
            irqst: 0xFF,
            kbcode: 0x7F,
            skctl: 0,
            skstat: 0xFF,
            pot: [0; 8],
            samples: Vec::new(),
            next: None,
            last_tick: None,
        }
    }

    #[test]
    fn divider_sums_from_base_and_audf() {
        let mut p = pokey();
        p.channels[0].audf = 3;
        p.reset_dividers();
        // The 64 kHz base of 28 machine cycles times two.
        assert_eq!(p.channels[0].div_max, 4 * 28 * 2);
        p.channels[0].div = p.channels[0].div_max;
        p.audctl |= 0x40; // The high clock on channel one.
        p.reset_dividers();
        assert_eq!(p.channels[0].div_max, (3 + 4) * 2);
    }

    #[test]
    fn volume_only_outputs_constant_volume() {
        let mut p = pokey();
        p.channels[0].audc = 0x10 | 0x08;
        let mut tick = Tick(0);
        for _ in 0..4 {
            p.audio_tick();
            tick = Tick(tick.0 + 4);
        }
        let samples = p.take_samples();
        assert_eq!(samples.len(), 4);
        assert_eq!(samples[0], (8 * 2048) as i16);
    }

    #[test]
    fn pure_tone_toggles() {
        let mut p = pokey();
        // Bits 7-5 = 110 select the poly-four noise; the odd selects
        // toggle as pure tone. Use 100 (poly seventeen) for a slow
        // alternation: the top-8 word drives the output bit.
        p.channels[0].audc = 0x80 | 0x0F;
        p.channels[0].audf = 9;
        p.reset_dividers();
        for _ in 0..30 {
            p.audio_tick();
        }
        // A poly source produces output, mixed with silent channels.
        assert!(p.take_samples().iter().any(|s| *s != 0));
    }

    #[test]
    fn irq_mask_and_acknowledge() {
        let mut p = pokey();
        p.irqen = 0x40;
        p.irqst = 0xFF;
        assert!(!p.irq_level());
        p.irqst &= !0x40;
        assert!(p.irq_level());
        p.write(AccessWidth::Byte, Address(0xE80E), 0x40, Tick(0));
        assert_eq!(p.irqst, 0xFF);
    }

    #[test]
    fn key_inject_raises_keyboard_irq() {
        let mut p = pokey();
        p.irqen = 0x40;
        assert!(p.inject("key 12"));
        assert!(p.irq_level(), "the key source pulls the IRQ line");
        assert!(p.inject("key 12-release"));
        assert!(!p.irq_level(), "the release lifts the source");
    }

    #[test]
    fn random_reads_polys() {
        let p = pokey();
        let word = p.poly17_word();
        let again = p.poly17_word();
        assert_eq!(word, again);
    }

    #[test]
    fn snapshot_round_trip() {
        let mut p = pokey();
        p.channels[0].audf = 9;
        p.irqen = 0x10;
        let snap = p.snapshot();
        let mut twin = pokey();
        twin.restore(&snap).expect("restore");
        assert_eq!(twin.snapshot(), snap);
        assert_eq!(twin.channels[0].audf, 9);
    }
}
