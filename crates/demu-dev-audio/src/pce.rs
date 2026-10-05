//! The PC Engine programmable sound generator of the `pce.psg`
//! device kind, sitting inside the HuC6280.
//!
//! Six waveform channels each carry a 32-entry table of five-bit
//! samples, a twelve-bit timer, a channel amplitude, a stereo balance,
//! and the direct-data-access form; channels 5 and 6 carry noise
//! generators. One global register pair holds the main amplitude left
//! and right; two hold the low frequency oscillator, which this build
//! holds as a recorded form.
//!
//! # Pinned register surface
//!
//! The PSG answers its window offsets 0x0 to 0x9 as the register
//! indexes R0-R9; the address lines select the register and R0
//! selects the channel. The published access forms write R0 with the
//! channel number and then write R2 through R7 of that channel:
//!
//! | Offset | Register | Content |
//! |--------|----------|---------|
//! | 0 | R0 | channel select, bits 2-0; values 0 to 5 |
//! | 1 | R1 | main amplitude, bits 7-4 left and 3-0 right |
//! | 2 | R2 | frequency low byte of the selected channel |
//! | 3 | R3 | frequency high nibble, bits 3-0 |
//! | 4 | R4 | channel on (7), DDA (6), wave pointer reset (5), amplitude bits 4-0 |
//! | 5 | R5 | balance, bits 7-4 left, 3-0 right |
//! | 6 | R6 | waveform data write, five bits |
//! | 7 | R7 | noise enable bit 7 and noise frequency bits 4-0, channels 5 and 6 only |
//! | 8 | R8 | LFO frequency (held, recorded approximation) |
//! | 9 | R9 | LFO control (held, recorded approximation) |
//!
//! A select of 6 or 7 keeps the per-channel registers answered by the
//! last valid channel; the R1, R8 and R9 forms always answer. The
//! noise registers reset to a nonzero shift state so the recorded taps
//! run.
//!
//! # Pinned waveform model
//!
//! The waveform pointer walks one of 32 five-bit entries per step. The
//! published rate of the PSG's internal sample stream is 178977 steps
//! per second at the 7.15909 MHz CPU form (one step per `psg` device
//! divider ticks of 40). A channel steps its pointer at the rate
//! 3579545 / (2 * (F + 1)), the published waveform frequency form
//! f = f_master / (2 * 32 * F) per pointer step of a 32-entry cycle,
//! which is 10 / (F + 1) pointer steps per internal sample here. The
//! DAC value centers on 16, so a table entry of 16 plays silence and
//! the entries play -16 through +15 before the amplitude chain. The
//! reset bit of R4 re-points the waveform pointer at 0; a channel-off
//! write of R6 stores a table entry and steps the pointer.
//!
//! The DDA form channels R6 writes into a circular sixteen-entry
//! buffer; the playback consumes one entry per pointer step at the
//! channel rate and repeats the last consumed value when the buffer
//! runs dry. The published buffer overflow behavior drops the oldest
//! sample.
//!
//! The noise form of channels 5 and 6 replaces the waveform value with
//! the bit of an eighteen-bit linear feedback register; the shift rate
//! is 3579545 / (4 * (NF + 1)), five noise steps per (NF + 1)
//! internal samples; the output is the full five-bit form (31 when the
//! bit reads one, 0 when zero). The exact tap positions of the real
//! die are not published; the tap pair of bits 17 and 14 is the
//! recorded pin.
//!
//! # Mixing
//!
//! The channel value multiplies by the channel amplitude, the balance
//! level, and the main amplitude, levels of 0 to 15 and 0 to 31 where
//! zero mutes the path. The left and right sums collapse to one mono
//! sample by halving the sum, and the sample scales by 24 and the 1/40
//! shift to stay inside the 16-bit range. `sample_rate` reports 178977
//! and `take_samples` drains the integer buffer; the same registers
//! produce the same samples.
//!
//! # Recorded approximations
//!
//! - The LFO channel pair (R8/R9) and its modulation of channel 1 by
//!   the channel 2 waveform are held, not simulated.
//! - The DAC port form of R0 bits 5-6, the direct-digital output used
//!   by the CD-ROM games, is recorded out of build.
//! - The DDA buffer consumes at the channel rate; the real die's
//!   overrun edge cases (a write during a DDA read, a buffer that
//!   underflows at the first sample) are simplified.
//! - The noise generator reproduces the published rate form and a
//!   recorded tap pair; the published material does not pin the tap
//!   positions.
//! - The DAC asymmetry at the five-bit ends (the real DAC tops out
//!   below the bottom of its range) is recorded out of build; the
//!   centering form of -16 makes the table entry 16 silent instead.
//!
//! Sources: the HuC6280 manual forms through the Geargrafx hardware
//! reference document for the PSG, the HuC6280 PSG chapter, and the
//! published 178977 per-second PSG sample form.

