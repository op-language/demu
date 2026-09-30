//! The NES APU: the sound generator of the RP2A03 and RP2A07.
//!
//! The frame counter runs on the CPU cycle: quarter steps drive the
//! envelopes and the triangle linear counter, half steps drive the
//! length counters and the sweeps, and the fourth step of the
//! four-step mode sets the frame IRQ flag. The five-step mode of
//! $4017 replaces the IRQ step with one more quarter step. The DMC
//! reader fetches its sample bytes through the CPU bus and holds the
//! CPU's ready line for the fetch, so the DMA timing is cycle-exact.
//!
//! One mixed sample lands per CPU cycle, which is the native rate
//! the audio source reports. The mix is an integer sum of the
//! channel outputs; the media pipeline resampler carries the exact
//! scaling.

use demu_config::model::BootState;
use demu_core::device::{AccessWidth, AudioSource, BusAccess, MachineCtx};
use demu_core::frequency::{Address, Tick};
use demu_core::signal::Level;
use demu_core::triggers::TriggerEvent;
use demu_core::{Device, Snapshot, SnapshotError};

/// The DMC rate table in CPU cycles, per region.
const DMC_RATES_NTSC: &[u16] = &[
    428, 380, 340, 320, 286, 254, 224, 214, 190, 160, 142, 128, 106, 84, 72, 58,
];
const DMC_RATES_PAL: &[u16] = &[
    398, 353, 315, 297, 265, 235, 208, 198, 176, 148, 132, 118, 98, 78, 66, 52,
];
/// The noise timer period table in CPU cycles, per region.
const NOISE_PERIODS_NTSC: &[u16] = &[
    4, 8, 16, 32, 64, 96, 128, 160, 202, 254, 380, 482, 708, 944, 1854, 3186,
];
const NOISE_PERIODS_PAL: &[u16] = &[
    4, 7, 14, 30, 60, 88, 118, 148, 188, 236, 354, 448, 658, 878, 1726, 2962,
];
/// The length counter load values.
const LENGTH_LOADS: &[u16] = &[
    10, 254, 20, 2, 40, 4, 6, 72, 8, 80, 10, 12, 14, 12, 80, 14, 24, 76, 40, 20, 96, 124, 176, 8,
    32, 72, 80, 24, 168, 144, 288, 32,
];
/// The frame sequencer quarter steps in CPU cycles, per region.
const FRAMES_NTSC: &[u32] = &[7457, 14913, 22371, 29830];
/// The extra step of the five-step mode.
const FRAME5_NTSC: u32 = 37281;
/// The PAL sequencer steps.
const FRAMES_PAL: &[u32] = &[8312, 16623, 24936, 33252];
/// The extra PAL step of the five-step mode.
const FRAME5_PAL: u32 = 41564;
/// The pulse duty waveforms, one bit per step of eight.
const DUTY_WAVES: &[u8] = &[0b01000000, 0b01100001, 0b01111001, 0b10011111];

/// One pulse channel's state.
#[derive(Debug, Default, Clone)]
struct Pulse {
    enabled: bool,
    /// $4000: duty in bits 7 and 6, the length-halt in bit 5, and
    /// the constant-volume bit 4.
    pub(crate) ctrl: u8,
    /// $4001: the sweep register.
    sweep: u8,
    /// The timer period, one under the true period.
    period: u16,
    /// The runtime timer countdown.
    timer_left: u16,
    duty_index: u8,
    length: u16,
    /// The envelope divider countdown and the decay level.
    envelope: u8,
    decay: u8,
    out: u8,
}

impl Pulse {
    /// The length-halt flag of the control register.
    fn halt(&self) -> bool {
        self.ctrl & 0x20 != 0
    }

    /// The sweep volume of the control register.
    fn sweep_shift(&self) -> u8 {
        (self.sweep >> 4) & 0x07
    }

    /// The sweep is armed with a nonzero shift.
    fn sweep_armed(&self) -> bool {
        self.sweep & 0x80 != 0 && self.sweep_shift() != 0
    }

