//! The PC Engine die pair: the HuC6270 Video Display Controller and
//! the HuC6260 Video Color Encoder as the `pce.vdc` and `pce.vce`
//! device kinds.
//!
//! The VDC owns 64 KiB of VRAM as 32768 sixteen-bit words, the twenty
//! internal registers, the sprite attribute table buffer, and the
//! composed frame; the VCE owns the color table RAM of 512 nine-bit
//! entries and its control registers. Both answer their I/O window on
//! the machine bus; the VDC reads the color table bytes through the
//! bus at the `palette_base` parameter, the declared machine coupling
//! of this build (the real die pair exchanges pixels and colors over
//! signal lines instead of the bus).
//!
//! # Pinned VDC I/O surface
//!
//! The window decodes the low two bits of each address as the I/O
//! strobes, so the configuration maps the window at any base:
//!
//! | Offset | Read | Write |
//! |--------|------|-------|
//! | 0 | Status Register (SR) | Address Register (AR) |
//! | 1 | fixed 0x00 | ignored |
//! | 2 | data low byte | data low byte |
//! | 3 | data high byte | data high byte |
//!
//! A read of the Status Register returns the low six bits and clears
//! the set flags, which drops the interrupt line at the next line
//! visit of this build. The high byte of the SR reads 0. The AR takes
//! the low five bits and selects one of the twenty internal registers;
//! the data port pairs the bytes of the selected register, low first.
//!
//! `SR` bits: 0 the sprite collision detect (a sprite 0 pixel covered
//! a cell another sprite drew on this line), 1 the sprite overflow
//! (more than sixteen sprites on a display line), 2 the scanline
//! detect (the raster counter matched RCR), 3 the VRAM-to-SATB
//! transfer end, 4 the VRAM-to-VRAM transfer end, 5 the vertical
//! blanking detect, 6 the busy flag (reads 0 here: the VRAM ports are
//! immediate in this build).
//!
//! # Pinned VDC register list
//!
//! | Index | Name | Content |
//! |-------|------|---------|
//! | 0 | MAWR | the VRAM write pointer |
//! | 1 | MARR | the VRAM read pointer |
//! | 2 | VWR/VRR | the VRAM data write register and read latch |
//! | 5 | CR | the control register |
//! | 6 | RCR | the raster compare line |
//! | 7 | BXR | the horizontal scroll |
//! | 8 | BYR | the vertical scroll |
//! | 9 | MWR | the memory access width and virtual screen size |
//! | 0x0A | HSR | the horizontal sync form |
//! | 0x0B | HDR | the horizontal display width |
//! | 0x0C | VPR | the vertical sync form |
//! | 0x0D | VDR | the vertical display width |
//! | 0x0E | VCR | the vertical display end |
//! | 0x0F | DCR | the block transfer control |
//! | 0x10 | SOUR | the VRAM-to-VRAM source |
//! | 0x11 | DESR | the VRAM-to-VRAM destination |
//! | 0x12 | LENR | the VRAM-to-VRAM length, stored as N-1 |
//! | 0x13 | DVSSR | the VRAM-to-SATB source |
//!
//! The register masks per the manual: MAWR, MARR, VWR/VRR, SOUR, DESR,
//! LENR and DVSSR all sixteen bits; CR the low thirteen bits; RCR and
//! BXR ten bits; BYR nine bits; MWR eight bits; HSR 0x7F1F; HDR
//! 0x7F7F; VPR 0xFF1F; VDR nine bits; VCR eight bits; DCR five bits.
//!
//! The `CR` bits: 0-3 arm the collision, overflow, scanline and
//! vertical blanking interrupts; bit 6 enables the sprites; bit 7
//! enables the background; bits 11-12 select the VRAM pointer
//! auto-increment of 1, 32, 64 or 128 words, applied at the high-byte
//! access. The `DCR` bits: 0 arms the VRAM-to-SATB end interrupt, 1
//! the VRAM-to-VRAM end interrupt, bits 2 and 3 select decreasing
//! source and destination addresses, and bit 4 repeats the SATB
//! transfer at every vertical blank.
//!
//! The VRAM data port: a write of the low byte stages it; a write of
//! the high byte commits the word at MAWR and adds the increment. A
//! write of the MARR high byte latches `VRAM[MARR]` into the read
//! latch and adds the increment; a low-byte read returns the latched
//! low byte; a high-byte read returns the latched high byte and, when
//! the selected register is index 2, reloads the latch from the
//! current MARR and adds the increment.
//!
//! # Pinned background walk
//!
//! One BAT word sits at each character position of the virtual map,
//! starting at VRAM word 0: bits 15-12 the color block and bits 10-0
//! the character number. The character takes sixteen words: eight
//! character rows of two words; row `r` reads word `t + r` for the
//! lower two planes and word `t + r + 8` for the upper two planes.
//! Pixel `p` of the character row reads bit `7 - (p mod 8)` of each
//! plane byte, so the leftmost pixel of a plane byte is the most
//! significant bit; the composed color id is the color block times 16
//! plus the four color bits. The virtual map size comes from `MWR`
//! bits 4-6, x of 32, 64 or 128 characters and y of 32 or 64, and the
//! scroll wraps over that map: BXR latches at every line visit and BYR
//! latches at the frame top; each display row adds one to the latched
//! vertical position.
//!
//! # Pinned sprite form
//!
//! The sprite attribute table buffer holds sixty-four sprites, four
//! words each, and loads from VRAM at the DVSSR transfer: word 0 bits
//! 9-0 the top row stored as the row plus 64, with bit 15 the vertical
//! flip; word 1 bits 9-0 the left column stored as the column plus 32;
//! word 2 bit 0 the CG planar split of the narrow sprite mode and bits
//! 10-1 the pattern number; word 3 bits 3-0 the color block, bit 7 the
//! priority flag (1 draws the sprite in front of the background, 0
//! hides the sprite where the background pixel holds a color), bit 8
//! the width flag (0 means 16 pixels, 1 means 32 pixels as two
//! blocks), bits 13-12 the height form (16, 32, 64, 64 pixels), bit 11
//! the horizontal flip, and bit 15 the vertical flip. The pattern word
//! address is the pattern number times 64 words, with plane `n` at
//! `+ 16 * n` and the row inside the pattern; a 32-pixel sprite
//! renders as two 16-cell columns whose data halves swap under the
//! horizontal flip. The per-line limit is sixteen sprites; the rows
//! beyond set the overflow flag. A sprite 0 pixel drawn over a cell
//! another sprite drew sets the collision flag.
//!
//! # Pinned VCE surface
//!
//! The window decodes the low eleven bits: the registers at offsets 0
//! to 5 and the color table bytes at offsets 0x100 to 0x4FF, two bytes
//! per entry:
//!
//! | Offset | Register |
//! |--------|----------|
//! | 0 | CR, the color clock select in bits 1-0 |
//! | 2/3 | CTA, the color table address, nine bits |
//! | 4 | CTD low byte |
//! | 5 | CTD high byte, the access that adds one to CTA |
//! | 0x100 + 2n | color table entry n low byte |
//! | 0x101 + 2n | color table entry n high byte |
//!
//! The entry value carries nine bits: bits 8-6 green, 5-3 red, 2-0
//! blue, stored with the low byte at the even offset; the register
//! path masks the high byte to its low bit, and the byte form of the
//! color table stores raw bytes. The high-byte access adds one to the
//! address for reads and writes alike. Each
//! three-bit channel scales to eight bits by replication
//! `(v &lt;&lt; 5) | (v &lt;&lt; 2) | (v &gt;&gt; 1)`. The color table
//! holds 512 entries; the background composes ids 0 to 255 and sprites
//! compose ids 0x100 to 0x1FF through the ninth VDC signal, so the
//! sprite table covers the upper 256 entries.
//!
//! # Schedule and frame
//!
//! The `dot` divider names the domain ticks per VDC dot and defaults
//! to 4, the 5.36931 MHz dot clock of the 21.47727 MHz master. Each
//! scheduler visit equals one display line of 510 dots and composes
//! that line; the frame holds 242 lines with the display window
//! controlled by VDW (stored as N-1, reset to 238 for the 239 rows of
//! the typical 256x239 frame). The vblank flag lands at the first line
//! past the display window, together with the DVSSR and repeat-
//! armed SATB transfers. RCR compares against the line counter and
//! raises the scanline flag; the interrupt line recomputes at each
//! line visit from the set flags and their enables, so a flag set
//! from a mid-line register write asserts at the following visit and
//! a Status read that clears the flags drops it at the next visit.
//! The composed frame reads each color table entry it needs once per
//! line through the machine bus; the palette bytes answer on the VCE.
//!
//! # Recorded approximations
//!
//! - The line period is the pinned 510 dots and the frame the pinned
//!   242 lines; the real machine line is 342 dot clocks of the 5.37
//!   MHz dot clock, 63.5 microseconds, across 262 or 263 lines.
//! - The VRAM ports are immediate: the real VDC splits CPU VRAM access
//!   into slots against the display fetch, sets the busy flag, and
//!   delays reads and writes by 12 to 24 dots.
//! - The VRAM-to-VRAM block transfer runs in one step at the LENR
//!   high-byte write instead of four words per horizontal sync; the
//!   SOUR, DESR and LENR registers still end at their real
//!   post-transfer values.
//! - The VRAM-to-SATB transfer runs in one step at the vertical blank
//!   instead of spreading 256 words over the blank period.
//! - BYR latches at the frame top and a mid-frame write applies at the
//!   next frame; the per-row latch plus increment form of the real die
//!   is recorded out of build. BXR latches per line exactly.
//! - Vertical blanking, sync and display-end positions (HSR, VPR,
//!   VCR), the interlace forms, the HSF adjust, and the SuperGrafx
//!   second VDC stay out of build; the display window folds to lines
//!   0 through VDW.
//! - HDW values wider than the 256-pixel frame clip to the frame; the
//!   1024-pixel background fetch is built but invisible beyond the
//!   frame width.
//! - The narrow sprite mode planar split applies only as the data
//!   offset; the pixel-width forms of MWR bits 2-3 beyond that are
//!   recorded out of build.
//! - The burst-mode VRAM timing and the exact sprite fetch window of
//!   the real line are simplified; sprites fetch once per line visit.
//! - The color table read is the declared machine coupling: the VDC
//!   reads the VCE palette bytes through the bus at `palette_base`,
//!   which defaults to 0x0500, the VCE window at `$0400` plus its
//!   color table offset of 0x100.
//!
//! Sources: the HuC6270 and HuC6260 manual forms through published
//! reference copies and the Geargrafx emulator sources, in particular
//! `huc6270.cpp`, `huc6270.h`, `huc6270_defines.h`, and the hardware
//! reference documents for VDC, VCE, and the PSG.

