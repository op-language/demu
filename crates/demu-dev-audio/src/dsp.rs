//! The SONY S-DSP engine of the Super Nintendo's APU.
//!
//! The chip carries eight voices over a 128-byte register file that
//! the SPC700 pokes through `$F2`/`$F3`. Each voice decodes BRR blocks
//! addressed through the sample directory page `$5D` out of the APU's
//! RAM, runs its 14-bit pitch and its ADSR or gain envelope, and feeds
//! the globals: the main and echo volumes, the echo chain with its
//! eight FIR taps and the feedback, the pitch modulation, the noise,
//! and the key-on and key-off paths.
//!
//! The engine is a component, not a `Device`. The owning APU drives
//! `step` once per SPC700 cycle and hands its RAM in as a slice; the
//! engine reads DIR entries and BRR blocks through bounds-checked
//! `.get` calls and never writes the slice back. One stereo sample
//! runs per 32 steps, the 32 kHz sample clock on the 1,024,000 Hz
//! domain, so the first sample lands on the 32nd step.
//!
//! Simplifications of this model:
//!
//! - The envelope rate table is the emulator-standard approximation:
//!   one 1/1024 envelope step per `ENV_PERIOD[index]` stereo samples.
//!   The attack climbs linearly, the decay falls linearly, the sustain
//!   holds, and the release falls exponentially by `1 + env >> 8`. The
//!   sustain rate register is stored but unused, as the hold covers
//!   it. The sustain level is `SSS * 0x100`, with `SSS` zero holding
//!   silence. The four gain modes collapse to a linear climb or the
//!   exponential fall; the direct gain sets the level at once. The
//!   exact hardware table is a follow-up.
//! - The voice sample is the raw BRR output: the Gaussian four-tap
//!   interpolation and the first-nibble skip of the real decoder are
//!   unmodeled. Shifts 13-15 are invalid on hardware; this decode
//!   shifts all sixteen forms and clamps each result to a 16-bit
//!   range.
//! - The echo buffer is internal and mono. `ESA` and `EDL` size a
//!   ring of `EDL * 128` entries, minimum one, and the engine never
//!   writes the echo page back to the APU RAM. Each entry holds the
//!   mono sum of the echo-enabled voices plus the FIR feedback, and
//!   the FIR is a plain eight-long shift-register dot product over
//!   the delayed values at the fixed `1/128` scale.
//! - The noise generator is a shared 16-bit LFSR seeded at reset. It
//!   shifts once per `1 << (FLG frequency >> 1)` stereo samples, and
//!   a noise voice takes the raw full-scale sign of the LFSR's LSB.
//! - Pitch modulation adds `(previous voice's decoded sample * pitch)
//!   >> 16` to the increment, clamped into the plain pitch range.
//! - KON bits latch and execute at the next sample boundary, and the
//!   latch and the register readback clear once executed, which also
//!   clears the voice's ENDX bit. KOFF is an immediate strobe that
//!   drops its voices to release, and its readback stays zero.
//! - FLG bit 7 is the soft reset: while set, each boundary clears the
//!   voices' decode and envelope state. Bit 6 mutes the pushed
//!   samples. Bit 5 blocks the echo buffer write.
//! - The mix scales the envelope product to 1/2048 with rounding and
//!   the main and echo volumes to 1/128, then pushes the mono mix
//!   `(L + R) / 2` per stereo sample through `take_samples`. The
//!   capture caps at `0x8000` samples, as the caller drains through
//!   `take_samples`.

/// One S-DSP voice.
#[derive(Debug, Clone, Copy)]
struct DspVoice {
    /// The pitch phase: one BRR nibble per `0x1000`.
    acc: u16,
    /// The APU RAM address of the current BRR block header.
    src: u16,
    /// The nibble index within the 16-nibble block, 0 to 15.
    nibble_index: u8,
    /// The block's filter form, from the header.
    filter: u8,
    /// The block's shift, from the header.
    shift: u8,
    /// The block's end flag, from the header.
    end_block: bool,
    /// The block's loop flag, from the header.
    loop_block: bool,
    /// The last decoded sample.
    p1: i16,
    /// The decoded sample before `p1`.
    p2: i16,
    /// The envelope level, 0 to `0x7FF`.
    env: u16,
    /// The envelope phase.
    env_phase: EnvPhase,
    /// The stereo samples toward the next envelope step.
    env_count: u16,
    /// The last decoded, or noise, sample.
    out: i16,
    /// The BRR decoder runs.
    keyed: bool,
}

impl DspVoice {
    /// A fresh voice at silence.
    fn new() -> Self {
        Self {
            acc: 0,
            src: 0,
            nibble_index: 0,
            filter: 0,
            shift: 0,
            end_block: false,
            loop_block: false,
            p1: 0,
            p2: 0,
            env: 0,
            env_phase: EnvPhase::Idle,
            env_count: 0,
            out: 0,
            keyed: false,
        }
    }
}

/// The envelope phase of one voice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EnvPhase {
    /// The voice sits at silence.
    Idle,
    /// The level climbs to full.
    Attack,
    /// The level falls linearly to the sustain.
    Decay,
    /// The level holds the sustain.
    Sustain,
    /// The level falls exponentially to zero.
    Release,
    /// The gain register rate drives the level.
    Gain,
}

