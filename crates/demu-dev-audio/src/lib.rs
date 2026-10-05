//! The audio device family: the SM83 APU, the NES APU, the Atari
//! POKEY, the C64 SID, the SNES S-SMP sound die with its S-DSP
//! engine, the Apple IIgs Ensoniq DOC, and the PC Engine PSG of the
//! HuC6280.
//!
//! The SM83 four channels: two pulse channels with sweep and envelope,
//! a wave channel with its own sample memory, and a noise channel. The
//! frame sequencer rides the DIV-APU edge 512 times a second and
//! clocks the length counters, the envelopes, and the channel-1 sweep.
//! Each channel timer counts at the CPU's T-cycle rate, and the mixed
//! output lands in a sample buffer for the media pipeline.

pub mod apu;
pub mod dsp;
pub mod iigsdoc;
pub mod nes;
pub mod pce;
pub mod pokey;
pub mod sid;
pub mod sn76489;
pub mod ym2610;
pub mod ym2612;

pub use dsp::Sdsp;
pub use iigsdoc::IigsDoc;
pub use nes::NesApu;
pub use pce::PcePsg;
pub use pokey::Pokey;
pub use sid::Sid;
pub use sn76489::Sn76489;
pub use ym2610::Ym2610;
pub use ym2612::Ym2612;

use demu_config::model::BootState;
use demu_core::device::{AccessWidth, BusAccess, MachineCtx};
use demu_core::frequency::{Address, Tick};
use demu_core::signal::Level;
use demu_core::{AudioSource, Device, Snapshot, SnapshotError};

/// A pulse channel.
#[derive(Debug, Clone, Copy, Default)]
struct Pulse {
    /// The duty cycle select, bits 6-7.
    duty: u8,
    /// The wave duty step.
    duty_step: u8,
    /// The length counter.
    length: u8,
    /// The length counter runs.
    length_enabled: bool,
    /// The shadow period register.
    shadow: u16,
    timer: u32,
    /// The channel-1 sweep byte, from `NR10`.
    nr10: u8,
    sweep_timer: u8,
    sweep_enabled: bool,
    /// The envelope byte, from `NR12`/`NR22`.
    nr12: u8,
    volume: u8,
    envelope_timer: u8,
    enabled: bool,
}

/// The wave channel.
#[derive(Debug, Clone, Copy, Default)]
struct Wave {
    /// The sample position, 0 to 31.
    step: u8,
    /// The volume code, bits 5-6 of `NR32`.
    code: u8,
    /// The period in T-cycles.
    period: u32,
    timer: u32,
    length: u16,
    length_enabled: bool,
    enabled: bool,
    dac: bool,
}

/// The noise channel.
#[derive(Debug, Clone, Copy, Default)]
struct Noise {
    /// The linear feedback shift register.
    lfsr: u16,
    /// The volume, from the upper nibble of `NR42`.
    volume: u8,
    /// The envelope pace and increase, from `NR42`.
    pace: u8,
    /// The envelope countdown.
    envelope_timer: u8,
    length: u8,
    length_enabled: bool,
    timer: u32,
    enabled: bool,
    /// The noise control byte `NR43`.
    nr43: u8,
    /// The control byte `NR44`.
    nr44: u8,
}

/// The audio device.
pub struct Apu {
    name: String,
    cgb: bool,
    /// The wave RAM undocumented read behavior applies.
    pub wave_ram_undoc: bool,
    /// The two pulse channels.
    pulse: [Pulse; 2],
    wave: Wave,
    noise: Noise,
    /// The wave sample memory at `0xFF30-0xFF3F`.
    wave_ram: [u8; 0x10],
    nr50: u8,
    nr51: u8,
    nr52: u8,
    /// The frame sequencer step.
    sequencer: u8,
    /// The CGB wave RAM bank.
    wave_bank: u8,
    /// The mixed output samples, one per T-cycle, capped in size.
    samples: Vec<i16>,
    last_tick: Option<Tick>,
    next: Option<Tick>,
}

