//! The TIA of the Atari 2600, and its sound-only slice of the 7800.
//!
//! The 2600 form is the complete picture-plus-sound chip. One device
//! visit walks one color clock: 228 color clocks per scanline, with 68
//! blank at each line start, and 262 scanlines per frame. The visible
//! pixels are the 160 color clocks between blanking; the picture rows
//! of the standard kernel map to lines 40 through 231.
//!
//! Object positioning follows the counter model of the TIA hardware
//! notes: a register strobe re-bases the position at the beam, with
//! the two-phase decode delay of the chip, and copies appear at the
//! fixed spacings the NUSIZ decodes give. The HMOVE strobe extends the
//! blanking by eight color clocks and then runs the motion counters:
//! while the ripple count has not reached the stored motion nibble,
//! each fourth color clock moves the object one pixel left. The net
//! shift of a stored nibble `S` is therefore `8 - S`.
//!
//! The 7800 form (`video` parameter off) keeps the audio registers,
//! the paddle and trigger inputs, and the input-port control register;
//! the display comes from the MARIA device. The same chip serves both
//! machines because the 7800 keeps the TIA die for sound and input.
//!
//! All colors come from an integer fixed-point YIQ conversion of the
//! color byte, so no float math appears anywhere in the device.

use demu_config::model::BootState;
use demu_core::device::{
    AccessWidth, BusAccess, Frame, InputSink, MachineCtx, VideoLayer, VideoSource,
};
use demu_core::frequency::{Address, Tick};
use demu_core::signal::Level;
use demu_core::triggers::TriggerEvent;
use demu_core::{Device, Snapshot, SnapshotError};

/// Color clocks per scanline.
const LINE_CLOCKS: u32 = 228;
/// Blank color clocks at the line start.
const HBLANK: u32 = 68;
/// Visible pixels per line.
const PX: u32 = 160;
/// Scanlines per frame (NTSC).
const LINES: u32 = 262;
/// The picture rows of the standard kernel land here.
const TOP_LINE: u32 = 40;
/// Picture rows.
const FRAME_LINES: usize = 192;
/// Color clocks per audio divider tick (the 31.4 kHz audio clock).
const AUDIO_TICK: u32 = 114;
/// The extended blanking of an HMOVE line, applied to positions.
const HBLANK_MOVE: u32 = 8;

/// The Div-31 duty pattern of the published divider: a 13:18 ratio.
const DIV31: [u8; 31] = [
    0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
];

/// The player copy layouts: the pixel spacings from the base.
const NUSIZ_COPIES: [&[usize]; 8] = [
    &[0],
    &[0, 16],
    &[0, 32],
    &[0, 16, 32],
    &[0, 64],
    &[0],
    &[0, 32, 64],
    &[0],
];

/// The fixed-point scale of the YIQ math.
/// The chroma amplitude in luma units.
const AMP: i64 = 40;

/// The Q14 chroma unit vector per hue: the 25.7-degree step begins at
/// the color-burst phase, and the demodulation turns I against sine,
/// Q against cosine, per the burst convention. Values derived once
/// from that formula; the runtime keeps the integer table.
const UNIT_I: [i32; 15] = [
    13818, 16269, 15501, 11666, 5523, -1713, -8609, -13803, -16265, -15510, -11686, -5550, 1684,
    8585, 13787,
];
const UNIT_Q: [i32; 15] = [
    -8803, -1940, 5307, 11504, 15425, 16294, 13940, 8827, 1968, -5280, -11484, -15415, -16297,
    -13955, -8851,
];

/// Build a polynomial table the way the TIA's linear-feedback counters
/// produce their sequences.
fn poly_bits(size: u32) -> Vec<u8> {
    let mask = (1u32 << size) - 1;
    let mut lfsr = mask;
    let mut out = Vec::with_capacity(mask as usize);
    for _ in 0..mask {
        let bit = (lfsr ^ (lfsr >> 1)) & 1;
        lfsr = (lfsr >> 1) | (bit << (size - 1));
        out.push(u8::from(bit == 1));
    }
    out
}

/// The integer YIQ conversion for one color byte.
///
/// The hue occupies bits 7-4 and the luma the low bits: three luma
/// bits on the 2600 with the lowest unused, four on the video chips
/// with a wider register. The chroma angle step is the 25.7-degree hue
/// spacing of the broadcast reference and the decode matrix follows the
/// NTSC receiver. The hue zero is gray scale.
pub fn color_rgb(byte: u8, lum_bits: u32) -> [u8; 3] {
    let hue = (byte >> 4) & 0x0F;
    let steps = 1u32 << lum_bits;
    let lum_mask = steps - 1;
    // The 2600 form keeps the luma in bits 3-1 with the lowest unused;
    // the four-bit chips put it in bits 3-0.
    let shift = 4 - lum_bits;
    let lum = u32::from(byte >> shift) & lum_mask;
    let span = i32::try_from(steps.saturating_sub(1)).unwrap_or(15).max(1);
    let y = 16 + (i32::try_from(lum).unwrap_or(0) * 219) / span;
    let gray = hue == 0;
    if gray {
        let c = y.clamp(0, 255) as u8;
        return [c, c, c];
    }
    // Chroma: the amplitude of 40 luma units turned by the hue angle,
    // then decoded by the standard NTSC receiver matrix. The unit
    // vectors live in the Q14 table, so the runtime is integer only.
    let idx = usize::from(hue.saturating_sub(1));
    let i_c = AMP * i64::from(UNIT_I[idx]);
    let q_c = AMP * i64::from(UNIT_Q[idx]);
    let r = i64::from(y) + (9563 * i_c + 6210 * q_c) / 10_000;
    let g = i64::from(y) - (2721 * i_c + 6474 * q_c) / 10_000;
    let b = i64::from(y) - (11_107 * i_c - 17_046 * q_c) / 10_000;
    let clamp = |v: i64| v.clamp(0, 255) as u8;
    [clamp(r), clamp(g), clamp(b)]
}