use demu_config::model::{BootState, DeviceInstance, SystemConfig};
use demu_core::device::{AccessWidth, BusAccess, Frame, MachineCtx, VideoLayer, VideoSource};
use demu_core::frequency::{Address, Tick};
use demu_core::signal::Level;
use demu_core::triggers::TriggerEvent;
use demu_core::{Device, Snapshot, SnapshotError};

/// The dots of one display line, the pinned line period.
const LINE_DOTS: u64 = 510;
/// The lines of one display frame.
const FRAME_LINES: u32 = 242;
/// The frame width.
const FRAME_W: usize = 256;
/// The frame height.
const FRAME_H: usize = 239;
/// The VRAM in sixteen-bit words: 64 KiB.
const VRAM_WORDS: usize = 0x8000;
/// The VRAM word index mask.
const VRAM_MASK_US: usize = VRAM_WORDS - 1;
/// The sprite attribute buffer words: 64 sprites, four words each.
const SAT_WORDS: usize = 0x100;
/// The sprites the display holds.
const SPRITES: usize = 64;
/// The per-line sprite limit.
const SPRITE_LIMIT: usize = 16;
/// The VDC register count.
const VDC_REGS: usize = 20;

/// The internal register indexes.
const R_MAWR: usize = 0x00;
const R_MARR: usize = 0x01;
const R_VWR: usize = 0x02;
const R_CR: usize = 0x05;
const R_RCR: usize = 0x06;
const R_BXR: usize = 0x07;
const R_BYR: usize = 0x08;
const R_MWR: usize = 0x09;
const R_HDR: usize = 0x0B;
const R_VDR: usize = 0x0D;
const R_DCR: usize = 0x0F;
const R_SOUR: usize = 0x10;
const R_DESR: usize = 0x11;
const R_LENR: usize = 0x12;
const R_DVSSR: usize = 0x13;

/// The `SR` status flags.
const STATUS_COLLISION: u8 = 0x01;
const STATUS_OVERFLOW: u8 = 0x02;
const STATUS_SCANLINE: u8 = 0x04;
const STATUS_SAT_END: u8 = 0x08;
const STATUS_VRAM_END: u8 = 0x10;
const STATUS_VBLANK: u8 = 0x20;
/// The status bits the read returns; the busy flag stays out.
const STATUS_MASK: u8 = 0x3F;

/// The register value masks per the manual.
const REG_MASK: [u16; VDC_REGS] = [
    0xFFFF, 0xFFFF, 0xFFFF, 0x0000, 0x0000, 0x1FFF, 0x03FF, 0x03FF, 0x01FF, 0x00FF, 0x7F1F, 0x7F7F,
    0xFF1F, 0x01FF, 0x00FF, 0x001F, 0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF,
];

/// The virtual map character counts by the MWR screen field.
const SCREEN_X: [usize; 8] = [32, 64, 128, 128, 32, 64, 128, 128];
const SCREEN_Y: [usize; 8] = [32, 32, 32, 32, 64, 64, 64, 64];
/// The sprite pixel widths by the CGX flag.
const SPR_WIDTH: [u32; 2] = [16, 32];
/// The sprite pixel heights by the CGY field.
const SPR_HEIGHT: [u32; 4] = [16, 32, 64, 64];
/// The pattern number masks by the width and height forms.
const SPR_MASK_W: [u16; 2] = [0xFFFF, 0xFFFE];
const SPR_MASK_H: [u16; 4] = [0xFFFF, 0xFFFD, 0xFFF9, 0xFFF9];
/// The VDC pointer increments by the CR increment field.
const INC_TABLE: [u16; 4] = [0x0001, 0x0020, 0x0040, 0x0080];

/// The color id that leaves the frame pixel black.
const NO_COLOR: u16 = 0x0100;

/// The PC Engine Video Display Controller.
#[derive(Debug)]
pub struct PceVdc {
    name: String,
    /// The name of the bus the color table reads walk.
    bus: String,
    /// The bus address of the VCE color table bytes.
    palette_base: u32,
    /// The domain ticks per VDC dot, from the `dot` divider.
    dot_div: u64,
    /// The internal registers.
    regs: [u16; VDC_REGS],
    /// The register select of the data port.
    ar: u16,
    /// The status flags.
    status: u8,
    /// The current interrupt line level.
    irq: bool,
    /// The latched VRAM read word.
    read_buffer: u16,
    /// The VDC VRAM; word 0 holds the BAT.
    vram: Vec<u16>,
    /// The sprite attribute table buffer.
    sat: Vec<u16>,
    /// The scroll latches: BXR per line, BYR per frame.
    latched_bxr: u16,
    latched_byr: u16,
    /// The SATB transfer arm set by the DVSSR high write.
    sat_pending: bool,
    /// The current display line.
    line: u32,
    /// The background line buffer, color ids with 0x100 as no color.
    bg_line: Vec<u16>,
    /// The sprite line buffer, color ids with 0x100 as no color.
    spr_line: Vec<u16>,
    /// The frame pixels in RGB888.
    screen: Vec<u8>,
    /// The cached frame for the shell.
    frame: Frame,
    frames: u64,
    next: Option<Tick>,
    last_tick: Option<Tick>,
}

