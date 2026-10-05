//! The Sinclair ULA of the ZX80, the ZX81, and the ZX Spectrum.
//!
//! One device carries the three Sinclair video forms. The published
//! ULA answers the keyboard and beeper port with bit 0 of the port
//! address clear, selects the keyboard half-row from the high byte of
//! the port, drives the frame timing lines, and in the Spectrum form
//! paints the attribute screen. The `spectrum128` capability is not a
//! separate form: the `shadow_ram` parameter marks the ULA of the 128
//! variant, the machine provides the timing numbers 228 / 311 of that
//! variant through the same parameters, and the banked memory lives
//! in the companion devices. Nothing here hardcodes a 48K form
//! against a 128K form except the `shadow_ram` flag.
//!
//! # Parameters
//!
//! - `form`: `zx80`, `zx81`, or `spectrum` (required).
//! - `line_t`: the T-states per scanline; 224 by default.
//! - `frame_lines`: the scanlines per frame; 312 by default.
//! - `int_lines`: the width of the frame INT pulse in scanlines; 1 by
//!   default.
//! - `shadow_ram`: marks the ULA of the 128 variant; false by
//!   default. The flag lets the ULA listen to the `shadow` line of
//!   the companion memory controller and render the banked screen
//!   span.
//!
//! # The companion devices
//!
//! The banked memory of the 128 variant lives outside this device:
//! the `zx.bankram` controller holds the 128 KiB of RAM and the
//! `$7FFD` paging port, and the `z80.bankrom` device holds the
//! two-page ROM. The controller drives three lines over the machine
//! connections: the `contend_page` line to the CPU core, the
//! `rom_bank` line to the ROM, and the `shadow` line to this ULA.
//! This device consumes the `shadow` and `halt` lines and drives the
//! `int` and `nmi` lines. The port decodes split the space: the ULA
//! answers every even port, and the companion answers the `$7FFD`
//! paging family, whose addresses the software produces with bit 0
//! set. The published paging mask leaves bit 0 open, so an even
//! member of the family meets the ULA decode, and the window order of
//! the machine decides which device answers it; the bundled machine
//! addresses the port at `$7FFD`.
//!
//! # The keyboard port
//!
//! The port read returns the byte `$BF` with the pressed bits of the
//! selected half-rows cleared: bits 0 to 4 carry the five keys of each
//! half-row selected by the high byte of the port (one row per set
//! bit; several set rows merge), bit 5 and bit 7 pull high, and bit 6
//! carries the EAR input line, which sits low here because this build
//! models no tape reader. The port write stores its byte: bits 1 to 3
//! latch the border color, bit 4 drives the beeper in the Spectrum
//! form, and bit 0 drives the MIC/EAR output line and drives the
//! speaker in the ZX80 and ZX81 forms. All three forms accept the same
//! event grammar and share the published 8 x 5 keyboard matrix; the
//! ZX80 and ZX81 use a compatible subset under the same matrix
//! addresses.
//!
//! # The frame lines
//!
//! One device visit is one scanline boundary, and the device re-arms
//! itself `line_t` ticks ahead. All forms emit the frame boundary
//! trigger at the line-0 visit.
//!
//! - The Spectrum form: the INT line rises at frame line 0 and drops
//!   when the scanline index reaches `int_lines` scanlines (one
//!   scanline by default). The published pulse is 32 T-states wide;
//!   the scanline-granularity build approximates the pulse with whole
//!   scanlines, and the CPU samples the line state at an instruction
//!   boundary.
//! - The ZX80 form: the NMI line rises at frame line 0 and drops at
//!   line 1.
//! - The ZX81 form reads the `zx.zx81_slow` quirk key of the `zx`
//!   family. The fast mode drives no INT and no NMI lines and publishes
//!   a border-only frame; the published fast mode lets the CPU generate
//!   and read the display itself. The slow mode runs the recorded
//!   line rhythm: the blank scanlines (frame lines 0 to 15 and
//!   112 to the frame end) drive the INT line low and the NMI line
//!   high, and the 96 display scanlines (frame lines 16 to 111) drive
//!   the INT line high and the NMI line low. The CPU wakes from its
//!   HALT on the display lines and services the frame; the device
//!   learns the halt through the incoming `halt` line and records the
//!   state, and while the CPU is halted the ULA keeps rendering at the
//!   line boundary as usual. The published machine renders the DFILE
//!   through the halt loop's own bus traffic; the peek-based render
//!   approximates that walk.
//!
//! # The ZX81 display walk
//!
//! The slow mode renders one DFILE row at a display-line boundary: the
//! row index reads `(line - 16) / 4` over the 96 display lines, so the
//! 24 rows paint four times each and the repeated paintings are
//! idempotent. A DFILE row sits at `$4000 + row * 33` and holds the 32
//! character bytes plus the `$76` HALT byte. The character forms: the
//! codes `$00` to `$3F` render the font ROM at `$1E00 + code * 8`, the
//! codes `$40` to `$7F` render the block graphics with the six pattern
//! bits of `code & 0x3F`, and the codes `$80` and above render the
//! same low-six-bit graphic with the display inverted. The published
//! inverse characters `$80` to `$BF` of the text set render as the
//! inverted blank graphic here; this is the recorded character-set
//! approximation. The block pattern mapping: bit 0 paints the
//! top-left quadrant's left column pair, bit 1 the top-left's right
//! pair, bit 2 the top-right's left pair, bit 3 the top-right's right
//! pair, bit 4 the whole bottom-left quadrant, and bit 5 the whole
//! bottom-right quadrant. The frame is 320 x 240: a 256 x 192 display
//! with a border of 32 pixels at each side and 24 pixels at each end;
//! the ZX border draws black and the display ink draws white.
//!
//! # The Spectrum display walk
//!
//! The Spectrum form composes the whole frame at the line-0 visit from
//! the screen memory: the pixel bytes follow the published interleaved
//! byte layout, third by Y line, pixel line by Y bit, char row by Y
//! (the `pix_addr` helper), and the attribute bytes sit at
//! `$5800 + row * 32 + column`. An attribute byte carries the ink at bits 0 to 2, the
//! paper at bits 3 to 5, the bright bit at bit 6, and the flash bit at
//! bit 7; the flash swaps the ink and paper of flashing cells every 16
//! frame boundaries. With `shadow_ram` set and the companion
//! controller driving the `shadow` line high, the render translates
//! the screen span into the paged home window: the pixel bytes peek
//! from `$C000` and the attribute bytes from `$D800`. The published
//! 128 screen pairs the shadow bit with bank 7, and the bundled
//! machine pages bank 7 when the shadow bit sets, so the shadow
//! screen renders the bank the controller pages into the home window
//! at the moment of the compose. Without the shadow bit the frame
//! reads the `$4000` screen span.
//!
//! # The beeper
//!
//! The device appends one beeper sample per scanline visit: the level
//! reads the form's drive bit (bit 4 in the Spectrum form, bit 0 in
//! the ZX80 and ZX81 forms) and the sample is `+8000` while the level
//! stands and `-8000` while it sits. The sample rate matches the
//! form's CPU rate: 3500000 for the Spectrum and 3250000 for the ZX80
//! and ZX81. The buffer holds up to 0x8000 samples until the consumer
//! drains it. The per-scanline sampling is the recorded audio
//! approximation.
//!
//! # Save state
//!
//! The save state rides the `ZXUL` tag, version 1, with a fixed
//! length: the `$FE` latch, the keyboard rows, the flash phase, the
//! frame and scanline counters, the driven INT and NMI states and the
//! halt record, and the shadow screen input. A restore validates the
//! length, the tag, and the version, and drops the pending schedule;
//! the machine re-seeds every device after a restore. The accumulated
//! samples are a drain surface, not machine state, and stay out of
//! the save state.

use demu_config::model::{BootState, SystemConfig};
use demu_core::device::{
    AccessWidth, AudioSource, BusAccess, Frame, InputSink, MachineCtx, VideoLayer, VideoSource,
};
use demu_core::frequency::{Address, Tick};
use demu_core::machine::MachineError;
use demu_core::signal::Level;
use demu_core::{Device, Snapshot, SnapshotError, TriggerEvent};

/// The composed frame width: a 256-pixel display with a 32-pixel side
/// border on both sides.
pub const FRAME_WIDTH: u32 = 320;
/// The composed frame height: a 192-pixel display with a 24-pixel end
/// border on both ends.
pub const FRAME_HEIGHT: u32 = 240;