/// The envelope rate table, in stereo samples per 1/1024 envelope
/// step, per rate index. The emulator-standard approximation set; the
/// index 0 entry is the hidden rate that never steps.
const ENV_PERIOD: [u16; 32] = [
    0, 2048, 1536, 1280, 1024, 768, 640, 512, //
    384, 320, 256, 192, 160, 128, 96, 64, //
    48, 40, 32, 24, 20, 16, 12, 10, //
    8, 6, 5, 4, 3, 2, 1, 1,
];

/// The fixed snapshot bytes through the ring and the sample FIFO
/// length prefixes.
const SNAPSHOT_PREFIX: usize = 160;

/// The fixed snapshot bytes after the variable rings: the ring
/// position and the eight voices at 21 bytes each.
const SNAPSHOT_TAIL: usize = 2 + 8 * 21;

/// The byte at `addr` in the APU RAM; an out-of-range read is zero.
fn ram_read(ram: &[u8], addr: u16) -> u8 {
    ram.get(usize::from(addr)).copied().unwrap_or(0)
}

/// The little-endian word at `addr`, wrapping inside the RAM space.
fn ram_read16(ram: &[u8], addr: u16) -> u16 {
    u16::from(ram_read(ram, addr)) | (u16::from(ram_read(ram, addr.wrapping_add(1))) << 8)
}

/// One decoded BRR sample: the signed nibble at its shift, the filter
/// over the history, and the 16-bit clamp. The published coefficient
/// forms use floor shifts: the first filter carries `p1 * 15/16`, the
/// second `p1 * 61/32 - p2 * 15/16`, and the third `p1 * 115/64 -
/// p2 * 13/16`.
fn brr_decode(nibble: u8, shift: u8, filter: u8, p1: i16, p2: i16) -> i16 {
    let nib = (if nibble & 0x08 != 0 {
        i32::from(nibble) - 16
    } else {
        i32::from(nibble)
    }) << usize::from(shift);
    let p1 = i32::from(p1);
    let p2 = i32::from(p2);
    let value = match filter & 0x03 {
        0 => nib,
        1 => nib + p1 - (p1 >> 4),
        2 => nib + 2 * p1 - 3 * (p1 >> 5) - p2 + (p2 >> 4),
        _ => nib + 2 * p1 - 13 * (p1 >> 6) - p2 + 3 * (p2 >> 4),
    };
    value.clamp(i16::MIN as i32, i16::MAX as i32) as i16
}

/// The sustain level target of the SSS bits, per the standard
/// published mapping: the level scales by `0x100`, and `SSS` zero
/// holds silence.
fn sustain_target(sss: u8) -> u16 {
    match sss & 0x07 {
        0 => 0,
        s => u16::from(s) << 8,
    }
}

/// The eight voices, the globals, and the SPC700-side state of the
/// S-DSP engine.
#[derive(Debug)]
pub struct Sdsp {
    /// The 128 DSP registers. Read-only registers ENVX, OUTX, and
    /// ENDX are not stored; reads derive them from the live state.
    regs: [u8; 128],
    /// The per-voice decode and envelope state.
    voices: [DspVoice; 8],
    /// The SPC700 tick count toward the next stereo sample.
    counter: u8,
    /// The latched KON bits waiting for the next sample boundary.
    kon_latch: u8,
    /// The ENDX bits, one per voice.
    end_flags: u8,
    /// The shared noise LFSR.
    lfsr: u16,
    /// The stereo samples toward the next noise LFSR shift.
    noise_count: u16,
    /// The FIR shift register, the newest delayed value at index 0.
    fir_buf: [i16; 8],
    /// The internal echo delay ring, one mono entry per stereo sample.
    ring: Vec<i16>,
    /// The echo ring read and write position.
    rpos: usize,
    /// The mixed mono samples, drained through `take_samples`, capped
    /// at `0x8000`.
    samples: Vec<i16>,
}

impl Sdsp {
    /// A fresh engine at silence.
    pub fn new() -> Self {
        let mut out = Self {
            regs: [0; 128],
            voices: std::array::from_fn(|_| DspVoice::new()),
            counter: 0,
            kon_latch: 0,
            end_flags: 0,
            lfsr: 0x7FFF,
            noise_count: 0,
            fir_buf: [0; 8],
            ring: Vec::new(),
            rpos: 0,
            samples: Vec::with_capacity(0x100),
        };
        out.set_register(0x7D, 0);
        out
    }

    /// Reset to the fresh state.
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// One SPC700-cycle tick. The engine maintains its own sample
    /// counter: one stereo sample per 32 ticks, the 32000 Hz sample
    /// clock on the 1024000 Hz domain. The slice is the APU's 64 KiB
    /// RAM for the engine's DIR and BRR reads.
    pub fn step(&mut self, ram: &[u8]) {
        self.counter += 1;
        if self.counter >= 32 {
            self.counter -= 32;
            self.run_sample(ram);
        }
    }