/// The pixel states of one visible pixel, for the collision matrix
/// and the layer priority.
#[derive(Debug, Clone, Copy)]
struct PixelLayers {
    pf: bool,
    p0: bool,
    p1: bool,
    m0: bool,
    m1: bool,
    ball: bool,
}

/// The audio channels and their state for the two sound generators.
#[derive(Debug)]
struct AudioChannel {
    audc: u8,
    audf: u8,
    audv: u8,
    /// The divide-by-N counter and maximum.
    div_cnt: u32,
    div_max: u32,
    /// The div-3 counter for the POLY5_DIV3 mode.
    div3_cnt: u8,
    /// The last output volume.
    outvol: i32,
    /// The poly positions.
    p4: u32,
    p5: u32,
    p9: u32,
}

impl AudioChannel {
    fn new() -> Self {
        Self {
            audc: 0,
            audf: 0,
            audv: 0,
            div_cnt: 0,
            div_max: 1,
            div3_cnt: 3,
            outvol: 0,
            p4: 0,
            p5: 0,
            p9: 0,
        }
    }

    /// Recompute the divider after an AUDC or AUDF write, keeping the
    /// running counter when the form is unchanged.
    fn refresh(&mut self) {
        let new_val = if self.audc == 0x00 || self.audc == 0x0B {
            self.outvol = i32::from(self.audv);
            0
        } else {
            let mut v = u32::from(self.audf) + 1;
            if (self.audc & 0x0C) == 0x0C && self.audc != 0x0F {
                v *= 3;
            }
            v
        };
        if new_val != self.div_max {
            self.div_max = new_val;
            // Reset the running counter only when the channel changed
            // to or from the volume-only form.
            if self.div_cnt == 0 || new_val == 0 {
                self.div_cnt = new_val;
            }
        }
    }
}

/// The pixel-processing unit and sound chip of the Atari 2600.
#[derive(Debug)]
pub struct Tia {
    name: String,
    /// The 2600 video pipeline runs only with the `video` parameter.
    video: bool,
    /// Master ticks per color clock.
    dot_div: u64,
    audio: [AudioChannel; 2],
    poly4: Vec<u8>,
    poly5: Vec<u8>,
    poly9: Vec<u8>,
    div31: Vec<u8>,
    /// The color clocks since the last audio divider tick.
    audio_cnt: u32,
    /// Collected audio samples at the native audio rate.
    samples: Vec<i16>,

    // The register file.
    vsync: u8,
    vblank: u8,
    colup: [u8; 2],
    colupf: u8,
    colubk: u8,
    ctrlpf: u8,
    refp: [u8; 2],
    pf: [u8; 3],
    nusiz: [u8; 2],
    /// The new graphics registers; a write always lands here.
    grp_new: [u8; 2],
    /// The old graphics registers for the vertical delay.
    grp_old: [u8; 2],
    enabl_new: u8,
    enabl_old: u8,
    vdelp: [u8; 2],
    vdelpbl: u8,
    resmp: [u8; 2],
    enam: [u8; 2],
    // The motion nibbles of players 0-1, missiles 0-1, ball.
    hm: [u8; 5],

    // The positions in visible pixels modulo 160.
    pos: [i32; 5],

    // The collision latches, one byte per read register.
    latches: [u8; 8],

    // The paddle and trigger input values: INPT0-5.
    inpt: [u8; 6],

    // Pending strobes: the bus write has no context, so the next visit
    // applies the effects.
    pending_wsync: bool,
    pending_hmove: bool,

    // The video state.
    cc: u32,
    line: u32,
    hmove_latch: bool,
    screen: Vec<u8>,
    cached_frame: Frame,
    frames: u64,
    next: Option<Tick>,
    last_tick: Option<Tick>,
}