/// The base of the shadow screen: the published pixel span inside the
/// paged home window.
const SHADOW_PIX_BASE: u32 = 0xC000;
/// The base of the shadow attribute memory: the shadow pixel base
/// plus the published attribute offset.
const SHADOW_ATTR_BASE: u32 = SHADOW_PIX_BASE + 0x1800;
/// The base of the Spectrum screen memory.
const SCREEN_PIX_BASE: u32 = 0x4000;
/// The base of the Spectrum attribute memory.
const SCREEN_ATTR_BASE: u32 = 0x5800;
/// The displayed character rows.
const CHARS_PER_ROW: u32 = 32;
/// The displayed character rows of the Spectrum screen.
const SCREEN_ROWS: u32 = 24;
/// The top border of the display inside the frame.
const DISPLAY_TOP: u32 = 24;
/// The left border of the display inside the frame.
const DISPLAY_LEFT: u32 = 32;

/// The DFILE row stride: 32 characters plus one `$76` HALT byte.
const DFILE_STRIDE: u32 = 33;
/// The base of the ZX81 font ROM.
const ZX81_FONT_BASE: u32 = 0x1E00;
/// The first display scanline of the ZX81 frame.
const ZX81_DISPLAY_FIRST: u64 = 16;
/// The display scanlines of the ZX81 frame.
const ZX81_DISPLAY_LINES: u64 = 96;
/// The display scanlines per DFILE row (recorded: the published frame
/// walks one row over eight scanlines, this build over four).
const ZX81_LINES_PER_ROW: u64 = 4;

/// The beeper amplitude.
const BEEPER_AMPLITUDE: i16 = 8000;
/// The sample cap of the beeper buffer.
const SAMPLE_CAP: usize = 0x8000;

/// The keyboard read base: the spare bits 5 and 7 pull high and the
/// EAR line at bit 6 sits low in this build.
const PORT_FE_BASE: u8 = 0xBF;

/// The eight non-bright colors of the ZX palette, RGB888.
const COLOR_NORMAL: [[u8; 3]; 8] = [
    [0, 0, 0],
    [0, 0, 205],
    [205, 0, 0],
    [205, 0, 205],
    [0, 205, 0],
    [0, 205, 205],
    [205, 205, 0],
    [205, 205, 205],
];
/// The eight bright variants of the palette: each channel of the
/// non-bright form scaled by 1.25 and capped at 255.
const COLOR_BRIGHT: [[u8; 3]; 8] = [
    [0, 0, 0],
    [0, 0, 255],
    [255, 0, 0],
    [255, 0, 255],
    [0, 255, 0],
    [0, 255, 255],
    [255, 255, 0],
    [255, 255, 255],
];
/// The ZX80/ZX81 display ink: white glyphs on black.
const ZX_INK: [u8; 3] = [255, 255, 255];
/// The ZX80/ZX81 paper: black.
const ZX_PAPER: [u8; 3] = [0, 0, 0];

/// The fixed save-state chunk length of the ULA.
const ZXUL_LEN: usize = 33;

/// The published ZX keyboard matrix: eight rows of five keys, one
/// entry per column position, row-major. The ZX80 and ZX81 use a
/// compatible subset under the same matrix addresses.
const KEY_MATRIX: [&str; 40] = [
    "1",
    "2",
    "3",
    "4",
    "5", //
    "q",
    "w",
    "e",
    "r",
    "t", //
    "a",
    "s",
    "d",
    "f",
    "g", //
    "0",
    "9",
    "8",
    "7",
    "6", //
    "p",
    "o",
    "i",
    "u",
    "y", //
    "enter",
    "l",
    "k",
    "j",
    "h", //
    "space",
    "symbol_shift",
    "m",
    "n",
    "b", //
    "caps_shift",
    "z",
    "x",
    "c",
    "v",
];

/// The Sinclair form of the ULA.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UlaForm {
    /// The ZX80.
    Zx80,
    /// The ZX81.
    Zx81,
    /// The ZX Spectrum, with or without the 128 variant.
    Spectrum,
}

/// The Sinclair ULA: the keyboard port, the beeper, the frame lines,
/// and the display walks.
#[derive(Debug)]
pub struct ZxUla {
    name: String,
    /// The Sinclair form.
    form: UlaForm,
    /// True when the ZX81 runs the slow-mode line rhythm; from the
    /// `zx.zx81_slow` quirk key of the `zx` family.
    zx81_slow: bool,
    /// The T-states per scanline, the `line_t` parameter.
    line_t: u64,
    /// The scanlines per frame, the `frame_lines` parameter.
    frame_lines: u64,
    /// The width of the frame INT pulse in scanlines, the `int_lines`
    /// parameter.
    int_lines: u64,
    /// True when this ULA belongs to the 128 variant, the
    /// `shadow_ram` parameter: the device then listens to the
    /// `shadow` line of the `zx.bankram` companion and renders the
    /// banked screen span.
    shadow: bool,
    /// The shadow-screen input of the companion controller, from the
    /// `shadow` line.
    shadow_screen: bool,
    /// The pressed state of the keyboard matrix, one bit per column
    /// (bit set = pressed).
    rows: [u8; 8],
    /// The stored write byte of the keyboard port: bits 1 to 3 carry
    /// the Spectrum border, bit 4 the Spectrum beeper drive, and bit 0
    /// the MIC/EAR output and the ZX beeper drive.
    fe_latch: u8,
    /// The flash phase over the 32-frame flash period.
    flash: u8,
    /// The accumulated beeper samples.
    samples: Vec<i16>,
    /// The composed screen as RGB888, three bytes per pixel.
    screen: Vec<u8>,
    /// The view of the screen for the shell's screenshot commands.
    cached_frame: Frame,
    /// The count of the completed frames.
    frames: u64,
    /// The scanline being processed at the current visit.
    line: u64,
    /// The driven INT level.
    int_out: bool,
    /// The driven NMI level.
    nmi_out: bool,
    /// The halted state the CPU reports over the `halt` line.
    cpu_halted: bool,
    /// The scanline boundary the machine visits next.
    next: Option<Tick>,
    last_tick: Option<Tick>,
}

impl ZxUla {
    /// A device from its configuration entry.
    ///
    /// # Errors
    ///
    /// Returns an error when the `form` parameter is missing or names
    /// no known form.
    pub fn new(
        device: &demu_config::model::DeviceInstance,
        config: &SystemConfig,
    ) -> Result<Self, MachineError> {
        let form = match device
            .params
            .get("form")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
        {
            "zx80" => UlaForm::Zx80,
            "zx81" => UlaForm::Zx81,
            "spectrum" => UlaForm::Spectrum,
            form => {
                return Err(MachineError::Assembly(format!(
                    "device '{}' of kind '{}' needs a known form, got '{}'",
                    device.name.as_ref(),
                    device.kind.as_str(),
                    form
                )));
            }
        };
        let line_t = param_u64(device, "line_t", 224).max(1);
        let frame_lines = param_u64(device, "frame_lines", 312).max(1);
        let int_lines = param_u64(device, "int_lines", 1).max(1);
        let shadow = device
            .params
            .get("shadow_ram")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        // The quirk key applies when its first segment names the device
        // kind family, and only the ZX81 form reads the slow mode.
        let zx81_slow =
            form == UlaForm::Zx81 && quirk_flag(config, "zx.zx81_slow", device.kind.family());
        Ok(Self {
            name: device.name.to_string(),
            form,
            zx81_slow,
            line_t,
            frame_lines,
            int_lines,
            shadow,
            rows: [0; 8],
            fe_latch: 0,
            shadow_screen: false,
            flash: 0,
            samples: Vec::new(),
            screen: vec![0; (FRAME_WIDTH * FRAME_HEIGHT * 3) as usize],
            cached_frame: Frame {
                width: FRAME_WIDTH,
                height: FRAME_HEIGHT,
                pixels: vec![0; (FRAME_WIDTH * FRAME_HEIGHT * 3) as usize],
            },
            frames: 0,
            line: 0,
            int_out: false,
            nmi_out: false,
            cpu_halted: false,
            next: None,
            last_tick: None,
        })
    }