    /// The read of one register. ENVX, OUTX, and ENDX derive from the
    /// live state; the rest read the register file.
    pub fn register(&self, index: u8) -> u8 {
        let r = index & 0x7F;
        let voice = usize::from((r >> 4) & 0x07);
        match r & 0x0F {
            0x08 => (self.voices[voice].env >> 4) as u8,
            0x09 => (self.voices[voice].out >> 8) as u8,
            _ => {
                if r == 0x7C {
                    self.end_flags
                } else {
                    self.regs[usize::from(r)]
                }
            }
        }
    }

    /// The write of one register. ENVX, OUTX, and ENDX ignore writes;
    /// KON latches, KOF strokes the voices to release, and DIR resizes
    /// the echo ring.
    pub fn set_register(&mut self, index: u8, value: u8) {
        let r = index & 0x7F;
        match r & 0x0F {
            0x08 | 0x09 => {}
            _ => match r {
                0x4C => {
                    self.regs[0x4C] = value;
                    self.kon_latch |= value;
                }
                0x5C => {
                    self.apply_koff(value);
                    self.regs[0x5C] = 0;
                }
                0x7C => {}
                0x7D => {
                    self.regs[0x7D] = value;
                    self.resize_echo();
                }
                r => self.regs[usize::from(r)] = value,
            },
        }
    }

    /// The drained samples since the last call, appended to `out`:
    /// one mono mixed i16 per stereo sample.
    pub fn take_samples(&mut self, out: &mut Vec<i16>) {
        out.extend_from_slice(&self.samples);
        self.samples.clear();
    }

    /// The fixed-layout chunk: the tag "SDSP", the version, the
    /// register file, the engine scalars, the FIR shift register, the
    /// length-prefixed echo ring, the length-prefixed sample FIFO, the
    /// ring position, and the eight voices.
    pub fn snapshot(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(
            SNAPSHOT_PREFIX + 2 * (self.ring.len() + self.samples.len()) + SNAPSHOT_TAIL,
        );
        out.extend_from_slice(b"SDSP");
        out.push(1);
        out.extend_from_slice(&self.regs);
        out.push(self.counter);
        out.push(self.kon_latch);
        out.push(self.end_flags);
        out.extend_from_slice(&self.lfsr.to_le_bytes());
        out.extend_from_slice(&self.noise_count.to_le_bytes());
        for value in &self.fir_buf {
            out.extend_from_slice(&value.to_le_bytes());
        }
        out.extend_from_slice(&(self.ring.len() as u16).to_le_bytes());
        out.extend_from_slice(&(self.samples.len() as u16).to_le_bytes());
        for value in &self.ring {
            out.extend_from_slice(&value.to_le_bytes());
        }
        for value in &self.samples {
            out.extend_from_slice(&value.to_le_bytes());
        }
        out.extend_from_slice(&(self.rpos as u16).to_le_bytes());
        for v in &self.voices {
            out.extend_from_slice(&v.acc.to_le_bytes());
            out.extend_from_slice(&v.src.to_le_bytes());
            out.push(v.nibble_index);
            out.push(v.filter);
            out.push(v.shift);
            out.push(u8::from(v.end_block));
            out.push(u8::from(v.loop_block));
            out.extend_from_slice(&v.p1.to_le_bytes());
            out.extend_from_slice(&v.p2.to_le_bytes());
            out.extend_from_slice(&v.env.to_le_bytes());
            out.push(match v.env_phase {
                EnvPhase::Idle => 0,
                EnvPhase::Attack => 1,
                EnvPhase::Decay => 2,
                EnvPhase::Sustain => 3,
                EnvPhase::Release => 4,
                EnvPhase::Gain => 5,
            });
            out.extend_from_slice(&v.env_count.to_le_bytes());
            out.extend_from_slice(&v.out.to_le_bytes());
            out.push(u8::from(v.keyed));
        }
        out
    }