impl Tia {
    /// A device from its configuration entry. The `video` parameter
    /// selects the full 2600 chip (the default) or the 7800 slice.
    pub fn new(device: &demu_config::model::DeviceInstance) -> Self {
        let video = device
            .params
            .get("video")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        Self {
            name: device.name.to_string(),
            video,
            dot_div: u64::from(device.dividers.get("dot").copied().unwrap_or(1)).max(1),
            audio: [AudioChannel::new(), AudioChannel::new()],
            poly4: poly_bits(4),
            poly5: poly_bits(5),
            poly9: poly_bits(9),
            div31: Vec::from(DIV31),
            audio_cnt: 0,
            samples: Vec::new(),
            vsync: 0,
            vblank: 0,
            colup: [0; 2],
            colupf: 0,
            colubk: 0,
            ctrlpf: 0,
            refp: [0; 2],
            pf: [0; 3],
            nusiz: [0; 2],
            grp_new: [0; 2],
            grp_old: [0; 2],
            enabl_new: 0,
            enabl_old: 0,
            vdelp: [0; 2],
            vdelpbl: 0,
            resmp: [0; 2],
            enam: [0; 2],
            hm: [0; 5],
            pos: [0; 5],
            latches: [0; 8],
            inpt: [0, 0, 0, 0, 0xFF, 0xFF],
            pending_wsync: false,
            pending_hmove: false,
            cc: 0,
            line: 0,
            hmove_latch: false,
            screen: vec![0; PX as usize * FRAME_LINES * 3],
            cached_frame: Frame {
                width: PX,
                height: FRAME_LINES as u32,
                pixels: vec![0; PX as usize * FRAME_LINES * 3],
            },
            frames: 0,
            next: None,
            last_tick: None,
        }
    }

    /// The color clock and scanline of a domain tick.
    fn beam_at(&self, tick: Tick) -> (u32, u32) {
        let d = tick.0 / self.dot_div;
        (
            (d % u64::from(LINE_CLOCKS)) as u32,
            (d % u64::from(LINES)) as u32,
        )
    }

    /// The visible pixel at the tick of an access; accesses during
    /// blanking re-base at the left edge.
    fn beam_pixel(&self, tick: Tick) -> i32 {
        let (cc, _) = self.beam_at(tick);
        if self.hmove_latch && cc < HBLANK + HBLANK_MOVE {
            return (cc - HBLANK) as i32 + HBLANK_MOVE as i32;
        }
        if cc < HBLANK + HBLANK_MOVE {
            0
        } else {
            (cc - HBLANK - HBLANK_MOVE) as i32
        }
    }

    /// Set a collision latch register.
    fn latch(&mut self, reg: usize, bit: u8) {
        self.latches[reg] |= bit;
    }

    /// Set all latches touched by one pixel's overlaps. Bits follow
    /// the published collision map: bit 7 is the second pair of the
    /// register, bit 6 the first.
    fn latch_overlaps(&mut self, l: &PixelLayers) {
        if l.m0 && (l.p0 || l.p1) {
            self.latch(0, 0x80);
        }
        if l.m1 && (l.p0 || l.p1) {
            self.latch(1, 0x80);
        }
        if l.p0 && l.pf {
            self.latch(2, 0x80);
        }
        if l.p0 && l.ball {
            self.latch(2, 0x40);
        }
        if l.p1 && l.pf {
            self.latch(3, 0x80);
        }
        if l.p1 && l.ball {
            self.latch(3, 0x40);
        }
        if l.m0 && l.pf {
            self.latch(4, 0x80);
        }
        if l.m0 && l.ball {
            self.latch(4, 0x40);
        }
        if l.m1 && l.pf {
            self.latch(5, 0x80);
        }
        if l.m1 && l.ball {
            self.latch(5, 0x40);
        }
        if l.ball && l.pf {
            self.latch(6, 0x80);
        }
        if l.p0 && l.p1 {
            self.latch(7, 0x80);
        }
    }

    /// The playfield state of one visible pixel. The pattern holds the
    /// 20 cells of one half: PF0 uses its high nibble with the bits in
    /// reverse order, PF1 the bits in direct order, and PF2 in reverse
    /// order. The reflect mode mirrors the right half of the screen.
    fn pf_bit(&self, x: u32) -> bool {
        // The left half holds the whole 20-cell pattern; the right
        // half repeats it, mirrored when the reflect bit is set.
        let half = PX / 2;
        let base = x % half;
        let px = if self.ctrlpf & 0x01 != 0 && x >= half {
            half - 1 - base
        } else {
            base
        };
        let cell = px / 4;
        match cell {
            0..=3 => self.pf[0] >> 4 & (1 << cell) != 0,
            4..=11 => self.pf[1] >> (7 - (cell - 4)) & 1 == 1,
            _ => self.pf[2] >> (cell - 12) & 1 == 1,
        }
    }

    /// The player pixel of one visible pixel.
    fn player_bit(&self, i: usize, x: u32) -> bool {
        let nusiz = self.nusiz[i];
        let copies = NUSIZ_COPIES[usize::from(nusiz & 0x07)];
        let stretch: i32 = match nusiz & 0x07 {
            5 => 2,
            7 => 4,
            _ => 1,
        };
        let gfx = if self.vdelp[i] & 0x01 != 0 {
            self.grp_old[i]
        } else {
            self.grp_new[i]
        };
        for &c in copies {
            let local = (x as i32 - self.pos[i] - c as i32).rem_euclid(PX as i32);
            if local >= 0 && local < 8 * stretch {
                let bit_idx = local / stretch;
                let bit = if self.refp[i] & 0x08 != 0 {
                    gfx >> bit_idx & 1
                } else {
                    gfx >> (7 - bit_idx) & 1
                };
                if bit == 1 {
                    return true;
                }
            }
        }
        false
    }