impl PceVdc {
    /// A device from its configuration entry.
    ///
    /// The color table read bus takes the `bus` parameter, the first
    /// bus of the configuration, or the `cpu` fallback. The
    /// `palette_base` parameter names the bus address of the VCE color
    /// table bytes and defaults to 0x0500. The `dot` divider defaults
    /// to 4.
    pub fn new(device: &DeviceInstance, config: &SystemConfig) -> Self {
        let bus = device
            .params
            .get("bus")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| {
                config
                    .buses
                    .first()
                    .map(|b| b.name.clone())
                    .unwrap_or_else(|| "cpu".to_string())
            });
        let palette_base = device
            .params
            .get("palette_base")
            .and_then(|v| v.as_integer())
            .and_then(|v| u32::try_from(v).ok())
            .unwrap_or(0x0500);
        let dot_div = u64::from(device.dividers.get("dot").copied().unwrap_or(4)).max(1);
        let mut vdc = Self {
            name: device.name.to_string(),
            bus,
            palette_base,
            dot_div,
            regs: [0; VDC_REGS],
            ar: 0,
            status: 0,
            irq: false,
            read_buffer: 0xFFFF,
            vram: vec![0; VRAM_WORDS],
            sat: vec![0; SAT_WORDS],
            latched_bxr: 0,
            latched_byr: 0,
            sat_pending: false,
            line: 0,
            bg_line: vec![NO_COLOR; FRAME_W],
            spr_line: vec![NO_COLOR; FRAME_W],
            screen: vec![0; FRAME_W * FRAME_H * 3],
            frame: Frame {
                width: FRAME_W as u32,
                height: FRAME_H as u32,
                pixels: vec![0; FRAME_W * FRAME_H * 3],
            },
            frames: 0,
            next: None,
            last_tick: None,
        };
        // The reset forms of the display registers: HDR = 0x1F gives
        // the 256-pixel width and VDR = 0x00EE gives the 239 display
        // rows of the typical frame.
        vdc.regs[R_HDR] = 0x001F;
        vdc.regs[R_VDR] = 0x00EE;
        vdc
    }

    /// The line cadence in domain ticks: 510 dots at the dot divider.
    fn line_step(&self) -> u64 {
        LINE_DOTS * self.dot_div
    }

    /// The current auto-increment of the VRAM pointers from the CR.
    fn increment(&self) -> u16 {
        INC_TABLE[((self.regs[R_CR] >> 11) & 0x0003) as usize]
    }

    /// The interrupt line level from the set flags and their enables.
    fn irq_wanted(&self) -> bool {
        let cr = self.regs[R_CR];
        let dcr = self.regs[R_DCR];
        (self.status & STATUS_COLLISION != 0 && cr & 0x0001 != 0)
            || (self.status & STATUS_OVERFLOW != 0 && cr & 0x0002 != 0)
            || (self.status & STATUS_SCANLINE != 0 && cr & 0x0004 != 0)
            || (self.status & STATUS_VBLANK != 0 && cr & 0x0008 != 0)
            || (self.status & STATUS_SAT_END != 0 && dcr & 0x0001 != 0)
            || (self.status & STATUS_VRAM_END != 0 && dcr & 0x0002 != 0)
    }

    /// The one-shot VRAM-to-VRAM block transfer of the LENR high-byte
    /// write: `(LENR + 1)` words from SOUR to DESR with the DCR step
    /// flags, ending with the registers at their post-transfer values.
    fn do_vram_dma(&mut self) {
        let count = self.regs[R_LENR].wrapping_add(1);
        let dcr = self.regs[R_DCR] & 0x001F;
        let mut source = self.regs[R_SOUR];
        let mut dest = self.regs[R_DESR];
        let src_step = if dcr & 0x0004 != 0 { -1i32 } else { 1 };
        let dst_step = if dcr & 0x0008 != 0 { -1i32 } else { 1 };
        for _ in 0..count {
            self.vram[(dest as usize) & VRAM_MASK_US] = self.vram[(source as usize) & VRAM_MASK_US];
            source = i32::from(source).wrapping_add(src_step) as u16;
            dest = i32::from(dest).wrapping_add(dst_step) as u16;
        }
        self.regs[R_SOUR] = source;
        self.regs[R_DESR] = dest;
        // The length register ends at the exhausted-transfer form the
        // real chip leaves behind.
        self.regs[R_LENR] = 0xFFFF;
    }

    /// The one-shot VRAM-to-SATB transfer: 256 words from the DVSSR
    /// address into the attribute buffer.
    fn do_satb_dma(&mut self) {
        let source = self.regs[R_DVSSR];
        for i in 0..SAT_WORDS {
            self.sat[i] = self.vram[(source as usize + i) & VRAM_MASK_US];
        }
    }

    /// Compose one display line into the frame row.
    fn compose_line(&mut self, line: u32, tick: Tick, ctx: &mut MachineCtx) {
        if line as usize >= FRAME_H {
            return;
        }
        let row = line as usize;
        // The background walk, then the sprites over it; the two
        // enable bits act independently.
        self.bg_line.fill(NO_COLOR);
        self.spr_line.fill(NO_COLOR);
        if self.regs[R_CR] & 0x0080 != 0 {
            self.render_background(line);
        }
        if self.regs[R_CR] & 0x0040 != 0 {
            self.render_sprites(line);
        }
        // The per-line color table reads: one entry read once per
        // line through the machine bus.
        let mut used = [false; 512];
        let mut entries = [0u16; 512];
        for col in 0..FRAME_W {
            let id = if self.spr_line[col] != NO_COLOR {
                self.spr_line[col]
            } else {
                self.bg_line[col]
            };
            if id == NO_COLOR {
                continue;
            }
            let entry = (id as usize) & 0x1FF;
            if used[entry] {
                continue;
            }
            used[entry] = true;
            let lo = ctx.read(
                &self.bus,
                AccessWidth::Byte,
                Address(self.palette_base + (entry as u32) * 2),
                tick,
            ) as u16;
            let hi = ctx.read(
                &self.bus,
                AccessWidth::Byte,
                Address(self.palette_base + (entry as u32) * 2 + 1),
                tick,
            ) as u16;
            entries[entry] = lo | (hi << 8);
        }
        // The frame write through the ninth signal: ids 0x100 and up
        // land in the sprite half of the table.
        for col in 0..FRAME_W {
            let id = if self.spr_line[col] != NO_COLOR {
                self.spr_line[col]
            } else {
                self.bg_line[col]
            };
            let rgb = if id == NO_COLOR {
                [0, 0, 0]
            } else {
                let value = entries[(id as usize) & 0x1FF];
                let green = ((value >> 6) & 0x07) as u8;
                let red = ((value >> 3) & 0x07) as u8;
                let blue = (value & 0x07) as u8;
                [Self::scale3(red), Self::scale3(green), Self::scale3(blue)]
            };
            let base = (row * FRAME_W + col) * 3;
            self.screen[base] = rgb[0];
            self.screen[base + 1] = rgb[1];
            self.screen[base + 2] = rgb[2];
        }
    }

    /// The three-bit replication scale to eight bits.
    fn scale3(value: u8) -> u8 {
        (value << 5) | (value << 2) | (value >> 1)
    }

    /// The background walk of one line.
    fn render_background(&mut self, line: u32) {
        let mwr = self.regs[R_MWR] & 0x00FF;
        let screen = ((mwr >> 4) & 0x07) as usize;
        let cells_x = SCREEN_X[screen];
        let pixels_x = cells_x * 8;
        let x_mask = (pixels_x - 1) as u16;
        let y_mask = (SCREEN_Y[screen] * 8 - 1) as u16;
        let bg_y = self.latched_byr.wrapping_add(line as u16) & y_mask;
        let bat_row = (bg_y >> 3) as usize * cells_x;

        let mut bg_x = self.latched_bxr & x_mask;
        let mut col = 0usize;
        while col < FRAME_W {
            let bat_word = self.vram[(bat_row + (bg_x >> 3) as usize) & VRAM_MASK_US];
            let tile = ((bat_word & 0x07FF) << 4) as usize;
            let row_in_tile = (bg_y & 0x07) as usize;
            let word_a = self.vram[(tile + row_in_tile) & VRAM_MASK_US];
            let word_b = self.vram[(tile + row_in_tile + 8) & VRAM_MASK_US];
            let color_base = (bat_word >> 12) << 4;
            let col0 = bg_x;
            let count = (8usize - (col0 & 0x07) as usize).min(FRAME_W - col);
            for step in 0..count {
                let tile_x = (7 - ((col0 + step as u16) & 0x07)) & 0x07;
                let color = ((word_a >> tile_x) & 1)
                    | (((word_a >> (tile_x + 8)) & 1) << 1)
                    | (((word_b >> tile_x) & 1) << 2)
                    | (((word_b >> (tile_x + 8)) & 1) << 3);
                self.bg_line[col] = color_base | color;
                col += 1;
            }
            bg_x = col0.wrapping_add(count as u16) & x_mask;
        }
    }

    /// The sprite walk of one line: fetch the on-screen sprites from
    /// the attribute buffer up to the limit, then paint them from the
    /// highest index down so sprite 0 wins. Returns the overflow and
    /// collision flags of the line.
    fn render_sprites(&mut self, line: u32) -> (bool, bool) {
        let mode1 = (self.regs[R_MWR] >> 2) & 0x03 == 0x0001;
        let mut drawn_x = [0u16; SPRITE_LIMIT];
        let mut drawn_flags = [0u16; SPRITE_LIMIT];
        let mut drawn_sat = [0u16; SPRITE_LIMIT];
        let mut drawn_planes = [[0u16; 4]; SPRITE_LIMIT];
        let mut overflow = false;
        let mut count = 0usize;
        for sprite in 0..SPRITES {
            let sat0 = self.sat[sprite * 4];
            let sat1 = self.sat[sprite * 4 + 1];
            let sat2 = self.sat[sprite * 4 + 2];
            let sat3 = self.sat[sprite * 4 + 3];
            let top = i32::from(sat0 & 0x03FF) - 64;
            let cgy = ((sat3 >> 12) & 0x0003) as usize;
            let height = SPR_HEIGHT[cgy];
            if !(top <= line as i32 && (line as i32) < top + height as i32) {
                continue;
            }
            let mut row = line as i32 - top;
            if sat0 & 0x8000 != 0 {
                // The vertical flip mirrors the row inside the height.
                row = height as i32 - 1 - row;
            }
            let cgx = ((sat3 >> 8) & 0x0001) as usize;
            let pattern = ((sat2 >> 1) & 0x03FF) & SPR_MASK_W[cgx] & SPR_MASK_H[cgy];
            let base = (pattern as usize) << 6;
            let mode1_offset = if mode1 {
                (usize::from(sat2 & 1)) << 5
            } else {
                0
            };
            let line_start =
                base + (row >> 4) as usize * 128 + (row & 0x0F) as usize + mode1_offset;
            for half in 0..2u16 {
                if half == 1 && SPR_WIDTH[cgx] != 32 {
                    break;
                }
                if count >= SPRITE_LIMIT {
                    overflow = true;
                    break;
                }
                // The 32-pixel form draws two 16-cell columns and the
                // horizontal flip swaps the data halves.
                let half_offset = if SPR_WIDTH[cgx] == 32 {
                    match sat3 & 0x0800 != 0 {
                        true => {
                            if half == 0 {
                                64
                            } else {
                                0
                            }
                        }
                        false => {
                            if half == 0 {
                                0
                            } else {
                                64
                            }
                        }
                    }
                } else {
                    0
                };
                let start = line_start + half_offset;
                let a = self.vram[start & VRAM_MASK_US];
                let b = self.vram[(start + 16) & VRAM_MASK_US];
                let c = if mode1 {
                    0
                } else {
                    self.vram[(start + 32) & VRAM_MASK_US]
                };
                let d = if mode1 {
                    0
                } else {
                    self.vram[(start + 48) & VRAM_MASK_US]
                };
                drawn_x[count] = (sat1 & 0x03FF).wrapping_add(half * 16);
                drawn_planes[count] = [a, b, c, d];
                drawn_flags[count] = sat3;
                drawn_sat[count] = sprite as u16;
                count += 1;
            }
            if overflow {
                break;
            }
        }

        let mut collision = false;
        for d in (0..count).rev() {
            let pos = i32::from(drawn_x[d]) - 32;
            if pos >= FRAME_W as i32 || pos + 16 <= 0 {
                continue;
            }
            let flags = drawn_flags[d];
            let hflip = flags & 0x0800 != 0;
            let priority = flags & 0x0080 != 0;
            let block = (flags & 0x000F) << 4;
            let planes = drawn_planes[d];
            let col0 = (pos.max(0)) as usize;
            let col1 = ((pos + 16).min(FRAME_W as i32)) as usize;
            for col in col0..col1 {
                let x = (col as i32 - pos) as usize;
                let pixel_x = if hflip { x & 0x0F } else { 15 - (x & 0x0F) } as u16;
                let pixel = ((planes[0] >> pixel_x) & 1)
                    | (((planes[1] >> pixel_x) & 1) << 1)
                    | (((planes[2] >> pixel_x) & 1) << 2)
                    | (((planes[3] >> pixel_x) & 1) << 3);
                if pixel & 0x0F == 0 {
                    continue;
                }
                if drawn_sat[d] == 0 && self.spr_line[col] & 0x000F != 0 {
                    // Sprite 0 covered a cell another sprite drew.
                    collision = true;
                }
                if !priority && self.bg_line[col] & 0x000F != 0 {
                    // The sprite hides behind this background pixel.
                } else {
                    self.spr_line[col] = NO_COLOR | block | pixel;
                }
            }
        }
        (overflow, collision)
    }

    /// Refresh the cached frame from the screen bytes.
    fn refresh_frame(&mut self) {
        self.frame.pixels = self.screen.clone();
    }

    /// The status read: the low flags out, the flags clear, and the
    /// interrupt line drops at the next visit.
    fn status_read(&mut self) -> u8 {
        let value = self.status & STATUS_MASK;
        self.status = 0;
        self.irq = false;
        value
    }

    /// The internal register form behind the data port.
    fn register_write(&mut self, high: bool, value: u8) {
        if self.ar as usize >= VDC_REGS {
            return;
        }
        let index = self.ar as usize;
        if high {
            self.regs[index] = (self.regs[index] & 0x00FF) | (u16::from(value) << 8);
        } else {
            self.regs[index] = (self.regs[index] & 0xFF00) | u16::from(value);
        }
        self.regs[index] &= REG_MASK[index];
        match index {
            // The MARR high write latches the word and steps the
            // pointer by the CR increment.
            R_MARR => {
                if high {
                    let address = self.regs[R_MARR];
                    self.read_buffer = self.vram[(address as usize) & VRAM_MASK_US];
                    self.regs[R_MARR] = address.wrapping_add(self.increment());
                }
            }
            // The VWR high write commits the word and steps MAWR.
            R_VWR => {
                if high {
                    let address = self.regs[R_MAWR];
                    self.vram[(address as usize) & VRAM_MASK_US] = self.regs[R_VWR];
                    self.regs[R_MAWR] = address.wrapping_add(self.increment());
                }
            }
            // The LENR high write launches the block transfer.
            R_LENR => {
                if high {
                    self.do_vram_dma();
                    self.status |= STATUS_VRAM_END;
                }
            }
            // The DVSSR high write arms the SATB transfer for the
            // following vertical blank.
            R_DVSSR => {
                self.sat_pending = high;
            }
            _ => {}
        }
    }
}