    /// The sweep target offset.
    fn target(&self) -> u16 {
        let delta = self.period >> self.sweep_shift();
        if self.sweep & 0x08 != 0 {
            self.period.wrapping_sub(delta)
        } else {
            self.period.wrapping_add(delta)
        }
    }
}

/// The triangle channel.
#[derive(Debug, Default, Clone)]
struct Triangle {
    enabled: bool,
    /// $4008: the control flag in bit 7 and the linear reload.
    ctrl: u8,
    period: u16,
    timer_left: u16,
    length: u16,
    linear: u8,
    linear_reload: u8,
    linear_reload_flag: bool,
    step: u16,
    out: u8,
}

/// The noise channel, with a 15-bit shift register.
#[derive(Debug, Default, Clone)]
struct Noise {
    enabled: bool,
    /// $400C: the loop and halt flags in bits 7 and 5, the volume.
    ctrl: u8,
    /// The period index.
    period: u8,
    length: u16,
    envelope: u8,
    decay: u8,
    short: bool,
    shift: u16,
    timer_left: u16,
    out: u8,
}

impl Noise {
    fn halt(&self) -> bool {
        self.ctrl & 0x20 != 0
    }
}

/// The DMC channel with its sample fetch and the CPU hold.
#[derive(Debug, Default, Clone)]
struct Dmc {
    enabled: bool,
    /// The rate index.
    rate: u8,
    loop_flag: bool,
    irq_enabled: bool,
    /// The sample address and length registers.
    addr_reg: u8,
    length_reg: u8,
    /// The output level counter.
    level: u8,
    /// The bytes left in the sample.
    remaining: u16,
    /// The next fetch address.
    address: u16,
    /// The shift register and its remaining bits.
    shift: u16,
    bits_left: u8,
    silence: bool,
    /// The output shift timer period and countdown.
    timer: u16,
    timer_left: u16,
    irq_flag: bool,
    out: u8,
}

impl Dmc {
    /// The sample address the address register names.
    fn start(&self) -> u16 {
        0x8000 + u16::from(self.addr_reg) * 64
    }

    /// The sample length in bytes.
    fn bytes(&self) -> u16 {
        (u16::from(self.length_reg) + 1) * 16
    }
}

/// The audio generator of the RP2A03.
#[derive(Debug)]
pub struct NesApu {
    name: String,
    /// The bus the DMC fetches walk.
    bus_name: String,
    /// Domain ticks per CPU cycle.
    cpu_div: u64,
    /// True on the PAL region.
    pal: bool,
    /// The current sequencer step pointer.
    frame_step: u8,
    /// CPU cycles into the current step.
    frame_elapsed: u32,
    /// The pending frame-counter reset, in CPU cycles.
    reset_in: Option<u32>,
    /// The five-step mode from $4017.
    frame_mode5: bool,
    /// True when $4017 inhibited the frame IRQ.
    irq_inhibit: bool,
    /// The frame IRQ flag of the four-step mode.
    frame_irq_flag: bool,
    /// True while the IRQ line is asserted.
    irq_asserted: bool,
    pulse: [Pulse; 2],
    triangle: Triangle,
    noise: Noise,
    dmc: Dmc,
    /// The CPU hold of the DMC fetch, until the release tick.
    halt_until: Option<u64>,
    /// The gathered samples at the native rate.
    samples: Vec<i16>,
    last_tick: Option<Tick>,
    next: Option<Tick>,
}

impl NesApu {
    /// An audio generator from its configuration entry.
    pub fn new(device: &demu_config::model::DeviceInstance) -> Self {
        let pal = device.kind.as_str().starts_with("rp2A07")
            || device
                .params
                .get("pal")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
        let default_div = if pal { 16 } else { 12 };
        Self {
            name: device.name.to_string(),
            bus_name: device
                .params
                .get("bus")
                .and_then(|v| v.as_str())
                .unwrap_or("cpu")
                .to_string(),
            cpu_div: u64::from(*device.dividers.get("cpu").unwrap_or(&default_div)),
            pal,
            frame_step: 0,
            frame_elapsed: 0,
            reset_in: None,
            frame_mode5: false,
            irq_inhibit: true,
            frame_irq_flag: false,
            irq_asserted: false,
            pulse: Default::default(),
            triangle: Triangle::default(),
            noise: Noise::default(),
            dmc: Dmc::default(),
            halt_until: None,
            samples: Vec::with_capacity(4096),
            last_tick: None,
            next: None,
        }
    }