    /// The missile pixel: a one-to-eight pixel bar from the player's
    /// NUSIZ width bits, disabled while pinned to the player.
    fn missile_pixel(&self, i: usize, x: u32) -> bool {
        if self.resmp[i] & 0x02 != 0 || self.enam[i] & 0x02 == 0 {
            return false;
        }
        let width: u32 = match (self.nusiz[i] >> 4) & 0x03 {
            1 => 2,
            2 => 4,
            3 => 8,
            _ => 1,
        };
        let local = (x as i32 - self.pos[i + 2]).rem_euclid(PX as i32);
        (local as u32) < width
    }

    /// The ball pixel: one to eight pixels wide from CTRLPF bits 5-4.
    fn ball_pixel(&self, x: u32) -> bool {
        let enable = if self.vdelpbl & 0x01 != 0 {
            self.enabl_old
        } else {
            self.enabl_new
        };
        if enable & 0x02 == 0 {
            return false;
        }
        let width: u32 = match (self.ctrlpf >> 4) & 0x03 {
            1 => 2,
            2 => 4,
            3 => 8,
            _ => 1,
        };
        let local = (x as i32 - self.pos[4]).rem_euclid(PX as i32);
        (local as u32) < width
    }

    /// The color byte at one visible pixel with the layer priority.
    fn compose(&mut self, x: u32) -> u8 {
        let blank = self.vblank & 0x02 != 0;
        if blank || self.hmove_latch && x < HBLANK_MOVE {
            // The blanking and the comb of the extended line render
            // the border color and latch nothing.
            return self.colubk & 0xFE;
        }
        let layers = PixelLayers {
            pf: self.pf_bit(x),
            p0: self.player_bit(0, x),
            p1: self.player_bit(1, x),
            m0: self.missile_pixel(0, x),
            m1: self.missile_pixel(1, x),
            ball: self.ball_pixel(x),
        };
        self.latch_overlaps(&layers);
        self.priority_color(&layers, x)
    }

    /// The layer priority of one pixel. The default order is players,
    /// missiles, ball, playfield; the priority bit of CTRLPF puts the
    /// playfield and the ball first.
    fn priority_color(&self, l: &PixelLayers, x: u32) -> u8 {
        let pf_priority = self.ctrlpf & 0x04 != 0;
        let score = self.ctrlpf & 0x02 != 0;
        let pfc = if score {
            self.colup[usize::from(x >= PX / 2)] & 0xFE
        } else {
            self.colupf & 0xFE
        };
        if pf_priority && l.pf {
            return pfc;
        }
        if pf_priority && l.ball {
            return self.colupf & 0xFE;
        }
        if l.p0 {
            return self.colup[0] & 0xFE;
        }
        if l.p1 {
            return self.colup[1] & 0xFE;
        }
        if l.m0 {
            return self.colup[0] & 0xFE;
        }
        if l.m1 {
            return self.colup[1] & 0xFE;
        }
        if l.pf {
            return pfc;
        }
        if l.ball {
            return self.colupf & 0xFE;
        }
        self.colubk & 0xFE
    }

    /// One color clock of the pipeline.
    fn step(&mut self, tick: Tick, ctx: &mut MachineCtx) {
        // The strobe effects flush one color clock after the write,
        // which matches the chip's decode delay.
        if self.pending_wsync {
            self.pending_wsync = false;
            ctx.notify("halt", Level::High, tick);
        }
        if self.pending_hmove {
            self.pending_hmove = false;
            self.hmove_latch = true;
            // The counters resume eight clocks late: apply now.
            for i in 0..5 {
                self.pos[i] = (self.pos[i] + Self::hm_shift(self.hm[i])).rem_euclid(PX as i32);
            }
        }
        self.cc += 1;
        if self.cc == LINE_CLOCKS {
            self.cc = 0;
            self.line = (self.line + 1) % LINES;
            // The WSYNC hold releases at the next line's blank start.
            ctx.notify("halt", Level::Low, tick);
            self.hmove_latch = false;
        }
        if self.video && self.cc >= HBLANK + HBLANK_MOVE {
            let x = self.cc - HBLANK - HBLANK_MOVE;
            let color = self.compose(x);
            let row_line = self.line.wrapping_sub(TOP_LINE);
            if row_line < FRAME_LINES as u32 {
                let y = row_line as usize;
                let [r, g, b] = color_rgb(color, 3);
                let base = (y * PX as usize + x as usize) * 3;
                self.screen[base] = r;
                self.screen[base + 1] = g;
                self.screen[base + 2] = b;
            }
        }
        if self.line == LINES - 1 && self.cc == LINE_CLOCKS - 1 {
            self.frames += 1;
            self.cached_frame.pixels = self.screen.clone();
            if self.video {
                ctx.emit(TriggerEvent::Vblank);
            }
        }
        self.audio_cnt += 1;
        if self.audio_cnt == AUDIO_TICK {
            self.audio_cnt = 0;
            self.audio_tick();
        }
    }