use demu_config::model::{BootState, DeviceInstance};
use demu_core::device::{AccessWidth, BusAccess, MachineCtx};
use demu_core::frequency::{Address, Tick};
use demu_core::signal::Level;
use demu_core::{AudioSource, Device, Snapshot, SnapshotError};

/// The internal sample rate of the PSG: the 7.15909 MHz CPU form
/// divided by the `psg` divider of 40.
const SAMPLE_RATE: u32 = 178_977;
/// The PSG divisor steps per internal sample:
/// 3579545 / 2 / 178977, the wave pointer constant.
const WAVE_STEPS_PER_SAMPLE: u32 = 10;
/// The noise generator divisor steps per internal sample.
const NOISE_STEPS_PER_SAMPLE: u32 = 5;
/// The waveform cycle length.
const WAVE_LEN: usize = 32;
/// The DDA buffer length.
const DDA_LEN: usize = 16;
/// The mixed sample buffer cap; the media pipeline drains earlier.
const SAMPLE_CAP: usize = 0x4000;
/// The sample push scale to keep six channels inside the 16-bit range.
const SAMPLE_SCALE: i32 = 24;

/// One PSG channel.
#[derive(Debug, Clone, Copy)]
struct PceChannel {
    /// The twelve-bit timer of the waveform pointer.
    freq: u16,
    /// The channel amplitude, bits 4-0 of R4.
    amplitude: u8,
    /// The balance byte R5: the left level in bits 7-4, the right in
    /// bits 3-0.
    balance: u8,
    /// The wave table, 32 five-bit entries.
    wave: [u8; WAVE_LEN],
    /// The wave pointer.
    ptr: u8,
    /// The channel plays (R4 bit 7).
    on: bool,
    /// The direct data access form (R4 bit 6).
    dda: bool,
    /// The DDA buffer with its push index and fill count.
    dda_buf: [u8; DDA_LEN],
    dda_head: u8,
    dda_count: u8,
    /// The last consumed DDA value.
    dda_last: u8,
    /// The noise form (R7 bit 7, channels 5 and 6).
    noise_on: bool,
    /// The noise frequency, bits 4-0 of R7.
    noise_freq: u8,
    /// The eighteen-bit noise register.
    noise_lfsr: u32,
    /// The wave pointer accumulator.
    wave_acc: u32,
    /// The noise shift accumulator.
    noise_acc: u32,
    /// The five-bit centered source value of the last step.
    step_value: i32,
}

impl PceChannel {
    fn new() -> Self {
        Self {
            freq: 0,
            amplitude: 0,
            balance: 0,
            wave: [0; WAVE_LEN],
            ptr: 0,
            on: false,
            dda: false,
            dda_buf: [0; DDA_LEN],
            dda_head: 0,
            dda_count: 0,
            dda_last: 0,
            noise_on: false,
            noise_freq: 0,
            noise_lfsr: 0x0002,
            wave_acc: 0,
            noise_acc: 0,
            step_value: -16,
        }
    }

