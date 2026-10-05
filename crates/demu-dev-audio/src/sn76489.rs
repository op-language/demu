//! The Texas Instruments SN76489 programmable sound generator of the
//! Sega machine families, the `sn76489.psg` device kind.
//!
//! The chip answers 8-bit write-only protocol bytes and holds eight
//! registers: three ten-bit tone periods, four four-bit attenuations,
//! and the three-bit noise control. A byte with bit 7 set latches a
//! channel and register form and stores its data bits; a byte with
//! bit 7 clear stores into the latched register again. The latched
//! register survives across writes and a data byte never clears it.
//!
//! # Pinned register surface
//!
//! The latch byte carries the published form `%1cctdddd`: bits 6-5
//! select the channel A to D, bit 4 selects the form (0 the period
//! register of A to C and the noise control of D, 1 the volume), and
//! the low four bits carry the data. The register index is then the
//! channel doubled for the period form and raised for the volume
//! form:
//!
//! | Index | Register | Content |
//! |-------|----------|---------|
//! | 0 2 4 | Tone period A, B, C | the ten-bit half-wave reset |
//! | 1 3 5 7 | Attenuation A, B, C, noise | 0 full level, 15 silence |
//! | 6 | Noise control | bit 1-0 shift rate, bit 2 white select |
//!
//! A latch write stores its four data bits as the low register bits;
//! the noise control discards the highest bit. A data byte stores:
//! the low six bits enter the high end of a latched tone period, the
//! low four bits update a latched attenuation, and the low three bits
//! update the noise control. Every store applies at once; the tone
//! period updates immediately without waiting for both writes.
//!
//! # The tone channels
//!
//! Each tone channel runs a countdown counter. One internal clock
//! decrements the counter; at zero the counter reloads from the
//! period register and the channel's output bit flips. The half-wave
//! span equals the period, so the square wave runs at
//! frequency = clock / (2 x period), the published form. A period of
//! 0 or 1 holds the channel level on instead of toggling; the
//! published constant-level form that the sample-playback DC offset
//! uses. This build reloads the counter when a period write lands.
//!
//! # The noise channel
//!
//! The noise counter follows the same countdown form. Its reload
//! value the control's shift-rate bits select:
//!
//! | Control bits 1-0 | Counter reload |
//! |------------------|----------------|
//! | 00 | 0x10 |
//! | 01 | 0x20 |
//! | 10 | 0x40 |
//! | 11 | the tone C period |
//!
//! The counter's flip is the drive of a sixteen-bit linear feedback
//! shift register, which shifts once per drive rise. A noise control
//! write resets the register to its highest bit. The white form feeds
//! the parity of tap bits 0 and 3 back in; the "periodic" form feeds
//! the output bit back, which walks the register over sixteen shifts
//! and holds the output on for one pass. A tone C period of zero
//! parks the counter and holds the drive. The discrete SG-1000 chip
//! (the `SN76489AN`) uses a fifteen-bit register with the tap pair
//! `$0003`; this build answers only the sixteen-bit Sega VDP form.
//!
//! # The attenuators and the mix
//!
//! Each channel level passes the channel attenuation: 0 is the full
//! level and 15 silence. The four channels mix into one mono sample
//! and the mix saturates into the 16-bit form. The attenuation steps
//! of this build are linear; the real ladder drops two decibels per
//! step and is outside this build.
//!
//! The chip divides its input clock by 16 into the internal sample
//! clock. At the published 3579545 Hz Sega clock the internal clock
//! runs at 223721 steps a second, and one device visit is one
//! internal step. The `psg` divider names the domain ticks per step
//! and defaults to 48, the form at the 10738635 Hz SG-1000 master.
//!
//! The device snapshots under the `SNPS` tag, version 1: the
//! registers, the latched index, the channel counters and output
//! bits, the shift register, and the noise counter. The samples do
//! not ride the chunk.
//!
//! Sources: the SMS Power! SN76489 development document for the
//! register writes, the noise forms, and the tap pairs, and the
//! Texas Instruments SN76489 datasheet for the attenuation form.

use demu_config::model::{BootState, DeviceInstance};
use demu_core::device::{AccessWidth, BusAccess, MachineCtx};
use demu_core::frequency::{Address, Tick};
use demu_core::signal::Level;
use demu_core::{AudioSource, Device, Snapshot, SnapshotError};