    /// The frame sequence of the region: one step past the last
    /// quarter step when the five-step mode runs.
    fn sequence_of(&self) -> [u32; 5] {
        let step5 = if self.pal { FRAME5_PAL } else { FRAME5_NTSC };
        let steps = if self.pal { FRAMES_PAL } else { FRAMES_NTSC };
        let mut seq = [0u32; 5];
        for (i, value) in steps.iter().enumerate() {
            seq[i] = *value;
        }
        seq[4] = step5;
        seq
    }

    /// Schedule the next visit one CPU cycle from now.
    fn schedule_next(&mut self, tick: Tick) {
        self.next = Some(Tick(tick.0 + self.cpu_div));
    }

    /// One CPU cycle: the frame sequencer, the channel timers, the
    /// DMC machinery, and the sample output.
    fn cycle(&mut self, tick: Tick, ctx: &mut MachineCtx) {
        // The frame counter reset follows a $4017 write by about
        // four cycles.
        if self.reset_in.is_some() {
            if self.reset_in.is_some_and(|r| r <= 1) {
                self.reset_in = None;
                self.frame_elapsed = 0;
                self.frame_step = 0;
            } else {
                self.reset_in = self.reset_in.map(|r| r - 1);
            }
        }
        self.frame_elapsed += 1;
        let seq = self.sequence_of();
        let due = seq[usize::from(self.frame_step)];
        if self.frame_elapsed >= due {
            match self.frame_step {
                0 => self.quarter_frame(),
                1 => {
                    self.quarter_frame();
                    self.half_frame();
                }
                2 => self.quarter_frame(),
                3 if self.frame_mode5 => {}
                3 | 4 => {
                    // The four-step mode's fourth quarter sets the
                    // frame IRQ flag unless inhibited; the five-step
                    // mode's tail step runs without it.
                    self.quarter_frame();
                    self.half_frame();
                    if !self.frame_mode5 && !self.irq_inhibit {
                        self.frame_irq_flag = true;
                    }
                }
                _ => {}
            }
            self.frame_step = (self.frame_step + 1) % if self.frame_mode5 { 5 } else { 4 };
            if self.frame_step == 0 {
                // The timing dues are absolute within a frame; the
                // elapsed counter only resets at the frame wrap.
                self.frame_elapsed = 0;
            }
        }
        self.dmc_machine(tick, ctx);
        self.run_timers();
        self.output_sample();
        self.update_irq(tick, ctx);
    }

    /// One quarter frame: the envelopes and the linear counter.
    fn quarter_frame(&mut self) {
        for pulse in &mut self.pulse {
            if pulse.envelope > 0 {
                pulse.envelope -= 1;
            } else {
                pulse.envelope = pulse.ctrl & 0x0F;
                if pulse.decay > 0 && pulse.ctrl & 0x1F != 0 {
                    pulse.decay -= 1;
                }
            }
        }
        if self.noise.envelope > 0 {
            self.noise.envelope -= 1;
        } else {
            self.noise.envelope = self.noise.ctrl & 0x0F;
            if self.noise.decay > 0 {
                self.noise.decay -= 1;
            }
        }
        if self.triangle.linear_reload_flag {
            self.triangle.linear = self.triangle.linear_reload;
        } else if self.triangle.linear > 0 {
            self.triangle.linear -= 1;
        }
        if self.triangle.ctrl & 0x80 != 0 {
            self.triangle.linear_reload_flag = true;
        }
    }

    /// One half frame: the length counters and the sweeps.
    fn half_frame(&mut self) {
        for pulse in &mut self.pulse {
            if pulse.sweep_armed() {
                let target = pulse.target();
                if target < 0x800 {
                    pulse.period = target;
                }
            }
            if !pulse.halt() && pulse.length > 0 {
                pulse.length -= 1;
            }
        }
        if self.triangle.ctrl & 0x80 == 0 && self.triangle.length > 0 {
            self.triangle.length -= 1;
        }
        if !self.noise.halt() && self.noise.length > 0 {
            self.noise.length -= 1;
        }
    }