    /// One wave pointer step: read the source value, then advance the
    /// pointer. The DDA form consumes one buffer entry per step and
    /// repeats the last consumed value when the buffer runs dry.
    fn step(&mut self) {
        let value = if self.noise_on {
            if self.noise_lfsr & 1 != 0 { 31 } else { 0 }
        } else if self.dda {
            if self.dda_count > 0 {
                let slot = usize::from(self.dda_head);
                let entry = self.dda_buf[slot];
                self.dda_buf[slot] = 0;
                self.dda_head = (self.dda_head + 1) & (DDA_LEN as u8 - 1);
                self.dda_count -= 1;
                self.dda_last = entry;
            }
            self.dda_last & 0x1F
        } else {
            self.wave[usize::from(self.ptr) & (WAVE_LEN - 1)]
        };
        self.step_value = i32::from(value) - 16;
        self.ptr = (self.ptr + 1) & (WAVE_LEN as u8 - 1);
    }

    /// One noise shift of the eighteen-bit register.
    fn noise_shift(&mut self) {
        // The recorded tap pair: bits 17 and 14 feed the input at the
        // low end.
        let bit = ((self.noise_lfsr >> 17) ^ (self.noise_lfsr >> 14)) & 1;
        self.noise_lfsr = (self.noise_lfsr << 1) | bit;
    }
}

/// The PC Engine PSG.
#[derive(Debug)]
pub struct PcePsg {
    name: String,
    /// The domain ticks per internal sample, from the `psg` divider.
    div: u64,
    /// The register select byte R0 (raw).
    select: u8,
    /// The channel the per-channel registers answer; the values 6 and
    /// 7 of R0 leave it.
    channel: usize,
    /// The global main amplitude byte R1.
    main_amp: u8,
    /// The held LFO frequency and control bytes.
    lfo_freq: u8,
    lfo_ctrl: u8,
    /// The six channels.
    channels: [PceChannel; 6],
    /// The mixed mono samples.
    samples: Vec<i16>,
    next: Option<Tick>,
    last_tick: Option<Tick>,
}

impl PcePsg {
    /// A device from its configuration entry.
    ///
    /// The `psg` divider names the domain ticks per internal sample
    /// and defaults to 40, the published 178977 per second form at the
    /// 7.15909 MHz CPU clock.
    pub fn new(device: &DeviceInstance) -> Self {
        let div = u64::from(device.dividers.get("psg").copied().unwrap_or(40)).max(1);
        Self {
            name: device.name.to_string(),
            div,
            select: 0,
            channel: 0,
            main_amp: 0,
            lfo_freq: 0,
            lfo_ctrl: 0,
            channels: [
                PceChannel::new(),
                PceChannel::new(),
                PceChannel::new(),
                PceChannel::new(),
                PceChannel::new(),
                PceChannel::new(),
            ],
            samples: Vec::new(),
            next: None,
            last_tick: None,
        }
    }

    /// The channel the per-channel registers answer.
    fn selected(&self) -> usize {
        self.channel
    }