    /// The keyboard byte of one port address. The high byte of the
    /// address selects one half-row per set bit; the pressed keys of
    /// all selected rows merge and read low.
    fn keyboard_byte(&self, a: u32) -> u8 {
        let mut pressed = 0u8;
        let high = ((a >> 8) & 0xFF) as u8;
        for r in 0..8u32 {
            if high & (1 << r) != 0 {
                pressed |= self.rows[r as usize] & 0x1F;
            }
        }
        PORT_FE_BASE & !pressed
    }

    /// The beeper drive level of the form.
    fn beeper_on(&self) -> bool {
        let bit = match self.form {
            UlaForm::Spectrum => 0x10,
            _ => 0x01,
        };
        self.fe_latch & bit != 0
    }

    /// The border palette entry: bits 1 to 3 of the port latch in the
    /// Spectrum form and black in the ZX80 and ZX81 forms.
    fn border_index(&self) -> u8 {
        match self.form {
            UlaForm::Spectrum => (self.fe_latch >> 1) & 0x07,
            _ => 0,
        }
    }

    /// Fill the whole screen with the current border color.
    fn fill_border(&mut self) {
        let rgb = match self.form {
            UlaForm::Spectrum => COLOR_NORMAL[self.border_index() as usize],
            _ => ZX_PAPER,
        };
        for px in self.screen.as_chunks_mut::<3>().0 {
            px.copy_from_slice(&rgb);
        }
    }

    /// Copy the composed screen into the cached frame.
    fn refresh_cached_frame(&mut self) {
        self.cached_frame.pixels = self.screen.clone();
    }

    /// Store one beeper sample of the current drive level.
    fn push_sample(&mut self) {
        if self.samples.len() >= SAMPLE_CAP {
            return;
        }
        self.samples.push(if self.beeper_on() {
            BEEPER_AMPLITUDE
        } else {
            -BEEPER_AMPLITUDE
        });
    }

    /// The port read form: the keyboard byte on an even port and the
    /// open-bus form `$FF` on an odd port.
    fn port_read(&self, a: u32) -> u8 {
        if a & 1 == 0 {
            return self.keyboard_byte(a);
        }
        0xFF
    }

    /// The port write form: the `$FE` latch takes every even port
    /// write and ignores odd writes.
    fn port_write(&mut self, a: u32, value: u8) {
        if a & 1 == 0 {
            self.fe_latch = value;
        }
    }

    /// The published interleaved address of one pixel-byte column.
    fn pix_addr(y: u32, x_byte: u32, shadow: bool) -> u32 {
        let base = if shadow {
            SHADOW_PIX_BASE
        } else {
            SCREEN_PIX_BASE
        };
        base + (y >> 6) * 0x800 + (y & 0x07) * 0x100 + ((y >> 3) & 0x07) * 0x20 + x_byte
    }

    /// Compose one Spectrum frame: the border fill, the attribute
    /// display, the flash phase, and the published screen spans.
    ///
    /// The shadow screen renders the bank the `zx.bankram` controller
    /// currently pages: the screen and attribute reads peek at the
    /// `$C000` and `$D800` regions of the home window. The published
    /// 128 screen pairs the shadow bit with bank 7, and the bundled
    /// machine pages bank 7 when the shadow bit sets.
    fn spectrum_frame(&mut self, ctx: &mut MachineCtx) {
        self.fill_border();
        let shadow = self.shadow && self.shadow_screen;
        let flash_on = self.flash >= 16;
        self.flash = (self.flash + 1) % 32;
        let attr_base = if shadow {
            SHADOW_ATTR_BASE
        } else {
            SCREEN_ATTR_BASE
        };
        for r in 0..SCREEN_ROWS {
            let mut attrs = [0u8; CHARS_PER_ROW as usize];
            for b in 0..CHARS_PER_ROW {
                attrs[b as usize] = ctx.peek("cpu", Address(attr_base + r * CHARS_PER_ROW + b));
            }
            for y in (r * 8)..(r * 8 + 8) {
                for b in 0..CHARS_PER_ROW {
                    let pix = ctx.peek("cpu", Address(Self::pix_addr(y, b, shadow)));
                    let (ink, paper) = attr_colors(attrs[b as usize], flash_on);
                    for c in 0..8u32 {
                        let on = (pix >> (7 - c)) & 1 != 0;
                        let rgb = if on { ink } else { paper };
                        let x = DISPLAY_LEFT + b * 8 + c;
                        let base = ((DISPLAY_TOP + y) * FRAME_WIDTH + x) as usize * 3;
                        self.screen[base..base + 3].copy_from_slice(&rgb);
                    }
                }
            }
        }
        self.refresh_cached_frame();
        self.frames += 1;
    }

    /// Compose the whole Spectrum frame at the line-0 visit.
    fn spectrum_visit(&mut self, line: u64, tick: Tick, ctx: &mut MachineCtx) {
        if line == 0 {
            self.spectrum_frame(ctx);
            ctx.emit(TriggerEvent::Vblank);
        }
        if line == 0 && !self.int_out {
            self.int_out = true;
            ctx.notify("int", Level::High, tick);
        }
        if self.int_out && line >= self.int_lines.max(1) {
            self.int_out = false;
            ctx.notify("int", Level::Low, tick);
        }
    }

    /// The ZX80 frame: the border fill, the frame trigger, and the
    /// NMI pulse of one scanline.
    fn zx80_visit(&mut self, line: u64, tick: Tick, ctx: &mut MachineCtx) {
        if line == 0 {
            self.fill_border();
            ctx.emit(TriggerEvent::Vblank);
        }
        let last = self.frame_lines.saturating_sub(1);
        if line == last {
            self.refresh_cached_frame();
            self.frames += 1;
        }
        if line == 0 && !self.nmi_out {
            self.nmi_out = true;
            ctx.notify("nmi", Level::High, tick);
        }
        if self.nmi_out && line >= 1 {
            self.nmi_out = false;
            ctx.notify("nmi", Level::Low, tick);
        }
    }

    /// The ZX81 frame: the border fill and the frame trigger in both
    /// modes, and in the slow mode the recorded line rhythm with the
    /// DFILE walk.
    fn zx81_visit(&mut self, line: u64, tick: Tick, ctx: &mut MachineCtx) {
        if line == 0 {
            self.fill_border();
            ctx.emit(TriggerEvent::Vblank);
        }
        let last = self.frame_lines.saturating_sub(1);
        if line == last {
            self.refresh_cached_frame();
            self.frames += 1;
        }
        if !self.zx81_slow {
            // The fast mode drives no INT and no NMI and publishes the
            // border-only frame.
            return;
        }
        let display = (ZX81_DISPLAY_FIRST..ZX81_DISPLAY_FIRST + ZX81_DISPLAY_LINES).contains(&line);
        if display != self.int_out {
            self.int_out = display;
            ctx.notify("int", level_of(display), tick);
        }
        let blank = !display;
        if blank != self.nmi_out {
            self.nmi_out = blank;
            ctx.notify("nmi", level_of(blank), tick);
        }
        if display {
            let row = (line - ZX81_DISPLAY_FIRST) / ZX81_LINES_PER_ROW;
            self.zx81_row(row, ctx);
        }
    }

    /// Paint one DFILE row of the ZX81 slow mode: the row bytes peek
    /// at `$4000 + row * 33` and the font peeks at `$1E00 + code * 8`.
    fn zx81_row(&mut self, row: u64, ctx: &mut MachineCtx) {
        for c in 0..CHARS_PER_ROW {
            let code = ctx.peek(
                "cpu",
                Address(SCREEN_PIX_BASE + row as u32 * DFILE_STRIDE + c),
            );
            let glyph = self.zx81_glyph(code, ctx);
            let x0 = (DISPLAY_LEFT + c * 8) as usize;
            let y0 = (DISPLAY_TOP + row as u32 * 8) as usize;
            for (r, bits) in glyph.iter().enumerate() {
                for b in 0..8u32 {
                    let on = (bits >> (7 - b)) & 1 != 0;
                    let rgb = if on { ZX_INK } else { ZX_PAPER };
                    let base = ((y0 + r) * FRAME_WIDTH as usize + x0 + b as usize) * 3;
                    self.screen[base..base + 3].copy_from_slice(&rgb);
                }
            }
        }
    }