impl Apu {
    /// An audio device from its configuration entry.
    pub fn new(device: &demu_config::model::DeviceInstance) -> Self {
        Self {
            name: device.name.to_string(),
            cgb: device
                .params
                .get("cgb")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            wave_ram_undoc: true,
            pulse: [Pulse::default(); 2],
            wave: Wave::default(),
            noise: Noise::default(),
            wave_ram: [0; 0x10],
            nr50: 0x77,
            nr51: 0x00,
            nr52: 0xF1,
            sequencer: 0,
            wave_bank: 0,
            samples: Vec::with_capacity(0x4000),
            last_tick: None,
            next: None,
        }
    }

    /// The T-cycle period at a period value.
    fn period_of(period: u16) -> u32 {
        (2048 - u32::from(period & 0x07FF)) * 4
    }

    /// The period of the noise timer in T-cycles, from `NR43`.
    fn noise_period_of(nr43: u8) -> u32 {
        let divisor = match nr43 & 0x07 {
            0 => 8u32,
            d => u32::from(d) * 16,
        };
        let shift = usize::from((nr43 >> 4) & 0x0F).min(14);
        (divisor << (15 - shift)).max(8)
    }

    /// Advance one T-cycle.
    fn t_cycle(&mut self) {
        if self.nr52 & 0x80 == 0 {
            return;
        }
        for i in 0..2 {
            let mut ch = self.pulse[i];
            if ch.timer > 0 {
                ch.timer -= 1;
            }
            if ch.timer == 0 && ch.enabled {
                ch.timer = Self::period_of(ch.shadow).max(1);
                ch.duty_step = (ch.duty_step + 1) % 8;
            }
            self.pulse[i] = ch;
        }
        let mut wave = self.wave;
        if wave.timer > 0 {
            wave.timer -= 1;
        }
        if wave.timer == 0 && wave.enabled {
            wave.timer = wave.period.max(1);
            wave.step = (wave.step + 1) % 32;
        }
        self.wave = wave;
        let mut noise = self.noise;
        if noise.timer > 0 {
            noise.timer -= 1;
        }
        if noise.timer == 0 && noise.enabled {
            noise.timer = Self::noise_period_of(noise.nr43);
            let bit = (noise.lfsr ^ (noise.lfsr >> 1)) & 1;
            noise.lfsr = (noise.lfsr >> 1) | (bit << 14);
        }
        self.noise = noise;
    }

    /// The frame sequencer advance from the timer's DIV-APU edge.
    fn sequencer_step(&mut self) {
        if self.nr52 & 0x80 == 0 {
            return;
        }
        self.sequencer = self.sequencer.wrapping_add(1);
        let clock_length = self.sequencer.is_multiple_of(2);
        let clock_envelope = self.sequencer % 8 == 7;
        let clock_sweep = self.sequencer % 4 == 2;
        for i in 0..2 {
            let mut ch = self.pulse[i];
            if clock_length && ch.length_enabled && ch.length > 0 {
                ch.length = ch.length.saturating_sub(1);
                if ch.length == 0 {
                    ch.enabled = false;
                }
            }
            if clock_envelope && ch.nr12 & 0x07 > 0 {
                let pace = ch.nr12 & 0x07;
                if ch.envelope_timer == 0 {
                    ch.envelope_timer = pace;
                }
                ch.envelope_timer = ch.envelope_timer.saturating_sub(1);
                if ch.envelope_timer == 0 {
                    ch.envelope_timer = pace;
                    let increase = ch.nr12 & 0x08 != 0;
                    if increase && ch.volume < 15 {
                        ch.volume += 1;
                    } else if !increase && ch.volume > 0 {
                        ch.volume -= 1;
                    }
                }
            }
            self.pulse[i] = ch;
        }
        if clock_sweep && self.pulse[0].sweep_enabled {
            self.sweep_step();
        }
        if clock_length && self.wave.length_enabled && self.wave.length > 0 {
            self.wave.length = self.wave.length.saturating_sub(1);
            if self.wave.length == 0 {
                self.wave.enabled = false;
            }
        }
        let mut noise = self.noise;
        if clock_length && noise.length_enabled && noise.length > 0 {
            noise.length -= 1;
            if noise.length == 0 {
                noise.enabled = false;
            }
        }
        if clock_envelope && noise.pace & 0x07 > 0 {
            if noise.envelope_timer == 0 {
                noise.envelope_timer = noise.pace & 0x07;
            }
            noise.envelope_timer = noise.envelope_timer.saturating_sub(1);
            if noise.envelope_timer == 0 {
                noise.envelope_timer = noise.pace & 0x07;
                let increase = noise.pace & 0x08 != 0;
                if increase && noise.volume < 15 {
                    noise.volume += 1;
                } else if !increase && noise.volume > 0 {
                    noise.volume -= 1;
                }
            }
        }
        self.noise = noise;
    }