/// The internal sample rate: the published 3579545 Hz Sega PSG clock
/// divided by 16.
const SAMPLE_RATE: u32 = 223_721;
/// The mixed sample buffer cap; the media pipeline drains earlier.
const SAMPLE_CAP: usize = 0x4000;
/// The per-channel amplitude at attenuation 0.
const AMPLITUDE: i32 = 8000;
/// The attenuation steps between the full level and silence.
const ATTENUATION_STEPS: i32 = 15;
/// The periods below this hold one constant level; the published 0
/// and 1 form that the sample-playback DC offset uses.
const CONSTANT_PERIOD: u16 = 2;
/// The shift register resets to its highest bit on a control write.
const LFSR_RESET: u16 = 0x8000;

/// The SN76489 programmable sound generator.
#[derive(Debug)]
pub struct Sn76489 {
    name: String,
    /// The domain ticks per internal PSG clock, from the `psg`
    /// divider.
    div: u64,
    /// The latched register index: the channel doubled for the period
    /// form and raised for the volume form.
    latched: u8,
    /// The ten-bit period registers of the three tone channels.
    tone_period: [u16; 3],
    /// The four-bit attenuations, channel order A, B, C, noise: 0 the
    /// full level, 15 silence.
    attenuation: [u8; 4],
    /// The three-bit noise control: bits 1-0 the shift rate, bit 2
    /// the white-noise feedback select.
    noise_control: u8,
    /// The half-wave counters of the tone channels.
    tone_counter: [u16; 3],
    /// The tone output bits.
    tone_bit: [bool; 3],
    /// The noise half-wave counter.
    noise_counter: u16,
    /// The noise drive bit, the counter flip that the shift register
    /// follows; the register shifts as the drive rises.
    noise_drive: bool,
    /// The sixteen-bit noise shift register.
    lfsr: u16,
    /// The mixed mono samples.
    samples: Vec<i16>,
    next: Option<Tick>,
    last_tick: Option<Tick>,
}

impl Sn76489 {
    /// A device from its configuration entry.
    ///
    /// The `psg` divider names the domain ticks per internal PSG
    /// clock and defaults to 48, the published 223721 per second
    /// internal clock at the 10738635 Hz SG-1000 master.
    pub fn new(device: &DeviceInstance) -> Self {
        let div = u64::from(device.dividers.get("psg").copied().unwrap_or(48)).max(1);
        Self {
            name: device.name.to_string(),
            div,
            latched: 0,
            tone_period: [0; 3],
            attenuation: [15; 4],
            noise_control: 0,
            tone_counter: [0; 3],
            tone_bit: [false; 3],
            noise_counter: 0x10,
            noise_drive: false,
            lfsr: LFSR_RESET,
            samples: Vec::new(),
            next: None,
            last_tick: None,
        }
    }

    /// One protocol byte. A latch byte latches the channel and
    /// register form and stores its four data bits; a data byte
    /// stores into the latched register again.
    fn write_byte(&mut self, b: u8) {
        if b & 0x80 != 0 {
            let channel = (b >> 5) & 0x03;
            self.latched = (channel << 1) | ((b >> 4) & 0x01);
            self.store(b & 0x0F);
        } else {
            self.store_6(b & 0x3F);
        }
    }

    /// Store the four latch-byte data bits into the latched register.
    fn store(&mut self, data: u8) {
        let register = usize::from(self.latched);
        if register == 6 {
            // The noise control takes only three bits; the highest is
            // discarded. The published control write resets the shift
            // register, and this build reloads the counter.
            self.noise_control = data & 0x07;
            self.reset_noise();
            return;
        }
        if register.is_multiple_of(2) {
            let channel = register / 2;
            self.tone_period[channel] =
                (self.tone_period[channel] & 0x3F0) | u16::from(data & 0x0F);
            let period = self.tone_period[channel];
            self.tone_counter[channel] = period;
        } else {
            self.attenuation[register / 2] = data & 0x0F;
        }
    }

    /// Store the six data-byte bits into the latched register.
    fn store_6(&mut self, field: u8) {
        let register = usize::from(self.latched);
        if register == 6 {
            // The data byte updates the noise control the same way,
            // through its low three bits.
            self.noise_control = field & 0x07;
            self.reset_noise();
            return;
        }
        if register.is_multiple_of(2) {
            // The six field bits enter the high end of the tone
            // period and the update applies at once.
            let channel = register / 2;
            self.tone_period[channel] =
                (self.tone_period[channel] & 0x00F) | (u16::from(field) << 4);
            let period = self.tone_period[channel];
            self.tone_counter[channel] = period;
        } else {
            // The narrow volume form takes the low four bits.
            self.attenuation[register / 2] = field & 0x0F;
        }
    }

