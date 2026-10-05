//! The SID 6581 and 8580 of the Commodore 64.
//!
//! The chip carries three voices, each with a 24-bit phase accumulator,
//! the four waveforms (triangle, sawtooth, pulse, noise), the test and
//! gate and sync and ring-mod control bits, and the ADSR envelope. The
//! register file sits at `$D400` with the offset stride 32, so the
//! device decodes by the low five bits of the bus address. Offsets 0 to
//! 20 hold the three seven-register voice blocks, offset `$15/$16` the
//! 11-bit filter cutoff, `$17` the resonator and the route nibbles, and
//! `$18` the volume and the filter mode bits.
//!
//! The audio engine advances one visit per CPU cycle: every phase
//! accumulator adds its 16-bit frequency, waveforms read from the top
//! twelve accumulator bits, and each voice's envelope steps per the
//! published rate tables converted to cycles at the CPU rate. Where the
//! true chip splits its envelope generator across hundreds of clocks
//! this model counts one cycle ramp and divides the published total
//! over 15 steps, so the total time still matches the tables.
//!
//! Simplifications of this model:
//!
//! - The filter stages are unmodeled. The route and mode registers
//!   store and read back, the voices mix unfiltered, and the volume
//!   nibble scales the mix.
//! - Multiple waveform bits produce the AND of the selected forms, as
//!   the chip does.
//! - The noise channel shifts its 23-bit LFSR on the rising edge of the
//!   accumulator bit 19, and the waveform uses the shift register's
//!   middle twelve bits.
//! - The pot inputs read the injected values at `$FF` by default; the
//!   external pin is never wired.
//! - Sample rate and envelope timing follow the `pal` parameter: the
//!   NTSC CPU rate is 1,022,727 cycles a second and the PAL rate is
//!   985,248.

use demu_config::model::BootState;
use demu_core::device::{AccessWidth, BusAccess, MachineCtx};
use demu_core::frequency::{Address, Tick};
use demu_core::signal::Level;
use demu_core::{AudioSource, Device, Snapshot, SnapshotError};

/// The registers per voice block.
const REGS_PER_VOICE: usize = 7;

/// The phase of one voice's ADSR envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EnvPhase {
    /// The gate is down and the level is zero.
    Idle,
    /// The level counts up to 15.
    Attack,
    /// The level counts down to the sustain.
    Decay,
    /// The level holds at the sustain.
    Sustain,
    /// The level counts down to zero.
    Release,
}

/// One voice's state.
#[derive(Debug, Clone, Copy)]
struct Voice {
    /// The 16-bit frequency period.
    freq: u16,
    /// The 12-bit pulse width.
    pw: u16,
    /// The control register: gate, sync, ring, test, waveforms.
    ctrl: u8,
    /// The attack and decay nibbles.
    ad: u8,
    /// The sustain and release nibbles.
    sr: u8,
    /// The 24-bit phase accumulator.
    acc: u32,
    /// The 23-bit noise shift register; a fixed seed keeps runs equal.
    lfsr: u32,
    /// The previous state of the accumulator bit 19, the noise clock.
    acc19: bool,
    /// The envelope state.
    env: Envelope,
}

/// One voice's envelope state, with the step countdown.
#[derive(Debug, Clone, Copy)]
struct Envelope {
    phase: EnvPhase,
    /// The level 0 to 15 the waveform multiplies.
    level: u8,
    /// The CPU cycles toward the next step.
    count: u32,
}

impl Envelope {
    fn new() -> Self {
        Self {
            phase: EnvPhase::Idle,
            level: 0,
            count: 0,
        }
    }
}

impl Voice {
    fn new() -> Self {
        Self {
            freq: 0,
            pw: 0,
            ctrl: 0,
            ad: 0,
            sr: 0,
            acc: 0,
            lfsr: 0x7F_FFFF,
            acc19: false,
            env: Envelope::new(),
        }
    }