    /// The channel timers of one CPU cycle.
    fn run_timers(&mut self) {
        for pulse in &mut self.pulse {
            let period = (pulse.period + 1) * 2;
            if pulse.timer_left > 0 {
                pulse.timer_left -= 1;
            }
            if pulse.timer_left == 0 {
                pulse.timer_left = period;
                pulse.duty_index = (pulse.duty_index + 1) & 0x07;
            }
            pulse.out = pulse_level(pulse);
        }
        // The triangle timer ticks every CPU cycle.
        if self.triangle.timer_left > 0 {
            self.triangle.timer_left -= 1;
        }
        if self.triangle.timer_left == 0 {
            self.triangle.timer_left = self.triangle.period.max(1);
            self.triangle.step = (self.triangle.step + 1) & 0x1F;
        }
        self.triangle.out = if self.triangle.length == 0 || self.triangle.linear == 0 {
            0
        } else {
            triangle_wave(self.triangle.step)
        };
        // The noise timer.
        let period = self.noise_period();
        if self.noise.timer_left > 0 {
            self.noise.timer_left -= 1;
        }
        if self.noise.timer_left == 0 {
            self.noise.timer_left = period.max(1);
            let tap = usize::from(self.noise.short) * 5 + 1;
            let feedback = (self.noise.shift & 1) ^ ((self.noise.shift >> tap) & 1);
            self.noise.shift = (self.noise.shift >> 1) | (feedback << 14);
        }
        self.noise.out = if self.noise.shift & 1 == 1 || self.noise.length == 0 {
            0
        } else {
            self.noise.decay
        };
        // The DMC runs in its own machine with the context in hand.
    }
}
/// The pulse level: the duty wave gate over the volume state.
fn pulse_level(pulse: &Pulse) -> u8 {
    let wave = DUTY_WAVES[usize::from(pulse.ctrl >> 6) & 0x03];
    let bit = (wave >> (7 - pulse.duty_index)) & 1;
    if pulse.period > 0x7FF || pulse.length == 0 || bit == 0 {
        return 0;
    }
    if pulse.ctrl & 0x10 != 0 {
        pulse.ctrl & 0x0F
    } else {
        pulse.decay
    }
}

/// The value of one triangle step: the 32-step sequence of the sum
/// register.
fn triangle_wave(step: u16) -> u8 {
    let i = step & 0x1F;
    if i < 16 { 15 - i as u8 } else { (i - 16) as u8 }
}

/// The noise period of one index.
fn noise_period_value(region: bool, index: u8) -> u16 {
    let table = if region {
        NOISE_PERIODS_PAL
    } else {
        NOISE_PERIODS_NTSC
    };
    table[usize::from(index & 0x0F)]
}

impl NesApu {
    /// The noise period of one index in this region's table.
    fn noise_period(&self) -> u16 {
        noise_period_value(self.pal, self.noise.period)
    }

    /// The DMC rate of one index in this region's table.
    fn dmc_rate(&self) -> u16 {
        let table = if self.pal {
            DMC_RATES_PAL
        } else {
            DMC_RATES_NTSC
        };
        table[usize::from(self.dmc.rate & 0x0F)]
    }