    /// The noise counter reload value: the published rate table, with
    /// rate three driving the counter from the tone C period.
    fn noise_reset_of(&self) -> u16 {
        match self.noise_control & 0x03 {
            0 => 0x10,
            1 => 0x20,
            2 => 0x40,
            _ => self.tone_period[2],
        }
    }

    /// The published form of a noise control write: the shift
    /// register resets to its highest bit, and this build reloads the
    /// counter from the new rate table entry.
    fn reset_noise(&mut self) {
        self.noise_counter = self.noise_reset_of();
        self.lfsr = LFSR_RESET;
    }

    /// One internal clock of one tone channel. A period below 2
    /// holds the published constant level; the counter counts down,
    /// flips the output bit at zero, and reloads.
    fn step_tone(&mut self, i: usize) {
        let period = self.tone_period[i];
        if period < CONSTANT_PERIOD {
            return;
        }
        self.tone_counter[i] -= 1;
        if self.tone_counter[i] == 0 {
            self.tone_counter[i] = period;
            self.tone_bit[i] = !self.tone_bit[i];
        }
    }

    /// One internal clock of the noise channel. The counter flips
    /// the drive bit; the drive rise shifts the register, the
    /// published once-per-two-toggle rule. A zero reload source parks
    /// the counter and holds the drive.
    fn step_noise(&mut self) {
        if self.noise_counter == 0 {
            return;
        }
        self.noise_counter -= 1;
        if self.noise_counter == 0 {
            let drove = self.noise_drive;
            self.noise_drive = !drove;
            self.noise_counter = self.noise_reset_of();
            if !drove {
                self.shift_lfsr();
            }
        }
    }

    /// One shift of the noise register. The white form feeds the
    /// parity of the tap pair of bits 0 and 3 back in; the
    /// "periodic" form feeds the output bit back, which repeats the
    /// register over sixteen shifts.
    fn shift_lfsr(&mut self) {
        let feedback = if self.noise_control & 0x04 != 0 {
            (self.lfsr ^ (self.lfsr >> 3)) & 1
        } else {
            self.lfsr & 1
        };
        self.lfsr = (self.lfsr >> 1) | (feedback << 15);
    }

    /// The output level of one tone channel in the mix.
    fn tone_level(&self, i: usize) -> i32 {
        let high = if self.tone_period[i] < CONSTANT_PERIOD {
            true
        } else {
            self.tone_bit[i]
        };
        i32::from(u8::from(high)) * Self::amplitude_of(self.attenuation[i])
    }

    /// The amplitude of one attenuation step. The four channels mix
    /// with a linear sixteen-step attenuation; the chip's per-step
    /// two-decibel ladder is not modeled.
    fn amplitude_of(attenuation: u8) -> i32 {
        let level = ATTENUATION_STEPS - i32::from(attenuation & 0x0F);
        AMPLITUDE * level / ATTENUATION_STEPS
    }

    /// The mixed mono sample of the current channel state.
    fn mix(&self) -> i32 {
        let mut sum = self.tone_level(0) + self.tone_level(1) + self.tone_level(2);
        sum += i32::from(u8::from(self.lfsr & 1 == 1)) * Self::amplitude_of(self.attenuation[3]);
        sum.clamp(i16::MIN as i32, i16::MAX as i32)
    }

    /// The device visit: one internal PSG clock.
    fn visit(&mut self) {
        for i in 0..3 {
            self.step_tone(i);
        }
        self.step_noise();
        let mono = self.mix();
        if self.samples.len() >= SAMPLE_CAP {
            self.samples.clear();
        }
        self.samples.push(mono as i16);
    }
}

/// The PSG snapshot byte length: tag and version, the three tone
/// periods, the four attenuations, the control and latch bytes, the
/// three tone counters, the packed output bits, and the noise counter
/// and shift register.
const PSG_SNAPSHOT_LEN: usize = 5 + 6 + 4 + 1 + 1 + 6 + 1 + 2 + 2;

impl BusAccess for Sn76489 {
    fn read(&mut self, _width: AccessWidth, _address: Address, _tick: Tick) -> u32 {
        // The chip is write-only; the window reads answer the
        // open-bus form.
        0xFF
    }