    /// The per-write path of the data register.
    fn register_write(&mut self, register: usize, value: u8) {
        match register {
            R_SELECT => {
                self.select = value;
                // Only the channel values 0 to 5 move the answered
                // channel; 6 and 7 keep it.
                let select = usize::from(value & 0x07);
                if select < 6 {
                    self.channel = select;
                }
            }
            R_MAIN_AMP => self.main_amp = value,
            r @ R_FREQ_LO | r @ R_FREQ_HI => {
                let index = self.selected();
                let ch = &mut self.channels[index];
                if r == R_FREQ_LO {
                    ch.freq = (ch.freq & 0x0F00) | u16::from(value);
                } else {
                    ch.freq = (ch.freq & 0x00FF) | (u16::from(value & 0x0F) << 8);
                }
            }
            R_CHANNEL => {
                let index = self.selected();
                let ch = &mut self.channels[index];
                ch.on = value & 0x80 != 0;
                ch.dda = value & 0x40 != 0;
                if value & 0x20 != 0 {
                    // The wave pointer reset form re-points at zero.
                    ch.ptr = 0;
                    ch.wave_acc = 0;
                }
                ch.amplitude = value & 0x1F;
            }
            R_BALANCE => {
                self.channels[self.selected()].balance = value;
            }
            R_WAVE => {
                let index = self.selected();
                let ch = &mut self.channels[index];
                let value = value & 0x1F;
                if ch.dda {
                    // The DDA form pushes into the circular buffer; a
                    // full buffer drops the oldest entry.
                    if ch.dda_count == DDA_LEN as u8 {
                        ch.dda_head = (ch.dda_head + 1) & (DDA_LEN as u8 - 1);
                        ch.dda_count -= 1;
                    }
                    let slot =
                        (usize::from(ch.dda_head) + usize::from(ch.dda_count)) & (DDA_LEN - 1);
                    ch.dda_buf[slot] = value;
                    ch.dda_count += 1;
                } else {
                    // The table form stores and steps the pointer.
                    ch.wave[usize::from(ch.ptr) & (WAVE_LEN - 1)] = value;
                    ch.ptr = (ch.ptr + 1) & (WAVE_LEN as u8 - 1);
                }
            }
            R_NOISE => {
                let index = self.selected();
                // R7 answers only on the noise channels, 5 and 6.
                if index >= 4 {
                    let ch = &mut self.channels[index];
                    ch.noise_on = value & 0x80 != 0;
                    ch.noise_freq = value & 0x1F;
                }
            }
            R_LFO_FREQ => self.lfo_freq = value,
            R_LFO_CTRL => self.lfo_ctrl = value,
            _ => {}
        }
    }

    /// One internal sample step of every channel.
    fn step_channels(&mut self) {
        for ch in &mut self.channels {
            // The wave pointer steps 10/(F+1) times per internal
            // sample; the accumulator carries the fraction.
            let period = u32::from(ch.freq & 0x07FF) + 1;
            let mut acc = ch.wave_acc + WAVE_STEPS_PER_SAMPLE;
            while acc >= period {
                acc -= period;
                ch.step();
            }
            ch.wave_acc = acc;
            if ch.noise_on {
                let span = u32::from(ch.noise_freq) + 1;
                let mut acc = ch.noise_acc + NOISE_STEPS_PER_SAMPLE;
                while acc >= span {
                    acc -= span;
                    ch.noise_shift();
                }
                ch.noise_acc = acc;
            }
        }
    }

    /// The mixed mono sample of the current register state. The DDA
    /// consumption stays in the channel step; the mix reads the step
    /// value only.
    fn mix(&self) -> i32 {
        let main_left = i32::from(self.main_amp >> 4);
        let main_right = i32::from(self.main_amp & 0x0F);
        let mut left = 0i32;
        let mut right = 0i32;
        for ch in &self.channels {
            if !ch.on {
                continue;
            }
            let value = ch.step_value;
            let amplitude = i32::from(ch.amplitude);
            if amplitude == 0 {
                continue;
            }
            let bal_left = i32::from(ch.balance >> 4);
            let bal_right = i32::from(ch.balance & 0x0F);
            left += value * amplitude * bal_left * main_left;
            right += value * amplitude * bal_right * main_right;
        }
        (left + right) >> 1
    }

    /// The device visit: one internal sample.
    fn visit(&mut self) {
        self.step_channels();
        let mono = self.mix();
        if self.samples.len() >= SAMPLE_CAP {
            self.samples.clear();
        }
        self.samples.push(((mono >> 10) * SAMPLE_SCALE) as i16);
    }
}