    /// The DMC machinery of one CPU cycle: the output timer, the
    /// buffer refill, and the release of the CPU hold.
    fn dmc_machine(&mut self, tick: Tick, ctx: &mut MachineCtx) {
        if let Some(until) = self.halt_until
            && tick.0 >= until
        {
            self.halt_until = None;
            ctx.notify("halt", Level::Low, tick);
        }
        if !self.dmc.enabled {
            return;
        }
        if self.dmc.timer_left > 0 {
            self.dmc.timer_left -= 1;
            return;
        }
        // The output unit shifts one bit per timer period.
        self.dmc.timer_left = self.dmc.timer;
        if !self.dmc.silence {
            let bit = self.dmc.shift & 0x01;
            if bit == 0 {
                self.dmc.level = self.dmc.level.saturating_sub(1);
            } else if self.dmc.level <= 125 {
                self.dmc.level += 2;
            }
        }
        self.dmc.shift >>= 1;
        self.dmc.out = self.dmc.level;
        if self.dmc.bits_left > 0 {
            self.dmc.bits_left -= 1;
        }
        if self.dmc.bits_left == 0 {
            if self.dmc.remaining > 0 {
                self.dmc_fetch(tick, ctx);
            } else if self.dmc.loop_flag && self.dmc.enabled {
                self.dmc.remaining = self.dmc.bytes();
                self.dmc.address = self.dmc.start();
                self.dmc_fetch(tick, ctx);
            } else {
                self.dmc.silence = true;
                if self.dmc.irq_enabled {
                    self.dmc.irq_flag = true;
                }
            }
        }
    }

    /// The DMC sample byte fetch through the CPU bus, holding the
    /// CPU's ready line for four cycles.
    fn dmc_fetch(&mut self, tick: Tick, ctx: &mut MachineCtx) {
        if self.halt_until.is_none() {
            ctx.notify("halt", Level::High, tick);
            self.halt_until = Some(tick.0 + 4 * self.cpu_div);
        }
        let addr = self.dmc.address;
        let value = ctx.read(
            &self.bus_name,
            AccessWidth::Byte,
            Address(u32::from(addr)),
            tick,
        ) as u8;
        ctx.emit(TriggerEvent::Dma(demu_core::triggers::DmaPhase::Start));
        self.dmc.shift = u16::from(value);
        self.dmc.bits_left = 8;
        self.dmc.silence = false;
        self.dmc.remaining = self.dmc.remaining.saturating_sub(1);
        self.dmc.address = self.dmc.address.wrapping_add(1);
        if self.dmc.remaining == 0 {
            if self.dmc.loop_flag {
                self.dmc.remaining = self.dmc.bytes();
                self.dmc.address = self.dmc.start();
            } else {
                if self.dmc.irq_enabled {
                    self.dmc.irq_flag = true;
                }
                self.dmc.enabled = false;
            }
        }
    }

    /// The mixed sample of one CPU cycle.
    fn output_sample(&mut self) {
        let total = i32::from(self.pulse[0].out)
            + i32::from(self.pulse[1].out)
            + i32::from(self.triangle.out)
            + i32::from(self.noise.out)
            + i32::from(self.dmc.out);
        self.samples.push(total.clamp(-64, 127) as i16);
    }

    /// The IRQ line matches the frame flag or the DMC flag.
    fn update_irq(&mut self, tick: Tick, ctx: &mut MachineCtx) {
        let wanted = (self.frame_irq_flag && !self.irq_inhibit) || self.dmc.irq_flag;
        if wanted != self.irq_asserted {
            self.irq_asserted = wanted;
            ctx.notify("irq", if wanted { Level::High } else { Level::Low }, tick);
        }
    }

    /// The $4015 status read; the read clears the frame IRQ flag.
    fn status_bits(&mut self) -> u8 {
        let mut bits = 0u8;
        for (i, pulse) in self.pulse.iter().enumerate() {
            bits |= (u8::from(pulse.length > 0)) << i;
        }
        bits |= (u8::from(self.triangle.length > 0)) << 2;
        bits |= (u8::from(self.noise.length > 0)) << 3;
        bits |= (u8::from(self.dmc.remaining > 0)) << 4;
        bits |= (u8::from(self.dmc.irq_flag)) << 6;
        bits |= (u8::from(self.frame_irq_flag)) << 7;
        self.frame_irq_flag = false;
        bits
    }

    /// Restart the sample from the addressed banks.
    fn restart_sample(&mut self) {
        self.dmc.remaining = self.dmc.bytes();
        self.dmc.address = self.dmc.start();
    }
}

impl BusAccess for NesApu {
    /// The sound generator registers on the CPU bus.
    fn read(&mut self, _width: AccessWidth, address: Address, _tick: Tick) -> u32 {
        match address.0 {
            0x4015 => u32::from(self.status_bits()),
            _ => 0,
        }
    }