    /// The glyph of one ZX81 character code as eight byte rows, the
    /// most significant bit being the leftmost pixel.
    fn zx81_glyph(&self, code: u8, ctx: &mut MachineCtx) -> [u8; 8] {
        let mut glyph = [0u8; 8];
        if code < 0x40 {
            // The text: the font ROM at $1E00.
            for r in 0..8u32 {
                glyph[r as usize] =
                    ctx.peek("cpu", Address(ZX81_FONT_BASE + u32::from(code) * 8 + r));
            }
        } else {
            // The block graphics: six pattern bits, with inverse display
            // from bit 7.
            let bits = code & 0x3F;
            for (r, glyph_row) in glyph.iter_mut().enumerate() {
                let mut byte = 0u8;
                for col in 0..8u32 {
                    let on = if r < 4 {
                        let bit = match col {
                            0..=1 => 0,
                            2..=3 => 1,
                            4..=5 => 2,
                            _ => 3,
                        };
                        bits & (1 << bit) != 0
                    } else {
                        let bit = if col < 4 { 4 } else { 5 };
                        bits & (1 << bit) != 0
                    };
                    if on {
                        byte |= 0x80 >> col;
                    }
                }
                *glyph_row = byte;
            }
        }
        if code & 0x80 != 0 {
            glyph.iter_mut().for_each(|b| *b = !*b);
        }
        glyph
    }
}

impl VideoSource for ZxUla {
    fn frame(&self) -> Option<&Frame> {
        Some(&self.cached_frame)
    }

    fn dump_layer(&self, layer: VideoLayer) -> Option<Vec<u8>> {
        match layer {
            VideoLayer::Framebuffer => Some(self.screen.clone()),
            // The 16-entry palette table: the eight non-bright colors
            // and the eight bright variants.
            VideoLayer::Palette if self.form == UlaForm::Spectrum => {
                let mut out = Vec::with_capacity(48);
                for row in &COLOR_NORMAL {
                    out.extend_from_slice(row);
                }
                for row in &COLOR_BRIGHT {
                    out.extend_from_slice(row);
                }
                Some(out)
            }
            _ => None,
        }
    }

    fn frames(&self) -> u64 {
        self.frames
    }
}

impl AudioSource for ZxUla {
    fn sample_rate(&self) -> u32 {
        match self.form {
            UlaForm::Spectrum => 3_500_000,
            _ => 3_250_000,
        }
    }

    fn take_samples(&mut self) -> Vec<i16> {
        std::mem::take(&mut self.samples)
    }
}

/// The keyboard matrix position of one key name, as the row index and
/// the column bit mask, or `None`.
fn key_pos(name: &str) -> Option<(usize, u8)> {
    for (i, key) in KEY_MATRIX.iter().enumerate() {
        if *key == name || (*key == "enter" && name == "return") {
            return Some((i / 5, 1u8 << (i % 5)));
        }
    }
    None
}

/// The flag form of one driven level.
fn level_of(high: bool) -> Level {
    if high { Level::High } else { Level::Low }
}

/// The little-endian `u64` of an eight-byte slice.
fn le_u64(bytes: &[u8]) -> u64 {
    let mut buf = [0u8; 8];
    buf.copy_from_slice(bytes);
    u64::from_le_bytes(buf)
}

/// The ink and paper colors of one attribute byte at the flash phase.
fn attr_colors(attr: u8, flash_on: bool) -> ([u8; 3], [u8; 3]) {
    let mut ink = attr & 0x07;
    let mut paper = (attr >> 3) & 0x07;
    if flash_on && attr & 0x80 != 0 {
        std::mem::swap(&mut ink, &mut paper);
    }
    let table = if attr & 0x40 != 0 {
        COLOR_BRIGHT
    } else {
        COLOR_NORMAL
    };
    (table[ink as usize], table[paper as usize])
}

/// The integer parameter of a device entry, or the default.
fn param_u64(device: &demu_config::model::DeviceInstance, key: &str, default: u64) -> u64 {
    device
        .params
        .get(key)
        .and_then(|v| v.as_integer())
        .map_or(default, |i| u64::try_from(i).unwrap_or(default))
}

/// True when a quirk key names its own device kind family and carries
/// a set flag.
fn quirk_flag(config: &SystemConfig, key: &str, family: &str) -> bool {
    if key.split('.').next() != Some(family) {
        return false;
    }
    config.quirks.get(key).is_some_and(|v| v.as_flag())
}

impl InputSink for ZxUla {
    fn inject(&mut self, event: &str) -> bool {
        let text = event.trim().to_ascii_lowercase();
        let (pressed, name) = if let Some((verb, rest)) = text.split_once(' ') {
            let pressed = match verb {
                "press" => true,
                "release" => false,
                _ => return false,
            };
            (pressed, rest)
        } else if let Some(base) = text.strip_suffix("-release") {
            (false, base)
        } else {
            (true, text.as_str())
        };
        let Some((row, mask)) = key_pos(name) else {
            return false;
        };
        if pressed {
            self.rows[row] |= mask;
        } else {
            self.rows[row] &= !mask;
        }
        true
    }
}

impl BusAccess for ZxUla {
    fn read(&mut self, _width: AccessWidth, address: Address, _tick: Tick) -> u32 {
        u32::from(self.port_read(address.0 & 0xFFFF))
    }

    fn write(&mut self, _width: AccessWidth, address: Address, value: u32, _tick: Tick) {
        self.port_write(address.0 & 0xFFFF, value as u8);
    }

    fn peek(&self, address: Address) -> u8 {
        self.port_read(address.0 & 0xFFFF)
    }

    fn decodes(&self, address: Address) -> bool {
        // The ULA answers every even port; the odd ports stay open for
        // the companion devices, and the `$7FFD` paging family sits
        // odd for the port values the software uses.
        (address.0 & 1) == 0
    }
}