    fn write(&mut self, _width: AccessWidth, _address: Address, value: u32, _tick: Tick) {
        // Every write in the window carries one protocol byte; the
        // host address selects the window only (the SG-1000 maps
        // $40-$7F, the SMS $7E-$7F).
        self.write_byte(value as u8);
    }

    fn peek(&self, _address: Address) -> u8 {
        0xFF
    }

    fn decodes(&self, _address: Address) -> bool {
        true
    }
}

impl Device for Sn76489 {
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
        // The published power-up form of the Sega integrated chip:
        // the period and control registers hold zero and the volume
        // registers hold silence.
        self.latched = 0;
        self.tone_period = [0; 3];
        self.attenuation = [15; 4];
        self.noise_control = 0;
        self.tone_counter = [0; 3];
        self.tone_bit = [false; 3];
        self.noise_counter = 0x10;
        self.noise_drive = false;
        self.lfsr = LFSR_RESET;
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
        out.extend_from_slice(b"SNPS");
        out.push(1);
        for period in self.tone_period {
            out.extend_from_slice(&period.to_le_bytes());
        }
        out.extend_from_slice(&self.attenuation);
        out.push(self.noise_control);
        out.push(self.latched);
        for counter in self.tone_counter {
            out.extend_from_slice(&counter.to_le_bytes());
        }
        let mut bits = u8::from(self.noise_drive) << 3;
        for (i, bit) in self.tone_bit.iter().enumerate() {
            bits |= u8::from(*bit) << i;
        }
        out.push(bits);
        out.extend_from_slice(&self.noise_counter.to_le_bytes());
        out.extend_from_slice(&self.lfsr.to_le_bytes());
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        if data.len() != PSG_SNAPSHOT_LEN || &data[..4] != b"SNPS" || data[4] != 1 {
            return Err(SnapshotError::Mismatch(format!(
                "sn76489 chunk must be {PSG_SNAPSHOT_LEN} bytes"
            )));
        }
        let mut pos = 5usize;
        for period in self.tone_period.iter_mut() {
            *period = u16::from_le_bytes(data[pos..pos + 2].try_into().expect("2 bytes"));
            pos += 2;
        }
        self.attenuation.copy_from_slice(&data[pos..pos + 4]);
        pos += 4;
        self.noise_control = data[pos] & 0x07;
        pos += 1;
        self.latched = data[pos] & 0x07;
        pos += 1;
        for counter in self.tone_counter.iter_mut() {
            *counter = u16::from_le_bytes(data[pos..pos + 2].try_into().expect("2 bytes"));
            pos += 2;
        }
        let bits = data[pos];
        pos += 1;
        for (i, bit) in self.tone_bit.iter_mut().enumerate() {
            *bit = bits & (1 << i) != 0;
        }
        self.noise_drive = bits & 0x08 != 0;
        self.noise_counter = u16::from_le_bytes(data[pos..pos + 2].try_into().expect("2 bytes"));
        pos += 2;
        self.lfsr = u16::from_le_bytes(data[pos..pos + 2].try_into().expect("2 bytes"));
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

impl AudioSource for Sn76489 {
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
    use demu_config::model::{Bus as CBus, ClockDomain, Window as CWindow};
    use demu_core::device::{BusBank, DeviceSlices, EventBuffers};
    use demu_core::machine::Machine;
    use std::collections::BTreeMap;

    fn instance() -> DeviceInstance {
        DeviceInstance {
            name: demu_config::newtypes::DeviceName::new("psg0").expect("name"),
            kind: demu_config::newtypes::KindName::new("sn76489.psg").expect("kind"),
            domain: "main".to_string(),
            params: BTreeMap::new(),
            dividers: BTreeMap::new(),
        }
    }

    fn psg() -> Sn76489 {
        Sn76489::new(&instance())
    }

    /// One protocol byte through the bus write; the window address
    /// carries no register meaning.
    fn wr(s: &mut Sn76489, b: u8) {
        s.write(AccessWidth::Byte, Address(0x7F), u32::from(b), Tick(0));
    }

    /// Run one device visit under an empty context: the PSG visit
    /// carries no notification, so the context needs no devices.
    fn tick(s: &mut Sn76489, tick: Tick) {
        let mut before: Vec<Box<dyn Device>> = Vec::new();
        let mut after: Vec<Box<dyn Device>> = Vec::new();
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
        s.tick(tick, &mut ctx);
    }