    /// One audio divider tick on both channels, ported from the
    /// published TIA sound engine. The mixed volume level lands in
    /// the capture buffer.
    fn audio_tick(&mut self) {
        for ch in &mut self.audio {
            if ch.div_max == 0 {
                ch.outvol = i32::from(ch.audv);
                continue;
            }
            if ch.div_cnt > 1 {
                ch.div_cnt -= 1;
                continue;
            }
            let prev_bit5 = i32::from(self.poly5[ch.p5 as usize]);
            ch.div_cnt = ch.div_max;
            ch.p5 += 1;
            if ch.p5 >= self.poly5.len() as u32 {
                ch.p5 = 0;
            }
            let bit5 = i32::from(self.poly5[ch.p5 as usize]);
            let clock_event = (ch.audc & 0x02) == 0
                || ((ch.audc & 0x01) == 0 && self.div31[ch.p5 as usize] != 0)
                || ((ch.audc & 0x01) == 1 && bit5 != 0)
                || (ch.audc == 0x0F && bit5 != prev_bit5);
            if !clock_event {
                continue;
            }
            if ch.audc & 0x04 != 0 {
                if ch.audc == 0x0F {
                    if bit5 != prev_bit5 {
                        ch.div3_cnt -= 1;
                        if ch.div3_cnt == 0 {
                            ch.div3_cnt = 3;
                            ch.outvol = if ch.outvol != 0 {
                                0
                            } else {
                                i32::from(ch.audv)
                            };
                        }
                    }
                } else {
                    ch.outvol = if ch.outvol != 0 {
                        0
                    } else {
                        i32::from(ch.audv)
                    };
                }
            } else if ch.audc & 0x08 != 0 {
                if ch.audc == 0x08 {
                    ch.p9 += 1;
                    if ch.p9 >= self.poly9.len() as u32 {
                        ch.p9 = 0;
                    }
                    ch.outvol = if self.poly9[ch.p9 as usize] != 0 {
                        i32::from(ch.audv)
                    } else {
                        0
                    };
                } else if ch.audc & 0x02 != 0 {
                    ch.outvol = if ch.outvol != 0 || ch.audc & 0x01 != 0 {
                        0
                    } else {
                        i32::from(ch.audv)
                    };
                } else {
                    ch.outvol = if bit5 != 0 { i32::from(ch.audv) } else { 0 };
                }
            } else {
                ch.p4 += 1;
                if ch.p4 >= self.poly4.len() as u32 {
                    ch.p4 = 0;
                }
                ch.outvol = if self.poly4[ch.p4 as usize] != 0 {
                    i32::from(ch.audv)
                } else {
                    0
                };
            }
        }
        let v0 = self.audio[0].outvol;
        let v1 = self.audio[1].outvol;
        self.samples.push(((v0 + v1) * 32) as i16);
    }

    /// The motion nibble's net shift: the extended blanking moves the
    /// object eight pixels right, and each stuffed clock one pixel
    /// left, until the ripple count reaches the stored nibble. The
    /// hardware-inverted bit 3 of the decode gives the classic table.
    fn hm_shift(nibble: u8) -> i32 {
        8 - i32::from((nibble & 0x0F) ^ 0x08)
    }
}

/// Wrap a position into the 160-pixel range.
fn wrap160(x: i32) -> i32 {
    x.rem_euclid(PX as i32)
}

impl BusAccess for Tia {
    fn read(&mut self, _width: AccessWidth, address: Address, _tick: Tick) -> u32 {
        let r = (address.0 & 0x0F) as usize;
        match r {
            0..=7 => u32::from(self.latches[r]),
            8..=13 => {
                // The paddles dump to ground while VBLANK bit 7 is on.
                let dumped = self.vblank & 0x80 != 0 && r < 12;
                u32::from(if dumped { 0 } else { self.inpt[r - 8] })
            }
            _ => 0,
        }
    }