    /// Restore the chunk; the tag, the version, the ring and FIFO
    /// bounds, and the exact fixed length validate ahead of the walk.
    pub fn restore(&mut self, data: &[u8]) -> Result<(), demu_core::SnapshotError> {
        if data.len() < SNAPSHOT_PREFIX + 2 || &data[..4] != b"SDSP" || data[4] != 1 {
            return Err(demu_core::SnapshotError::Mismatch(format!(
                "sdsp chunk needs {} header bytes with the tag SDSP",
                SNAPSHOT_PREFIX + 2
            )));
        }
        let ring_len = u16::from_le_bytes(data[156..158].try_into().expect("2 bytes")) as usize;
        let fifo_len = u16::from_le_bytes(data[158..160].try_into().expect("2 bytes")) as usize;
        let expected = SNAPSHOT_PREFIX + 2 * (ring_len + fifo_len) + SNAPSHOT_TAIL;
        if ring_len == 0 || ring_len > 0xFFusize * 128 || data.len() != expected {
            return Err(demu_core::SnapshotError::Mismatch(format!(
                "sdsp chunk must be {expected} bytes"
            )));
        }
        let mut pos = SNAPSHOT_PREFIX + 2 * (ring_len + fifo_len);
        let rpos = u16::from_le_bytes(data[pos..pos + 2].try_into().expect("2 bytes")) as usize;
        pos += 2;
        if rpos >= ring_len {
            return Err(demu_core::SnapshotError::Mismatch(
                "sdsp ring position out of range".into(),
            ));
        }
        self.regs.copy_from_slice(&data[5..133]);
        self.counter = data[133] & 0x1F;
        self.kon_latch = data[134];
        self.end_flags = data[135];
        self.lfsr = u16::from_le_bytes(data[136..138].try_into().expect("2 bytes"));
        self.noise_count = u16::from_le_bytes(data[138..140].try_into().expect("2 bytes"));
        for (i, slot) in self.fir_buf.iter_mut().enumerate() {
            let at = 140 + i * 2;
            *slot = i16::from_le_bytes(data[at..at + 2].try_into().expect("2 bytes"));
        }
        self.ring = vec![0; ring_len];
        for (i, slot) in self.ring.iter_mut().enumerate() {
            let at = SNAPSHOT_PREFIX + i * 2;
            *slot = i16::from_le_bytes(data[at..at + 2].try_into().expect("2 bytes"));
        }
        self.samples = Vec::with_capacity(fifo_len);
        for i in 0..fifo_len {
            let at = SNAPSHOT_PREFIX + 2 * ring_len + i * 2;
            self.samples.push(i16::from_le_bytes(
                data[at..at + 2].try_into().expect("2 bytes"),
            ));
        }
        self.rpos = rpos;
        for v in self.voices.iter_mut() {
            v.acc = u16::from_le_bytes(data[pos..pos + 2].try_into().expect("2 bytes"));
            pos += 2;
            v.src = u16::from_le_bytes(data[pos..pos + 2].try_into().expect("2 bytes"));
            pos += 2;
            v.nibble_index = data[pos];
            pos += 1;
            v.filter = data[pos];
            pos += 1;
            v.shift = data[pos];
            pos += 1;
            v.end_block = data[pos] != 0;
            pos += 1;
            v.loop_block = data[pos] != 0;
            pos += 1;
            v.p1 = i16::from_le_bytes(data[pos..pos + 2].try_into().expect("2 bytes"));
            pos += 2;
            v.p2 = i16::from_le_bytes(data[pos..pos + 2].try_into().expect("2 bytes"));
            pos += 2;
            v.env = u16::from_le_bytes(data[pos..pos + 2].try_into().expect("2 bytes")) & 0x7FF;
            pos += 2;
            v.env_phase = match data[pos] {
                1 => EnvPhase::Attack,
                2 => EnvPhase::Decay,
                3 => EnvPhase::Sustain,
                4 => EnvPhase::Release,
                5 => EnvPhase::Gain,
                _ => EnvPhase::Idle,
            };
            pos += 1;
            v.env_count = u16::from_le_bytes(data[pos..pos + 2].try_into().expect("2 bytes"));
            pos += 2;
            v.out = i16::from_le_bytes(data[pos..pos + 2].try_into().expect("2 bytes"));
            pos += 2;
            v.keyed = data[pos] != 0;
            pos += 1;
        }
        Ok(())
    }
}

impl Default for Sdsp {
    fn default() -> Self {
        Self::new()
    }
}

impl Sdsp {
    /// Key one voice on: reset its decode over the DIR start address
    /// and start the envelope at its configured phase. The key-on
    /// clears the voice's ENDX bit.
    fn key_on(&mut self, v: usize, ram: &[u8]) {
        let base = 0x10 * v;
        let entry = (u16::from(self.regs[0x5D]) << 8) + u16::from(self.regs[base + 4]) * 4;
        let start = ram_read16(ram, entry);
        let adsr1 = self.regs[base + 5];
        let gain = self.regs[base + 7];
        let voice = &mut self.voices[v];
        voice.acc = 0;
        voice.src = start;
        voice.nibble_index = 0;
        voice.filter = 0;
        voice.shift = 0;
        voice.end_block = false;
        voice.loop_block = false;
        voice.p1 = 0;
        voice.p2 = 0;
        voice.out = 0;
        voice.keyed = true;
        voice.env_count = 0;
        if adsr1 & 0x80 != 0 {
            voice.env_phase = EnvPhase::Attack;
            voice.env = 0;
        } else if gain & 0x80 != 0 {
            voice.env_phase = EnvPhase::Gain;
            voice.env = 0;
        } else {
            voice.env_phase = EnvPhase::Sustain;
            voice.env = u16::from(gain & 0x7F) << 4;
        }
        self.end_flags &= !(1 << v);
    }

    /// The KOF strobe: each set bit drops its voice to release.
    fn apply_koff(&mut self, bits: u8) {
        for (i, v) in self.voices.iter_mut().enumerate() {
            if bits & (1 << i) != 0 && v.env_phase != EnvPhase::Idle {
                v.env_phase = EnvPhase::Release;
                v.env_count = 0;
            }
        }
    }

    /// Resize the internal echo ring to `EDL * 128` mono entries,
    /// minimum one, and clear it.
    fn resize_echo(&mut self) {
        let size = usize::from(u16::from(self.regs[0x7D]) * 128).max(1);
        self.ring = vec![0; size];
        self.rpos = 0;
    }