    /// The channel-1 sweep step at 128 Hz.
    fn sweep_step(&mut self) {
        let pace = (self.pulse[0].nr10 >> 4) & 0x07;
        if pace == 0 {
            self.pulse[0].sweep_enabled = false;
            return;
        }
        let mut timer = self.pulse[0].sweep_timer.saturating_sub(1);
        if timer == 0 {
            timer = pace;
            let step = self.pulse[0].nr10 & 0x07;
            let delta = self.pulse[0].shadow >> step;
            let new = if self.pulse[0].nr10 & 0x08 != 0 {
                self.pulse[0].shadow.wrapping_sub(delta)
            } else {
                self.pulse[0].shadow.wrapping_add(delta)
            };
            if new > 2047 {
                self.pulse[0].enabled = false;
                self.pulse[0].sweep_timer = timer;
                return;
            }
            if step != 0 {
                self.pulse[0].shadow = new;
                self.pulse[0].timer = Self::period_of(new).max(1);
            }
        }
        self.pulse[0].sweep_timer = timer;
    }

    /// The mixed sample of one T-cycle.
    fn mix(&self) -> i16 {
        if self.nr52 & 0x80 == 0 {
            return 0;
        }
        let mut left = 0i32;
        let mut right = 0i32;
        for i in 0..2 {
            let ch = self.pulse[i];
            if !ch.enabled || ch.nr12 & 0xF8 == 0 {
                continue;
            }
            let sample = DUTY_TABLE[usize::from((ch.duty >> 6) & 3)][usize::from(ch.duty_step)];
            let value = i32::from(sample) * i32::from(ch.volume);
            if self.nr51 & (1 << (i * 2 + 1)) != 0 {
                left += value;
            }
            if self.nr51 & (1 << (i * 2)) != 0 {
                right += value;
            }
        }
        if self.wave.enabled && self.wave.dac {
            let entry = usize::from(self.wave_ram[usize::from(self.wave.step) / 2]);
            let nibble = if self.wave.step.is_multiple_of(2) {
                entry >> 4
            } else {
                entry & 0x0F
            };
            let mut value = i32::from(nibble as u8);
            value = match self.wave.code {
                0 => 0,
                1 => value,
                2 => value >> 1,
                _ => value >> 2,
            };
            if self.nr51 & 0x10 != 0 {
                left += value;
            }
            if self.nr51 & 0x20 != 0 {
                right += value;
            }
        }
        if self.noise.enabled && self.noise.volume > 0 {
            let output = if self.noise.lfsr & 1 == 1 { 15 } else { 0 };
            let scaled = i32::from(output * self.noise.volume);
            if self.nr51 & 0x20 != 0 {
                left += scaled;
            }
            if self.nr51 & 0x40 != 0 {
                right += scaled;
            }
        }
        let left_volume = i32::from((self.nr50 >> 4) & 0x07) + 1;
        let right_volume = i32::from(self.nr50 & 0x07) + 1;
        ((left * left_volume + right * right_volume) / 16).clamp(i16::MIN as i32, i16::MAX as i32)
            as i16
    }