    #[test]
    fn latch_bytes_route_channel_and_form() {
        let mut s = psg();
        // %1 00 1 1111: channel A volume 15, silence.
        wr(&mut s, 0x9F);
        assert_eq!(s.latched, 1);
        assert_eq!(s.attenuation[0], 15);
        // %1 01 1 0000: channel B volume 0, the full level.
        wr(&mut s, 0xB0);
        assert_eq!(s.latched, 3);
        assert_eq!(s.attenuation[1], 0);
        // %1 10 0 0110: channel C tone period, low bits 6.
        wr(&mut s, 0xC6);
        assert_eq!(s.latched, 4);
        assert_eq!(s.tone_period[2], 0x006);
        // %1 11 0 0101: the published noise form, white at rate 1.
        wr(&mut s, 0xE5);
        assert_eq!(s.latched, 6);
        assert_eq!(s.noise_control, 0b101);
    }

    #[test]
    fn data_bytes_assemble_tone_periods() {
        let mut s = psg();
        // The published 440 Hz walk: latch low 1110, then the data
        // byte 001111 into the high end.
        wr(&mut s, 0x8E);
        assert_eq!(s.tone_period[0], 0x00E);
        wr(&mut s, 0x0F);
        assert_eq!(s.tone_period[0], 0x0FE);
        // A data byte replaces the high end and keeps the low.
        wr(&mut s, 0x1D);
        assert_eq!(s.tone_period[0], 0x1DE);
        wr(&mut s, 0x8A);
        assert_eq!(s.tone_period[0], 0x1DA);
        // The published immediate-update walk.
        wr(&mut s, 0x80);
        assert_eq!(s.tone_period[0], 0x1D0);
        wr(&mut s, 0x00);
        assert_eq!(s.tone_period[0], 0x000);
        wr(&mut s, 0x8F);
        assert_eq!(s.tone_period[0], 0x00F);
        wr(&mut s, 0x3F);
        assert_eq!(s.tone_period[0], 0x3FF);
    }

    #[test]
    fn data_bytes_update_volume_and_noise() {
        let mut s = psg();
        // The published Alex Kidd form: the data byte after the
        // volume latch is not ignored.
        wr(&mut s, 0xDF);
        assert_eq!(s.attenuation[2], 15);
        wr(&mut s, 0x00);
        assert_eq!(s.attenuation[2], 0);
        // The published Micro Machines form: the data byte updates
        // the noise rate and mode.
        wr(&mut s, 0xE5);
        wr(&mut s, 0x04);
        assert_eq!(s.noise_control, 0b100);
        // The narrow volume form takes the low four bits.
        wr(&mut s, 0x9F);
        wr(&mut s, 0x5A);
        assert_eq!(s.attenuation[0], 0x0A);
    }

    #[test]
    fn attenuation_steps_scale_the_mix() {
        // Channel A wide open at period 2: the output flips every two
        // internal clocks.
        let mut s = psg();
        wr(&mut s, 0x82);
        wr(&mut s, 0x90);
        for _ in 0..40 {
            s.visit();
        }
        let full = s.take_samples();
        assert!(full.iter().all(|v| *v == 8000 || *v == 0));
        // The attenuation of 7 rides 8/15 of the level.
        let mut mid_s = psg();
        wr(&mut mid_s, 0x82);
        wr(&mut mid_s, 0x97);
        for _ in 0..40 {
            mid_s.visit();
        }
        let mid = mid_s.take_samples();
        assert!(mid.iter().all(|v| *v == 4266 || *v == 0));
        // The attenuation of 15 mutes.
        let mut muted = psg();
        wr(&mut muted, 0x82);
        for _ in 0..40 {
            muted.visit();
        }
        assert!(muted.take_samples().iter().all(|v| *v == 0));
    }

    #[test]
    fn small_periods_hold_the_constant_level() {
        let mut s = psg();
        wr(&mut s, 0x90);
        // The published period 0 and 1 form holds the level on; no
        // half-wave runs.
        wr(&mut s, 0x81);
        for _ in 0..40 {
            s.visit();
        }
        assert!(s.take_samples().iter().all(|v| *v == 8000));
        wr(&mut s, 0x80);
        for _ in 0..40 {
            s.visit();
        }
        assert!(s.take_samples().iter().all(|v| *v == 8000));
    }