    fn write(&mut self, _width: AccessWidth, address: Address, value: u32, tick: Tick) {
        let v = value as u8;
        match address.0 & 0x3F {
            0x00 => self.vsync = v,
            0x01 => self.vblank = v,
            0x02 => self.pending_wsync = true,
            0x03 => {} // RSYNC: the software realigns the counter.
            0x04 => self.nusiz[0] = v,
            0x05 => self.nusiz[1] = v,
            0x06 => self.colup[0] = v & 0xFE,
            0x07 => self.colup[1] = v & 0xFE,
            0x08 => self.colupf = v & 0xFE,
            0x09 => self.colubk = v & 0xFE,
            0x0A => self.ctrlpf = v,
            0x0B => self.refp[0] = v,
            0x0C => self.refp[1] = v,
            0x0D => self.pf[0] = v,
            0x0E => self.pf[1] = v,
            0x0F => self.pf[2] = v,
            0x10 => {
                self.pos[0] = wrap160(self.beam_pixel(tick) + 3);
            }
            0x11 => {
                self.pos[1] = wrap160(self.beam_pixel(tick) + 3);
            }
            0x12 => {
                self.pos[2] = wrap160(self.beam_pixel(tick) + 2);
            }
            0x13 => {
                self.pos[3] = wrap160(self.beam_pixel(tick) + 2);
            }
            0x14 => {
                self.pos[4] = wrap160(self.beam_pixel(tick) + 2);
            }
            0x15 => {
                self.audio[0].audc = v & 0x0F;
                self.audio[0].refresh();
            }
            0x16 => {
                self.audio[1].audc = v & 0x0F;
                self.audio[1].refresh();
            }
            0x17 => {
                self.audio[0].audf = v & 0x1F;
                self.audio[0].refresh();
            }
            0x18 => {
                self.audio[1].audf = v & 0x1F;
                self.audio[1].refresh();
            }
            0x19 => self.audio[0].audv = v & 0x0F,
            0x1A => self.audio[1].audv = v & 0x0F,
            0x1B => {
                self.grp_old[1] = self.grp_new[1];
                self.grp_new[0] = v;
            }
            0x1C => {
                self.grp_old[0] = self.grp_new[0];
                self.grp_new[1] = v;
                self.enabl_old = self.enabl_new;
            }
            0x1D => self.enam[0] = v,
            0x1E => self.enam[1] = v,
            0x1F => self.enabl_new = v,
            0x20 => self.hm[0] = v >> 4,
            0x21 => self.hm[1] = v >> 4,
            0x22 => self.hm[2] = v >> 4,
            0x23 => self.hm[3] = v >> 4,
            0x24 => self.hm[4] = v >> 4,
            0x25 => self.vdelp[0] = v,
            0x26 => self.vdelp[1] = v,
            0x27 => self.vdelpbl = v,
            0x28 => self.resmp[0] = v & 0x02,
            0x29 => self.resmp[1] = v & 0x02,
            0x2A => self.pending_hmove = true,
            0x2B => self.hm = [0; 5],
            0x2C => self.latches = [0; 8],
            _ => {}
        }
    }

    fn peek(&self, address: Address) -> u8 {
        match address.0 & 0x0F {
            8..=13 => self.inpt[(address.0 & 0x0F) as usize - 8],
            _ => 0,
        }
    }
}

impl VideoSource for Tia {
    fn frame(&self) -> Option<&Frame> {
        if self.video {
            Some(&self.cached_frame)
        } else {
            None
        }
    }

    fn dump_layer(&self, layer: VideoLayer) -> Option<Vec<u8>> {
        match layer {
            VideoLayer::Framebuffer => Some(self.screen.clone()),
            _ => None,
        }
    }

    fn frames(&self) -> u64 {
        self.frames
    }
}

impl Device for Tia {
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

    fn video(&self) -> Option<&dyn VideoSource> {
        if self.video { Some(self) } else { None }
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
        self.audio = [AudioChannel::new(), AudioChannel::new()];
        self.audio_cnt = 0;
        self.samples.clear();
        self.vsync = 0;
        self.vblank = 0;
        self.colup = [0; 2];
        self.colupf = 0;
        self.colubk = 0;
        self.ctrlpf = 0;
        self.refp = [0; 2];
        self.pf = [0; 3];
        self.nusiz = [0; 2];
        self.grp_new = [0; 2];
        self.grp_old = [0; 2];
        self.enabl_new = 0;
        self.enabl_old = 0;
        self.vdelp = [0; 2];
        self.vdelpbl = 0;
        self.resmp = [0; 2];
        self.enam = [0; 2];
        self.hm = [0; 5];
        self.pos = [0; 5];
        self.latches = [0; 8];
        self.inpt = [0, 0, 0, 0, 0xFF, 0xFF];
        self.pending_wsync = false;
        self.pending_hmove = false;
        self.cc = 0;
        self.line = 0;
        self.hmove_latch = false;
        self.frames = 0;
        self.next = None;
        self.last_tick = None;
    }