    /// Trigger one channel from its register block.
    fn trigger(&mut self, channel: usize) {
        match channel {
            0 | 1 => {
                let mut ch = self.pulse[channel];
                ch.enabled = ch.nr12 & 0xF8 != 0;
                if ch.length == 0 {
                    ch.length = 64;
                }
                ch.timer = Self::period_of(ch.shadow).max(1);
                ch.duty_step = 0;
                if channel == 0 {
                    let pace = (ch.nr10 >> 4) & 0x07;
                    let step = ch.nr10 & 0x07;
                    if pace > 0 || step > 0 {
                        ch.sweep_enabled = true;
                        ch.sweep_timer = pace;
                    } else {
                        ch.sweep_enabled = false;
                    }
                    if step > 0 {
                        let delta = ch.shadow >> step;
                        let new = if ch.nr10 & 0x08 != 0 {
                            ch.shadow.wrapping_sub(delta)
                        } else {
                            ch.shadow.wrapping_add(delta)
                        };
                        if new > 2047 {
                            ch.enabled = false;
                        }
                    }
                }
                self.pulse[channel] = ch;
            }
            2 => {
                let mut wave = self.wave;
                wave.enabled = wave.dac;
                if wave.length == 0 {
                    wave.length = 256;
                }
                wave.step = 0;
                wave.timer = wave.period.max(1);
                self.wave = wave;
            }
            _ => {
                let mut noise = self.noise;
                noise.enabled = noise.volume > 0;
                if noise.length == 0 {
                    noise.length = 64;
                }
                noise.lfsr = 0x7FFF;
                noise.timer = Self::noise_period_of(noise.nr43);
                noise.envelope_timer = noise.pace & 0x07;
                self.noise = noise;
            }
        }
    }
}

/// The wave duty patterns.
const DUTY_TABLE: [[u8; 8]; 4] = [
    [0, 0, 0, 0, 0, 0, 0, 1],
    [0, 0, 0, 0, 0, 0, 1, 1],
    [0, 0, 0, 0, 1, 1, 1, 1],
    [0, 1, 1, 1, 1, 1, 1, 0],
];

impl Apu {
    /// The wave channel's read of its own memory during playback. The
    /// CGB banks the wave RAM through bit 5 of `NR30`.
    fn wave_ram_read(&self, offset: usize) -> u8 {
        if self.wave.enabled && self.wave_ram_undoc {
            // The channel grabs the byte at its own position.
            let pos = usize::from(self.wave.step) / 2;
            return self.wave_ram[pos % 0x10];
        }
        self.wave_ram[offset]
    }
}

impl Apu {
    /// The device visit: one T-cycle.
    fn visit(&mut self, tick: Tick, _ctx: &mut MachineCtx) {
        self.t_cycle();
        let sample = self.mix();
        if self.samples.len() >= 0x4000 {
            // The buffer caps; the media pipeline drains it between
            // frames.
            self.samples.clear();
        }
        self.samples.push(sample);
        self.next = Some(Tick(tick.0 + 4));
    }
}

impl Device for Apu {
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

    fn reset(&mut self) {
        self.nr52 = 0xF1;
        self.sequencer = 0;
        self.pulse = [Pulse::default(); 2];
        self.wave = Wave::default();
        self.noise = Noise::default();
        self.next = None;
        self.last_tick = None;
    }

    fn boot(&mut self, state: Option<&BootState>) {
        let _ = state;
        self.reset();
        self.next = Some(Tick(0));
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(0x1A);
        out.extend_from_slice(b"APU1");
        out.push(self.nr50);
        out.push(self.nr51);
        out.push(self.nr52);
        out.push(self.sequencer);
        out.push(self.wave_bank);
        out.extend_from_slice(&self.wave_ram);
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        if data.len() != 4 + 4 + 1 + 0x10 || &data[..4] != b"APU1" {
            return Err(SnapshotError::Mismatch("bad apu chunk".into()));
        }
        self.nr50 = data[4];
        self.nr51 = data[5];
        self.nr52 = data[6];
        self.sequencer = data[7];
        self.wave_bank = data[8];
        self.wave_ram.copy_from_slice(&data[9..0x19]);
        self.next = None;
        self.last_tick = None;
        Ok(())
    }

    fn notify_line(&mut self, line: &str, _level: Level, _tick: Tick) {
        // The timer delivers the DIV-APU edge 512 times a second.
        if line == "div_apu" {
            self.sequencer_step();
        }
    }

    fn next_event(&self) -> Option<Tick> {
        self.next
    }