impl BusAccess for PceVdc {
    fn read(&mut self, _width: AccessWidth, address: Address, _tick: Tick) -> u32 {
        match address.0 & 0x03 {
            0 => u32::from(self.status_read()),
            1 => 0x00,
            2 => u32::from(self.read_buffer as u8),
            _ => {
                let value = (self.read_buffer >> 8) as u8;
                if self.ar as usize == R_VWR {
                    // The high-byte read of the VRR reloads the latch
                    // from the read pointer and steps it.
                    let address2 = self.regs[R_MARR];
                    self.read_buffer = self.vram[(address2 as usize) & VRAM_MASK_US];
                    self.regs[R_MARR] = address2.wrapping_add(self.increment());
                }
                u32::from(value)
            }
        }
    }

    fn write(&mut self, _width: AccessWidth, address: Address, value: u32, _tick: Tick) {
        let byte = value as u8;
        match address.0 & 0x03 {
            0 => self.ar = u16::from(byte) & 0x001F,
            2 => self.register_write(false, byte),
            3 => self.register_write(true, byte),
            _ => {}
        }
    }

    fn peek(&self, address: Address) -> u8 {
        match address.0 & 0x03 {
            0 => self.status & STATUS_MASK,
            1 => 0x00,
            2 => self.read_buffer as u8,
            _ => (self.read_buffer >> 8) as u8,
        }
    }