/// The register indexes of the PSG.
const R_SELECT: usize = 0;
const R_MAIN_AMP: usize = 1;
const R_FREQ_LO: usize = 2;
const R_FREQ_HI: usize = 3;
const R_CHANNEL: usize = 4;
const R_BALANCE: usize = 5;
const R_WAVE: usize = 6;
const R_NOISE: usize = 7;
const R_LFO_FREQ: usize = 8;
const R_LFO_CTRL: usize = 9;

/// The bytes of one channel in the snapshot: the timer, the forms,
/// the wave table, the DDA buffer, the noise register, the running
/// accumulators, and the last step value.
const PSG_CHANNEL_LEN: usize = 71;
/// The PSG snapshot byte length: tag and version, the global
/// registers (the select byte and its answered channel), and the six
/// channels.
const PSG_SNAPSHOT_LEN: usize = 5 + 5 + 6 * PSG_CHANNEL_LEN;
impl BusAccess for PcePsg {
    fn read(&mut self, _width: AccessWidth, address: Address, _tick: Tick) -> u32 {
        let register = (address.0 & 0x0F) as usize;
        match register {
            R_SELECT => u32::from(self.select),
            R_MAIN_AMP => u32::from(self.main_amp),
            R_FREQ_LO => u32::from(self.channels[self.selected()].freq as u8),
            R_FREQ_HI => u32::from((self.channels[self.selected()].freq >> 8) & 0x0F),
            R_CHANNEL => {
                let ch = &self.channels[self.selected()];
                let mut value = 0u8;
                if ch.on {
                    value |= 0x80;
                }
                if ch.dda {
                    value |= 0x40;
                }
                u32::from(value | (ch.amplitude & 0x1F))
            }
            R_BALANCE => u32::from(self.channels[self.selected()].balance),
            R_WAVE => {
                let ch = &self.channels[self.selected()];
                u32::from(ch.wave[usize::from(ch.ptr) & (WAVE_LEN - 1)])
            }
            R_NOISE => {
                let ch = &self.channels[self.selected()];
                let mut value = 0u8;
                if ch.noise_on {
                    value |= 0x80;
                }
                u32::from(value | ch.noise_freq)
            }
            R_LFO_FREQ => u32::from(self.lfo_freq),
            R_LFO_CTRL => u32::from(self.lfo_ctrl),
            _ => 0x00,
        }
    }

    fn write(&mut self, _width: AccessWidth, address: Address, value: u32, _tick: Tick) {
        let register = (address.0 & 0x0F) as usize;
        if register > R_LFO_CTRL {
            return;
        }
        self.register_write(register, value as u8);
    }

    fn peek(&self, address: Address) -> u8 {
        let register = (address.0 & 0x0F) as usize;
        match register {
            R_SELECT => self.select,
            R_MAIN_AMP => self.main_amp,
            R_WAVE => {
                let ch = &self.channels[self.selected()];
                ch.wave[usize::from(ch.ptr) & (WAVE_LEN - 1)]
            }
            _ => 0,
        }
    }

    fn decodes(&self, _address: Address) -> bool {
        true
    }
}