    /// The waveform's raw 12-bit value at the current accumulator
    /// state. The triangle, the sawtooth, the pulse, and the noise each
    /// mask into the common value, which is the chip's AND behavior.
    /// The index gives the voice's own block, so the ring partner maps
    /// 1 to 3, 2 to 1, and 3 to 2 across the three voices.
    fn wave_raw(&self, i: usize, voices: &[Voice; 3]) -> u32 {
        let t = (self.acc >> 12) & 0xFFF;
        let mut bits = 0xFFFu32;
        let mut used = false;
        if self.ctrl & 0x10 != 0 {
            // The triangle: an 11-bit value mirrored around the half,
            // then stretched to 12 bits. Ring modulation XORs the top
            // of the value with the partner accumulator's MSB.
            let mut tri = if t >= 0x800 { 0x7FF - (t & 0x7FF) } else { t };
            if self.ctrl & 0x04 != 0 && voices[(i + 2) % 3].acc & (1 << 23) != 0 {
                tri ^= 1 << 10;
            }
            bits &= tri << 1;
            used = true;
        }
        if self.ctrl & 0x20 != 0 {
            bits &= t;
            used = true;
        }
        if self.ctrl & 0x40 != 0 {
            // The pulse: high while the top twelve bits sit below the
            // width compare.
            bits &= u32::from(t < u32::from(self.pw & 0x0FFF));
            used = true;
        }
        if self.ctrl & 0x80 != 0 {
            bits &= (self.lfsr >> 11) & 0xFFF;
            used = true;
        }
        if !used { 0 } else { bits }
    }

    /// The period of the current envelope phase, in CPU cycles. The
    /// published total attack and decay times, in milliseconds, times
    /// 1023 cycles per millisecond, split over the 15 steps.
    fn env_period(&self) -> u32 {
        match self.env.phase {
            EnvPhase::Attack => Self::step_period(&MS_ATTACK, self.ad >> 4),
            EnvPhase::Decay => Self::step_period(&MS_DECAY, self.ad),
            EnvPhase::Release => Self::step_period(&MS_DECAY, self.sr),
            _ => 0,
        }
    }

    /// The step period of one envelope state.
    fn step_period(table: &[u16], rate: u8) -> u32 {
        let total = u32::from(table[usize::from(rate & 0x0F)]) * 1023;
        (total / 15).max(1)
    }

    /// Advance the envelope one CPU cycle.
    fn env_tick(&mut self) {
        let period = self.env_period();
        if period == 0 {
            return;
        }
        self.env.count += 1;
        if self.env.count < period {
            return;
        }
        self.env.count = 0;
        let sustain = self.sr >> 4;
        match self.env.phase {
            EnvPhase::Attack => {
                if self.env.level >= 15 {
                    self.env.phase = EnvPhase::Decay;
                } else {
                    self.env.level += 1;
                }
            }
            EnvPhase::Decay => {
                if self.env.level == sustain {
                    self.env.phase = EnvPhase::Sustain;
                } else {
                    self.env.level -= 1;
                }
            }
            EnvPhase::Release => {
                if self.env.level == 0 {
                    self.env.phase = EnvPhase::Idle;
                } else {
                    self.env.level -= 1;
                }
            }
            _ => {}
        }
    }
}

/// The published attack totals in milliseconds per rate value.
const MS_ATTACK: [u16; 16] = [
    2, 8, 16, 24, 38, 56, 68, 80, 100, 250, 500, 1000, 2500, 4000, 8000, 16000,
];

/// The published decay and release totals in milliseconds.
const MS_DECAY: [u16; 16] = [
    6, 24, 48, 72, 114, 168, 204, 240, 300, 750, 1500, 3000, 7500, 12000, 24000, 48000,
];

/// One step of the 23-bit noise LFSR: the taps at the bits 17 and 22.
fn lfsr_next(lfsr: u32) -> u32 {
    let bit = ((lfsr >> 17) ^ (lfsr >> 22)) & 1;
    ((lfsr >> 1) | (bit << 22)) & 0x7F_FFFF
}

/// The three voices and the mix registers of the sound chip.
#[derive(Debug)]
pub struct Sid {
    name: String,
    /// Domain ticks per CPU cycle visit.
    cpu_div: u64,
    /// The PAL rate flag: it picks the native sample rate.
    pal: bool,
    /// The mixed output samples at the native CPU rate.
    samples: Vec<i16>,
    voices: [Voice; 3],
    /// The 11-bit filter cutoff `$D415/$D416` (stored, unmodeled).
    fc: u16,
    /// The resonator and route byte `$D417` (stored, unmodeled).
    res_filt: u8,
    /// The volume and mode byte `$D418`.
    volume: u8,
    /// The pot input values.
    potx: u8,
    poty: u8,
    next: Option<Tick>,
    last_tick: Option<Tick>,
}