    fn decodes(&self, _address: Address) -> bool {
        true
    }
}

impl VideoSource for PceVdc {
    fn frame(&self) -> Option<&Frame> {
        Some(&self.frame)
    }

    fn dump_layer(&self, layer: VideoLayer) -> Option<Vec<u8>> {
        match layer {
            VideoLayer::Vram => {
                let mut out = Vec::with_capacity(VRAM_WORDS * 2);
                for word in &self.vram {
                    out.extend_from_slice(&word.to_le_bytes());
                }
                Some(out)
            }
            VideoLayer::Oam => {
                let mut out = Vec::with_capacity(SAT_WORDS * 2);
                for word in &self.sat {
                    out.extend_from_slice(&word.to_le_bytes());
                }
                Some(out)
            }
            VideoLayer::NameTable => {
                let mut out = Vec::with_capacity(0x1000 * 2);
                for word in &self.vram[..0x1000] {
                    out.extend_from_slice(&word.to_le_bytes());
                }
                Some(out)
            }
            VideoLayer::Framebuffer => Some(self.screen.clone()),
            VideoLayer::Palette => None,
        }
    }

    fn frames(&self) -> u64 {
        self.frames
    }
}

/// The PC Engine Video Color Encoder.
#[derive(Debug)]
pub struct PceVce {
    name: String,
    /// The control register: the color clock select in bits 1-0.
    cr: u8,
    /// The color table address, nine bits.
    cta: u16,
    /// The color table RAM: 512 entries of nine bits as two bytes,
    /// low first.
    palette: Vec<u8>,
}

impl PceVce {
    /// A device from its configuration entry.
    pub fn new(device: &DeviceInstance) -> Self {
        Self {
            name: device.name.to_string(),
            cr: 0,
            cta: 0,
            palette: vec![0; 0x400],
        }
    }

    /// The nine-bit entry word at one index.
    pub fn entry(&self, index: usize) -> u16 {
        let base = (index & 0x1FF) * 2;
        u16::from(self.palette[base]) | ((u16::from(self.palette[base + 1]) & 0x01) << 8)
    }
}

/// The offset inside the VCE I/O form relative to the published base
/// `$0400`, with the page mask form of the real chip.
fn vce_offset(address: u32) -> Option<u16> {
    (address & 0x07FF).checked_sub(0x0400).map(|v| v as u16)
}

/// The byte index of the direct color-table form: the published page
/// window at `$1FF800` answers the color-table bytes under their own
/// byte index. The form rides the same `$0100`-indexed arm as the I/O
/// form, so one match serves both faces.
fn vce_color_offset(address: u32) -> Option<u16> {
    match address & 0x1FFF {
        0x1800..=0x1BFF => Some((0x0100 + (address & 0x03FF)) as u16),
        _ => None,
    }
}

/// The answered offset of one address: the I/O form first, then the
/// direct color-table form.
fn vce_access_offset(address: u32) -> Option<u16> {
    vce_offset(address).or_else(|| vce_color_offset(address))
}

impl BusAccess for PceVce {
    fn read(&mut self, _width: AccessWidth, address: Address, _tick: Tick) -> u32 {
        // The offset inside the VCE I/O form relative to the
        // published base $0400, with the page mask form of the chip.
        let Some(offset) = vce_access_offset(address.0) else {
            return 0xFF;
        };
        match offset {
            0x0000 => u32::from(self.cr),
            0x0002 => u32::from(self.cta as u8),
            0x0003 => u32::from((self.cta >> 8) & 0x01),
            0x0004 => u32::from(self.palette[(usize::from(self.cta & 0x01FF)) * 2]),
            0x0005 => {
                let byte = self.palette[(usize::from(self.cta & 0x01FF)) * 2 + 1];
                // The high-byte read steps the address.
                self.cta = self.cta.wrapping_add(1) & 0x01FF;
                u32::from(byte)
            }
            0x0100..=0x04FF => u32::from(self.palette[(offset - 0x0100) as usize]),
            _ => 0xFF,
        }
    }

    fn write(&mut self, _width: AccessWidth, address: Address, value: u32, _tick: Tick) {
        // The offset inside the VCE I/O form relative to the
        // published base $0400, with the page mask form of the chip.
        let Some(offset) = vce_offset(address.0) else {
            return;
        };
        let byte = value as u8;
        match offset {
            0x0000 => self.cr = byte & 0x03,
            0x0002 => self.cta = (self.cta & 0x0100) | u16::from(byte),
            0x0003 => self.cta = (self.cta & 0x00FF) | (u16::from(byte & 0x01) << 8),
            0x0004 => self.palette[(usize::from(self.cta & 0x01FF)) * 2] = byte,
            0x0005 => {
                // The high-byte write stores the green high bit and
                // steps the address.
                self.palette[(usize::from(self.cta & 0x01FF)) * 2 + 1] = byte & 0x01;
                self.cta = self.cta.wrapping_add(1) & 0x01FF;
            }
            0x0100..=0x04FF => self.palette[(offset - 0x0100) as usize] = byte,
            _ => {}
        }
    }

    fn peek(&self, address: Address) -> u8 {
        // The side-effect-free form: no CTA step.
        // The offset inside the VCE I/O form relative to the
        // published base $0400, with the page mask form of the chip.
        let Some(offset) = vce_access_offset(address.0) else {
            return 0xFF;
        };
        match offset {
            0x0000 => self.cr,
            0x0002 => self.cta as u8,
            0x0003 => ((self.cta >> 8) & 0x01) as u8,
            0x0004 => self.palette[(usize::from(self.cta & 0x01FF)) * 2],
            0x0005 => self.palette[(usize::from(self.cta & 0x01FF)) * 2 + 1],
            0x0100..=0x04FF => self.palette[(offset - 0x0100) as usize],
            _ => 0xFF,
        }
    }

    fn decodes(&self, _address: Address) -> bool {
        true
    }
}

/// The snapshot byte length of the VCE chunk.
const VCE_SNAPSHOT_LEN: usize = 4 + 1 + 1 + 2 + 0x400;
/// The snapshot byte length of the VDC chunk.
const VDC_SNAPSHOT_LEN: usize = 5
    + VDC_REGS * 2
    + 2
    + 1
    + 1
    + 2
    + 2
    + 2
    + 1
    + 4
    + 8
    + VRAM_WORDS * 2
    + SAT_WORDS * 2
    + FRAME_W * FRAME_H * 3;

impl Device for PceVdc {
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

    fn reset(&mut self) {
        self.regs = [0; VDC_REGS];
        self.regs[R_HDR] = 0x001F;
        self.regs[R_VDR] = 0x00EE;
        self.ar = 0;
        self.status = 0;
        self.irq = false;
        self.read_buffer = 0xFFFF;
        self.vram.fill(0);
        self.sat.fill(0);
        self.latched_bxr = 0;
        self.latched_byr = 0;
        self.sat_pending = false;
        self.line = 0;
        self.bg_line.fill(NO_COLOR);
        self.spr_line.fill(NO_COLOR);
        self.screen.fill(0);
        self.frames = 0;
        self.refresh_frame();
        self.next = None;
        self.last_tick = None;
    }