impl Device for PcePsg {
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
        self.select = 0;
        self.channel = 0;
        self.main_amp = 0;
        self.lfo_freq = 0;
        self.lfo_ctrl = 0;
        self.channels = [
            PceChannel::new(),
            PceChannel::new(),
            PceChannel::new(),
            PceChannel::new(),
            PceChannel::new(),
            PceChannel::new(),
        ];
        self.samples.clear();
        self.next = None;
        self.last_tick = None;
    }

    fn boot(&mut self, _state: Option<&BootState>) {
        self.reset();
        self.next = Some(Tick(0));
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(PSG_SNAPSHOT_LEN);
        out.extend_from_slice(b"PPSG");
        out.push(1);
        out.push(self.select);
        // The answered channel rides the chunk: the R0 select values 6
        // and 7 keep the answered channel, so the restore cannot
        // recompute it from the select byte alone.
        out.push(self.channel as u8);
        out.push(self.main_amp);
        out.push(self.lfo_freq);
        out.push(self.lfo_ctrl);
        for ch in &self.channels {
            out.extend_from_slice(&ch.freq.to_le_bytes());
            out.push(ch.amplitude);
            out.push(ch.balance);
            out.extend_from_slice(&ch.wave);
            out.push(ch.ptr);
            let mut flags = 0u8;
            if ch.on {
                flags |= 0x01;
            }
            if ch.dda {
                flags |= 0x02;
            }
            if ch.noise_on {
                flags |= 0x04;
            }
            out.push(flags);
            out.push(ch.dda_head);
            out.push(ch.dda_count);
            out.push(ch.dda_last);
            out.extend_from_slice(&ch.dda_buf);
            out.push(ch.noise_freq);
            out.extend_from_slice(&ch.noise_lfsr.to_le_bytes());
            out.extend_from_slice(&ch.wave_acc.to_le_bytes());
            out.extend_from_slice(&ch.noise_acc.to_le_bytes());
            // The five-bit centered step value rides one byte above
            // center 16.
            out.push((ch.step_value + 16) as u8);
        }
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        if data.len() != PSG_SNAPSHOT_LEN || &data[..4] != b"PPSG" || data[4] != 1 {
            return Err(SnapshotError::Mismatch(format!(
                "pce PSG chunk must be {PSG_SNAPSHOT_LEN} bytes"
            )));
        }
        let mut pos = 5usize;
        self.select = data[pos];
        pos += 1;
        self.channel = usize::from(data[pos].min(5));
        pos += 1;
        self.main_amp = data[pos];
        pos += 1;
        self.lfo_freq = data[pos];
        pos += 1;
        self.lfo_ctrl = data[pos];
        pos += 1;
        for ch in self.channels.iter_mut() {
            ch.freq = u16::from_le_bytes(data[pos..pos + 2].try_into().expect("2 bytes"));
            pos += 2;
            ch.amplitude = data[pos];
            pos += 1;
            ch.balance = data[pos];
            pos += 1;
            ch.wave.copy_from_slice(&data[pos..pos + WAVE_LEN]);
            pos += WAVE_LEN;
            ch.ptr = data[pos] & (WAVE_LEN as u8 - 1);
            pos += 1;
            let flags = data[pos];
            pos += 1;
            ch.on = flags & 0x01 != 0;
            ch.dda = flags & 0x02 != 0;
            ch.noise_on = flags & 0x04 != 0;
            ch.dda_head = data[pos] & (DDA_LEN as u8 - 1);
            pos += 1;
            ch.dda_count = data[pos].min(DDA_LEN as u8);
            pos += 1;
            ch.dda_last = data[pos];
            pos += 1;
            ch.dda_buf.copy_from_slice(&data[pos..pos + DDA_LEN]);
            pos += DDA_LEN;
            ch.noise_freq = data[pos];
            pos += 1;
            ch.noise_lfsr = u32::from_le_bytes(data[pos..pos + 4].try_into().expect("4 bytes"));
            pos += 4;
            ch.wave_acc = u32::from_le_bytes(data[pos..pos + 4].try_into().expect("4 bytes"));
            pos += 4;
            ch.noise_acc = u32::from_le_bytes(data[pos..pos + 4].try_into().expect("4 bytes"));
            pos += 4;
            ch.step_value = i32::from(data[pos]) - 16;
            pos += 1;
        }
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
        self.visit();
        self.next = Some(Tick(tick.0 + self.div));
    }
}

impl AudioSource for PcePsg {
    fn sample_rate(&self) -> u32 {
        SAMPLE_RATE
    }