impl Sid {
    /// A sound chip from its configuration entry.
    pub fn new(device: &demu_config::model::DeviceInstance) -> Self {
        Self {
            name: device.name.to_string(),
            cpu_div: u64::from(device.dividers.get("cpu").copied().unwrap_or(1)).max(1),
            pal: device
                .params
                .get("pal")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            samples: Vec::new(),
            voices: [Voice::new(), Voice::new(), Voice::new()],
            fc: 0,
            res_filt: 0,
            volume: 0,
            potx: 0xFF,
            poty: 0xFF,
            next: None,
            last_tick: None,
        }
    }

    /// The waveform byte of one voice, for the OSC3 readback.
    fn wave_output(&self, i: usize) -> u32 {
        let raw = self.voices[i].wave_raw(i, &self.voices);
        (raw >> 4) & 0xFF
    }

    /// One CPU cycle of the audio engine: the phase advances, the noise
    /// shifts on the bit-19 edge, and one mixed sample lands in the
    /// capture buffer.
    fn audio_tick(&mut self) {
        // The sync and ring partners read the accumulator MSBs of the
        // state before this cycle.
        let msbs: [u32; 3] = [self.voices[0].acc, self.voices[1].acc, self.voices[2].acc];
        for i in 0..3usize {
            let partner = (i + 2) % 3;
            let v = &mut self.voices[i];
            // The sync: the partner's MSB resets this phase.
            if v.ctrl & 0x02 != 0 && msbs[partner] & (1 << 23) != 0 {
                v.acc = 0;
            }
            // The test bit holds the accumulator at zero; the envelope
            // keeps running (voice 3 carries ENV3 while muted).
            if v.ctrl & 0x08 != 0 {
                v.acc = 0;
            } else if v.freq != 0 {
                v.acc = (v.acc + u32::from(v.freq)) & 0x00FF_FFFF;
            }
            // The noise LFSR shifts on the rising edge of bit 19.
            let b19 = v.acc & (1 << 19) != 0;
            if b19 && !v.acc19 {
                v.lfsr = lfsr_next(v.lfsr);
            }
            v.acc19 = b19;
            v.env_tick();
        }
        // The unfiltered mix: each voice's waveform times its
        // envelope, summed and scaled by the volume nibble. Voice 3
        // drops out with the mode bit 7.
        let mut sum = 0i32;
        let wave: [u32; 3] = std::array::from_fn(|i| self.voices[i].wave_raw(i, &self.voices));
        for (i, v) in self.voices.iter().enumerate() {
            let mut level = i32::from(v.env.level);
            if i == 2 && self.volume & 0x80 != 0 {
                level = 0;
            }
            sum += wave[i] as i32 * level;
        }
        let vol = i32::from(self.volume & 0x0F);
        let sample = ((sum * vol) >> 9).clamp(i16::MIN as i32, i16::MAX as i32) as i16;
        self.samples.push(sample);
    }

    /// The register offset a bus address decodes to: the low five bits
    /// hold the stride-32 register window.
    fn reg_index(address: u32) -> usize {
        (address & 0x1F) as usize
    }

    /// The read of one register offset.
    fn reg_read(&self, r: usize) -> u8 {
        if r < 3 * REGS_PER_VOICE {
            let v = &self.voices[r / REGS_PER_VOICE];
            match r % REGS_PER_VOICE {
                0 => v.freq as u8,
                1 => (v.freq >> 8) as u8,
                2 => v.pw as u8,
                3 => ((v.pw >> 8) & 0x0F) as u8,
                4 => v.ctrl,
                5 => v.ad,
                _ => v.sr,
            }
        } else {
            match r {
                0x15 => self.fc as u8,
                0x16 => (self.fc >> 8) as u8,
                0x17 => self.res_filt,
                0x18 => self.volume,
                0x19 => self.potx,
                0x1A => self.poty,
                0x1B => self.wave_output(2) as u8,
                0x1C => self.voices[2].env.level * 17,
                _ => 0xFF,
            }
        }
    }