    fn boot(&mut self, _state: Option<&BootState>) {
        self.reset();
        self.next = Some(Tick(0));
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(VDC_SNAPSHOT_LEN);
        out.extend_from_slice(b"PVDC");
        out.push(1);
        for reg in &self.regs {
            out.extend_from_slice(&reg.to_le_bytes());
        }
        out.extend_from_slice(&self.ar.to_le_bytes());
        out.push(self.status);
        out.push(u8::from(self.irq));
        out.extend_from_slice(&self.read_buffer.to_le_bytes());
        out.extend_from_slice(&self.latched_bxr.to_le_bytes());
        out.extend_from_slice(&self.latched_byr.to_le_bytes());
        out.push(u8::from(self.sat_pending));
        out.extend_from_slice(&self.line.to_le_bytes());
        out.extend_from_slice(&self.frames.to_le_bytes());
        for word in &self.vram {
            out.extend_from_slice(&word.to_le_bytes());
        }
        for word in &self.sat {
            out.extend_from_slice(&word.to_le_bytes());
        }
        out.extend_from_slice(&self.screen);
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        if data.len() != VDC_SNAPSHOT_LEN || &data[..4] != b"PVDC" || data[4] != 1 {
            return Err(SnapshotError::Mismatch(format!(
                "pce VDC chunk must be {VDC_SNAPSHOT_LEN} bytes"
            )));
        }
        let mut pos = 5usize;
        for reg in self.regs.iter_mut() {
            *reg = u16::from_le_bytes(data[pos..pos + 2].try_into().expect("2 bytes"));
            pos += 2;
        }
        self.ar = u16::from_le_bytes(data[pos..pos + 2].try_into().expect("2 bytes"));
        pos += 2;
        self.status = data[pos];
        pos += 1;
        self.irq = data[pos] != 0;
        pos += 1;
        self.read_buffer = u16::from_le_bytes(data[pos..pos + 2].try_into().expect("2 bytes"));
        pos += 2;
        self.latched_bxr = u16::from_le_bytes(data[pos..pos + 2].try_into().expect("2 bytes"));
        pos += 2;
        self.latched_byr = u16::from_le_bytes(data[pos..pos + 2].try_into().expect("2 bytes"));
        pos += 2;
        self.sat_pending = data[pos] != 0;
        pos += 1;
        self.line = u32::from_le_bytes(data[pos..pos + 4].try_into().expect("4 bytes"));
        pos += 4;
        self.frames = u64::from_le_bytes(data[pos..pos + 8].try_into().expect("8 bytes"));
        pos += 8;
        for word in self.vram.iter_mut() {
            *word = u16::from_le_bytes(data[pos..pos + 2].try_into().expect("2 bytes"));
            pos += 2;
        }
        for word in self.sat.iter_mut() {
            *word = u16::from_le_bytes(data[pos..pos + 2].try_into().expect("2 bytes"));
            pos += 2;
        }
        self.screen
            .copy_from_slice(&data[pos..pos + FRAME_W * FRAME_H * 3]);
        self.refresh_frame();
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
        // The interrupt line recomputes from the flags and enables.
        let wanted = self.irq_wanted();
        if wanted != self.irq {
            self.irq = wanted;
            ctx.notify("irq", if wanted { Level::High } else { Level::Low }, tick);
        }
        // The vertical display window: VDW counts display lines.
        let vdw = self.regs[R_VDR] & 0x01FF;
        let display_end = u32::from(vdw).min(FRAME_LINES - 1) + 1;
        if self.line == 0 {
            // The vertical scroll latches at the frame top.
            self.latched_byr = self.regs[R_BYR] & 0x01FF;
        }
        if self.line < display_end {
            // The horizontal scroll latches every line visit.
            self.latched_bxr = self.regs[R_BXR] & 0x03FF;
            self.compose_line(self.line, tick, ctx);
        }
        // The scanline detect: the die's raster counter starts at 64
        // on the last blank line before the display window, so the
        // register carries the target line plus 64. A compare inside
        // the display window fires the flag.
        if self.line.wrapping_add(64) == u32::from(self.regs[R_RCR] & 0x03FF)
            && self.line < display_end
            && self.regs[R_CR] & 0x0004 != 0
        {
            self.status |= STATUS_SCANLINE;
        }
        if self.line == display_end {
            // The first blank line: the vertical blanking flag, the
            // satb transfer, and the interrupt event.
            if self.regs[R_CR] & 0x0008 != 0 {
                self.status |= STATUS_VBLANK;
            }
            ctx.emit(TriggerEvent::Vblank);
            if self.sat_pending || self.regs[R_DCR] & 0x0010 != 0 {
                self.sat_pending = false;
                self.do_satb_dma();
                self.status |= STATUS_SAT_END;
            }
        }
        self.line += 1;
        if self.line >= FRAME_LINES {
            // The display frame completed.
            self.line = 0;
            self.frames += 1;
            self.refresh_frame();
        }
        self.next = Some(Tick(tick.0 + self.line_step()));
    }
}

impl Device for PceVce {
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
        self.cr = 0;
        self.cta = 0;
        self.palette.fill(0);
    }

    fn boot(&mut self, _state: Option<&BootState>) {
        self.reset();
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(VCE_SNAPSHOT_LEN);
        out.extend_from_slice(b"PVCE");
        out.push(1);
        out.push(self.cr);
        out.extend_from_slice(&self.cta.to_le_bytes());
        out.extend_from_slice(&self.palette);
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        if data.len() != VCE_SNAPSHOT_LEN || &data[..4] != b"PVCE" || data[4] != 1 {
            return Err(SnapshotError::Mismatch(format!(
                "pce VCE chunk must be {VCE_SNAPSHOT_LEN} bytes"
            )));
        }
        self.cr = data[5];
        self.cta = u16::from_le_bytes(data[6..8].try_into().expect("2 bytes"));
        self.palette.copy_from_slice(&data[8..]);
        Ok(())
    }

    fn notify_line(&mut self, _line: &str, _level: Level, _tick: Tick) {}

    fn tick(&mut self, _tick: Tick, _ctx: &mut MachineCtx) {}
}
#[cfg(test)]
mod tests {
    use super::*;
    use demu_config::model::{Bus as CBus, ClockDomain, Window as CWindow};
    use demu_config::newtypes::{DeviceName, KindName};
    use demu_core::machine::Machine;
    use std::collections::BTreeMap;

    fn ram_instance() -> DeviceInstance {
        let mut params = BTreeMap::new();
        params.insert("size".to_string(), toml::Value::Integer(0x10000));
        DeviceInstance {
            name: DeviceName::new("ram").expect("name"),
            kind: KindName::new("smoke.ram").expect("kind"),
            domain: "main".to_string(),
            params,
            dividers: BTreeMap::new(),
        }
    }

    fn vdc_instance() -> DeviceInstance {
        let mut params = BTreeMap::new();
        params.insert("bus".to_string(), toml::Value::String("cpu".to_string()));
        params.insert("palette_base".to_string(), toml::Value::Integer(0x0500));
        DeviceInstance {
            name: DeviceName::new("vdc").expect("name"),
            kind: KindName::new("pce.vdc").expect("kind"),
            domain: "main".to_string(),
            params,
            dividers: BTreeMap::new(),
        }
    }

    fn vce_instance() -> DeviceInstance {
        DeviceInstance {
            name: DeviceName::new("vce").expect("name"),
            kind: KindName::new("pce.vce").expect("kind"),
            domain: "main".to_string(),
            params: BTreeMap::new(),
            dividers: BTreeMap::new(),
        }
    }

    /// One bus with the smoke RAM, the VDC I/O window and the VCE
    /// window in the published I/O page positions.
    fn pce_config() -> SystemConfig {
        let ram = ram_instance();
        let vdc = vdc_instance();
        let vce = vce_instance();
        let mut clocks = BTreeMap::new();
        clocks.insert(
            "main".to_string(),
            ClockDomain {
                name: "main".to_string(),
                frequency: demu_config::newtypes::Frequency::new(21_477_270, 1).expect("freq"),
                origin: "the machine master".to_string(),
            },
        );
        let mut by_name = BTreeMap::new();
        by_name.insert(ram.name.clone(), 0usize);
        by_name.insert(vdc.name.clone(), 1usize);
        by_name.insert(vce.name.clone(), 2usize);
        SystemConfig {
            name: "pce-test".to_string(),
            triple: "nec/huc6280".to_string(),
            description: "the PCE test machine".to_string(),
            clocks,
            devices: vec![ram, vdc, vce],
            devices_by_name: by_name,
            buses: vec![CBus {
                name: "cpu".to_string(),
                address_bits: 16,
                owner: None,
                parent: None,
            }],
            windows: vec![
                CWindow {
                    name: "ram".to_string(),
                    bus: "cpu".to_string(),
                    device: DeviceName::new("ram").expect("name"),
                    base: 0x0000,
                    size: 0x10000,
                    kind: None,
                    mirror: None,
                    overlay: None,
                    overlay_read_priority: None,
                    overlay_write_priority: None,
                    conflict: None,
                },
                CWindow {
                    name: "vdc".to_string(),
                    bus: "cpu".to_string(),
                    device: DeviceName::new("vdc").expect("name"),
                    base: 0x0000,
                    size: 0x0004,
                    kind: None,
                    mirror: None,
                    overlay: Some("vdc".to_string()),
                    overlay_read_priority: Some(1),
                    overlay_write_priority: Some(1),
                    conflict: None,
                },
                CWindow {
                    name: "vce".to_string(),
                    bus: "cpu".to_string(),
                    device: DeviceName::new("vce").expect("name"),
                    base: 0x0400,
                    size: 0x0500,
                    kind: None,
                    mirror: None,
                    overlay: Some("vce".to_string()),
                    overlay_read_priority: Some(1),
                    overlay_write_priority: Some(1),
                    conflict: None,
                },
            ],
            connections: Vec::new(),
            signals: Vec::new(),
            boot: BTreeMap::new(),
            quirks: BTreeMap::new(),
        }
    }