    fn boot(&mut self, _state: Option<&BootState>) {
        self.reset();
        self.next = Some(Tick(0));
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(100);
        out.extend_from_slice(b"TIAS");
        out.push(1);
        out.extend_from_slice(&self.vsync.to_le_bytes());
        out.extend_from_slice(&self.vblank.to_le_bytes());
        out.extend_from_slice(&self.colup[..]);
        out.push(self.colupf);
        out.push(self.colubk);
        out.push(self.ctrlpf);
        out.extend_from_slice(&self.refp[..]);
        out.extend_from_slice(&self.pf[..]);
        out.extend_from_slice(&self.nusiz[..]);
        out.extend_from_slice(&self.grp_new[..]);
        out.extend_from_slice(&self.grp_old[..]);
        out.push(self.enabl_new);
        out.push(self.enabl_old);
        out.extend_from_slice(&self.vdelp[..]);
        out.push(self.vdelpbl);
        out.extend_from_slice(&self.resmp[..]);
        out.extend_from_slice(&self.enam[..]);
        out.extend_from_slice(&self.hm[..]);
        for p in &self.pos {
            out.extend_from_slice(&p.to_le_bytes());
        }
        out.extend_from_slice(&self.latches[..]);
        out.extend_from_slice(&self.inpt[..]);
        out.push(u8::from(self.pending_wsync));
        out.push(u8::from(self.pending_hmove));
        out.extend_from_slice(&self.cc.to_le_bytes());
        out.extend_from_slice(&self.line.to_le_bytes());
        out.push(u8::from(self.hmove_latch));
        out.extend_from_slice(&self.audio_cnt.to_le_bytes());
        for a in &self.audio {
            out.push(a.audc);
            out.push(a.audf);
            out.push(a.audv);
            out.extend_from_slice(&a.div_cnt.to_le_bytes());
            out.extend_from_slice(&a.div_max.to_le_bytes());
            out.push(a.div3_cnt);
            out.extend_from_slice(&a.outvol.to_le_bytes());
            out.extend_from_slice(&a.p5.to_le_bytes());
            out.extend_from_slice(&a.p4.to_le_bytes());
            out.extend_from_slice(&a.p9.to_le_bytes());
        }
        out.extend_from_slice(&self.frames.to_le_bytes());
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        let expected = 4
            + 1
            + 1 // vsync
            + 1
            + 2
            + 1
            + 1
            + 1
            + 2
            + 3
            + 2
            + 2
            + 2
            + 1
            + 1
            + 2
            + 1
            + 2
            + 2
            + 5
            + 5 * 4
            + 8
            + 6
            + 1
            + 1
            + 4
            + 4
            + 1
            + 4 // the audio tick counter
            + 2 * 28 // each channel: regs, dividers, volume, poly positions
            + 8;
        if data.len() != expected || &data[..4] != b"TIAS" || data[4] != 1 {
            return Err(SnapshotError::Mismatch("bad tia chunk".into()));
        }
        let mut pos = 5usize;
        self.vsync = data[pos];
        pos += 1;
        self.vblank = data[pos];
        pos += 1;
        self.colup.copy_from_slice(&data[pos..pos + 2]);
        pos += 2;
        self.colupf = data[pos];
        pos += 1;
        self.colubk = data[pos];
        pos += 1;
        self.ctrlpf = data[pos];
        pos += 1;
        self.refp.copy_from_slice(&data[pos..pos + 2]);
        pos += 2;
        self.pf.copy_from_slice(&data[pos..pos + 3]);
        pos += 3;
        self.nusiz.copy_from_slice(&data[pos..pos + 2]);
        pos += 2;
        self.grp_new.copy_from_slice(&data[pos..pos + 2]);
        pos += 2;
        self.grp_old.copy_from_slice(&data[pos..pos + 2]);
        pos += 2;
        self.enabl_new = data[pos];
        pos += 1;
        self.enabl_old = data[pos];
        pos += 1;
        self.vdelp.copy_from_slice(&data[pos..pos + 2]);
        pos += 2;
        self.vdelpbl = data[pos];
        pos += 1;
        self.resmp.copy_from_slice(&data[pos..pos + 2]);
        pos += 2;
        self.enam.copy_from_slice(&data[pos..pos + 2]);
        pos += 2;
        self.hm.copy_from_slice(&data[pos..pos + 5]);
        pos += 5;
        for p in self.pos.iter_mut() {
            *p = i32::from_le_bytes(data[pos..pos + 4].try_into().expect("4 bytes"));
            pos += 4;
        }
        self.latches.copy_from_slice(&data[pos..pos + 8]);
        pos += 8;
        self.inpt.copy_from_slice(&data[pos..pos + 6]);
        pos += 6;
        self.pending_wsync = data[pos] != 0;
        pos += 1;
        self.pending_hmove = data[pos] != 0;
        pos += 1;
        self.cc = u32::from_le_bytes(data[pos..pos + 4].try_into().expect("4 bytes")) % LINE_CLOCKS;
        pos += 4;
        self.line = u32::from_le_bytes(data[pos..pos + 4].try_into().expect("4 bytes")) % LINES;
        pos += 4;
        self.hmove_latch = data[pos] != 0;
        pos += 1;
        self.audio_cnt =
            u32::from_le_bytes(data[pos..pos + 4].try_into().expect("4 bytes")) % AUDIO_TICK;
        pos += 4;
        for a in self.audio.iter_mut() {
            a.audc = data[pos];
            pos += 1;
            a.audf = data[pos];
            pos += 1;
            a.audv = data[pos];
            pos += 1;
            a.div_cnt = u32::from_le_bytes(data[pos..pos + 4].try_into().expect("4 bytes"));
            pos += 4;
            a.div_max = u32::from_le_bytes(data[pos..pos + 4].try_into().expect("4 bytes"));
            pos += 4;
            a.div3_cnt = data[pos].max(1);
            pos += 1;
            a.outvol = i32::from_le_bytes(data[pos..pos + 4].try_into().expect("4 bytes"));
            pos += 4;
            a.p5 = u32::from_le_bytes(data[pos..pos + 4].try_into().expect("4 bytes"));
            pos += 4;
            a.p4 = u32::from_le_bytes(data[pos..pos + 4].try_into().expect("4 bytes"));
            pos += 4;
            a.p9 = u32::from_le_bytes(data[pos..pos + 4].try_into().expect("4 bytes"));
            pos += 4;
        }
        self.frames = u64::from_le_bytes(data[pos..pos + 8].try_into().expect("8 bytes"));
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
        self.step(tick, ctx);
        self.next = Some(Tick(tick.0 + self.dot_div));
    }
}