    /// The write of one register offset.
    fn reg_write(&mut self, r: usize, v: u8) {
        if r < 3 * REGS_PER_VOICE {
            let voice = r / REGS_PER_VOICE;
            let v_idx = voice;
            match r % REGS_PER_VOICE {
                0 => self.voices[v_idx].freq = (self.voices[v_idx].freq & 0xFF00) | u16::from(v),
                1 => {
                    self.voices[v_idx].freq =
                        (self.voices[v_idx].freq & 0x00FF) | (u16::from(v) << 8)
                }
                2 => self.voices[v_idx].pw = (self.voices[v_idx].pw & 0x0F00) | u16::from(v),
                3 => {
                    self.voices[v_idx].pw =
                        (self.voices[v_idx].pw & 0x00FF) | (u16::from(v & 0x0F) << 8)
                }
                4 => {
                    let old = self.voices[v_idx].ctrl;
                    self.voices[v_idx].ctrl = v;
                    // The gate edge moves the envelope: raise starts the
                    // attack from the current level, drop starts the
                    // release.
                    if !(old & 0x01 != 0) && v & 0x01 != 0 {
                        self.voices[v_idx].env.phase = EnvPhase::Attack;
                        self.voices[v_idx].env.count = 0;
                    } else if old & 0x01 != 0 && v & 0x01 == 0 {
                        self.voices[v_idx].env.phase = EnvPhase::Release;
                        self.voices[v_idx].env.count = 0;
                    }
                }
                5 => self.voices[v_idx].ad = v,
                _ => self.voices[v_idx].sr = v,
            }
        } else {
            match r {
                0x15 => self.fc = (self.fc & 0x0700) | u16::from(v),
                0x16 => self.fc = (self.fc & 0x00FF) | (u16::from(v & 0x07) << 8),
                0x17 => self.res_filt = v,
                0x18 => self.volume = v,
                _ => {}
            }
        }
    }
}

impl BusAccess for Sid {
    fn read(&mut self, _width: AccessWidth, address: Address, _tick: Tick) -> u32 {
        u32::from(self.reg_read(Self::reg_index(address.0)))
    }

    fn write(&mut self, _width: AccessWidth, address: Address, value: u32, _tick: Tick) {
        self.reg_write(Self::reg_index(address.0), value as u8);
    }

    fn peek(&self, address: Address) -> u8 {
        self.reg_read(Self::reg_index(address.0))
    }
}

impl Device for Sid {
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

    fn audio(&self) -> Option<&dyn AudioSource> {
        Some(self)
    }

    fn audio_mut(&mut self) -> Option<&mut dyn demu_core::AudioSource> {
        Some(self)
    }

    fn reset(&mut self) {
        for v in &mut self.voices {
            *v = Voice::new();
        }
        self.fc = 0;
        self.res_filt = 0;
        self.volume = 0;
        self.potx = 0xFF;
        self.poty = 0xFF;
        self.samples.clear();
        self.next = None;
        self.last_tick = None;
    }

    fn boot(&mut self, _state: Option<&BootState>) {
        self.reset();
        self.next = Some(Tick(0));
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(5 + 3 * 16 + 4 + 2);
        out.extend_from_slice(b"SID6");
        out.push(1);
        for v in &self.voices {
            out.extend_from_slice(&v.freq.to_le_bytes());
            out.extend_from_slice(&v.pw.to_le_bytes());
            out.push(v.ctrl);
            out.push(v.ad);
            out.push(v.sr);
            out.extend_from_slice(&v.acc.to_le_bytes());
            out.extend_from_slice(&(v.lfsr & 0x7F_FFFF).to_le_bytes());
            out.push(u8::from(v.acc19));
            out.push(match v.env.phase {
                EnvPhase::Idle => 0,
                EnvPhase::Attack => 1,
                EnvPhase::Decay => 2,
                EnvPhase::Sustain => 3,
                EnvPhase::Release => 4,
            });
            out.push(v.env.level);
            out.extend_from_slice(&v.env.count.to_le_bytes());
        }
        out.extend_from_slice(&self.fc.to_le_bytes());
        out.push(self.res_filt);
        out.push(self.volume);
        out.push(self.potx);
        out.push(self.poty);
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        // Per voice: the frequency and pulse words, the control and
        // envelope bytes, the accumulator, the shift register, the
        // bit-19 latch, and the envelope state with its countdown.
        let expected = 5 + 3 * (2 + 2 + 3 + 4 + 4 + 1 + 1 + 1 + 4) + 2 + 1 + 1 + 1 + 1;
        if data.len() != expected || &data[..4] != b"SID6" || data[4] != 1 {
            return Err(SnapshotError::Mismatch(format!(
                "sid chunk must be {expected} bytes"
            )));
        }
        let mut pos = 5usize;
        for v in self.voices.iter_mut() {
            v.freq = u16::from_le_bytes(data[pos..pos + 2].try_into().expect("2 bytes"));
            pos += 2;
            v.pw = u16::from_le_bytes(data[pos..pos + 2].try_into().expect("2 bytes"));
            pos += 2;
            v.ctrl = data[pos];
            pos += 1;
            v.ad = data[pos];
            pos += 1;
            v.sr = data[pos];
            pos += 1;
            v.acc = u32::from_le_bytes(data[pos..pos + 4].try_into().expect("4 bytes")) & 0xFF_FFFF;
            pos += 4;
            v.lfsr =
                u32::from_le_bytes(data[pos..pos + 4].try_into().expect("4 bytes")) & 0x7F_FFFF;
            pos += 4;
            v.acc19 = data[pos] != 0;
            pos += 1;
            v.env.phase = match data[pos] {
                1 => EnvPhase::Attack,
                2 => EnvPhase::Decay,
                3 => EnvPhase::Sustain,
                4 => EnvPhase::Release,
                _ => EnvPhase::Idle,
            };
            pos += 1;
            v.env.level = data[pos] & 0x0F;
            pos += 1;
            v.env.count = u32::from_le_bytes(data[pos..pos + 4].try_into().expect("4 bytes"));
            pos += 4;
        }
        self.fc = u16::from_le_bytes(data[pos..pos + 2].try_into().expect("2 bytes")) & 0x07FF;
        pos += 2;
        self.res_filt = data[pos];
        pos += 1;
        self.volume = data[pos];
        pos += 1;
        self.potx = data[pos];
        pos += 1;
        self.poty = data[pos];
        self.samples.clear();
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
        self.audio_tick();
        self.next = Some(Tick(tick.0 + self.cpu_div));
    }
}