    fn take_samples(&mut self) -> Vec<i16> {
        std::mem::take(&mut self.samples)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instance() -> DeviceInstance {
        DeviceInstance {
            name: demu_config::newtypes::DeviceName::new("psg").expect("name"),
            kind: demu_config::newtypes::KindName::new("pce.psg").expect("kind"),
            domain: "main".to_string(),
            params: std::collections::BTreeMap::new(),
            dividers: std::collections::BTreeMap::new(),
        }
    }

    fn psg() -> PcePsg {
        PcePsg::new(&instance())
    }

    #[test]
    fn register_select_follows_r0() {
        let mut p = psg();
        p.write(AccessWidth::Byte, Address(0x0800), 3, Tick(0));
        p.write(AccessWidth::Byte, Address(0x0802), 0x55, Tick(0));
        assert_eq!(p.channels[3].freq, 0x0055);
        // The out-of-range select of 6 keeps the answered channel: the
        // R2 write still reaches channel 3, the last valid select.
        p.write(AccessWidth::Byte, Address(0x0800), 6, Tick(0));
        p.write(AccessWidth::Byte, Address(0x0802), 0x77, Tick(0));
        assert_eq!(p.channels[3].freq, 0x0077);
        assert_eq!(p.read(AccessWidth::Byte, Address(0x0802), Tick(0)), 0x77);
    }

    #[test]
    fn wave_writes_store_and_step_pointer() {
        let mut p = psg();
        p.write(AccessWidth::Byte, Address(0x0800), 1, Tick(0));
        for i in 0..40u8 {
            p.write(
                AccessWidth::Byte,
                Address(0x0806),
                u32::from(i % 32),
                Tick(0),
            );
        }
        // The pointer wrapped after 32 writes and the first eight
        // entries re-store 0.
        assert_eq!(p.channels[1].ptr, 0x08);
        assert_eq!(p.channels[1].wave[0], 0);
        // The reset form re-points the waveform pointer.
        p.write(AccessWidth::Byte, Address(0x0804), 0x20, Tick(0));
        assert_eq!(p.channels[1].ptr, 0);
    }

    #[test]
    fn dda_form_pushes_into_buffer() {
        let mut p = psg();
        p.write(AccessWidth::Byte, Address(0x0800), 2, Tick(0));
        p.write(AccessWidth::Byte, Address(0x0804), 0x40, Tick(0)); // DDA
        for i in 0..20u8 {
            p.write(AccessWidth::Byte, Address(0x0806), u32::from(i), Tick(0));
        }
        // The buffer holds the last sixteen entries; the push dropped
        // the first four.
        let ch = &p.channels[2];
        assert_eq!(ch.dda_count, 16);
        assert_eq!(ch.dda_head, 4);
        assert_eq!(ch.wave[0], 0);
    }

    #[test]
    fn wave_channel_sounds_and_is_deterministic() {
        let run = || -> Vec<i16> {
            let mut p = psg();
            p.write(AccessWidth::Byte, Address(0x0800), 0, Tick(0));
            // The channel on with full amplitude and a triangle form
            // that rises 0..27: the timer 0xFF = one step per sample
            // pair.
            p.write(AccessWidth::Byte, Address(0x0804), 0x9F, Tick(0));
            p.write(AccessWidth::Byte, Address(0x0803), 0x00, Tick(0));
            p.write(AccessWidth::Byte, Address(0x0802), 0xFF, Tick(0));
            // The balance and the main amplitude wide open.
            p.write(AccessWidth::Byte, Address(0x0805), 0xFF, Tick(0));
            p.write(AccessWidth::Byte, Address(0x0801), 0xFF, Tick(0));
            for i in 0..32u8 {
                p.write(AccessWidth::Byte, Address(0x0806), u32::from(i), Tick(0));
            }
            for _ in 0..2000 {
                p.visit();
                p.next = Some(Tick(1));
            }
            p.take_samples()
        };
        let first = run();
        assert!(!first.is_empty());
        assert!(first.iter().any(|s| *s != 0));
        assert_eq!(first, run());
    }

    #[test]
    fn balance_levels_silence_channels() {
        let mut p = psg();
        p.write(AccessWidth::Byte, Address(0x0800), 0, Tick(0));
        p.write(AccessWidth::Byte, Address(0x0804), 0x9F, Tick(0));
        for _i in 0..32u8 {
            p.write(AccessWidth::Byte, Address(0x0806), 20, Tick(0));
        }
        // No balance and no main amplitude: the mix rides zero.
        for _ in 0..600 {
            p.visit();
        }
        assert!(p.take_samples().iter().all(|s| *s == 0));
    }

    #[test]
    fn noise_form_makes_bits() {
        let mut p = psg();
        p.write(AccessWidth::Byte, Address(0x0800), 4, Tick(0));
        p.write(AccessWidth::Byte, Address(0x0804), 0x9F, Tick(0));
        p.write(AccessWidth::Byte, Address(0x0807), 0x80 | 0x01, Tick(0));
        p.write(AccessWidth::Byte, Address(0x0805), 0xFF, Tick(0));
        p.write(AccessWidth::Byte, Address(0x0801), 0xFF, Tick(0));
        // The R7 form on a non-noise channel stays silent-only.
        p.write(AccessWidth::Byte, Address(0x0800), 1, Tick(0));
        p.write(AccessWidth::Byte, Address(0x0807), 0x80, Tick(0));
        let ch = &p.channels[1];
        assert!(!ch.noise_on);
        for _ in 0..600 {
            p.visit();
        }
        assert!(p.take_samples().iter().any(|s| *s != 0));
    }

    #[test]
    fn snapshot_round_trips() {
        let mut p = psg();
        p.write(AccessWidth::Byte, Address(0x0800), 2, Tick(0));
        p.write(AccessWidth::Byte, Address(0x0802), 0x34, Tick(0));
        p.write(AccessWidth::Byte, Address(0x0803), 0x01, Tick(0));
        // The table form: a full cycle of entries, each offset above
        // the center, so a stepped channel carries a distinct value
        // form.
        for i in 0..32u8 {
            p.write(
                AccessWidth::Byte,
                Address(0x0806),
                u32::from(i) + 5,
                Tick(0),
            );
        }
        p.write(AccessWidth::Byte, Address(0x0804), 0x80 | 0x1F, Tick(0));
        p.write(AccessWidth::Byte, Address(0x0801), 0xFF, Tick(0));
        // Run enough steps that the pointer of the preset frequency
        // steps at least once: the answered channel then carries a
        // real step-value form, not the reset value.
        p.write(AccessWidth::Byte, Address(0x0800), 0x02, Tick(0));
        for _ in 0..32 {
            p.visit();
        }
        // The select of 6 keeps the answered channel: the register
        // writes and reads keep reaching channel 2 across a restore.
        p.write(AccessWidth::Byte, Address(0x0800), 6, Tick(0));
        let snap = p.snapshot();
        assert_eq!(snap.len(), PSG_SNAPSHOT_LEN);
        let mut twin = psg();
        twin.restore(&snap).expect("restore");
        assert_eq!(twin.snapshot(), snap);
        assert_eq!(twin.channels[2].freq, 0x0134);
        assert_eq!(twin.main_amp, 0xFF);
        // The answered channel survived: the R6 write through the kept
        // select reaches channel 2 on the twin as on the source.
        let step_before = twin.channels[2].step_value;
        twin.write(AccessWidth::Byte, Address(0x0802), 0x77, Tick(0));
        assert_eq!(
            twin.channels[2].freq, 0x0177,
            "the kept select answered channel 2 after the restore"
        );
        assert_eq!(twin.channels[2].step_value, step_before);
        // The stepped form rode the chunk.
        assert_ne!(
            twin.channels[2].step_value, -16,
            "the stepped value restored"
        );
    }

    #[test]
    fn restore_rejects_wrong_lengths() {
        let mut p = psg();
        let snap = p.snapshot();
        assert!(matches!(
            p.restore(&snap[..snap.len() - 1]),
            Err(SnapshotError::Mismatch(_))
        ));
    }
}