impl InputSink for Tia {
    fn inject(&mut self, event: &str) -> bool {
        // Grammar: `p0-fire` presses the trigger, `p0-fire-release`
        // lifts it, `paddle0 <value>` sets the paddle input.
        let form = event.trim().to_ascii_lowercase();
        if let Some(v) = form.strip_prefix("paddle0 ") {
            return v.trim().parse::<u8>().map(|v| self.inpt[0] = v).is_ok();
        }
        if form == "p0-fire" {
            self.inpt[4] = 0;
            return true;
        }
        if form == "p0-fire-release" {
            self.inpt[4] = 0xFF;
            return true;
        }
        false
    }
}

impl demu_core::AudioSource for Tia {
    fn sample_rate(&self) -> u32 {
        31_400
    }

    fn take_samples(&mut self) -> Vec<i16> {
        std::mem::take(&mut self.samples)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tia() -> Tia {
        Tia {
            name: "tia".to_string(),
            video: true,
            dot_div: 1,
            audio: [AudioChannel::new(), AudioChannel::new()],
            poly4: poly_bits(4),
            poly5: poly_bits(5),
            poly9: poly_bits(9),
            div31: Vec::from(DIV31),
            audio_cnt: 0,
            samples: Vec::new(),
            vsync: 0,
            vblank: 0,
            colup: [0; 2],
            colupf: 0,
            colubk: 0,
            ctrlpf: 0,
            refp: [0; 2],
            pf: [0; 3],
            nusiz: [0; 2],
            grp_new: [0; 2],
            grp_old: [0; 2],
            enabl_new: 0,
            enabl_old: 0,
            vdelp: [0; 2],
            vdelpbl: 0,
            resmp: [0; 2],
            enam: [0; 2],
            hm: [0; 5],
            pos: [0; 5],
            latches: [0; 8],
            inpt: [0, 0, 0, 0, 0xFF, 0xFF],
            pending_wsync: false,
            pending_hmove: false,
            cc: 0,
            line: 0,
            hmove_latch: false,
            screen: vec![0; PX as usize * FRAME_LINES * 3],
            cached_frame: Frame {
                width: PX,
                height: FRAME_LINES as u32,
                pixels: vec![0; PX as usize * FRAME_LINES * 3],
            },
            frames: 0,
            next: None,
            last_tick: None,
        }
    }

    #[test]
    fn hm_shift_matches_motion_table() {
        // The empirical motion table: a nibble of eight shifts right
        // eight, the low values shift left, zero stays.
        let expect: [i32; 16] = [0, -1, -2, -3, -4, -5, -6, -7, 8, 7, 6, 5, 4, 3, 2, 1];
        for (v, want) in expect.iter().enumerate() {
            assert_eq!(Tia::hm_shift(v as u8), *want, "nibble {v:#x}");
        }
    }

    #[test]
    fn poly_tables_have_full_periods() {
        assert_eq!(poly_bits(4).len(), 15);
        assert_eq!(poly_bits(5).len(), 31);
        assert_eq!(poly_bits(9).len(), 511);
        assert_eq!(DIV31.len(), 31);
    }

    #[test]
    fn colors_are_deterministic_bytes() {
        for h in 0u8..=15 {
            for l in 0u8..=15 {
                let [r, g, b] = color_rgb((h << 4) | l, 4);
                let [r2, g2, b2] = color_rgb((h << 4) | l, 4);
                assert_eq!((r, g, b), (r2, g2, b2));
            }
        }
        // The hue zero is the gray ramp: the darkest value sits at 16.
        assert_eq!(color_rgb(0x00, 3), [16, 16, 16]);
    }

    #[test]
    fn audio_volume_only_mode_outputs_constant() {
        let mut t = tia();
        t.audio[0].audc = 0x00;
        t.audio[0].audv = 8;
        t.audio[0].refresh();
        t.audio_tick();
        assert_eq!(t.samples.len(), 1);
        assert_eq!(t.samples[0], (i32::from(8u8) * 8 * 4) as i16);
    }

    #[test]
    fn snapshot_round_trip() {
        let mut t = tia();
        t.pos[0] = 42;
        t.colubk = 0x54;
        let snap = t.snapshot();
        let mut twin = tia();
        twin.restore(&snap).expect("restore");
        assert_eq!(twin.snapshot(), snap);
        assert_eq!(twin.colubk, 0x54);
    }

    #[test]
    fn read_map_decodes_collisions_and_inputs() {
        let mut t = tia();
        t.latches[2] = 0x80;
        assert_eq!(t.read(AccessWidth::Byte, Address(0x02), Tick(0)), 0x80);
        assert_eq!(t.read(AccessWidth::Byte, Address(0x32), Tick(0)), 0x80);
        t.inpt[4] = 0x55;
        assert_eq!(t.read(AccessWidth::Byte, Address(0x0C), Tick(0)), 0x55);
        // VBLANK bit 7 dumps the paddle inputs to ground.
        t.vblank = 0x80;
        assert_eq!(t.read(AccessWidth::Byte, Address(0x08), Tick(0)), 0);
    }
}