    fn write(&mut self, _width: AccessWidth, address: Address, value: u32, _tick: Tick) {
        if !matches!(address.0, 0x4000..=0x4017) {
            return;
        }
        let v = value as u8;
        match address.0 {
            0x4000 => {
                self.pulse[0].ctrl = v;
                self.pulse[0].decay = v & 0x0F;
            }
            0x4001 => self.pulse[0].sweep = v,
            0x4002 => self.pulse[0].period = self.pulse[0].period & 0xFF00 | u16::from(v),
            0x4003 => {
                self.pulse[0].period = self.pulse[0].period & 0x00FF | (u16::from(v & 0x07) << 8);
                self.pulse[0].duty_index = 0;
                if self.pulse[0].enabled {
                    self.pulse[0].length = LENGTH_LOADS[usize::from(v >> 3) & 0x1F];
                }
            }
            0x4004 => {
                self.pulse[1].ctrl = v;
                self.pulse[1].decay = v & 0x0F;
            }
            0x4005 => self.pulse[1].sweep = v,
            0x4006 => self.pulse[1].period = self.pulse[1].period & 0xFF00 | u16::from(v),
            0x4007 => {
                self.pulse[1].period = self.pulse[1].period & 0x00FF | (u16::from(v & 0x07) << 8);
                self.pulse[1].duty_index = 0;
                if self.pulse[1].enabled {
                    self.pulse[1].length = LENGTH_LOADS[usize::from(v >> 3) & 0x1F];
                }
            }
            0x4008 => {
                self.triangle.ctrl = v;
                self.triangle.linear_reload = v & 0x7F;
            }
            0x400A => self.triangle.period = self.triangle.period & 0xFF00 | u16::from(v),
            0x400B => {
                self.triangle.period = self.triangle.period & 0x00FF | (u16::from(v & 0x07) << 8);
                if self.triangle.enabled {
                    self.triangle.length = LENGTH_LOADS[usize::from(v >> 3) & 0x1F];
                }
                self.triangle.linear_reload_flag = true;
            }
            0x400C => {
                self.noise.ctrl = v;
                self.noise.short = v & 0x80 != 0;
            }
            0x400E => self.noise.period = v,
            0x400F => {
                if self.noise.enabled {
                    self.noise.length = LENGTH_LOADS[usize::from(v >> 3) & 0x1F];
                }
                self.noise.decay = self.noise.ctrl & 0x0F;
            }
            0x4010 => {
                self.dmc.irq_enabled = v & 0x80 != 0;
                if !self.dmc.irq_enabled {
                    self.dmc.irq_flag = false;
                }
                self.dmc.loop_flag = v & 0x40 != 0;
                self.dmc.rate = v & 0x0F;
                self.dmc.timer = self.dmc_rate();
                self.dmc.timer_left = self.dmc.timer;
            }
            0x4011 => self.dmc.level = v & 0x7F,
            0x4012 => self.dmc.addr_reg = v,
            0x4013 => self.dmc.length_reg = v,
            0x4015 => {
                if v & 0x10 != 0 {
                    self.dmc.enabled = true;
                    if self.dmc.remaining == 0 {
                        self.restart_sample();
                    }
                } else {
                    self.dmc.remaining = 0;
                    self.dmc.enabled = false;
                }
                if v & 0x01 == 0 {
                    self.pulse[0].enabled = false;
                    self.pulse[0].length = 0;
                }
                if v & 0x02 == 0 {
                    self.pulse[1].enabled = false;
                    self.pulse[1].length = 0;
                }
                if v & 0x04 == 0 {
                    self.triangle.enabled = false;
                    self.triangle.length = 0;
                }
                if v & 0x08 == 0 {
                    self.noise.enabled = false;
                    self.noise.length = 0;
                }
            }
            0x4017 => {
                self.frame_mode5 = v & 0x80 != 0;
                self.irq_inhibit = v & 0x40 != 0;
                if self.irq_inhibit {
                    self.frame_irq_flag = false;
                }
                self.reset_in = Some(4);
            }
            _ => {}
        }
    }