    #[test]
    fn noise_drive_follows_the_rate_table() {
        // The control write resets the register to its highest bit.
        let mut s = psg();
        wr(&mut s, 0xE0);
        assert_eq!(s.lfsr, LFSR_RESET);
        // Rate 0 parks the counter reload at 0x10: the drive flips on
        // the sixteenth internal clock.
        for _ in 0..15 {
            s.visit();
        }
        assert!(!s.noise_drive);
        s.visit();
        assert!(s.noise_drive);
        assert_ne!(s.lfsr, LFSR_RESET, "the drive rise shifted");
        for _ in 0..15 {
            s.visit();
        }
        assert!(s.noise_drive);
        s.visit();
        assert!(!s.noise_drive, "the drive fall shifted nothing");
    }

    #[test]
    fn noise_rate_codes_follow_the_published_table() {
        for (code, reset) in [(0u8, 0x10u16), (1, 0x20), (2, 0x40)] {
            let mut s = psg();
            wr(&mut s, 0xE0 | code);
            assert_eq!(s.noise_reset_of(), reset);
        }
        // Rate 3 drives the counter with the tone C period.
        let mut s = psg();
        wr(&mut s, 0xC7);
        wr(&mut s, 0x03);
        assert_eq!(s.tone_period[2], 0x037);
        wr(&mut s, 0xE3);
        assert_eq!(s.noise_reset_of(), 0x037);
        // The drive flips one period later.
        for _ in 0..0x37 {
            s.visit();
        }
        assert!(s.noise_drive);
    }

    #[test]
    fn white_and_periodic_forms_diverge() {
        let run = |control: u8| -> Vec<i16> {
            let mut s = psg();
            wr(&mut s, control);
            wr(&mut s, 0xF0);
            for _ in 0..1200 {
                s.visit();
            }
            s.take_samples()
        };
        let white = run(0xE4);
        let periodic = run(0xE0);
        assert!(white.iter().any(|v| *v != 0));
        assert!(periodic.iter().any(|v| *v != 0));
        assert_ne!(white, periodic, "the feedback forms differ");
    }

    #[test]
    fn sample_buffer_drains_and_stays_capped() {
        let mut s = psg();
        wr(&mut s, 0x82);
        wr(&mut s, 0x90);
        for _ in 0..(SAMPLE_CAP * 2) {
            s.visit();
        }
        assert!(s.samples.len() <= SAMPLE_CAP);
        let first = s.take_samples();
        assert!(first.len() <= SAMPLE_CAP);
        for _ in 0..10 {
            s.visit();
        }
        assert_eq!(s.take_samples().len(), 10);
        assert!(s.take_samples().is_empty());
    }

    #[test]
    fn tick_guards_and_rearms_by_the_divider() {
        let mut s = psg();
        s.boot(None);
        assert_eq!(s.next, Some(Tick(0)));
        wr(&mut s, 0x82);
        wr(&mut s, 0x90);
        tick(&mut s, Tick(0));
        assert_eq!(s.next, Some(Tick(48)));
        assert_eq!(s.samples.len(), 1);
        // The stale-visit guard drops the second visit at the tick.
        tick(&mut s, Tick(0));
        assert_eq!(s.samples.len(), 1);
        tick(&mut s, Tick(48));
        assert_eq!(s.next, Some(Tick(96)));
        assert_eq!(s.samples.len(), 2);
    }

    #[test]
    fn reset_mutes_and_powers_channels() {
        let mut s = psg();
        wr(&mut s, 0x8A);
        wr(&mut s, 0x90);
        wr(&mut s, 0xE4);
        s.reset();
        assert_eq!(s.attenuation, [15; 4]);
        assert_eq!(s.tone_period, [0; 3]);
        assert_eq!(s.noise_control, 0);
        assert_eq!(s.lfsr, LFSR_RESET);
        s.boot(None);
        assert_eq!(s.next, Some(Tick(0)));
        for _ in 0..40 {
            s.visit();
        }
        assert!(s.take_samples().iter().all(|v| *v == 0));
    }

    #[test]
    fn the_write_only_window_answers_open_bus_reads() {
        let mut s = psg();
        wr(&mut s, 0x82);
        // The chip is write-only: the window read and the peek return
        // the open-bus form at both window ends.
        assert_eq!(s.read(AccessWidth::Byte, Address(0x40), Tick(0)), 0xFF);
        assert_eq!(s.peek(Address(0x7F)), 0xFF);
        assert!(s.decodes(Address(0x40)));
        assert!(s.decodes(Address(0x7F)));
    }