    /// Advance one BRR nibble: at a block edge load the header, decode
    /// against the filter history, and on the block's end set the
    /// voice's ENDX bit and apply the loop jump or the release.
    fn consume_nibble(&mut self, v: usize, ram: &[u8]) {
        if self.voices[v].nibble_index == 0 {
            let header = ram_read(ram, self.voices[v].src);
            self.voices[v].filter = (header >> 6) & 0x03;
            self.voices[v].shift = header & 0x0F;
            self.voices[v].end_block = header & 0x20 != 0;
            self.voices[v].loop_block = header & 0x10 != 0;
        }
        let voice = &mut self.voices[v];
        let byte = ram_read(
            ram,
            voice
                .src
                .wrapping_add(1 + u16::from(voice.nibble_index / 2)),
        );
        let nibble = if voice.nibble_index.is_multiple_of(2) {
            byte >> 4
        } else {
            byte & 0x0F
        };
        let decoded = brr_decode(nibble, voice.shift, voice.filter, voice.p1, voice.p2);
        voice.p2 = voice.p1;
        voice.p1 = decoded;
        voice.out = decoded;
        voice.nibble_index += 1;
        if voice.nibble_index == 16 {
            voice.nibble_index = 0;
            if voice.end_block {
                self.end_flags |= 1 << v;
                if voice.loop_block {
                    let entry =
                        (u16::from(self.regs[0x5D]) << 8) + u16::from(self.regs[0x10 * v + 4]) * 4;
                    voice.src = ram_read16(ram, entry + 2);
                } else {
                    voice.keyed = false;
                    voice.env_phase = EnvPhase::Release;
                    voice.env_count = 0;
                }
            } else {
                voice.src = voice.src.wrapping_add(9);
            }
        }
    }

    /// The FIR dot product over the shift register: the newest
    /// delayed value lands at index 0, the `$0F` to `$7F` taps
    /// multiply in place, and the sum shifts down by the fixed
    /// `1/128` scale.
    fn fir_step(&mut self, delayed: i16, taps: [i8; 8]) -> i32 {
        for i in (1..8).rev() {
            self.fir_buf[i] = self.fir_buf[i - 1];
        }
        self.fir_buf[0] = delayed;
        let mut sum = 0i32;
        for (k, tap) in taps.iter().enumerate() {
            sum += i32::from(*tap) * i32::from(self.fir_buf[k]);
        }
        sum >> 7
    }

    /// One stereo sample boundary. The order is fixed: the soft reset,
    /// the KON execution, the noise clock, the envelopes and the voice
    /// walks, the echo chain, and the mix.
    fn run_sample(&mut self, ram: &[u8]) {
        let flg = self.regs[0x6C];
        // The soft reset wipes each voice's decode and envelope state.
        if flg & 0x80 != 0 {
            for v in self.voices.iter_mut() {
                v.keyed = false;
                v.env = 0;
                v.env_phase = EnvPhase::Idle;
                v.env_count = 0;
            }
            self.end_flags = 0;
        }
        // The latched key-ons execute once, then clear.
        let kon = self.kon_latch;
        if kon != 0 {
            self.kon_latch = 0;
            self.regs[0x4C] = 0;
            for v in 0..8usize {
                if kon & (1 << v) != 0 {
                    self.key_on(v, ram);
                }
            }
        }
        // The shared noise LFSR shifts by the FLG rate.
        let noise_shift = 1u16 << (usize::from(flg & 0x1F) >> 1);
        self.noise_count += 1;
        if self.noise_count >= noise_shift {
            self.noise_count = 0;
            let bit = (self.lfsr ^ (self.lfsr >> 1)) & 1;
            self.lfsr = (self.lfsr >> 1) | (bit << 15);
        }
        let noise_sample = if self.lfsr & 1 == 1 { 0x7FFF } else { -0x7FFF };
        // The per-voice walk: envelope, then pitch and sample.
        let pmon = self.regs[0x2D];
        let non = self.regs[0x3D];
        let eon = self.regs[0x4D];
        let mut mix_l = 0i32;
        let mut mix_r = 0i32;
        let mut eco_in = 0i32;
        for v in 0..8usize {
            let base = 0x10 * v;
            self.voices[v].env_step(
                self.regs[base + 5],
                self.regs[base + 6],
                self.regs[base + 7],
            );
            if self.voices[v].keyed {
                if non & (1 << v) != 0 {
                    self.voices[v].out = noise_sample;
                } else {
                    let pitch = u16::from(self.regs[base + 2])
                        | (u16::from(self.regs[base + 3] & 0x3F) << 8);
                    let mut inc = i32::from(pitch);
                    if pmon & (1 << v) != 0 && v > 0 {
                        let prev = i32::from(self.voices[v - 1].out);
                        inc += (prev * inc) >> 16;
                    }
                    inc = inc.clamp(0, 0x3FFF);
                    self.voices[v].acc += inc as u16;
                    while self.voices[v].acc >= 0x1000 && self.voices[v].keyed {
                        self.voices[v].acc -= 0x1000;
                        self.consume_nibble(v, ram);
                    }
                }
            }
            let voice = self.voices[v];
            let value = (i32::from(voice.out) * i32::from(voice.env) + 0x400) >> 11;
            mix_l += (value * i32::from(self.regs[base] as i8)) >> 7;
            mix_r += (value * i32::from(self.regs[base + 1] as i8)) >> 7;
            if eon & (1 << v) != 0 {
                eco_in += value;
            }
        }
        // The echo chain: the delayed entry, the FIR, the feedback
        // write, and the echo return into both channels.
        let taps: [i8; 8] = std::array::from_fn(|k| self.regs[0x0F + 0x10 * k] as i8);
        let rpos = self.rpos;
        let delayed = self.ring[rpos];
        let fir_out = self.fir_step(delayed, taps);
        if flg & 0x20 == 0 {
            let feedback = (fir_out * i32::from(self.regs[0x0D] as i8)) >> 7;
            self.ring[rpos] = (eco_in + feedback).clamp(i16::MIN as i32, i16::MAX as i32) as i16;
        }
        self.rpos = (rpos + 1) % self.ring.len();
        let echo_l = (fir_out * i32::from(self.regs[0x2C] as i8)) >> 7;
        let echo_r = (fir_out * i32::from(self.regs[0x3C] as i8)) >> 7;
        // The main volumes scale the voice mix, then one mono sample
        // lands, zero while muted.
        let left = ((mix_l * i32::from(self.regs[0x0C] as i8)) >> 7) + echo_l;
        let right = ((mix_r * i32::from(self.regs[0x1C] as i8)) >> 7) + echo_r;
        let stereo = ((left + right) / 2).clamp(i16::MIN as i32, i16::MAX as i32) as i16;
        if self.samples.len() >= 0x8000 {
            self.samples.clear();
        }
        self.samples.push(if flg & 0x40 != 0 { 0 } else { stereo });
    }
}