    /// The debug-status read without side effects.
    fn peek(&self, address: Address) -> u8 {
        match address.0 {
            0x4015 => {
                (u8::from(self.pulse[0].length > 0))
                    | (u8::from(self.pulse[1].length > 0)) << 1
                    | (u8::from(self.triangle.length > 0)) << 2
            }
            _ => 0,
        }
    }

    fn decodes(&self, address: Address) -> bool {
        matches!(address.0, 0x4000..=0x4015 | 0x4017)
    }
}

impl AudioSource for NesApu {
    /// One sample per CPU cycle: the native rate of the generator.
    fn sample_rate(&self) -> u32 {
        if self.pal { 1_662_607 } else { 1_789_773 }
    }

    fn take_samples(&mut self) -> Vec<i16> {
        std::mem::take(&mut self.samples)
    }
}

impl Device for NesApu {
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

    fn reset(&mut self) {
        self.frame_step = 0;
        self.frame_elapsed = 0;
        self.reset_in = None;
        self.frame_mode5 = false;
        self.irq_inhibit = true;
        self.frame_irq_flag = false;
        self.irq_asserted = false;
        self.pulse = Default::default();
        self.triangle = Triangle::default();
        self.noise = Noise::default();
        self.dmc = Dmc::default();
        self.halt_until = None;
        self.samples.clear();
        self.next = None;
        self.last_tick = None;
    }