    fn tick(&mut self, tick: Tick, ctx: &mut MachineCtx) {
        if self.last_tick.is_some_and(|t| t.0 >= tick.0) {
            return;
        }
        self.last_tick = Some(tick);
        self.visit(tick, ctx);
    }
}

impl BusAccess for Apu {
    fn read(&mut self, _width: AccessWidth, address: Address, _tick: Tick) -> u32 {
        match address.0 {
            0xFF10 => u32::from(self.pulse[0].nr10),
            0xFF11 => u32::from(self.pulse[0].duty & 0xC0) | u32::from(64 - self.pulse[0].length),
            0xFF12 => u32::from(self.pulse[0].nr12),
            0xFF13 => 0xFF,
            0xFF14 => {
                let ch = &self.pulse[0];
                u32::from(u8::from(ch.length_enabled) << 6)
            }
            0xFF16 => u32::from(self.pulse[1].duty & 0xC0) | u32::from(64 - self.pulse[1].length),
            0xFF17 => u32::from(self.pulse[1].nr12),
            0xFF18 => 0xFF,
            0xFF19 => {
                let ch = &self.pulse[1];
                u32::from(u8::from(ch.length_enabled))
            }
            0xFF1A => u32::from(u8::from(self.wave.dac) << 7) | 0x7F,
            0xFF1B => u32::from(256u16 - self.wave.length),
            0xFF1C => u32::from(self.wave.code << 5) | 0x9F,
            0xFF1D => 0xFF,
            0xFF1E => u32::from(u8::from(self.wave.length_enabled) << 6),
            0xFF20 => u32::from(64 - self.noise.length) | 0x3F,
            0xFF21 => u32::from(self.noise.volume << 4) | u32::from(self.noise.pace & 0x07),
            0xFF22 => u32::from(self.noise.nr43),
            0xFF23 => u32::from(u8::from(self.noise.length_enabled) << 6),
            0xFF24 => u32::from(self.nr50),
            0xFF25 => u32::from(self.nr51),
            0xFF26 => {
                let powered = self.nr52 & 0x80;
                let mut status = 0u8;
                if self.pulse[0].enabled {
                    status |= 0x01;
                }
                if self.pulse[1].enabled {
                    status |= 0x02;
                }
                if self.wave.enabled {
                    status |= 0x04;
                }
                if self.noise.enabled {
                    status |= 0x08;
                }
                u32::from(powered | status | 0x70)
            }
            0xFF76 => {
                let ch1 = self.channel_output(0);
                let ch2 = self.channel_output(1);
                u32::from(ch1 | (ch2 << 4))
            }
            0xFF77 => {
                let ch3 = self.wave_output();
                let noise = if self.noise.enabled && self.noise.lfsr & 1 == 1 {
                    15
                } else {
                    0
                };
                u32::from(ch3 | (noise << 4))
            }
            0xFF30..=0xFF3F => u32::from(self.wave_ram_read((address.0 - 0xFF30) as usize)),
            _ => 0xFF,
        }
    }