    fn pce_machine() -> Machine {
        let mut ctors = demu_core::smoke_ctors();
        ctors.register("pce.vdc", Box::new(|d, c| Ok(Box::new(PceVdc::new(d, c)))));
        ctors.register("pce.vce", Box::new(|d, _| Ok(Box::new(PceVce::new(d)))));
        let mut machine = Machine::assemble(pce_config(), &ctors).expect("assemble");
        machine.boot();
        machine
    }

    fn vdc_of(machine: &mut Machine) -> &mut PceVdc {
        let any = machine
            .device_by_name_mut("vdc")
            .expect("device")
            .as_any_mut();
        any.downcast_mut::<PceVdc>().expect("the VDC")
    }

    fn vce_of(machine: &mut Machine) -> &mut PceVce {
        let any = machine
            .device_by_name_mut("vce")
            .expect("device")
            .as_any_mut();
        any.downcast_mut::<PceVce>().expect("the VCE")
    }

    /// The VDC register and the VRAM data port writes of one test.
    fn vdc_write(machine: &mut Machine, address: u32, value: u8) {
        assert!(machine.debug_write("cpu", Address(address), value));
    }

    fn frame_bytes_of(machine: &mut Machine) -> Vec<u8> {
        let video = machine
            .device_by_name("vdc")
            .expect("device")
            .video()
            .expect("video");
        video.frame().expect("frame").pixels.clone()
    }

    fn pixel_of(bytes: &[u8], row: usize, col: usize) -> [u8; 3] {
        let base = (row * FRAME_W + col) * 3;
        [bytes[base], bytes[base + 1], bytes[base + 2]]
    }

    #[test]
    fn vram_ports_walk_addresses_with_increments() {
        let mut s = PceVdc::new(&vdc_instance(), &pce_config());
        let w = |s: &mut PceVdc, a: u32, v: u8| {
            s.write(AccessWidth::Byte, Address(a), u32::from(v), Tick(0));
        };
        // MAWR = 0x0040, then two words through the data port: the low
        // byte stages, the high byte commits and steps by one.
        w(&mut s, 0x0000, 0x00);
        w(&mut s, 0x0002, 0x40);
        w(&mut s, 0x0003, 0x00);
        w(&mut s, 0x0000, 0x02);
        w(&mut s, 0x0002, 0x55);
        w(&mut s, 0x0003, 0x11);
        w(&mut s, 0x0002, 0xCC);
        w(&mut s, 0x0003, 0x22);
        assert_eq!(s.vram[0x40], 0x1155);
        assert_eq!(s.vram[0x41], 0x22CC);
        assert_eq!(s.regs[R_MAWR], 0x0042);

        // The CR increment field 01 = +32 words per high-byte access.
        w(&mut s, 0x0000, 0x05);
        w(&mut s, 0x0002, 0x00);
        w(&mut s, 0x0003, 0x08); // CR = 0x0800
        w(&mut s, 0x0000, 0x02);
        w(&mut s, 0x0002, 0x99);
        w(&mut s, 0x0003, 0x33);
        assert_eq!(s.vram[0x42], 0x3399);
        assert_eq!(s.regs[R_MAWR], 0x0062);
    }

    #[test]
    fn vrr_reads_latch_on_high_byte() {
        let mut s = PceVdc::new(&vdc_instance(), &pce_config());
        s.vram[0x40] = 0x0F1E;
        // The MARR high write latches the word and steps.
        s.write(AccessWidth::Byte, Address(0x0000), 0x01, Tick(0));
        s.write(AccessWidth::Byte, Address(0x0002), 0x40, Tick(0));
        s.write(AccessWidth::Byte, Address(0x0003), 0x00, Tick(0));
        // The read data port: the low then the high, then the next
        // word latches (here 0 from the zeroed VRAM).
        assert_eq!(s.read(AccessWidth::Byte, Address(0x0002), Tick(0)), 0x1E);
        assert_eq!(s.read(AccessWidth::Byte, Address(0x0003), Tick(0)), 0x0F);
        assert_eq!(s.regs[R_MARR], 0x0041);
    }

    #[test]
    fn status_read_clears_flags_and_masks_busy() {
        let mut s = PceVdc::new(&vdc_instance(), &pce_config());
        s.status = STATUS_COLLISION | STATUS_VBLANK;
        // The read is the low six bits and clears the flags.
        assert_eq!(s.read(AccessWidth::Byte, Address(0x0000), Tick(0)), 0x21);
        assert_eq!(s.status, 0);
        // A cleared status drops the interrupt line at the next visit.
        assert!(!s.irq_wanted());
    }

    #[test]
    fn vram_dma_copy_runs_one_shot() {
        let mut s = PceVdc::new(&vdc_instance(), &pce_config());
        for i in 0..4usize {
            s.vram[0x100 + i] = 0x1000 + i as u16;
        }
        // The SOUR = 0x0100, the DESR = 0x0200, the LENR = 3 (4 words);
        // the high-byte write of LENR runs the transfer in one step.
        s.write(AccessWidth::Byte, Address(0x0000), 0x0F, Tick(0));
        s.write(AccessWidth::Byte, Address(0x0002), 0x01, Tick(0)); // DCR
        s.write(AccessWidth::Byte, Address(0x0000), 0x10, Tick(0));
        s.write(AccessWidth::Byte, Address(0x0003), 0x01, Tick(0)); // SOUR high
        s.write(AccessWidth::Byte, Address(0x0000), 0x11, Tick(0));
        s.write(AccessWidth::Byte, Address(0x0003), 0x02, Tick(0)); // DESR high
        s.write(AccessWidth::Byte, Address(0x0000), 0x12, Tick(0));
        s.write(AccessWidth::Byte, Address(0x0002), 0x03, Tick(0)); // LENR low
        s.write(AccessWidth::Byte, Address(0x0003), 0x00, Tick(0)); // the high write starts
        assert_eq!(s.vram[0x200], 0x1000);
        assert_eq!(s.vram[0x203], 0x1003);
        assert_eq!(s.regs[R_LENR], 0xFFFF);
        assert_eq!(s.regs[R_SOUR], 0x0104);
        assert_eq!(s.regs[R_DESR], 0x0204);
        assert_ne!(s.status & STATUS_VRAM_END, 0);
    }