    #[test]
    fn snapshot_round_trips() {
        let mut s = psg();
        wr(&mut s, 0x82);
        wr(&mut s, 0x97);
        wr(&mut s, 0xE4);
        wr(&mut s, 0xF0);
        for _ in 0..40 {
            s.visit();
        }
        let snap = s.snapshot();
        assert_eq!(snap.len(), PSG_SNAPSHOT_LEN);
        let mut twin = psg();
        twin.restore(&snap).expect("restore");
        assert_eq!(twin.snapshot(), snap);
        assert_eq!(twin.tone_period[0], 2);
        assert_eq!(twin.attenuation, [7, 15, 15, 0]);
        assert_eq!(twin.noise_control, 0b100);
        assert_eq!(twin.noise_counter, s.noise_counter);
        assert_eq!(twin.lfsr, s.lfsr);
        assert!(twin.samples.is_empty());
        assert!(twin.next.is_none());
        // The restored state renders on its own registers.
        for _ in 0..40 {
            twin.visit();
        }
        let rendered = twin.take_samples();
        assert!(rendered.iter().any(|v| *v != 0));
    }

    #[test]
    fn restore_rejects_wrong_lengths() {
        let mut s = psg();
        let snap = s.snapshot();
        assert!(matches!(
            s.restore(&snap[..snap.len() - 1]),
            Err(SnapshotError::Mismatch(_))
        ));
        s.restore(&snap).expect("restore");
        assert!(matches!(
            s.restore(&[0u8; PSG_SNAPSHOT_LEN]),
            Err(SnapshotError::Mismatch(_))
        ));
    }

    /// The mini machine: one main domain at the SG-1000 master and
    /// the PSG window at the published SG-1000 port form $40-$7F.
    fn psg_config() -> demu_config::model::SystemConfig {
        let mut dividers = BTreeMap::new();
        dividers.insert("psg".to_string(), 48);
        let mut psg = instance();
        psg.dividers = dividers;
        let mut clocks = BTreeMap::new();
        clocks.insert(
            "main".to_string(),
            ClockDomain {
                name: "main".to_string(),
                frequency: demu_config::newtypes::Frequency::new(10_738_635, 1).expect("freq"),
                origin: "the machine master".to_string(),
            },
        );
        let mut by_name = BTreeMap::new();
        by_name.insert(psg.name.clone(), 0usize);
        demu_config::model::SystemConfig {
            name: "sg1000-test".to_string(),
            triple: "sega/z80".to_string(),
            description: "the PSG test machine".to_string(),
            clocks,
            devices: vec![psg],
            devices_by_name: by_name,
            buses: vec![CBus {
                name: "cpu".to_string(),
                address_bits: 21,
                owner: None,
                parent: None,
            }],
            windows: vec![CWindow {
                name: "psg".to_string(),
                bus: "cpu".to_string(),
                device: demu_config::newtypes::DeviceName::new("psg0").expect("name"),
                base: 0x0040,
                size: 0x0040,
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
    fn machine_renders_psg_samples_through_the_window() {
        let mut ctors = demu_core::smoke_ctors();
        ctors.register(
            "sn76489.psg",
            Box::new(|d, _| Ok(Box::new(Sn76489::new(d)))),
        );
        let mut machine = Machine::assemble(psg_config(), &ctors).expect("assemble");
        machine.boot();
        // Tone C at period 0x010 through the published two-write
        // form, then the volume wide open.
        assert!(machine.debug_write("cpu", Address(0x7F), 0xC0));
        assert!(machine.debug_write("cpu", Address(0x7F), 0x01));
        assert!(machine.debug_write("cpu", Address(0x7F), 0xD0));
        machine.run_until(Tick(3000));
        let first = take_samples_of(&mut machine);
        assert!(!first.is_empty());
        assert!(first.iter().any(|v| *v != 0));
        machine.run_until(Tick(5000));
        let second = take_samples_of(&mut machine);
        assert!(!second.is_empty(), "the second drain renders on");
    }

    /// Drain the PSG samples of the machine through the device slot.
    fn take_samples_of(machine: &mut Machine) -> Vec<i16> {
        machine
            .device_by_name_mut("psg0")
            .expect("device")
            .as_any_mut()
            .downcast_mut::<Sn76489>()
            .expect("the PSG")
            .take_samples()
    }
}