impl Device for ZxUla {
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
        Some(self)
    }

    fn audio(&self) -> Option<&dyn AudioSource> {
        Some(self)
    }

    fn input_mut(&mut self) -> Option<&mut dyn InputSink> {
        Some(self)
    }

    fn reset(&mut self) {
        self.rows = [0; 8];
        self.fe_latch = 0;
        self.shadow_screen = false;
        self.flash = 0;
        self.samples.clear();
        for px in self.screen.as_chunks_mut::<3>().0 {
            px.copy_from_slice(&ZX_PAPER);
        }
        self.refresh_cached_frame();
        self.frames = 0;
        self.line = 0;
        self.int_out = false;
        self.nmi_out = false;
        self.cpu_halted = false;
        self.next = None;
        self.last_tick = None;
    }

    fn boot(&mut self, state: Option<&BootState>) {
        let _ = state;
        self.reset();
        self.next = Some(Tick(0));
    }

    fn snapshot(&self) -> Snapshot {
        // The fixed chunk: the `$FE` latch, the keyboard rows, the
        // flash phase, the frame and line counters, the line states,
        // and the shadow screen input.
        let mut out = Vec::with_capacity(ZXUL_LEN);
        out.extend_from_slice(b"ZXUL");
        out.push(1);
        out.push(self.fe_latch);
        out.extend_from_slice(&self.rows);
        out.push(self.flash);
        out.extend_from_slice(&self.frames.to_le_bytes());
        out.extend_from_slice(&self.line.to_le_bytes());
        out.push(
            u8::from(self.int_out)
                | (u8::from(self.nmi_out) << 1)
                | (u8::from(self.cpu_halted) << 2),
        );
        out.push(u8::from(self.shadow_screen));
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        if data.len() != ZXUL_LEN || &data[..4] != b"ZXUL" || data[4] != 1 {
            return Err(SnapshotError::Mismatch(format!(
                "zx ULA chunk must be {ZXUL_LEN} bytes"
            )));
        }
        self.fe_latch = data[5];
        self.rows.copy_from_slice(&data[6..14]);
        self.flash = data[14] % 32;
        self.frames = le_u64(&data[15..23]);
        self.line = le_u64(&data[23..31]) % self.frame_lines;
        self.int_out = data[31] & 0x01 != 0;
        self.nmi_out = data[31] & 0x02 != 0;
        self.cpu_halted = data[31] & 0x04 != 0;
        self.shadow_screen = data[32] != 0;
        // The tick guard declines a device with no next visit, so the
        // restored ULA schedules at its own next scanline boundary;
        // the run loop stamps the correct absolute tick on dispatch.
        self.next = Some(Tick(0));
        self.last_tick = None;
        Ok(())
    }

    fn notify_line(&mut self, line: &str, level: Level, _tick: Tick) {
        match line {
            // The CPU announces its halt over the incoming halt line;
            // the ULA records the state for the slow-mode rhythm.
            "halt" => self.cpu_halted = level == Level::High,
            // The bank RAM controller announces the shadow screen
            // select over the shadow line.
            "shadow" => self.shadow_screen = level == Level::High,
            _ => {}
        }
    }

    fn next_event(&self) -> Option<Tick> {
        self.next
    }

    fn tick(&mut self, tick: Tick, ctx: &mut MachineCtx) {
        if self.last_tick.is_some_and(|t| t.0 >= tick.0) || self.next.is_none_or(|n| n.0 > tick.0) {
            // The device runs at its own scanline boundary only.
            return;
        }
        self.last_tick = Some(tick);
        // The beeper takes one sample per scanline visit.
        self.push_sample();
        let line = self.line;
        match self.form {
            UlaForm::Spectrum => self.spectrum_visit(line, tick, ctx),
            UlaForm::Zx80 => self.zx80_visit(line, tick, ctx),
            UlaForm::Zx81 => self.zx81_visit(line, tick, ctx),
        }
        self.line = (self.line + 1) % self.frame_lines;
        self.next = Some(Tick(tick.0 + self.line_t));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use demu_config::model::{
        Bus as CBus, ClockDomain, Connection, ConnectionDirection, ConnectionSemantics, QuirkValue,
        Window as CWindow,
    };
    use demu_config::newtypes::{DeviceName, KindName};
    use demu_core::machine::Machine;
    use std::collections::BTreeMap;

    const LINE_T: u64 = 224;
    const FRAME_LINES: u64 = 312;

    fn config(quirks: &[(&str, bool)]) -> SystemConfig {
        let mut q = BTreeMap::new();
        for (k, v) in quirks {
            q.insert((*k).to_string(), QuirkValue::Bool(*v));
        }
        SystemConfig {
            name: "test".to_string(),
            triple: "test".to_string(),
            description: String::new(),
            clocks: BTreeMap::new(),
            devices: Vec::new(),
            devices_by_name: BTreeMap::new(),
            buses: Vec::new(),
            windows: Vec::new(),
            connections: Vec::new(),
            signals: Vec::new(),
            boot: BTreeMap::new(),
            quirks: q,
        }
    }

    fn instance(form: &str, extras: &[(&str, toml::Value)]) -> demu_config::model::DeviceInstance {
        let mut params = BTreeMap::new();
        params.insert("form".to_string(), toml::Value::String(form.to_string()));
        for (k, v) in extras {
            params.insert((*k).to_string(), v.clone());
        }
        demu_config::model::DeviceInstance {
            name: DeviceName::new("ula").expect("name"),
            kind: KindName::new("zx.ula").expect("kind"),
            domain: "main".to_string(),
            params,
            dividers: BTreeMap::new(),
        }
    }

    fn spec(shadow: bool) -> ZxUla {
        let extras: Vec<(&str, toml::Value)> = if shadow {
            vec![("shadow_ram", toml::Value::Boolean(true))]
        } else {
            Vec::new()
        };
        ZxUla::new(&instance("spectrum", &extras), &config(&[])).expect("ula")
    }

    fn zx81(slow: bool) -> ZxUla {
        ZxUla::new(&instance("zx81", &[]), &config(&[("zx.zx81_slow", slow)])).expect("ula")
    }

    /// Run one device visit under an empty context: the notifications
    /// carry no routes and the peeks read the open-bus form.
    fn visit(d: &mut ZxUla, tick: Tick) {
        let mut before: Vec<Box<dyn Device>> = Vec::new();
        let mut after: Vec<Box<dyn Device>> = Vec::new();
        let mut buses: Vec<demu_core::Bus> = Vec::new();
        let mut scratch: Vec<demu_core::DecodeHit> = Vec::new();
        let mut events: Vec<TriggerEvent> = Vec::new();
        let mut dirty: Vec<usize> = Vec::new();
        let index = Default::default();
        let mut ctx = MachineCtx::new(
            demu_core::device::DeviceSlices {
                before: before.as_mut_slice(),
                after: after.as_mut_slice(),
                caller_slot: 0,
            },
            demu_core::device::BusBank {
                buses: buses.as_mut_slice(),
                index: &index,
            },
            &[],
            &mut scratch,
            demu_core::device::EventBuffers {
                events: &mut events,
                dirty: &mut dirty,
            },
            false,
        );
        d.tick(tick, &mut ctx);
    }

    fn pixel_of(frame: &Frame, y: usize, x: usize) -> [u8; 3] {
        let base = (y * FRAME_WIDTH as usize + x) * 3;
        [
            frame.pixels[base],
            frame.pixels[base + 1],
            frame.pixels[base + 2],
        ]
    }

    fn entry_name(name: &str) -> DeviceName {
        DeviceName::new(name).expect("name")
    }

    fn window(name: &str, bus: &str, device: &str, base: u32, size: u32) -> CWindow {
        CWindow {
            name: name.to_string(),
            bus: bus.to_string(),
            device: entry_name(device),
            base,
            size,
            kind: None,
            mirror: None,
            overlay: None,
            overlay_read_priority: None,
            overlay_write_priority: None,
            conflict: None,
        }
    }

    fn connection(from: &str, to: &str) -> Connection {
        Connection {
            from: demu_config::newtypes::LinePath::parse(from).expect("route"),
            to: demu_config::newtypes::LinePath::parse(to).expect("route"),
            semantics: ConnectionSemantics::Irq,
            direction: ConnectionDirection::Forward,
        }
    }

    /// A recorder device: it collects the notified line edges of the
    /// machine's connections.
    struct LineRecorder {
        name: String,
        lines: Vec<(String, Level, u64)>,
    }

    impl LineRecorder {
        fn new(name: &str) -> Self {
            Self {
                name: name.to_string(),
                lines: Vec::new(),
            }
        }
    }

    impl Device for LineRecorder {
        fn name(&self) -> &str {
            &self.name
        }

        fn as_any(&self) -> &dyn std::any::Any {
            self
        }

        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }

        fn reset(&mut self) {
            self.lines.clear();
        }

        fn boot(&mut self, _state: Option<&BootState>) {
            self.lines.clear();
        }

        fn snapshot(&self) -> Snapshot {
            Vec::new()
        }

        fn restore(&mut self, _data: &[u8]) -> Result<(), SnapshotError> {
            Ok(())
        }

        fn notify_line(&mut self, line: &str, level: Level, tick: Tick) {
            self.lines.push((line.to_string(), level, tick.0));
        }

        fn tick(&mut self, _tick: Tick, _ctx: &mut MachineCtx) {}
    }

    /// The Spectrum test machine: the ULA with the port window, the
    /// screen RAM, the home-window RAM, and the recorder on the INT
    /// and NMI lines. The flag only marks the ULA's `shadow_ram`
    /// parameter; the memory windows stay on the static RAM devices.
    fn spectrum_machine(shadow: bool) -> Machine {
        let mut params = BTreeMap::new();
        params.insert(
            "form".to_string(),
            toml::Value::String("spectrum".to_string()),
        );
        if shadow {
            params.insert("shadow_ram".to_string(), toml::Value::Boolean(true));
        }
        let ula = demu_config::model::DeviceInstance {
            name: entry_name("ula"),
            kind: KindName::new("zx.ula").expect("kind"),
            domain: "main".to_string(),
            params,
            dividers: BTreeMap::new(),
        };
        let mut ram_params = BTreeMap::new();
        ram_params.insert("size".to_string(), toml::Value::Integer(0x8000));
        let ram = demu_config::model::DeviceInstance {
            name: entry_name("ram"),
            kind: KindName::new("smoke.ram").expect("kind"),
            domain: "main".to_string(),
            params: ram_params,
            dividers: BTreeMap::new(),
        };
        let mut home_params = BTreeMap::new();
        home_params.insert("size".to_string(), toml::Value::Integer(0x4000));
        let homeram = demu_config::model::DeviceInstance {
            name: entry_name("homeram"),
            kind: KindName::new("smoke.ram").expect("kind"),
            domain: "main".to_string(),
            params: home_params,
            dividers: BTreeMap::new(),
        };
        let lines = demu_config::model::DeviceInstance {
            name: entry_name("lines"),
            kind: KindName::new("test.lines").expect("kind"),
            domain: "main".to_string(),
            params: BTreeMap::new(),
            dividers: BTreeMap::new(),
        };
        let cp = demu_config::model::DeviceInstance {
            name: entry_name("cp"),
            kind: KindName::new("smoke.cpu").expect("kind"),
            domain: "main".to_string(),
            params: BTreeMap::new(),
            dividers: BTreeMap::new(),
        };
        let mut clocks = BTreeMap::new();
        clocks.insert(
            "main".to_string(),
            ClockDomain {
                name: "main".to_string(),
                frequency: demu_config::newtypes::Frequency::new(3_500_000, 1).expect("freq"),
                origin: "the machine master".to_string(),
            },
        );
        let mut by_name = BTreeMap::new();
        by_name.insert(ula.name.clone(), 0usize);
        by_name.insert(ram.name.clone(), 1usize);
        by_name.insert(homeram.name.clone(), 2usize);
        by_name.insert(lines.name.clone(), 3usize);
        by_name.insert(cp.name.clone(), 4usize);
        let windows = vec![
            window("ram", "cpu", "ram", 0x4000, 0x8000),
            window("homeram", "cpu", "homeram", 0xC000, 0x4000),
            window("ulaio", "io", "ula", 0x0000, 0x10000),
        ];
        let config = demu_config::model::SystemConfig {
            name: "spec-test".to_string(),
            triple: "test".to_string(),
            description: "the Spectrum ULA test machine".to_string(),
            clocks,
            devices: vec![ula, ram, homeram, lines, cp],
            devices_by_name: by_name,
            buses: vec![
                CBus {
                    name: "cpu".to_string(),
                    address_bits: 16,
                    owner: None,
                    parent: None,
                },
                CBus {
                    name: "io".to_string(),
                    address_bits: 16,
                    owner: None,
                    parent: None,
                },
            ],
            windows,
            connections: vec![
                connection("ula.int", "lines.int"),
                connection("ula.nmi", "lines.nmi"),
            ],
            signals: Vec::new(),
            boot: BTreeMap::new(),
            quirks: BTreeMap::new(),
        };
        let mut ctors = demu_core::smoke_ctors();
        ctors.register("zx.ula", Box::new(|d, c| Ok(Box::new(ZxUla::new(d, c)?))));
        ctors.register(
            "test.lines",
            Box::new(|d, _| Ok(Box::new(LineRecorder::new(d.name.as_ref())))),
        );
        let mut machine = Machine::assemble(config, &ctors).expect("assembly");
        machine.boot();
        machine
    }

    /// The ZX81 test machine: the ULA, a boot ROM image with the font
    /// bytes at `$1E00`, and the DFILE RAM.
    fn zx81_machine(slow: bool) -> Machine {
        let mut rom_bytes = vec![0u8; 0x2000];
        // The font of the text code 1: the leftmost pixel of every row.
        rom_bytes[0x1E00 + 8] = 0x80;
        let ula = demu_config::model::DeviceInstance {
            name: entry_name("ula"),
            kind: KindName::new("zx.ula").expect("kind"),
            domain: "main".to_string(),
            params: [("form".to_string(), toml::Value::String("zx81".to_string()))]
                .into_iter()
                .collect(),
            dividers: BTreeMap::new(),
        };
        let mut rom_params = BTreeMap::new();
        rom_params.insert("size".to_string(), toml::Value::Integer(0x2000));
        let rom = demu_config::model::DeviceInstance {
            name: entry_name("rom"),
            kind: KindName::new("smoke.rom").expect("kind"),
            domain: "main".to_string(),
            params: rom_params,
            dividers: BTreeMap::new(),
        };
        let mut ram_params = BTreeMap::new();
        ram_params.insert("size".to_string(), toml::Value::Integer(0x2000));
        let ram = demu_config::model::DeviceInstance {
            name: entry_name("ram"),
            kind: KindName::new("smoke.ram").expect("kind"),
            domain: "main".to_string(),
            params: ram_params,
            dividers: BTreeMap::new(),
        };
        let lines = demu_config::model::DeviceInstance {
            name: entry_name("lines"),
            kind: KindName::new("test.lines").expect("kind"),
            domain: "main".to_string(),
            params: BTreeMap::new(),
            dividers: BTreeMap::new(),
        };
        let cp = demu_config::model::DeviceInstance {
            name: entry_name("cp"),
            kind: KindName::new("smoke.cpu").expect("kind"),
            domain: "main".to_string(),
            params: BTreeMap::new(),
            dividers: BTreeMap::new(),
        };
        let mut clocks = BTreeMap::new();
        clocks.insert(
            "main".to_string(),
            ClockDomain {
                name: "main".to_string(),
                frequency: demu_config::newtypes::Frequency::new(3_250_000, 1).expect("freq"),
                origin: "the machine master".to_string(),
            },
        );
        let mut by_name = BTreeMap::new();
        by_name.insert(ula.name.clone(), 0usize);
        by_name.insert(rom.name.clone(), 1usize);
        by_name.insert(ram.name.clone(), 2usize);
        by_name.insert(lines.name.clone(), 3usize);
        by_name.insert(cp.name.clone(), 4usize);
        let mut quirks = BTreeMap::new();
        if slow {
            quirks.insert("zx.zx81_slow".to_string(), QuirkValue::Bool(true));
        }
        let mut boot = BTreeMap::new();
        boot.insert(entry_name("rom"), BootState::Hex { bytes: rom_bytes });
        let config = demu_config::model::SystemConfig {
            name: "zx81-test".to_string(),
            triple: "test".to_string(),
            description: "the ZX81 ULA test machine".to_string(),
            clocks,
            devices: vec![ula, rom, ram, lines, cp],
            devices_by_name: by_name,
            buses: vec![
                CBus {
                    name: "cpu".to_string(),
                    address_bits: 16,
                    owner: None,
                    parent: None,
                },
                CBus {
                    name: "io".to_string(),
                    address_bits: 16,
                    owner: None,
                    parent: None,
                },
            ],
            windows: vec![
                window("romw", "cpu", "rom", 0x0000, 0x2000),
                window("ramw", "cpu", "ram", 0x4000, 0x2000),
                window("ulaio", "io", "ula", 0x0000, 0x10000),
            ],
            connections: vec![
                connection("ula.int", "lines.int"),
                connection("ula.nmi", "lines.nmi"),
            ],
            signals: Vec::new(),
            boot,
            quirks,
        };
        let mut ctors = demu_core::smoke_ctors();
        ctors.register("zx.ula", Box::new(|d, c| Ok(Box::new(ZxUla::new(d, c)?))));
        ctors.register(
            "test.lines",
            Box::new(|d, _| Ok(Box::new(LineRecorder::new(d.name.as_ref())))),
        );
        let mut machine = Machine::assemble(config, &ctors).expect("assembly");
        machine.boot();
        machine
    }

    /// The ZX80 test machine: the ULA, the recorder, and the CPU shim.
    fn zx80_machine() -> Machine {
        let ula = demu_config::model::DeviceInstance {
            name: entry_name("ula"),
            kind: KindName::new("zx.ula").expect("kind"),
            domain: "main".to_string(),
            params: [("form".to_string(), toml::Value::String("zx80".to_string()))]
                .into_iter()
                .collect(),
            dividers: BTreeMap::new(),
        };

        let lines = demu_config::model::DeviceInstance {
            name: entry_name("lines"),
            kind: KindName::new("test.lines").expect("kind"),
            domain: "main".to_string(),
            params: BTreeMap::new(),
            dividers: BTreeMap::new(),
        };
        let cp = demu_config::model::DeviceInstance {
            name: entry_name("cp"),
            kind: KindName::new("smoke.cpu").expect("kind"),
            domain: "main".to_string(),
            params: BTreeMap::new(),
            dividers: BTreeMap::new(),
        };
        let mut clocks = BTreeMap::new();
        clocks.insert(
            "main".to_string(),
            ClockDomain {
                name: "main".to_string(),
                frequency: demu_config::newtypes::Frequency::new(3_250_000, 1).expect("freq"),
                origin: "the machine master".to_string(),
            },
        );
        let mut by_name = BTreeMap::new();
        by_name.insert(ula.name.clone(), 0usize);
        by_name.insert(lines.name.clone(), 1usize);
        by_name.insert(cp.name.clone(), 2usize);
        let config = demu_config::model::SystemConfig {
            name: "zx80-test".to_string(),
            triple: "test".to_string(),
            description: "the ZX80 ULA test machine".to_string(),
            clocks,
            devices: vec![ula, lines, cp],
            devices_by_name: by_name,
            buses: vec![
                CBus {
                    name: "cpu".to_string(),
                    address_bits: 16,
                    owner: None,
                    parent: None,
                },
                CBus {
                    name: "io".to_string(),
                    address_bits: 16,
                    owner: None,
                    parent: None,
                },
            ],
            windows: vec![window("ulaio", "io", "ula", 0x0000, 0x10000)],
            connections: vec![connection("ula.nmi", "lines.nmi")],
            signals: Vec::new(),
            boot: BTreeMap::new(),
            quirks: BTreeMap::new(),
        };
        let mut ctors = demu_core::smoke_ctors();
        ctors.register("zx.ula", Box::new(|d, c| Ok(Box::new(ZxUla::new(d, c)?))));
        ctors.register(
            "test.lines",
            Box::new(|d, _| Ok(Box::new(LineRecorder::new(d.name.as_ref())))),
        );
        let mut machine = Machine::assemble(config, &ctors).expect("assembly");
        machine.boot();
        machine
    }

    fn recorder(machine: &mut Machine, slot: usize) -> &mut LineRecorder {
        machine.devices[slot]
            .as_any_mut()
            .downcast_mut::<LineRecorder>()
            .expect("recorder")
    }

    fn ula_of(machine: &mut Machine) -> &mut ZxUla {
        machine
            .device_by_name_mut("ula")
            .expect("device")
            .as_any_mut()
            .downcast_mut::<ZxUla>()
            .expect("the ULA")
    }

    // ---------------------------------------------------------------
    // Keyboard port and event grammar

    #[test]
    fn the_keyboard_rows_merge_and_read_low_when_pressed() {
        let mut u = spec(false);
        assert_eq!(
            u.peek(Address(0x01FE)),
            0xBF,
            "the idle row read pulls the spare bits high"
        );
        assert!(u.inject("press 5"), "the digit 5 sits in row 0 col 4");
        assert_eq!(u.peek(Address(0x01FE)), 0xAF, "the pressed bit reads low");
        assert!(u.inject("press p"), "the letter p sits in row 4 col 0");
        assert_eq!(
            u.peek(Address(0x11FE)),
            0xAE,
            "the set rows merge their pressed bits"
        );
        assert!(u.inject("release 5"));
        assert_eq!(u.peek(Address(0x01FE)), 0xBF);
        // The no-row form keeps the spare bits.
        assert_eq!(u.peek(Address(0x00FE)), 0xBF);
    }

    #[test]
    fn the_event_grammar_normalizes_and_rejects() {
        let mut u = spec(false);
        assert!(u.inject("press SPACE"), "case-insensitive names");
        assert_eq!(u.rows[6] & 0x01, 0x01);
        assert!(u.inject("release space"));
        assert_eq!(u.rows[6], 0);
        assert!(u.inject("press return"), "the return alias of enter");
        assert_eq!(u.rows[5] & 0x01, 0x01);
        assert!(!u.inject("press F5"));
        assert!(!u.inject("toggle q"));
        assert!(!u.inject("release p9"));
    }

    #[test]
    fn the_port_write_latches_border_and_beeper_bits() {
        let mut spec_u = spec(false);
        spec_u.write(AccessWidth::Byte, Address(0x00FE), 0x5A, Tick(0));
        // Spectrum: bits 1 to 3 carry the border and bit 4 the beeper.
        assert_eq!(spec_u.border_index(), 5);
        assert!(spec_u.beeper_on());
        spec_u.write(AccessWidth::Byte, Address(0x00FE), 0x04, Tick(0));
        assert_eq!(spec_u.border_index(), 2);
        assert!(!spec_u.beeper_on());
        // ZX80/ZX81: bit 0 drives the speaker and the border stays black.
        let mut zx = zx81(false);
        zx.write(AccessWidth::Byte, Address(0x00FE), 0x01, Tick(0));
        assert!(zx.beeper_on());
        assert_eq!(zx.border_index(), 0);
        assert_eq!(zx.sample_rate(), 3_250_000);
        assert_eq!(spec_u.sample_rate(), 3_500_000);
    }

    #[test]
    fn the_beeper_takes_one_sample_per_visit_and_caps() {
        let mut u = spec(false);
        u.boot(None);
        u.write(AccessWidth::Byte, Address(0x00FE), 0x10, Tick(0));
        visit(&mut u, Tick(0));
        visit(&mut u, Tick(224));
        assert_eq!(u.take_samples(), vec![8000, 8000], "the drive level");
        u.write(AccessWidth::Byte, Address(0x00FE), 0x00, Tick(224));
        visit(&mut u, Tick(448));
        assert_eq!(u.take_samples(), vec![-8000]);
        // The buffer drains to empty and caps at $8000 samples.
        for i in 0..(SAMPLE_CAP + 8) as u64 {
            visit(&mut u, Tick(448 + (i + 1) * 224));
        }
        assert_eq!(u.take_samples().len(), SAMPLE_CAP);
        assert!(u.take_samples().is_empty());
    }

    #[test]
    fn decodes_gates_the_even_ports_only() {
        let mut u = spec(true);
        assert!(
            u.bus().expect("bus").decodes(Address(0x00FE)),
            "an even port decodes"
        );
        assert!(
            u.bus().expect("bus").decodes(Address(0x0010)),
            "any even port decodes"
        );
        assert!(
            !u.bus().expect("bus").decodes(Address(0x7FFD)),
            "the paging port sits odd for the ULA"
        );
        assert!(
            !u.bus().expect("bus").decodes(Address(0x00FD)),
            "an odd port stays out"
        );
        // The odd ports read the open-bus form.
        assert_eq!(u.read(AccessWidth::Byte, Address(0x00FD), Tick(0)), 0xFF);
        assert_eq!(u.peek(Address(0x7FFD)), 0xFF);
    }

    #[test]
    fn the_shadow_line_sets_the_render_input() {
        let mut u = spec(true);
        u.notify_line("shadow", Level::High, Tick(0));
        assert!(u.shadow_screen, "the high edge selects the shadow span");
        u.notify_line("shadow", Level::Low, Tick(0));
        assert!(!u.shadow_screen, "the low edge returns the plain span");
        u.notify_line("shadow", Level::High, Tick(0));
        u.reset();
        assert!(!u.shadow_screen, "the reset clears the line input");
        // Other line names change nothing.
        u.notify_line("halt", Level::High, Tick(0));
        assert!(!u.shadow_screen);
    }

    #[test]
    fn the_halt_line_flips_the_record_state() {
        let mut u = zx81(true);
        u.notify_line("halt", Level::High, Tick(0));
        assert!(u.cpu_halted);
        u.notify_line("halt", Level::Low, Tick(1));
        assert!(!u.cpu_halted);
        // The line name matches the routed name only.
        assert!(u.zx81_slow);
    }

    #[test]
    fn the_frame_timing_reads_the_parameters() {
        let extras = vec![
            ("line_t", toml::Value::Integer(228)),
            ("frame_lines", toml::Value::Integer(311)),
            ("int_lines", toml::Value::Integer(4)),
        ];
        let mut u = ZxUla::new(&instance("spectrum", &extras), &config(&[])).expect("ula");
        u.boot(None);
        assert_eq!(u.next_event(), Some(Tick(0)));
        visit(&mut u, Tick(0));
        assert_eq!(u.line, 1);
        assert!(u.int_out, "the pulse rises at line 0");
        assert_eq!(u.next_event(), Some(Tick(228)));
        visit(&mut u, Tick(228));
        assert_eq!(u.line, 2);
        visit(&mut u, Tick(456));
        assert_eq!(u.line, 3);
        visit(&mut u, Tick(684));
        assert!(u.int_out, "pulse lines 0 to 3");
        // A visit off the cadence runs nothing.
        visit(&mut u, Tick(700));
        assert_eq!(u.line, 4);
        assert!(u.int_out);
        // Line 4 ends the pulse at the int_lines scanline count.
        visit(&mut u, Tick(4 * 228));
        assert!(!u.int_out);
        // The 311-line frame wraps at line 310: the remaining line
        // visits walk the counter to the wrap.
        let mut at = 5 * 228;
        for _ in 5..310 {
            visit(&mut u, Tick(at));
            at += 228;
        }
        assert_eq!(u.line, 310);
        visit(&mut u, Tick(at));
        assert_eq!(u.line, 0, "the frame wrapped");
    }

    #[test]
    fn the_int_pulse_and_the_frame_trigger_run_on_the_machine() {
        let mut m = spectrum_machine(false);
        // The hook stops the run at the first frame trigger, which the
        // line-0 visit emits after the compose.
        m.run_hook(
            Tick(u64::MAX),
            &mut |e| matches!(e, TriggerEvent::Vblank),
            true,
        );
        // The second run lets the pulse fall inside the frame.
        m.run_hook(Tick(LINE_T + 5), &mut |_| false, true);
        assert_eq!(
            recorder(&mut m, 3).lines,
            vec![
                ("int".to_string(), Level::High, 0),
                ("int".to_string(), Level::Low, LINE_T)
            ]
        );
        assert_eq!(
            m.device_by_name("ula")
                .expect("device")
                .video()
                .expect("video")
                .frames(),
            1,
            "the composed frame published"
        );
    }

    #[test]
    fn snapshot_round_trips_with_a_fixed_length() {
        let mut u = spec(true);
        u.boot(None);
        u.write(AccessWidth::Byte, Address(0x00FE), 0x14, Tick(0));
        u.inject("press space");
        u.notify_line("halt", Level::High, Tick(0));
        u.notify_line("shadow", Level::High, Tick(0));
        visit(&mut u, Tick(0));
        let snap = u.snapshot();
        assert_eq!(snap.len(), ZXUL_LEN);
        let mut twin = spec(true);
        twin.restore(&snap).expect("restore");
        assert_eq!(twin.snapshot(), snap);
        assert!(twin.cpu_halted, "the halt record rides");
        assert!(twin.shadow_screen, "the shadow input rides");
        assert_eq!(twin.border_index(), 2, "the border bits ride");
        assert_eq!(twin.rows[6], 0x01, "the pressed key rides");
        assert_eq!(twin.flash, 1, "the flash phase rides");
        assert_eq!(twin.frames, 1, "the frame counter rides");
        assert_eq!(
            twin.next_event(),
            Some(Tick(0)),
            "the restored schedule re-arms at the next scanline boundary"
        );
        // The chunk rejects a short body, a wrong tag, and a wrong
        // version.
        let mut any = spec(false);
        assert!(any.restore(&snap[..snap.len() - 1]).is_err());
        let mut retag = snap.clone();
        retag[2] = b'X';
        assert!(any.restore(&retag).is_err());
        let mut reversion = snap.clone();
        reversion[4] = 2;
        assert!(any.restore(&reversion).is_err());
    }

    #[test]
    fn the_spectrum_frame_renders_ink_paper_and_border() {
        let mut m = spectrum_machine(false);
        assert!(m.debug_write("cpu", Address(0x4000), 0xFF));
        assert!(m.debug_write("cpu", Address(0x4001), 0xF0));
        // The attributes of the first two columns: ink 5, paper 0.
        assert!(m.debug_write("cpu", Address(0x5800), 0x05));
        assert!(m.debug_write("cpu", Address(0x5801), 0x05));
        assert!(m.debug_write("io", Address(0x00FE), 0x02));
        m.run_until(Tick(LINE_T * FRAME_LINES));
        m.run_until(Tick(LINE_T * FRAME_LINES));
        let frame = m
            .device_by_name("ula")
            .expect("device")
            .video()
            .expect("video")
            .frame()
            .expect("produced")
            .clone();
        // The border corner reads the latched blue.
        assert_eq!(pixel_of(&frame, 0, 0), [0, 0, 205]);
        // The display starts at the row 24: ink cyan from the
        // attribute.
        assert_eq!(pixel_of(&frame, 24, 32), [0, 205, 205]);
        // The second byte splits ink and paper on the bit pattern: the
        // four high bits ink, the four low bits paper.
        assert_eq!(pixel_of(&frame, 24, 40), [0, 205, 205]);
        assert_eq!(pixel_of(&frame, 24, 44), [0, 0, 0], "the paper bit");
        // The second row block keeps the attribute of its own row.
        assert_eq!(pixel_of(&frame, 32, 32), [0, 0, 0]);
    }

    #[test]
    fn the_shadow_screen_renders_the_banked_span() {
        let mut m = spectrum_machine(true);
        // The home-window RAM holds the shadow screen bytes.
        assert!(m.debug_write("cpu", Address(0xC000), 0xFF));
        assert!(m.debug_write("cpu", Address(0xD800), 0x05));
        assert!(m.debug_write("io", Address(0x00FE), 0x02));
        ula_of(&mut m).notify_line("shadow", Level::High, Tick(0));
        m.run_until(Tick(LINE_T * FRAME_LINES));
        let frame = m
            .device_by_name("ula")
            .expect("device")
            .video()
            .expect("video")
            .frame()
            .expect("produced")
            .clone();
        assert_eq!(pixel_of(&frame, 24, 32), [0, 205, 205], "the shadow ink");
        assert_eq!(pixel_of(&frame, 0, 0), [0, 0, 205], "the border");
        // The shadow input of a paging-less ULA keeps the published
        // screen span.
        let mut plain = spectrum_machine(false);
        assert!(plain.debug_write("cpu", Address(0xC000), 0xFF));
        assert!(plain.debug_write("cpu", Address(0xD800), 0x05));
        assert!(plain.debug_write("io", Address(0x00FE), 0x02));
        ula_of(&mut plain).notify_line("shadow", Level::High, Tick(0));
        plain.run_until(Tick(LINE_T * FRAME_LINES));
        let frame = plain
            .device_by_name("ula")
            .expect("device")
            .video()
            .expect("video")
            .frame()
            .expect("produced")
            .clone();
        assert_eq!(
            pixel_of(&frame, 24, 32),
            [0, 0, 0],
            "the plain span stays black"
        );
    }

    #[test]
    fn the_zx80_form_pulses_nmi_each_frame() {
        let mut m = zx80_machine();
        m.run_until(Tick(LINE_T * 2));
        assert_eq!(
            recorder(&mut m, 1).lines,
            vec![
                ("nmi".to_string(), Level::High, 0),
                ("nmi".to_string(), Level::Low, LINE_T)
            ]
        );
    }

    #[test]
    fn the_zx81_slow_mode_walks_the_dfile_with_the_halt_lines() {
        let mut m = zx81_machine(true);
        assert!(m.debug_write("cpu", Address(0x4000), 0x01));
        {
            let u = ula_of(&mut m);
            u.notify_line("halt", Level::High, Tick(0));
        }
        // Run to the end of the frame: the display rows painted and
        // the frame composed at the last line.
        m.run_until(Tick(LINE_T * (FRAME_LINES - 1) + 1));
        assert_eq!(
            recorder(&mut m, 3).lines,
            vec![
                ("nmi".to_string(), Level::High, 0),
                ("int".to_string(), Level::High, ZX81_DISPLAY_FIRST * LINE_T),
                ("nmi".to_string(), Level::Low, ZX81_DISPLAY_FIRST * LINE_T),
                (
                    "int".to_string(),
                    Level::Low,
                    (ZX81_DISPLAY_FIRST + ZX81_DISPLAY_LINES) * LINE_T
                ),
                (
                    "nmi".to_string(),
                    Level::High,
                    (ZX81_DISPLAY_FIRST + ZX81_DISPLAY_LINES) * LINE_T
                ),
            ]
        );
        // The first DFILE row painted the font glyph of code 1: the
        // leftmost display pixel reads white over black.
        let frame = m
            .device_by_name("ula")
            .expect("device")
            .video()
            .expect("video")
            .frame()
            .expect("produced")
            .clone();
        assert_eq!(pixel_of(&frame, 24, 32), [255, 255, 255]);
        assert_eq!(pixel_of(&frame, 24, 33), [0, 0, 0]);
        assert!(ula_of(&mut m).cpu_halted, "the record rides too");
    }

    #[test]
    fn the_zx81_fast_mode_drives_no_lines_and_publishes_border() {
        let mut m = zx81_machine(false);
        m.run_until(Tick(LINE_T * (FRAME_LINES - 1) + 1));
        assert!(
            recorder(&mut m, 3).lines.is_empty(),
            "the fast mode notified nothing"
        );
        let frame = m
            .device_by_name("ula")
            .expect("device")
            .video()
            .expect("video")
            .frame()
            .expect("produced")
            .clone();
        assert_eq!(pixel_of(&frame, 24, 32), [0, 0, 0], "the border-only form");
        assert_eq!(
            m.device_by_name("ula")
                .expect("device")
                .video()
                .expect("video")
                .frames(),
            1
        );
    }
}