impl DspVoice {
    /// Advance the envelope one stereo sample. The phase picks its
    /// rate index; the hidden rate never steps. The attack climbs
    /// linearly, the decay falls linearly to the sustain level, the
    /// sustain holds, the release falls exponentially, and the gain
    /// phase climbs or falls per its mode bits.
    fn env_step(&mut self, adsr1: u8, adsr2: u8, gain: u8) {
        let period: u16 = match self.env_phase {
            EnvPhase::Idle => return,
            EnvPhase::Attack => {
                let ari = usize::from(adsr1 & 0x0F);
                if ari == 0 {
                    // The hidden attack never steps.
                    return;
                }
                ENV_PERIOD[ari * 2 - 1]
            }
            EnvPhase::Decay => ENV_PERIOD[usize::from((adsr1 >> 4) & 0x07) * 2 + 16],
            EnvPhase::Sustain => return,
            EnvPhase::Release => ENV_PERIOD[usize::from(adsr2 & 0x1F)],
            EnvPhase::Gain => ENV_PERIOD[usize::from(gain & 0x1F)],
        };
        if period == 0 {
            return;
        }
        self.env_count += 1;
        if self.env_count < period {
            return;
        }
        self.env_count = 0;
        match self.env_phase {
            EnvPhase::Attack => {
                if self.env < 0x7FF {
                    self.env += 1;
                }
                if self.env >= 0x7FF {
                    self.env_phase = EnvPhase::Decay;
                }
            }
            EnvPhase::Decay => {
                self.env -= 1;
                let target = sustain_target((adsr2 >> 5) & 0x07);
                if self.env <= target {
                    self.env = target;
                    self.env_phase = EnvPhase::Sustain;
                }
            }
            EnvPhase::Release => {
                self.env = self.env.saturating_sub(1 + (self.env >> 8));
                if self.env == 0 {
                    self.env_phase = EnvPhase::Idle;
                }
            }
            EnvPhase::Gain => {
                if (gain >> 5) & 0x03 == 0x03 {
                    self.env = self.env.saturating_sub(1 + (self.env >> 8));
                } else if self.env < 0x7FF {
                    self.env += 1;
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The standard setup: DIR page `$02` with the entry zero at
    /// `$0200` pointing the start and the loop at `$0100`, a BRR
    /// block of the filter 0 with the shift 6 over the descending
    /// nibble pairs 7, 6, 5, 4, 3, 2, 1, 0, and the engine registers
    /// set for the plain decode: full volumes, the pitch 4096, and
    /// the direct gain `$7F`.
    fn setup() -> (Sdsp, Vec<u8>) {
        let mut s = Sdsp::new();
        let mut ram = vec![0u8; 0x1000];
        ram[0x0100] = 0x06;
        for (i, byte) in [0x77u8, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11, 0x00]
            .into_iter()
            .enumerate()
        {
            ram[0x0101 + i] = byte;
        }
        ram[0x0200] = 0x00;
        ram[0x0201] = 0x01;
        ram[0x0202] = 0x00;
        ram[0x0203] = 0x01;
        s.set_register(0x00, 0x7F);
        s.set_register(0x01, 0x7F);
        s.set_register(0x02, 0x00);
        s.set_register(0x03, 0x10);
        s.set_register(0x04, 0x00);
        s.set_register(0x05, 0x00);
        s.set_register(0x07, 0x7F);
        s.set_register(0x0C, 0x7F);
        s.set_register(0x1C, 0x7F);
        s.set_register(0x2C, 0x00);
        s.set_register(0x3C, 0x00);
        s.set_register(0x5D, 0x02);
        (s, ram)
    }

    /// Run `samples` stereo samples and return the drained mono mix.
    fn run(s: &mut Sdsp, ram: &[u8], samples: usize) -> Vec<i16> {
        for _ in 0..samples * 32 {
            s.step(ram);
        }
        let mut out = Vec::new();
        s.take_samples(&mut out);
        out
    }

    #[test]
    fn registers_round_trip() {
        let mut s = Sdsp::new();
        s.set_register(0x00, 0x7F);
        s.set_register(0x02, 0x34);
        s.set_register(0x03, 0x12);
        s.set_register(0x0F, 0x42);
        s.set_register(0x4F, 0xAA);
        s.set_register(0x5D, 0x02);
        assert_eq!(s.register(0x00), 0x7F);
        assert_eq!(s.register(0x02), 0x34);
        assert_eq!(s.register(0x03), 0x12);
        assert_eq!(s.register(0x0F), 0x42);
        assert_eq!(s.register(0x4F), 0xAA);
        assert_eq!(s.register(0x5D), 0x02);
        // The read-only registers ignore writes.
        s.set_register(0x08, 0xFF);
        s.set_register(0x09, 0xFF);
        s.set_register(0x7C, 0xFF);
        assert_eq!(s.register(0x08), 0x00);
        assert_eq!(s.register(0x09), 0x00);
        assert_eq!(s.register(0x7C), 0x00);
        // The KON readback clears once executed.
        s.set_register(0x4C, 0x04);
        assert_eq!(s.register(0x4C), 0x04);
        for _ in 0..32 {
            s.step(&[]);
        }
        assert_eq!(s.register(0x4C), 0x00);
        assert!(s.voices[2].keyed);
        // The KOF strobe reads back zero.
        s.set_register(0x5C, 0x04);
        assert_eq!(s.register(0x5C), 0x00);
        assert_eq!(s.voices[2].env_phase, EnvPhase::Release);
    }

    #[test]
    fn brr_filters_decode_and_clamp() {
        // The filter forms over the history p1 = 112, p2 = 112.
        assert_eq!(brr_decode(7, 4, 0, 112, 112), 112);
        assert_eq!(brr_decode(7, 4, 1, 112, 112), 112 + 112 - 7);
        // The second form: 61/32 of p1 minus 15/16 of p2.
        assert_eq!(
            brr_decode(7, 4, 2, 112, 112),
            112 + 2 * 112 - 3 * 3 - (112 - 7)
        );
        // The third form: 115/64 of p1 minus 13/16 of p2.
        assert_eq!(
            brr_decode(7, 4, 3, 112, 112),
            112 + 2 * 112 - 13 - (112 - 3 * 7)
        );
        // The signed nibble extends before the shift.
        assert_eq!(brr_decode(8, 4, 1, 0, 0), -128);
        // The 16-bit clamp covers the invalid shifts.
        assert_eq!(brr_decode(7, 15, 0, 0, 0), i16::MAX);
        assert_eq!(brr_decode(8, 15, 0, 0, 0), i16::MIN);
    }

    #[test]
    fn kon_starts_voice_and_decodes() {
        let (mut s, ram) = setup();
        s.set_register(0x4C, 0x01);
        let samples = run(&mut s, &ram, 6);
        // The nibble pairs 7, 7, 6, 6, 5, 5 decode at 448, 448, 384,
        // 384, 320, 320 through the volume chain.
        assert_eq!(samples, [437, 437, 375, 375, 312, 312]);
        // The identical setup plays identically.
        let (mut twin, twin_ram) = setup();
        twin.set_register(0x4C, 0x01);
        assert_eq!(run(&mut twin, &twin_ram, 6), samples);
        // ENVX reads the direct gain level.
        assert_eq!(s.register(0x08), 0x7F);
    }

    #[test]
    fn pitch_paces_nibbles_per_samples() {
        // The pitch 2048 lands one nibble every second sample.
        let (mut s, ram) = setup();
        s.set_register(0x02, 0x00);
        s.set_register(0x03, 0x08);
        s.set_register(0x4C, 0x01);
        let samples = run(&mut s, &ram, 6);
        assert_eq!(samples, [0, 437, 437, 437, 437, 375]);
    }

    #[test]
    fn endx_sets_and_release_drains() {
        // A single end block without the loop flag stops the voice.
        let (mut s, mut ram) = setup();
        ram[0x0100] = 0x26;
        s.set_register(0x06, 0x1F);
        s.set_register(0x4C, 0x01);
        let _ = run(&mut s, &ram, 20);
        assert_eq!(s.end_flags, 0x01);
        assert_eq!(s.voices[0].env_phase, EnvPhase::Release);
        assert!(!s.voices[0].keyed);
        // The release at rate index 31 drains the envelope to idle.
        let _ = run(&mut s, &ram, 900);
        assert_eq!(s.voices[0].env_phase, EnvPhase::Idle);
        assert_eq!(s.voices[0].env, 0);
    }

    #[test]
    fn endx_with_loop_jumps_back() {
        let (mut s, mut ram) = setup();
        ram[0x0100] = 0x36;
        s.set_register(0x4C, 0x01);
        let samples = run(&mut s, &ram, 40);
        assert_eq!(s.end_flags, 0x01);
        // The loop address returns the voice to the same block.
        assert!(s.voices[0].keyed);
        assert_eq!(s.voices[0].env_phase, EnvPhase::Sustain);
        assert_eq!(s.voices[0].src, 0x0100);
        // The replayed block decodes the same samples again.
        assert_eq!(&samples[32..36], &[437, 437, 375, 375]);
    }

    #[test]
    fn envelope_climbs_decays_holds_and_releases() {
        let (mut s, ram) = setup();
        // ADSR: attack 15, decay 7, sustain level 7, rate 31.
        s.set_register(0x05, 0xFF);
        s.set_register(0x06, 0xFF);
        s.set_register(0x4C, 0x01);
        run(&mut s, &ram, 10);
        assert_eq!(s.voices[0].env_phase, EnvPhase::Attack);
        // The attack at rate index 29 finishes near sample 4094.
        run(&mut s, &ram, 4300);
        // The decay at rate index 30 falls to the level 7 sustain.
        run(&mut s, &ram, 100);
        assert_eq!(s.voices[0].env_phase, EnvPhase::Sustain);
        assert_eq!(s.voices[0].env, 0x700);
        assert_eq!(s.register(0x08), 0x70);
        // KOFF drops to release, which drains to idle.
        s.set_register(0x5C, 0x01);
        run(&mut s, &ram, 900);
        assert_eq!(s.voices[0].env, 0);
        assert_eq!(s.voices[0].env_phase, EnvPhase::Idle);
    }

    #[test]
    fn mute_and_zero_volume_silence() {
        let (mut s, ram) = setup();
        s.set_register(0x4C, 0x01);
        s.set_register(0x6C, 0x40);
        assert!(run(&mut s, &ram, 3).iter().all(|v| *v == 0));
        // A cleared mute plays; a zero volume silences again.
        s.set_register(0x6C, 0x00);
        s.set_register(0x00, 0x00);
        s.set_register(0x01, 0x00);
        assert!(run(&mut s, &ram, 3).iter().all(|v| *v == 0));
        // The soft reset clears the voices each boundary.
        s.set_register(0x00, 0x7F);
        s.set_register(0x01, 0x7F);
        s.set_register(0x6C, 0x00);
        s.set_register(0x4C, 0x01);
        run(&mut s, &ram, 1);
        assert!(s.voices[0].env > 0);
        s.set_register(0x6C, 0x80);
        run(&mut s, &ram, 2);
        assert_eq!(s.voices[0].env, 0);
        assert_eq!(s.voices[0].env_phase, EnvPhase::Idle);
    }

    #[test]
    fn take_samples_paces_and_clears() {
        let (mut s, ram) = setup();
        s.set_register(0x4C, 0x01);
        // 65 steps carry two sample boundaries.
        for _ in 0..65 {
            s.step(&ram);
        }
        let mut out = Vec::new();
        s.take_samples(&mut out);
        assert_eq!(out.len(), 2);
        s.take_samples(&mut out);
        assert_eq!(out.len(), 2, "the second drain adds nothing");
        assert!(s.samples.is_empty());
    }

    #[test]
    fn noise_replaces_brr_sample() {
        let (mut s, ram) = setup();
        // The noise voice ignores its BRR source and plays the LFSR.
        s.set_register(0x3D, 0x01);
        s.set_register(0x4C, 0x01);
        // The end flags stay clear: the decoder never reads.
        assert_eq!(s.end_flags, 0x00);
        let samples = run(&mut s, &ram, 80);
        assert!(samples.iter().any(|v| *v > 0), "the noise makes sound");
        assert!(samples.iter().any(|v| *v < 0), "the noise flips signs");
        // The noise source leaves the BRR cursor alone.
        assert_eq!(s.voices[0].nibble_index, 0);
    }

    #[test]
    fn bounds_checked_ram_reads_are_safe() {
        let mut s = Sdsp::new();
        s.set_register(0x4C, 0xFF);
        // Every read falls out of the empty slice to zero.
        let samples = run(&mut s, &[], 3);
        assert!(samples.iter().all(|v| *v == 0));
        assert_eq!(s.end_flags, 0x00);
    }

    #[test]
    fn snapshot_round_trip() {
        let (mut s, ram) = setup();
        s.set_register(0x4C, 0x01);
        let _ = run(&mut s, &ram, 3);
        s.set_register(0x4D, 0x01);
        s.set_register(0x2C, 0x30);
        run(&mut s, &ram, 4);
        let snap = s.snapshot();
        let mut twin = Sdsp::new();
        twin.restore(&snap).expect("restore");
        assert_eq!(twin.snapshot(), snap, "the round trip is byte-identical");
        assert_eq!(twin.register(0x5D), 0x02);
        assert!(twin.voices[0].keyed);
        // The twin continues the same play after the restore.
        let a = run(&mut s, &ram, 5);
        let b = run(&mut twin, &ram, 5);
        assert_eq!(a, b);
        // The bad tag and the short chunk refuse.
        let mut corrupt = snap.clone();
        corrupt[0] = b'X';
        assert!(twin.restore(&corrupt).is_err());
        assert!(twin.restore(&snap[..snap.len() - 1]).is_err());
    }
}