impl AudioSource for Sid {
    fn sample_rate(&self) -> u32 {
        if self.pal { 985_248 } else { 1_022_727 }
    }

    fn take_samples(&mut self) -> Vec<i16> {
        std::mem::take(&mut self.samples)
    }
}

impl demu_core::InputSink for Sid {
    fn inject(&mut self, event: &str) -> bool {
        // The grammar: `potx <value>` and `poty <value>` set the paddle
        // inputs the POTX and POTY reads return.
        let form = event.trim().to_ascii_lowercase();
        let (name, text) = form.split_once(' ').unwrap_or((form.as_str(), ""));
        let value = text.trim().parse::<u8>();
        match name {
            "potx" => {
                let Ok(v) = value else { return false };
                self.potx = v;
                true
            }
            "poty" => {
                let Ok(v) = value else { return false };
                self.poty = v;
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use demu_core::InputSink;

    fn instance() -> demu_config::model::DeviceInstance {
        demu_config::model::DeviceInstance {
            name: demu_config::newtypes::DeviceName::new("sid").expect("name"),
            kind: demu_config::newtypes::KindName::new("c64.sid").expect("kind"),
            domain: "main".to_string(),
            params: std::collections::BTreeMap::new(),
            dividers: [("cpu".to_string(), 1)].into_iter().collect(),
        }
    }

    fn wr(s: &mut Sid, addr: u32, val: u8) {
        s.write(AccessWidth::Byte, Address(addr), u32::from(val), Tick(0));
    }

    fn rd(s: &mut Sid, addr: u32) -> u8 {
        s.read(AccessWidth::Byte, Address(addr), Tick(0)) as u8
    }

    #[test]
    fn registers_decode_by_voice_blocks() {
        let mut s = Sid::new(&instance());
        wr(&mut s, 0xD400, 0x22);
        wr(&mut s, 0xD402, 0x33);
        wr(&mut s, 0xD404, 0x01);
        assert_eq!(s.voices[0].freq, 0x22);
        assert_eq!(s.voices[0].pw, 0x33);
        assert_eq!(s.voices[0].ctrl, 0x01);
        // Voice 3's block starts at the offset $0E.
        wr(&mut s, 0xD40E, 0x11);
        assert_eq!(s.voices[2].freq, 0x11);
        // The pulse width high register carries only its low nibble.
        wr(&mut s, 0xD403, 0xF5);
        assert_eq!(s.voices[0].pw, 0x0533);
        // The reads return the stored bytes.
        assert_eq!(rd(&mut s, 0xD400), 0x22);
        assert_eq!(rd(&mut s, 0xD403), 0x05);
        // The unused offsets read one.
        assert_eq!(rd(&mut s, 0xD41D), 0xFF);
        // The pots read the injected values.
        assert!(s.inject("potx 3"));
        assert_eq!(rd(&mut s, 0xD419), 3);
        assert_eq!(rd(&mut s, 0xD41A), 0xFF);
        // Unknown inject events are refused.
        assert!(!s.inject("wobble 3"));
    }

    #[test]
    fn envelope_climbs_sustains_and_releases() {
        let mut s = Sid::new(&instance());
        // The fastest attack, the full sustain, the fastest release.
        wr(&mut s, 0xD405, 0x00);
        wr(&mut s, 0xD406, 0xF0);
        // The gate raises: the attack runs about two milliseconds,
        // 2046 CPU cycles here, then the ramp holds at the sustain.
        wr(&mut s, 0xD404, 0x01);
        for _ in 0..3000 {
            s.audio_tick();
        }
        assert_eq!(s.voices[0].env.phase, EnvPhase::Sustain);
        assert_eq!(s.voices[0].env.level, 15);
        // ENV3 rides voice 3 only: voice 0's envelope stays its own.
        assert_eq!(s.voices[2].env.level, 0);
        // The gate drops: the release drains to idle.
        wr(&mut s, 0xD404, 0x00);
        for _ in 0..8000 {
            s.audio_tick();
        }
        assert_eq!(s.voices[0].env.phase, EnvPhase::Idle);
        assert_eq!(s.voices[0].env.level, 0);
    }

    #[test]
    fn envelope3_tracks_voice_three_gate() {
        let mut s = Sid::new(&instance());
        wr(&mut s, 0xD412, 0x01);
        // The fastest attack steps once per 136 cycles; after 300 steps
        // the level sits at 2 and ENV3 reads 34.
        for _ in 0..300 {
            s.audio_tick();
        }
        assert_eq!(s.voices[2].env.level, 2);
        assert_eq!(rd(&mut s, 0xD41C), 34, "ENV3 scales the level by 17");
    }

    fn gate_saw_setup(s: &mut Sid) {
        wr(s, 0xD400, 0x00);
        wr(s, 0xD401, 0x01); // The frequency 0x0100.
        wr(s, 0xD405, 0x00); // The fastest attack.
        wr(s, 0xD406, 0xF0); // The full sustain.
        wr(s, 0xD404, 0x21); // The gate plus the sawtooth.
        wr(s, 0xD418, 0x0F);
    }

    #[test]
    fn waveform_and_gate_make_sound() {
        let mut s = Sid::new(&instance());
        gate_saw_setup(&mut s);
        for _ in 0..400 {
            s.audio_tick();
        }
        let samples = s.take_samples();
        assert_eq!(samples.len(), 400);
        assert!(samples.iter().any(|v| *v != 0), "the gated saw makes sound");
        // The identical register sequence plays identically.
        let mut twin = Sid::new(&instance());
        gate_saw_setup(&mut twin);
        for _ in 0..400 {
            twin.audio_tick();
        }
        assert_eq!(
            samples,
            twin.take_samples(),
            "identical setups play identically"
        );
    }

    #[test]
    fn osc3_reads_waveform_byte() {
        let mut s = Sid::new(&instance());
        wr(&mut s, 0xD40F, 0x10); // Voice 3 frequency high: 0x1000.
        wr(&mut s, 0xD412, 0x20); // Voice 3 control: sawtooth.
        for _ in 0..24 {
            s.audio_tick();
        }
        let a = rd(&mut s, 0xD41B);
        let b = rd(&mut s, 0xD41B);
        assert_eq!(a, b, "the readback is stable between engine steps");
        // The top byte of the 12-bit saw value after 24 steps of 4096.
        assert_eq!(a, 1);
    }

    #[test]
    fn snapshot_round_trip() {
        let mut s = Sid::new(&instance());
        wr(&mut s, 0xD406, 0xF0);
        wr(&mut s, 0xD401, 0x42);
        wr(&mut s, 0xD418, 0x0F);
        for _ in 0..100 {
            s.audio_tick();
        }
        let snap = s.snapshot();
        let mut twin = Sid::new(&instance());
        twin.restore(&snap).expect("restore");
        assert_eq!(twin.snapshot(), snap, "the round trip is byte-identical");
        assert_eq!(twin.voices[0].freq, 0x4200);
        assert_eq!(twin.volume, 0x0F);
    }
}