    fn boot(&mut self, state: Option<&BootState>) {
        let _ = state;
        self.reset();
        self.next = Some(Tick(0));
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(72);
        out.extend_from_slice(b"NAPU");
        out.push(1);
        out.push(self.frame_step);
        out.extend_from_slice(&self.frame_elapsed.to_le_bytes());
        out.push(if let Some(_reset) = self.reset_in {
            1
        } else {
            0
        });
        out.extend_from_slice(&self.reset_in.unwrap_or(0).to_le_bytes());
        out.push(u8::from(self.frame_mode5));
        out.push(u8::from(self.irq_inhibit));
        out.push(u8::from(self.frame_irq_flag));
        out.push(self.pulse[0].ctrl);
        out.push(self.pulse[0].sweep);
        out.extend_from_slice(&self.pulse[0].period.to_le_bytes());
        out.extend_from_slice(&self.pulse[0].length.to_le_bytes());
        out.push(self.pulse[0].decay);
        out.push(self.pulse[0].duty_index);
        out.push(self.pulse[1].ctrl);
        out.push(self.pulse[1].sweep);
        out.extend_from_slice(&self.pulse[1].period.to_le_bytes());
        out.extend_from_slice(&self.pulse[1].length.to_le_bytes());
        out.push(self.pulse[1].decay);
        out.push(self.pulse[1].duty_index);
        out.push(self.triangle.ctrl);
        out.extend_from_slice(&self.triangle.period.to_le_bytes());
        out.extend_from_slice(&self.triangle.length.to_le_bytes());
        out.push(self.triangle.linear);
        out.push(self.triangle.linear_reload);
        out.push(u8::from(self.triangle.linear_reload_flag));
        out.push(self.noise.ctrl);
        out.push(self.noise.period);
        out.push(self.noise.short.into());
        out.extend_from_slice(&self.noise.shift.to_le_bytes());
        out.push(self.dmc.rate);
        out.push(self.dmc.enabled.into());
        out.push(self.dmc.irq_enabled.into());
        out.push(self.dmc.loop_flag.into());
        out.push(self.dmc.addr_reg);
        out.push(self.dmc.length_reg);
        out.push(self.dmc.level);
        out.extend_from_slice(&self.dmc.remaining.to_le_bytes());
        out.extend_from_slice(&self.dmc.address.to_le_bytes());
        out.extend_from_slice(&self.dmc.shift.to_le_bytes());
        out.push(self.dmc.bits_left);
        out.push(self.dmc.silence.into());
        out.push(self.dmc.irq_flag.into());
        out.extend_from_slice(&self.dmc.timer_left.to_le_bytes());
        out.push(self.irq_asserted.into());
        out.push(u8::from(self.halt_until.is_some()));
        out.extend_from_slice(&self.halt_until.unwrap_or(0).to_le_bytes());
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        // The lengths: the 4-byte tag, the version, and the fields
        // in the snapshot order plus the trailing hold. The total is
        // 75 bytes; the field count below must match the writes.
        if data.len() != 75 || &data[..4] != b"NAPU" || data[4] != 1 {
            return Err(SnapshotError::Mismatch("bad nes apu chunk".into()));
        }
        let mut pos = 5usize;
        let read_u8 = |buf: &[u8], pos: &mut usize| -> u8 {
            let v = buf[*pos];
            *pos += 1;
            v
        };
        let read_u16 = |buf: &[u8], pos: &mut usize| -> u16 {
            let v = u16::from_le_bytes(buf[*pos..*pos + 2].try_into().expect("2 bytes"));
            *pos += 2;
            v
        };
        let read_u32 = |buf: &[u8], pos: &mut usize| -> u32 {
            let v = u32::from_le_bytes(buf[*pos..*pos + 4].try_into().expect("4 bytes"));
            *pos += 4;
            v
        };
        self.frame_step = read_u8(data, &mut pos);
        self.frame_elapsed = read_u32(data, &mut pos);
        let has_reset = read_u8(data, &mut pos) != 0;
        let reset = read_u32(data, &mut pos);
        self.reset_in = has_reset.then_some(reset);
        self.frame_mode5 = read_u8(data, &mut pos) != 0;
        self.irq_inhibit = read_u8(data, &mut pos) != 0;
        self.frame_irq_flag = read_u8(data, &mut pos) != 0;
        for pulse in &mut self.pulse {
            pulse.ctrl = read_u8(data, &mut pos);
            pulse.sweep = read_u8(data, &mut pos);
            pulse.period = read_u16(data, &mut pos);
            pulse.length = read_u16(data, &mut pos);
            pulse.decay = read_u8(data, &mut pos);
            pulse.duty_index = read_u8(data, &mut pos);
        }
        self.triangle.ctrl = read_u8(data, &mut pos);
        self.triangle.period = read_u16(data, &mut pos);
        self.triangle.length = read_u16(data, &mut pos);
        self.triangle.linear = read_u8(data, &mut pos);
        self.triangle.linear_reload = read_u8(data, &mut pos);
        self.triangle.linear_reload_flag = read_u8(data, &mut pos) != 0;
        self.noise.ctrl = read_u8(data, &mut pos);
        self.noise.period = read_u8(data, &mut pos);
        self.noise.short = read_u8(data, &mut pos) != 0;
        self.noise.shift = read_u16(data, &mut pos);
        self.dmc.rate = read_u8(data, &mut pos);
        self.dmc.enabled = read_u8(data, &mut pos) != 0;
        self.dmc.irq_enabled = read_u8(data, &mut pos) != 0;
        self.dmc.loop_flag = read_u8(data, &mut pos) != 0;
        self.dmc.addr_reg = read_u8(data, &mut pos);
        self.dmc.length_reg = read_u8(data, &mut pos);
        self.dmc.level = read_u8(data, &mut pos) & 0x7F;
        self.dmc.remaining = read_u16(data, &mut pos);
        self.dmc.address = read_u16(data, &mut pos);
        self.dmc.shift = read_u16(data, &mut pos);
        self.dmc.bits_left = read_u8(data, &mut pos);
        self.dmc.silence = read_u8(data, &mut pos) != 0;
        self.dmc.irq_flag = read_u8(data, &mut pos) != 0;
        self.dmc.timer_left = read_u16(data, &mut pos);
        self.irq_asserted = read_u8(data, &mut pos) != 0;
        let has_hold = read_u8(data, &mut pos) != 0;
        let hold = u64::from_le_bytes(data[pos..pos + 8].try_into().expect("8 bytes"));
        self.halt_until = has_hold.then_some(hold);
        self.dmc.timer = self.dmc_rate();
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
        self.cycle(tick, ctx);
        self.schedule_next(tick);
    }
}