    fn write(&mut self, _width: AccessWidth, address: Address, value: u32, tick: Tick) {
        let _ = tick;
        let v = value as u8;
        match address.0 {
            0xFF10 => self.pulse[0].nr10 = v & 0x7F,
            0xFF11 => {
                self.pulse[0].duty = v & 0xC0;
                self.pulse[0].length = 64 - (v & 0x3F);
            }
            0xFF12 => {
                self.pulse[0].nr12 = v;
                self.pulse[0].volume = v >> 4;
            }
            0xFF13 => {
                self.pulse[0].shadow = (self.pulse[0].shadow & 0x0700) | u16::from(v);
            }
            0xFF14 => {
                self.pulse[0].shadow = (self.pulse[0].shadow & 0x00FF) | (u16::from(v & 0x07) << 8);
                self.pulse[0].length_enabled = v & 0x40 != 0;
                if v & 0x80 != 0 {
                    self.trigger(0);
                }
            }
            0xFF16 => {
                self.pulse[1].duty = v & 0xC0;
                self.pulse[1].length = 64 - (v & 0x3F);
            }
            0xFF17 => {
                self.pulse[1].nr12 = v;
                self.pulse[1].volume = v >> 4;
            }
            0xFF18 => self.pulse[1].shadow = (self.pulse[1].shadow & 0x0700) | u16::from(v),
            0xFF19 => {
                self.pulse[1].shadow = (self.pulse[1].shadow & 0x00FF) | (u16::from(v & 0x07) << 8);
                self.pulse[1].length_enabled = v & 0x40 != 0;
                if v & 0x80 != 0 {
                    self.trigger(1);
                }
            }
            0xFF1A => {
                self.wave.dac = v & 0x80 != 0;
                self.wave.enabled &= self.wave.dac;
                if self.cgb {
                    // The CGB banks the wave RAM through bit 5 of NR30.
                    self.wave_bank = v & 0x01;
                }
            }
            0xFF1B => self.wave.length = u16::from(v),
            0xFF1C => self.wave.code = (v >> 5) & 0x03,
            0xFF1D => {
                self.wave.period =
                    Self::period_of(((self.wave.period & 0x0700) | u32::from(v)) as u16);
            }
            0xFF1E => {
                self.wave.period = (self.wave.period & 0x00FF) | (u32::from(v & 0x07) << 8);
                self.wave.length_enabled = v & 0x40 != 0;
                if v & 0x80 != 0 {
                    self.trigger(2);
                }
            }
            0xFF20 => self.noise.length = 64 - (v & 0x3F),
            0xFF21 => {
                self.noise.volume = v >> 4;
                self.noise.pace = v;
            }
            0xFF22 => self.noise.nr43 = v,
            0xFF23 => {
                self.noise.nr44 = v & 0xC0;
                self.noise.length_enabled = v & 0x40 != 0;
                if v & 0x80 != 0 {
                    self.trigger(3);
                }
            }
            0xFF24 => self.nr50 = v,
            0xFF25 => self.nr51 = v,
            0xFF26 => {
                let was = self.nr52 & 0x80 != 0;
                self.nr52 = v & 0x80 | (self.nr52 & 0x0F);
                if was && v & 0x80 == 0 {
                    // The power-on write clears every register block.
                    self.pulse = [Pulse::default(); 2];
                    self.wave = Wave::default();
                    self.noise = Noise::default();
                    self.wave_ram = [0; 0x10];
                }
            }
            0xFF30..=0xFF3F => {
                self.wave_ram[(address.0 - 0xFF30) as usize] = v;
            }
            _ => {}
        }
    }

    fn peek(&self, address: Address) -> u8 {
        match address.0 {
            0xFF10 => self.pulse[0].nr10,
            0xFF11 => (self.pulse[0].duty & 0xC0) | (64 - self.pulse[0].length),
            0xFF12 => self.pulse[0].nr12,
            0xFF1A => u8::from(self.wave.dac) << 7 | 0x7F,
            0xFF24 => self.nr50,
            0xFF25 => self.nr51,
            0xFF30..=0xFF3F => self.wave_ram[(address.0 - 0xFF30) as usize],
            _ => 0xFF,
        }
    }
}

impl Apu {
    /// The output level of one pulse channel.
    fn channel_output(&self, channel: usize) -> u8 {
        let ch = &self.pulse[channel];
        if !ch.enabled {
            return 0;
        }
        let sample = DUTY_TABLE[usize::from((ch.duty >> 6) & 3)][usize::from(ch.duty_step)];
        sample * (ch.volume >> 2)
    }

    /// The output level of the wave channel.
    fn wave_output(&self) -> u8 {
        if !self.wave.enabled {
            return 0;
        }
        let entry = usize::from(self.wave_ram[usize::from(self.wave.step) / 2]);
        let nibble = (if self.wave.step.is_multiple_of(2) {
            entry >> 4
        } else {
            entry & 0x0F
        }) as u8;
        match self.wave.code {
            0 => 0u8,
            1 => nibble >> 2,
            2 => nibble >> 3,
            _ => nibble >> 4,
        }
    }
}

impl AudioSource for Apu {
    fn sample_rate(&self) -> u32 {
        1048576
    }

    fn take_samples(&mut self) -> Vec<i16> {
        std::mem::take(&mut self.samples)
    }
}