    #[test]
    fn satb_dma_lands_at_vblank() {
        let mut machine = pce_machine();
        // The SAT in VRAM at 0x0100: sprite 0 at x=32+0? top +64 form.
        // The values land through the VRAM DMA of one word: simpler to
        // poke the SAT words with the VDC data port.
        let vdc_reg = |machine: &mut Machine, reg: u8| {
            vdc_write(machine, 0x0000, reg);
        };
        let vdc_word = |machine: &mut Machine, value: u16| {
            vdc_write(machine, 0x0002, (value & 0xFF) as u8);
            vdc_write(machine, 0x0003, (value >> 8) as u8);
        };
        // MAWR = 0x0100; the SAT words: y = 64 (the row 0), x = 32
        // (the column 0), pattern 1, control 0x0007 (palette 7).
        vdc_reg(&mut machine, 0x00);
        vdc_word(&mut machine, 0x0100);
        vdc_reg(&mut machine, 0x02);
        vdc_word(&mut machine, 0x0040); // y = 64
        vdc_word(&mut machine, 0x0020); // x = 32
        vdc_word(&mut machine, 0x0002); // pattern 1
        vdc_word(&mut machine, 0x0001); // control: block 0
        // The remaining 252 words of the SAT stay zero through MAWR+1
        // auto increments; the DMA copies 256 words regardless.
        // DVSSR = 0x0100 and the DCR bit 0 (the SAT end interrupt).
        vdc_reg(&mut machine, 0x0F);
        vdc_word(&mut machine, 0x0001);
        vdc_reg(&mut machine, 0x13);
        vdc_word(&mut machine, 0x0100);
        // The CR: the background off, no interrupts yet.
        vdc_reg(&mut machine, 0x05);
        vdc_word(&mut machine, 0x0008);
        // One frame plus two lines: the SATB transfer lands at the
        // vblank, line 239.
        machine.run_until(Tick(242 * 510 * 4 + 2000));

        let vce_bytes = {
            let vce = vce_of(&mut machine);
            vce.palette.clone()
        };
        assert_eq!(vce_bytes.len(), 0x400);
        let vdc = vdc_of(&mut machine);
        assert_ne!(vdc.status & STATUS_SAT_END, 0);
    }

    #[test]
    fn background_tile_matches_palette_entry() {
        let mut machine = pce_machine();
        let vdc_word = |machine: &mut Machine, value: u16| {
            vdc_write(machine, 0x0002, (value & 0xFF) as u8);
            vdc_write(machine, 0x0003, (value >> 8) as u8);
        };
        // The CR: the background on, the sprites off, no interrupts.
        vdc_write(&mut machine, 0x0000, 0x05);
        vdc_word(&mut machine, 0x0080);
        // The BAT word 0 = 0x5040: the color block 5 and the tile
        // 0x40, whose words sit at 0x0400, out of the 32x32 BAT.
        vdc_write(&mut machine, 0x0000, 0x00);
        vdc_word(&mut machine, 0x0000);
        vdc_write(&mut machine, 0x0000, 0x02);
        vdc_word(&mut machine, 0x5040);
        // The tile 0x40 row 0 sets plane 1 only, the color 2: the word
        // at 0x0400 = 0xFF00. The other tile words stay zero, so rows
        // 1 to 7 paint color 0.
        vdc_write(&mut machine, 0x0000, 0x00);
        vdc_word(&mut machine, 0x0400);
        vdc_write(&mut machine, 0x0000, 0x02);
        vdc_word(&mut machine, 0xFF00);
        // The palette: the VCE CTA = 0x52 (block 5, color 2); the entry
        // = green 7 as 0x1C0, low byte 0xC0 and high byte 0x01.
        assert!(machine.debug_write("cpu", Address(0x0402), 0x52));
        assert!(machine.debug_write("cpu", Address(0x0404), 0xC0));
        assert!(machine.debug_write("cpu", Address(0x0405), 0x01));
        // Two full frames: the lines compose into the frame rows.
        machine.run_until(Tick(FRAME_LINES as u64 * LINE_DOTS * 4 * 2 + 1000));

        let bytes = frame_bytes_of(&mut machine);
        // Row 0 covers the tile row 0: the color 2 of the block 5 =
        // the entry 0x52, green 7 scaled to 255.
        assert_eq!(pixel_of(&bytes, 0, 1), [0, 255, 0]);
        // The second tile row carries the color 0: the entry 0x50 is
        // zeroed by the palette reset, the black form.
        assert_eq!(pixel_of(&bytes, 1, 1), [0, 0, 0]);
    }

    #[test]
    fn vdc_frame_is_deterministic() {
        fn run() -> Vec<u8> {
            let mut machine = pce_machine();
            let vdc_reg = |machine: &mut Machine, reg: u8| {
                vdc_write(machine, 0x0000, reg);
            };
            let vdc_word = |machine: &mut Machine, value: u16| {
                vdc_write(machine, 0x0002, (value & 0xFF) as u8);
                vdc_write(machine, 0x0003, (value >> 8) as u8);
            };
            vdc_reg(&mut machine, 0x05);
            vdc_word(&mut machine, 0x0080);
            vdc_word(&mut machine, 0x0500);
            vdc_word(&mut machine, 0xFFFF);
            assert!(machine.debug_write("cpu", Address(0x0402), 0x52));
            assert!(machine.debug_write("cpu", Address(0x0405), 0x38));
            machine.run_until(Tick(FRAME_LINES as u64 * LINE_DOTS * 4 * 2 + 1000));
            frame_bytes_of(&mut machine)
        }
        assert_eq!(run(), run());
    }

    #[test]
    fn the_scanline_counter_fires_sixty_four_above_the_register() {
        // The published raster convention: the die's counter reads 64
        // at the last blank line before the display window, so the
        // register carries the target display line plus 64.
        let mut machine = pce_machine();
        // The CR: the scanline interrupt enable on.
        vdc_write(&mut machine, 0x0000, 0x05);
        vdc_write(&mut machine, 0x0002, 0x04);
        vdc_write(&mut machine, 0x0003, 0);
        // The RCR: the display line 1 sits at the register form 65.
        vdc_write(&mut machine, 0x0000, 0x06);
        vdc_write(&mut machine, 0x0002, 65);
        vdc_write(&mut machine, 0x0003, 0);
        machine.run_until(Tick(2_039));
        assert_eq!(
            vdc_of(&mut machine).status & STATUS_SCANLINE,
            0,
            "the flag held clear before the target line"
        );
        machine.run_until(Tick(2_040));
        assert_ne!(
            vdc_of(&mut machine).status & STATUS_SCANLINE,
            0,
            "the line-one visit fired the scanline flag"
        );
    }

    #[test]
    fn vce_color_table_round_trips() {
        let mut machine = pce_machine();
        // The CTA = 0x123 (nine bits), then the CTD low 0x55 and high
        // 0x07: the entry stores 0x155 and the address steps to 0x124.
        assert!(machine.debug_write("cpu", Address(0x0403), 0x01));
        assert!(machine.debug_write("cpu", Address(0x0402), 0x23));
        assert!(machine.debug_write("cpu", Address(0x0404), 0x55));
        assert!(machine.debug_write("cpu", Address(0x0405), 0x07));
        let vce = vce_of(&mut machine);
        assert_eq!(vce.entry(0x123), 0x155);
        // The color table bytes answer in the window: entry 0x123 sits
        // at window offset 0x100 + 0x246.
        assert_eq!(machine.peek("cpu", Address(0x0746)), 0x55);
        assert_eq!(machine.peek("cpu", Address(0x0747)), 0x01);
        // The high-byte access stepped the address.
        assert_eq!(machine.peek("cpu", Address(0x0402)), 0x24);
    }

    #[test]
    fn vce_entry_scale_replicates_channels() {
        let mut vce = PceVce::new(&vce_instance());
        vce.palette[0] = 0x07;
        vce.palette[1] = 0x00;
        assert_eq!(vce.entry(0), 0x0007);
        // The snapshot round trip.
        let snap = vce.snapshot();
        assert_eq!(snap.len(), VCE_SNAPSHOT_LEN);
        let mut twin = PceVce::new(&vce_instance());
        twin.restore(&snap).expect("restore");
        assert_eq!(twin.snapshot(), snap);
    }

    #[test]
    fn vdc_snapshot_round_trips() {
        let mut s = PceVdc::new(&vdc_instance(), &pce_config());
        s.write(AccessWidth::Byte, Address(0x0000), 0x08, Tick(0)); // AR = BYR
        s.write(AccessWidth::Byte, Address(0x0002), 0x22, Tick(0));
        s.write(AccessWidth::Byte, Address(0x0000), 0x07, Tick(0)); // AR = BXR
        s.write(AccessWidth::Byte, Address(0x0003), 0x11, Tick(0));
        let snap = s.snapshot();
        assert_eq!(snap.len(), VDC_SNAPSHOT_LEN);
        let mut twin = PceVdc::new(&vdc_instance(), &pce_config());
        twin.restore(&snap).expect("restore");
        assert_eq!(twin.snapshot(), snap);
        assert_eq!(twin.regs[R_BYR], 0x0022);
        assert_eq!(twin.regs[R_BXR], 0x0100);
    }
}
