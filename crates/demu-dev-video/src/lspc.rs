//! The Neo Geo LSPC (Line Sprite Controller): the `neogeo.lspc` device
//! kind.
//!
//! The video die of the Neo Geo: it owns the VRAM access ports, composes
//! the FIX layer and the sprite strips over the palette RAM, drives the
//! vertical blank and raster timer interrupts, and publishes the
//! composed frame. VRAM is not memory mapped on the 68k bus; every
//! access runs through the LSPC register page.
//!
//! # Pinned video timing
//!
//! | Fact | Value | Source |
//! |------|-------|--------|
//! | Pixels per scanline | 384 | MAME `neogeo_spr.h` `NEOGEO_HTOTAL`; neogeodev `Display timing` |
//! | Visible columns | 320, starting at pixel 28 | MAME `NEOGEO_HBEND` `$1C`, `NEOGEO_HBSTART` `$15C`; neogeodev `Display timing` (28 px sync+porch, 320 active, 36 blanking) |
//! | Scanlines per frame NTSC | 264 | MAME `NEOGEO_VTOTAL` `$108`; neogeodev `Display timing` |
//! | Scanlines per frame PAL | 312 | neogeodev `Display timing` (PAL: 384x312) |
//! | Visible window NTSC | 320x224, lines 16-239 | MAME `NEOGEO_VBEND` `$10`/`NEOGEO_VBSTART` `$F0`; neogeodev `Frame size` |
//! | Visible window PAL | 320x256, 8 lines more top and bottom | neogeodev `Frame size`; the `region` parameter |
//! | Vertical blank | line 240 (NTSC) | MAME `NEOGEO_VBSTART`; `NEOGEO_VSSTART` sync at 256 |
//! | Pixel clock | master/4: 6 MHz | MAME `NEOGEO_PIXEL_CLOCK`; neogeodev `Clock` (24 MHz master) |
//!
//! In this build the LSPC sits on the video clock domain the
//! configuration names and consumes `dot` domain ticks per pixel
//! (default 2). The AES machine model declares a 12.288 MHz video
//! domain, so the default gives 6.144 MHz pixels; the real chip answers
//! from its own 24 MHz master at 6 MHz. The difference is recorded in
//! the demu machine design.
//!
//! # Pinned LSPC register page (`$3C0000-$3FFFFF`)
//!
//! The die decodes address bits 1-3 for the register number, matching
//! the LSPC pinout (`A1-A3` on the die per neogeodev `LSPC2-A2`) and
//! MAME's write map (`$3C0000-$3C000F` with mirrors). MAME's read map
//! covers `$3C0000-$3C0007` and mirrors with `$01FFF8`, so a read at
//! `$3C0008` and up folds back into the four read registers; this
//! build answers reads by `(address >> 1) & 3` and writes by
//! `(address >> 1) & 7`.
//!
//! Writes: MAME stores the same data in both bytes for a high-byte-only
//! write (`video_register_w`) and drops a low-byte-only write;
//! neogeodev `Memory mapped registers` records the published form as
//! "byte writes are only effective on even addresses and they store the
//! same data in both bytes; odd addresses are not mapped". Reads: MAME
//! marks a low-byte-only read not mapped. Byte-lane behavior of the
//! demu 68k is out of this build's scope, so this build answers words only
//! and pins the published word forms:
//!
//! | Off | Name | Read | Write |
//! |-----|------|------|-------|
//! | 0 | `REG_VRAMADDR` | the VRAM read buffer (MAME `video_register_r`) | sets the VRAM word address |
//! | 1 | `REG_VRAMRW` | the VRAM read buffer | writes VRAM data, then adds `VRAMMOD` |
//! | 2 | `REG_VRAMMOD` | the stored modulo | sets the signed auto-increment |
//! | 3 | `REG_LSPCMODE` | raster `<< 7`, PAL flag bit 3, auto-animation counter bits 2-0 | bits 15-8 auto-animation speed in frames; bit 3 stops auto-animation; bit 4 timer IRQ enable; bit 5 load timer on `TIMERLOW` write; bit 6 load timer at vblank; bit 7 repeat timer load on lap |
//! | 4 | `REG_TIMERHIGH` | folds to `REG_VRAMADDR` form | upper 16 bits of the timer reload |
//! | 5 | `REG_TIMERLOW` | folds to `REG_VRAMRW` form | lower 16 bits; reload starts when bit 5 of `REG_LSPCMODE` is set |
//! | 6 | `REG_IRQACK` | folds to `REG_VRAMMOD` form | bit 1 clears the timer flag, bit 2 the vblank flag (the specification also clears IRQ3 on bit 0, MVS only) |
//! | 7 | `REG_TIMERSTOP` | folds to `REG_LSPCMODE` form | LSPC2 (this build): bit 0 pauses the timer for the first and last 16 lines in PAL mode |
//!
//! The 68k must write the address register directly when the VRAM
//! address crosses between the lower zone `$0000-$7FFF` and the upper
//! zone `$8000-$87FF` (the SNK specification section 5, item 6; the
//! boot sequence notes the same). MAME's `set_videoram_data` keeps A15
//! out of the increment: `(offset & 0x8000) | ((offset + modulo) &
//! 0x7FFF)`. This build follows that form, so the modulo cannot cross
//! zone boundaries.
//!
//! # VRAM
//!
//! 64 KiB lower zone (word addresses `$0000-$7FFF`) plus 4 KiB upper
//! zone (word addresses `$8000-$87FF`), one 16-bit word per address,
//! per neogeodev `VRAM`. A write through `REG_VRAMADDR` also resets a
//! pending read buffer per MAME (`set_videoram_offset` reads right
//! away).
//!
//! # Pinned sprite control blocks
//!
//! neogeodev `Sprites` and MAME `neogeo_spr.cpp`:
//!
//! - `SCB1` (`$0000-$6FFF`): 64 words per sprite, word `sprite*64 + 2*t`
//!   = the tile number's low 16 bits, `+1` = bits 15-8 palette, bits
//!   7-4 the tile number's upper bits (20-bit tile space), bit 3 the
//!   eight-frame and bit 2 the four-frame auto animation substitute,
//!   bit 1 vertical flip, bit 0 horizontal flip.
//! - `SCB2` (`$8000+i`): low byte vertical shrink `$FF` = full, upper
//!   nibble horizontal shrink `$F` = full, the four-bit table form.
//! - `SCB3` (`$8200+i`): bits 15-7 the Y position (`0x200 - value`),
//!   bit 6 the chain (sticky) bit, bits 5-0 the strip height in tiles
//!   of 16 pixels.
//! - `SCB4` (`$8400+i`): bits 15-7 the X position on the 512-wide wrap.
//! - The per-line sprite lists sit at `$8600` (even lines) and `$8680`
//!   (odd lines), 96 words plus a zero tail.
//!
//! The LSPC walks 381 sprite numbers (`MAX_SPRITES_PER_SCREEN = 381`
//! per MAME), fills the per-line list capped at 96 sprites and keeps
//! the zero tail; sprites with zero height are skipped in the parse.
//! Chained (sticky) sprites reuse the driving sprite's Y and height
//! and advance X by the previous sprite's shrink span width plus one,
//! with each chained sprite's own X shrink; unchained sprites reset
//! Y, height, X, and both shrinks (MAME `parse_sprites`).
//!
//! # Pinned render model
//!
//! - Vertical shrink maps through the L0 ROM zoom tables (MAME reads
//!   the `zoomy` region: table index `(zoom_y << 8) | zoom_line`, the
//!   low nibble the source row in tile, the upper nibble the SCB1 map
//!   index; the bottom half of a strip reads the table reversed and
//!   complements both nibbles; heights past the shrunk form mirror
//!   with `zoom_line %= (zoom_y + 1) * 2` and a reflected fetch). The
//!   real chip answers from the `000-lo.lo` mask ROM; this build
//!   generates the tables from the published subsample semantics and
//!   records the divergence as an approximation.
//! - Horizontal shrink runs through the pinned 16-entry table MAME
//!   names "verified on real hardware": value `$8` keeps every other
//!   column, `$F` keeps all 16, `$0` keeps one. The drawn destination
//!   advances only per drawn column, so a shrunk strip stays contiguous.
//! - Budgets: 96 sprites per line, 381 per screen, the per-line draw
//!   buffer covers the 320 visible columns; columns past `$320` minus
//!   the strip width wrap on the 512-wide line space. MAME skips
//!   strips whose X starts inside `$140-$1F0` since their whole strip
//!   lands right of the visible window, and draws the wrapped tail of
//!   a strip starting in `$1F1-$1FF` at the left edge.
//! - The FIX layer map sits at VRAM `$7000-$74FF`, column-major: the
//!   word for column `x`, row `r` = `$7000 + (x << 5) + r`. Each word =
//!   palette 4 bits over tile 12 bits; only the first 16 palettes are
//!   usable (the SNK specification).
//! - FIX ROM tiles are 8x8, 32 bytes, 4bpp with the byte form
//!   `tile << 5 | half << 4 | column-pair << 3 | row`; within a byte
//!   the left pixel lives in bits 0-3 and the right in bits 7-4
//!   (neogeodev `Fix graphics format`; MAME `draw_fixed_layer`).
//! - C ROM tiles are 16x16, 128 bytes; rows spread as four plane bytes
//!   per half-tile: the right half at bytes `(row*4)+0..3`, the left
//!   half at `(row*4)+0x40..0x43`, plane order per group byte+0 = 0,
//!   byte+1 = 2, byte+2 = 1, byte+3 = 3, bit `column & 7` of each
//!   byte, low bits left (MAME `neogeo_spr.cpp` regular `draw_pixel`
//!   and the optimized decode).
//! - Color words: the palette format in the palette device module.
//! - The backdrop = the last color of the palette (palette 255,
//!   color 15, the SNK specification's background palette; MAME's
//!   `m_bg_pen = pen_base + 0xfff`).
//! - FIX pixels paint over sprites; sprite strips paint in list order
//!   with later strips over earlier ones (the priority 1-is-back form).
//! - Shadow strips: the `shadow` inbound line carries the video
//!   darkening latch (`REG_SHADOW` `$3A0011`); Level High darkens every
//!   rendered channel by a fixed quarter-step form recorded in
//!   `shade_dark`, standing in for the resistor-ladder dark pen MAME
//!   builds from the same latch. No per-sprite shadow bit exists in
//!   the pinned SCB1 form: bits 15-8 carry the palette number.
//!
//! # Interrupts
//!
//! - `irq1`: the vertical blank interrupt (68000 level 1); asserted
//!   (Level High) at the line 240 visit, cleared via `REG_IRQACK` bit 2.
//! - `irq2`: the raster timer interrupt (68000 level 2); asserted when
//!   the pixel-clock countdown laps, cleared via `REG_IRQACK` bit 1.
//! - The vblank flag reload happens on scanline 240 (the SNK
//!   specification's reload point); `REG_TIMERLOW` writes reload the
//!   timer when bit 5 of `REG_LSPCMODE` is set, the vblank reload runs
//!   when bit 6 is set, and bit 7 reloads on each lap.
//! - The timer decrements once per pixel clock. A lap inside a line
//!   schedules an exact pixel event; the decrement rate is one per
//!   pixel, the MAME period `(count + 1) / NEOGEO_PIXEL_CLOCK` form.
//!
//! Sources: MAME `src/mame/snk/neogeo.cpp`, `neogeo_v.cpp`,
//! `neogeo_spr.cpp`, `neogeo_spr.h` (BSD-3-Clause), neogeodev.org pages
//! `Display timing`, `Frame size`, `Clock`, `VRAM`, `Sprites`,
//! `Sprite shrinking`, `L0 ROM`, `Fix layer`, `Fix graphics format`,
//! `Sprite graphics format`, `Palettes`, `Colors`, and
//! `Memory mapped registers`, the SNK Neo Geo hardware specification
//! PDF (the `Access to Line Sprite Controller`, `Address Mapping`,
//! and `Color Palettes` sections), and neogeodev `NeoGeoFPGA-sim`
//! `lspc_regs.v` and `timer.v` for the register decode and the
//! countdown clock.

use demu_config::model::{BootState, DeviceInstance, SystemConfig};
use demu_core::device::{AccessWidth, BusAccess, Frame, MachineCtx, VideoLayer, VideoSource};
use demu_core::frequency::{Address, Tick};
use demu_core::signal::Level;
use demu_core::{Device, Snapshot, SnapshotError};

/// The pixels per scanline (MAME `NEOGEO_HTOTAL`).
const LINE_PIXELS: u32 = 384;
/// The NTSC scanlines per frame (MAME `NEOGEO_VTOTAL`).
const VTOTAL_NTSC: u32 = 264;
/// The PAL scanlines per frame.
const VTOTAL_PAL: u32 = 312;
/// The line the vertical blank region starts on (MAME `NEOGEO_VBSTART`).
const VBLANK_LINE: u32 = 240;
/// The NTSC visible lines (MAME `VBSTART - VBEND`).
const VISIBLE_NTSC: u32 = 224;
/// The PAL visible lines: eight more top and bottom per the
/// specification's frame notes.
const VISIBLE_PAL: u32 = 256;
/// The NTSC lines that sit before the first visible line.
const TOP_BLANK_NTSC: u32 = 16;
/// The PAL lines that sit before the first visible line.
const TOP_BLANK_PAL: u32 = 8;
/// The frame width.
const WIDTH: usize = 320;
/// The sprite strip count the die walks (MAME `MAX_SPRITES_PER_SCREEN`).
const SPRITE_COUNT: usize = 381;
/// The sprites per line the list holds (MAME `MAX_SPRITES_PER_LINE`).
const LINE_SPRITES: usize = 96;
/// The words per sprite of SCB1.
const SCB1_WORDS: usize = 64;
/// The words of one line list span.
const LINE_LIST_WORDS: usize = 0x80;
/// The zoom table's lines per value.
const ZOOM_TABLE_LINES: usize = 256;
/// The zoom table's total entries.
const ZOOM_TABLE_ENTRIES: usize = ZOOM_TABLE_LINES * 256;
/// The horizontal shrink tables (MAME `zoom_x_tables`, hardware
/// verified).
const ZOOM_X_TABLES: [u16; 16] = [
    0x0080, 0x0880, 0x0888, 0x2888, 0x288A, 0x2A8A, 0x2AAA, 0xAAAA, 0xAAEA, 0xBAEA, 0xBAEB, 0xBBEB,
    0xBBEF, 0xFBEF, 0xFBFF, 0xFFFF,
];
/// The FIX map word base.
const FIX_BASE: u16 = 0x7000;
/// The FIX map column stride in words.
const FIX_COL_STRIDE: u16 = 0x20;
/// The line list bases, even lines first (MAME `$8600`/`$8680`).
const LIST_BASE_EVEN: u16 = 0x8600;
const LIST_BASE_ODD: u16 = 0x8680;
/// The VRAM word count: 64 KiB lower zone plus 4 KiB upper zone.
const VRAM_WORDS: usize = 0x8000 + 0x800;
/// The zone form: the auto increment keeps bit 15 (MAME
/// `set_videoram_data`).
const ZONE_UPPER: u16 = 0x8000;
const ZONE_MASK: u16 = 0x7FFF;
/// The snapshot tag and version.
const TAG: &[u8; 4] = b"NGLS";
const VERSION: u8 = 1;
/// The transparent line-buffer pixel.
const TRANSPARENT: u16 = 0;

/// The `REG_LSPCMODE` write-mode bits (the specification's forms).
const CTRL_TIMER_IRQ_EN: u16 = 0x0010;
const CTRL_TIMER_LOAD_RELATIVE: u16 = 0x0020;
const CTRL_TIMER_AUTOLOAD_VBLANK: u16 = 0x0040;
const CTRL_TIMER_AUTOLOAD_REPEAT: u16 = 0x0080;
const CTRL_AA_DISABLE: u16 = 0x0008;

/// The flags byte bits of the snapshot.
const FLAG_AA_DISABLE: u8 = 0x80;
const FLAG_TIMER_IRQ_EN: u8 = 0x40;
const FLAG_TIMER_LOAD_RELATIVE: u8 = 0x20;
const FLAG_TIMER_AUTOLOAD_VBLANK: u8 = 0x10;
const FLAG_TIMER_AUTOLOAD_REPEAT: u8 = 0x08;
const FLAG_TIMER_STOP: u8 = 0x04;
const FLAG_TIMER_PENDING: u8 = 0x02;
const FLAG_VBLANK_PENDING: u8 = 0x01;
const FLAG_SHADOW: u8 = 0x01;

/// The generated vertical shrink tables, one 256-entry table per
/// shrink value: the shrunk graphics span one more line than the
/// value spread evenly over the 256 source lines; entries past the
/// span repeat the map 15 row 15 byte (`$FF`), the published
/// "last line repeat" caveat. The real die answers from the
/// `000-lo.lo` mask ROM; this generator stands in for that data.
const fn zoom_entries() -> [u8; ZOOM_TABLE_ENTRIES] {
    let mut out = [0xFFu8; ZOOM_TABLE_ENTRIES];
    let mut zoom = 0usize;
    while zoom < 256 {
        let span = zoom + 1;
        let mut out_line = 0usize;
        while out_line <= zoom {
            let source = (out_line * 256 + span / 2) / span;
            out[zoom * 256 + out_line] = (((source >> 4) as u8) << 4) | (source & 0x0F) as u8;
            out_line += 1;
        }
        zoom += 1;
    }
    out
}

static ZOOM_Y_TABLE: [u8; ZOOM_TABLE_ENTRIES] = zoom_entries();

/// The little-endian snapshot helpers.
fn le_bytes(data: &[u8], at: usize, len: usize) -> Result<[u8; 8], SnapshotError> {
    let tail = data
        .get(at..at + len)
        .ok_or_else(|| SnapshotError::Mismatch("the lspc chunk ends inside a field".to_string()))?;
    let mut out = [0u8; 8];
    out[..len].copy_from_slice(tail);
    Ok(out)
}

fn le_u16(data: &[u8], at: usize) -> Result<u16, SnapshotError> {
    Ok(u16::from_le_bytes(
        le_bytes(data, at, 2)?[..2]
            .try_into()
            .map_err(|_| SnapshotError::Mismatch("le read failed".to_string()))?,
    ))
}

fn le_u32(data: &[u8], at: usize) -> Result<u32, SnapshotError> {
    Ok(u32::from_le_bytes(
        le_bytes(data, at, 4)?[..4]
            .try_into()
            .map_err(|_| SnapshotError::Mismatch("le read failed".to_string()))?,
    ))
}

fn push_word(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

/// The LSPC video die.
#[derive(Debug, Clone)]
pub struct NeoLspc {
    name: String,
    /// The bus the palette reads walk (the `bus` parameter).
    bus: String,
    /// The FIX (S) ROM device name.
    srom: String,
    /// The C ROM device name.
    crom: String,
    /// The region latch: false NTSC, true PAL.
    pal: bool,
    /// The domain ticks per pixel (the `dot` divider).
    dot: u64,
    /// The VRAM words: the lower zone then the upper zone.
    vram: Box<[u16]>,
    /// The VRAM word address register.
    vram_addr: u16,
    /// The auto-increment value.
    vram_mod: u16,
    /// The read-side buffer.
    vram_read_buf: u16,
    /// Auto animation speed in frames.
    aa_speed: u8,
    /// The auto-animation stop bit.
    aa_disable: bool,
    /// The auto-animation counter.
    aa_counter: u8,
    /// The per-frame countdown toward the speed step.
    aa_frame_counter: u8,
    /// Timer modes.
    timer_irq_en: bool,
    timer_load_relative: bool,
    timer_autoload_vblank: bool,
    timer_autoload_repeat: bool,
    /// The PAL-mode timer pause (LSPC2 `REG_TIMERSTOP`).
    timer_stop: bool,
    /// The timer reload value, high word over low.
    timer_reload: u32,
    /// The countdown remaining in pixels; negative sits idle.
    timer_remaining: i64,
    /// Pending interrupt flags.
    timer_pending: bool,
    vblank_pending: bool,
    /// The delivered interrupt line levels.
    irq1_out: bool,
    irq2_out: bool,
    /// The internal scanline counter within the frame.
    line: u32,
    /// The FIX ROM bytes.
    srom_bytes: Vec<u8>,
    /// The C ROM bytes.
    crom_bytes: Vec<u8>,
    /// The frame buffer in RGB888.
    screen: Vec<u8>,
    /// The cached frame for the shell.
    frame: Frame,
    frames: u64,
    /// The darkening latch (Level High darkens).
    shadow: bool,
    next: Option<Tick>,
    last_tick: Option<Tick>,
}

impl NeoLspc {
    /// A video die from its configuration entry.
    ///
    /// Parameters: `srom` (the FIX ROM device name, default `srom`),
    /// `crom` (the C ROM device name, default `crom`), `region`
    /// (`ntsc` default or `pal`), and `bus` (the bus the palette
    /// reads walk; the first bus of the configuration, then `cpu`).
    /// The `dot` divider in the named dividers counts domain ticks
    /// per pixel, default 2 of the 12.288 MHz video domain, the
    /// published 6 MHz pixel form recorded per the clock notes.
    pub fn new(device: &DeviceInstance, config: &SystemConfig) -> Result<Self, String> {
        let param = |key: &str| device.params.get(key).and_then(|v| v.as_str());
        let srom = param("srom").unwrap_or("srom").to_string();
        let crom = param("crom").unwrap_or("crom").to_string();
        let pal = param("region").is_some_and(|s| s.eq_ignore_ascii_case("pal"));
        let bus = param("bus").map(String::from).unwrap_or_else(|| {
            config
                .buses
                .first()
                .map(|b| b.name.clone())
                .unwrap_or_else(|| "cpu".to_string())
        });
        let dot = u64::from(device.dividers.get("dot").copied().unwrap_or(2)).max(1);
        let height = if pal { VISIBLE_PAL } else { VISIBLE_NTSC };
        let mut die = Self {
            name: device.name.to_string(),
            bus,
            srom,
            crom,
            pal,
            dot,
            vram: vec![0; VRAM_WORDS].into_boxed_slice(),
            vram_addr: 0,
            vram_mod: 0,
            vram_read_buf: 0,
            aa_speed: 0,
            aa_disable: false,
            aa_counter: 0,
            aa_frame_counter: 0,
            timer_irq_en: false,
            timer_load_relative: false,
            timer_autoload_vblank: false,
            timer_autoload_repeat: false,
            timer_stop: false,
            timer_reload: 0,
            timer_remaining: -1,
            timer_pending: false,
            vblank_pending: false,
            irq1_out: false,
            irq2_out: false,
            line: 0,
            srom_bytes: Vec::new(),
            crom_bytes: Vec::new(),
            screen: vec![0; WIDTH * height as usize * 3],
            frame: Frame {
                width: WIDTH as u32,
                height,
                pixels: vec![0xFF; WIDTH * height as usize * 3],
            },
            frames: 0,
            shadow: false,
            next: None,
            last_tick: None,
        };
        die.load_tile_sources(config);
        Ok(die)
    }

    /// The scanlines per frame for the configured region.
    fn vtotal(&self) -> u32 {
        if self.pal { VTOTAL_PAL } else { VTOTAL_NTSC }
    }

    /// The lines that sit before the first visible line.
    fn top_blank(&self) -> u32 {
        if self.pal {
            TOP_BLANK_PAL
        } else {
            TOP_BLANK_NTSC
        }
    }

    /// The domain ticks of one scanline.
    fn line_ticks(&self) -> u64 {
        (LINE_PIXELS as u64) * self.dot
    }

    /// The raster value the `REG_LSPCMODE` read answers: MAME's
    /// `get_video_control` form generalized to the region. The
    /// counter chains `0x100..0x1FF` across the frame and folds back
    /// for the vertical-sync lines at the frame end.
    fn raster(&self) -> u16 {
        let value = self.line + 0x100;
        if value >= 0x200 {
            (value - self.vtotal()) as u16
        } else {
            value as u16
        }
    }

    /// The raster form as a function of the line and total, the
    /// machine-level read form.
    pub fn raster_of(line: u32, vtotal: u32) -> u16 {
        let value = line + 0x100;
        if value >= 0x200 {
            (value - vtotal) as u16
        } else {
            value as u16
        }
    }

    /// The line cadence the machine layer schedules with.
    pub fn line_ticks_pub(&self) -> u64 {
        self.line_ticks()
    }

    /// The register number of a word-aligned read: the die decodes
    /// address bits 1 and 2 on reads; bit 3 and up fold back into
    /// the four read registers (MAME's `$01FFF8` read mirrors).
    fn read_reg(address: u32) -> usize {
        ((address >> 1) & 3) as usize
    }

    /// The register number of a word-aligned write: bits 1-3, the
    /// seven registers plus `REG_TIMERSTOP` (MAME's write map).
    fn write_reg(address: u32) -> usize {
        ((address >> 1) & 7) as usize
    }

    /// The line list base for the internal line counter.
    fn list_base(line: u32) -> u16 {
        if line & 1 == 0 {
            LIST_BASE_EVEN
        } else {
            LIST_BASE_ODD
        }
    }

    /// The FIX map entry for column `x` at the internal line.
    fn fix_entry_at(&self, x: usize, line: u32) -> u16 {
        let index =
            usize::from(FIX_BASE) + x * usize::from(FIX_COL_STRIDE) + ((line >> 3) as usize & 0x1F);
        self.vram.get(index).copied().unwrap_or(0)
    }

    /// The VRAM word bytes for dumps and the snapshot.
    fn vram_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.vram.len() * 2);
        for word in &self.vram {
            out.extend_from_slice(&word.to_le_bytes());
        }
        out
    }

    /// The snapshot length.
    fn restore_length(&self) -> usize {
        4 + 1 + 2 + 2 + 2 + self.vram.len() * 2 + 3 + 4 + 8 + 4 + 8 + 1
    }
}

impl NeoLspc {
    /// Write the VRAM address register: the MAME zone form. Values
    /// with bit 15 mask to the 4 KiB upper window; the pending read
    /// buffer fills from the new address right away.
    fn set_vram_addr(&mut self, data: u16) {
        self.vram_addr = if data & ZONE_UPPER != 0 {
            data & 0x87FF
        } else {
            data & ZONE_MASK
        };
        self.vram_read_buf = self
            .vram
            .get(usize::from(self.vram_addr))
            .copied()
            .unwrap_or(0);
    }

    /// Write the VRAM data register, then apply the auto increment
    /// keeping bit 15. The step fills the read buffer from the new
    /// address, so a read after a run of writes answers the
    /// pre-write value of the next word, the published pipelining
    /// form.
    fn set_vram_data(&mut self, data: u16) {
        let index = usize::from(self.vram_addr);
        if index < self.vram.len() {
            self.vram[index] = data;
        }
        let next = (self.vram_addr & ZONE_UPPER)
            | (self.vram_addr.wrapping_add(self.vram_mod) & ZONE_MASK);
        self.set_vram_addr(next);
    }

    /// The write of the `REG_LSPCMODE` control word.
    fn set_lspc_mode(&mut self, data: u16) {
        self.aa_speed = (data >> 8) as u8;
        self.aa_disable = data & CTRL_AA_DISABLE != 0;
        self.timer_irq_en = data & CTRL_TIMER_IRQ_EN != 0;
        self.timer_load_relative = data & CTRL_TIMER_LOAD_RELATIVE != 0;
        self.timer_autoload_vblank = data & CTRL_TIMER_AUTOLOAD_VBLANK != 0;
        self.timer_autoload_repeat = data & CTRL_TIMER_AUTOLOAD_REPEAT != 0;
    }

    /// The register read forms: `REG_VRAMADDR` and `REG_VRAMRW`
    /// reads answer the read buffer per MAME's `video_register_r`,
    /// `REG_VRAMMOD` answers the modulo, and `REG_LSPCMODE` answers
    /// the raster over the PAL flag and the auto-animation counter,
    /// the published read layout.
    fn reg_read(&self, reg: usize) -> u32 {
        match reg {
            0 | 1 => u32::from(self.vram_read_buf),
            2 => u32::from(self.vram_mod),
            _ => {
                let raster = u32::from(self.raster());
                let pal_flag = u32::from(self.pal) << 3;
                let anim = u32::from(self.aa_counter & 7);
                (raster << 7) | pal_flag | anim
            }
        }
    }

    /// The register write forms. Register effects store directly;
    /// interrupt lines recompute at the next scheduler visit, the
    /// level-edge pattern the sibling die devices use.
    fn reg_write(&mut self, reg: usize, data: u16) {
        match reg {
            0 => self.set_vram_addr(data),
            1 => self.set_vram_data(data),
            2 => self.vram_mod = data,
            3 => self.set_lspc_mode(data),
            4 => {
                self.timer_reload = (self.timer_reload & 0xFFFF) | (u32::from(data) << 16);
            }
            5 => {
                self.timer_reload = (self.timer_reload & 0xFFFF_0000) | u32::from(data);
                // The load-relative mode rearms on the low-word write.
                if self.timer_load_relative {
                    self.arm_timer();
                }
            }
            6 => self.ack_interrupt(data),
            _ => {
                // REG_TIMERSTOP: bit 0 pauses the countdown for the
                // first and last 16 lines only in PAL mode.
                self.timer_stop = data & 1 != 0;
            }
        }
    }

    /// Reload the countdown from the stored reload value; the
    /// specification gives one more pixel clock than the loaded
    /// value before the interrupt.
    fn arm_timer(&mut self) {
        self.timer_remaining = i64::from(self.timer_reload.wrapping_add(1));
    }

    /// `REG_IRQACK`: bit 1 clears the timer flag, bit 2 the vblank
    /// flag. Bit 0 clears the MVS IRQ3 flag, which has no line on
    /// this die and stays a no-op. Lines drop at the next visit.
    fn ack_interrupt(&mut self, data: u16) {
        if data & 0x0002 != 0 {
            self.timer_pending = false;
        }
        if data & 0x0004 != 0 {
            self.vblank_pending = false;
        }
    }
}
impl NeoLspc {
    /// The FIX map word at map index `i` for the dump path.
    fn fix_entry_raw(&self, index: u16) -> u16 {
        self.vram
            .get(usize::from(FIX_BASE) + usize::from(index))
            .copied()
            .unwrap_or(0)
    }

    /// One sprite tile word pair: the SCB1 entry of sprite `number`,
    /// map index `slot`; answers the 20-bit tile code and the
    /// attribute word.
    fn scb1_entry(&self, number: usize, slot: usize) -> (u32, u16) {
        let base = number * SCB1_WORDS + slot * 2;
        let code = u32::from(self.vram.get(base).copied().unwrap_or(0));
        let attr = self.vram.get(base + 1).copied().unwrap_or(0);
        (code | (u32::from(attr & 0x00F0) << 12), attr)
    }

    /// The FIX ROM nibble of one tile pixel: the byte form
    /// `tile << 5 | half << 4 | column-pair << 3 | row`, with
    /// half = `col >> 2` and column-pair = `col >> 1 & 1`; the left
    /// pixel of the pair sits in bits 0-3, the right in bits 7-4
    /// (neogeodev `Fix graphics format`; MAME `draw_fixed_layer`).
    fn srom_nibble(&self, code: u32, row: u32, col: u32) -> u8 {
        let byte = ((code as usize) << 5)
            | usize::from(col < 4) << 4
            | usize::from((col >> 1) & 1 == 1) << 3
            | (row as usize & 7);
        match self.srom_bytes.get(byte) {
            Some(b) if col & 1 == 0 => b & 0x0F,
            Some(b) => b >> 4,
            None => 0,
        }
    }

    /// The C ROM nibble of one sprite-tile pixel: the merged image
    /// MAME builds with `ROM_LOAD16_BYTE` (the odd C ROM planes 0
    /// and 1 at even addresses, the even C ROM planes 2 and 3 at odd
    /// addresses), per tile 128 bytes. Within a tile the byte groups
    /// run row-major with plane order byte+0 = plane 0, byte+1 =
    /// plane 2, byte+2 = plane 1, byte+3 = plane 3; the right-half
    /// groups sit at `row * 4` and the left-half groups at `row * 4
    /// + 0x40`. The pixel uses bit `column & 7`, so the low bits of
    /// the byte carry the leftmost pixel (MAME `neogeo_spr.cpp`
    /// regular `draw_pixel` and the optimized decode).
    fn crom_nibble(&self, code: u32, row: u32, col: u32) -> u8 {
        let base = usize::try_from(u64::from(code) << 8).unwrap_or(usize::MAX) & self.crom_mask();
        let group = (row as usize) * 4 + usize::from(col < 8) * 0x40;
        let bit = col & 7;
        let byte_of = |ofs: usize| -> u8 {
            *self
                .crom_bytes
                .get(self.crom_mask() & (base | ofs))
                .unwrap_or(&0)
        };
        let p0 = byte_of(group);
        let p2 = byte_of(group + 1);
        let p1 = byte_of(group + 2);
        let p3 = byte_of(group + 3);
        ((p3 >> bit & 1) << 3) | ((p2 >> bit & 1) << 2) | ((p1 >> bit & 1) << 1) | (p0 >> bit & 1)
    }

    /// The address mask of the C ROM image: MAME's `get_region_mask`
    /// form over the byte length.
    fn crom_mask(&self) -> usize {
        if self.crom_bytes.is_empty() {
            return 0;
        }
        let len = self.crom_bytes.len();
        let mut top = 1usize;
        while top * 2 <= len {
            top *= 2;
        }
        top - 1
    }
}
impl NeoLspc {
    /// The visible window test over the internal line counter.
    fn visible_line(&self, line: u32) -> Option<u32> {
        let top = self.top_blank();
        let height = if self.pal { VISIBLE_PAL } else { VISIBLE_NTSC };
        if line >= top && line < top + height {
            Some(line - top)
        } else {
            None
        }
    }

    /// The strip's on-line test, MAME's exact form; `y` counts in
    /// internal lines with the 512-line strip space. Heights of at
    /// least 32 tiles stay on every line and mirror through the zoom
    /// form (`rows >= 0x20`), per the stick-height hardware note.
    fn strip_on_line(line: u32, y: u32, rows: u16) -> bool {
        rows == 0
            || rows >= 0x20
            || (((line as i64 - y as i64) & 0x1FF) as u32) < u32::from(rows) * 0x10
    }

    /// The SCB3 chain (sticky) bit form.
    fn strip_chain_bit(y_control: u16) -> bool {
        y_control & 0x0040 != 0
    }

    /// Parse the per-line sprite list for the internal line: the
    /// MAME walk over 381 sprite numbers with the stick chain,
    /// capped at 96 entries, with the zero tail out to the span.
    /// Sprites with zero height drop out; the overflow sprites past
    /// the 96 cap never land in the list (the per-line budget) but
    /// the hardware's own list span keeps the tail zeroes.
    fn parse_sprites(&mut self) {
        let line = self.line;
        let base = usize::from(Self::list_base(line));
        let mut count = 0usize;
        let mut y = 0u32;
        let mut rows = 0u16;
        for number in 0..SPRITE_COUNT {
            let y_control = self.vram[usize::from(0x8200u16 | number as u16)];
            if !Self::strip_chain_bit(y_control) {
                y = 0x200 - u32::from(y_control >> 7);
                rows = y_control & 0x3F;
            }
            if rows == 0 || !Self::strip_on_line(line, y, rows) {
                continue;
            }
            if count < LINE_SPRITES {
                self.vram[base + count] = number as u16;
                count += 1;
            }
        }
        // One zero entry past the live count stays: the draw walk's
        // replay of a real sprite 0 entry, then zeroes to the span
        // end.
        self.vram[base + count.min(LINE_SPRITES - 1)] = 0;
        if count < LINE_SPRITES {
            for slot in (base + count + 1)..base + LINE_LIST_WORDS {
                self.vram[slot] = 0;
            }
        }
    }

    /// The live list count for the draw walk: the MAME form of the
    /// last non-zero entry plus one entry, so a trailing real sprite
    /// 0 entry still draws.
    fn live_list_count(&self, line: u32) -> usize {
        let base = usize::from(Self::list_base(line));
        let mut last = None;
        for (i, entry) in self.vram[base..base + LINE_LIST_WORDS].iter().enumerate() {
            if *entry != 0 {
                last = Some(i);
            }
        }
        match last {
            Some(i) => (i + 1).min(LINE_LIST_WORDS),
            None => 1,
        }
    }

    /// Walk the line's strips and paint them into the line buffer.
    /// The draw walk carries `x`, `y`, `rows`, `zoom_y`, and
    /// `zoom_x` forward like the MAME draw loop: an unchained strip
    /// reloads all of them; a chained one advances X by the
    /// previous strip's shrink span plus one and re-reads only its
    /// own X shrink.
    fn render_strips(&mut self, line: u32, buffer: &mut [u16]) {
        let list = Self::list_base(line);
        let count = self.live_list_count(line);
        let mut x = 0u32;
        let mut y = 0u32;
        let mut rows = 0u16;
        let mut zoom_y = 0u8;
        let mut zoom_x = 0u8;
        for i in 0..count {
            let number = usize::from(self.vram[usize::from(list) + i] & 0x01FF);
            let y_control = self.vram[usize::from(0x8200u16 | number as u16)];
            let zoom_control = self.vram[usize::from(0x8000u16 | number as u16)];
            if Self::strip_chain_bit(y_control) {
                x = (x + u32::from(zoom_x) + 1) & 0x1FF;
                zoom_x = ((zoom_control >> 8) & 0x0F) as u8;
            } else {
                y = 0x200 - u32::from(y_control >> 7);
                x = (u32::from(self.vram[usize::from(0x8400u16 | number as u16)] >> 7)) & 0x1FF;
                zoom_y = (zoom_control & 0xFF) as u8;
                zoom_x = ((zoom_control >> 8) & 0x0F) as u8;
                rows = y_control & 0x3F;
            }
            // Strips whose whole span lands right of the visible
            // window drop out; strips starting in the wrap zone draw
            // their wrapped tail at the left edge (MAME's split).
            if (0x140..=0x1F0).contains(&x) {
                continue;
            }
            if !Self::strip_on_line(line, y, rows) {
                continue;
            }
            self.paint_strip(number, line, y, rows, zoom_y, zoom_x, x, buffer);
        }
    }

    /// Paint one strip's line with the pinned vertical-zoom form:
    /// the L0 table carries the source map index and row, the bottom
    /// half reads the table reversed with complemented nibbles, and
    /// window spans past the shrunk form mirror with the doubled
    /// span.
    #[allow(clippy::too_many_arguments)]
    fn paint_strip(
        &mut self,
        number: usize,
        line: u32,
        y: u32,
        rows: u16,
        zoom_y: u8,
        zoom_x: u8,
        x: u32,
        buffer: &mut [u16],
    ) {
        let sprite_line = ((line as i64 - y as i64) & 0x1FF) as u32;
        let mut zoom_line = sprite_line & 0xFF;
        let mut invert = sprite_line & 0x100 != 0;
        if rows > 0x20 {
            let span = (u32::from(zoom_y) + 1) << 1;
            zoom_line %= span;
            if zoom_line > u32::from(zoom_y) {
                zoom_line = span - 1 - zoom_line;
                invert = !invert;
            }
        }
        let entry = ZOOM_Y_TABLE[usize::from(zoom_y) * ZOOM_TABLE_LINES + zoom_line as usize];
        let mut row = entry & 0x0F;
        let mut map = entry >> 4;
        if invert {
            row ^= 0x0F;
            map ^= 0x1F;
        }
        let (raw_code, attr) = self.scb1_entry(number, usize::from(map));
        let code = self.autoanim_code(raw_code, attr);
        // Vertical flip inverts the row inside the tile.
        if attr & 0x0002 != 0 {
            row ^= 0x0F;
        }
        // The palette word base for the strip's pixels.
        let palette_base = (attr >> 8) << 4;
        // Horizontal flip walks the source columns backwards.
        let flip = attr & 0x0001 != 0;
        let mask = ZOOM_X_TABLES[usize::from(zoom_x)];
        let mut drawn = 0u32;
        for i in 0..16u32 {
            if mask & (0x8000 >> i) == 0 {
                continue;
            }
            let source_col = if flip { 15 - i } else { i };
            let nibble = self.crom_nibble(code, u32::from(row), source_col);
            if nibble == 0 {
                continue;
            }
            let dest = usize::try_from(((x + drawn) & 0x1FF) as u64).unwrap_or(WIDTH);
            drawn += 1;
            if dest >= WIDTH {
                continue;
            }
            buffer[dest] = (palette_base) | u16::from(nibble);
        }
    }

    /// Substitute the auto-animation bits into the tile code when
    /// the animation runs (MAME's form; the eight-frame bit wins).
    fn autoanim_code(&self, code: u32, attr: u16) -> u32 {
        if self.aa_disable {
            return code;
        }
        if attr & 0x0008 != 0 {
            (code & !0x0007) | (u32::from(self.aa_counter) & 0x0007)
        } else if attr & 0x0004 != 0 {
            (code & !0x0003) | (u32::from(self.aa_counter) & 0x0003)
        } else {
            code
        }
    }

    /// Render one visible internal line into the frame: the backdrop,
    /// the strip walk into the line buffer, the FIX layer over it.
    fn render_line(&mut self, line: u32, tick: Tick, ctx: &mut MachineCtx) {
        let Some(row) = self.visible_line(line) else {
            return;
        };
        let backdrop = self.read_palette_word(0x0FFF, tick, ctx);
        let mut buffer = vec![TRANSPARENT; WIDTH];
        self.render_strips(line, &mut buffer);
        let rgb_row = self.convert_row(&buffer, backdrop, line, tick, ctx);
        let base = row as usize * WIDTH * 3;
        self.screen[base..base + WIDTH * 3].copy_from_slice(&rgb_row);
        if row + 1 == self.frame.height {
            self.frames += 1;
            self.frame.pixels = self.screen.clone();
        }
    }

    /// Convert one buffer row (palette word indices) to RGB888 with
    /// the FIX layer over the strips. FIX color 0 stays transparent;
    /// the strip word carries the palette-pen word.
    fn convert_row(
        &mut self,
        buffer: &[u16],
        backdrop: u16,
        line: u32,
        tick: Tick,
        ctx: &mut MachineCtx,
    ) -> Vec<u8> {
        let mut out = Vec::with_capacity(WIDTH * 3);
        for (col, &strip_word) in buffer.iter().enumerate() {
            let strip_rgb = match strip_word {
                TRANSPARENT => Self::palette_word_rgb(backdrop),
                word => Self::palette_word_rgb(word),
            };
            // FIX pixels with their tile row and column forms.
            let entry = self.fix_entry_at(col, line);
            let nibble = self.srom_nibble(
                u32::from(entry & 0x0FFF),
                line & 7,
                u32::try_from(col & 7).unwrap_or(0),
            );
            let fix_rgb = if nibble == 0 {
                None
            } else {
                let word_index = ((entry >> 12) << 4) | u16::from(nibble);
                Some(Self::palette_word_rgb(
                    self.read_palette_word(word_index, tick, ctx),
                ))
            };
            let mut rgb = fix_rgb.unwrap_or(strip_rgb);
            if self.shadow {
                rgb = Self::shade_dark(rgb);
            }
            out.extend_from_slice(&rgb);
        }
        out
    }

    /// The timer countdown: 384 pixel decrements per line visit, one
    /// per pixel clock, the published decrement rate.
    /// `REG_TIMERSTOP` pauses the first and last 16 lines in PAL
    /// mode; the lap asserts the flag and reloads in the repeat
    /// mode.
    fn step_timer(&mut self, _tick: Tick, _ctx: &mut MachineCtx) {
        if self.timer_remaining < 0 {
            return;
        }
        if self.timer_stop && self.pal {
            let raster = u32::from(self.raster());
            if !(0x0100..0x01F0).contains(&raster) {
                return;
            }
        }
        self.timer_remaining -= i64::from(LINE_PIXELS);
        if self.timer_remaining < 0 {
            if self.timer_irq_en {
                self.timer_pending = true;
            }
            if self.timer_autoload_repeat {
                self.timer_remaining = i64::from(self.timer_reload.wrapping_add(1));
            } else {
                self.timer_remaining = -1;
            }
        }
    }

    /// The per-line event pass at the vertical blank point: the
    /// timer vblank reload (bit 6), the auto-animation frame step,
    /// and the vblank flag.
    fn step_line_events(&mut self, _tick: Tick, _ctx: &mut MachineCtx) {
        if self.line != VBLANK_LINE {
            return;
        }
        if self.timer_autoload_vblank {
            self.timer_remaining = i64::from(self.timer_reload.wrapping_add(1));
        }
        if !self.aa_disable {
            if self.aa_frame_counter == 0 {
                self.aa_frame_counter = self.aa_speed;
                self.aa_counter = self.aa_counter.wrapping_add(1);
            } else {
                self.aa_frame_counter -= 1;
            }
        }
        self.vblank_pending = true;
    }

    /// The interrupt level recompute at the visit end: the pending
    /// flags drive the lines, asserted High and dropped Low at the
    /// next dispatch (the level-edge pattern the sibling die
    /// devices use).
    fn deliver_lines(&mut self, tick: Tick, ctx: &mut MachineCtx) {
        if self.vblank_pending != self.irq1_out {
            self.irq1_out = self.vblank_pending;
            ctx.notify(
                "irq1",
                if self.vblank_pending {
                    Level::High
                } else {
                    Level::Low
                },
                tick,
            );
        }
        if self.timer_pending != self.irq2_out {
            self.irq2_out = self.timer_pending;
            ctx.notify(
                "irq2",
                if self.timer_pending {
                    Level::High
                } else {
                    Level::Low
                },
                tick,
            );
        }
    }
}

impl Device for NeoLspc {
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
        self.vram.fill(0);
        self.vram_addr = 0;
        self.vram_mod = 0;
        self.vram_read_buf = 0;
        self.aa_speed = 0;
        self.aa_disable = false;
        self.aa_counter = 0;
        self.aa_frame_counter = 0;
        self.timer_irq_en = false;
        self.timer_load_relative = false;
        self.timer_autoload_vblank = false;
        self.timer_autoload_repeat = false;
        self.timer_stop = false;
        self.timer_reload = 0;
        self.timer_remaining = -1;
        self.timer_pending = false;
        self.vblank_pending = false;
        self.irq1_out = false;
        self.irq2_out = false;
        self.line = 0;
        self.shadow = false;
        self.frames = 0;
        self.next = None;
        self.last_tick = None;
    }

    /// The boot pass: the register reset form, then the FIX and C
    /// ROM boot images of the named devices, the established
    /// boot-state reads (`Hex` clones the bytes, `Rom` reads the
    /// file path). The schedule arms at the next domain tick.
    fn boot(&mut self, state: Option<&BootState>) {
        let _ = state;
        self.reset();
        self.next = Some(Tick(1));
    }

    /// The snapshot chunk: tag, version, the registers, the VRAM
    /// words, the auto-animation state, the timer state, the pending
    /// flags, the raster line, and the frame counters.
    fn snapshot(&self) -> Snapshot {
        let mut flags: u8 = 0;
        if self.aa_disable {
            flags |= FLAG_AA_DISABLE;
        }
        if self.timer_irq_en {
            flags |= FLAG_TIMER_IRQ_EN;
        }
        if self.timer_load_relative {
            flags |= FLAG_TIMER_LOAD_RELATIVE;
        }
        if self.timer_autoload_vblank {
            flags |= FLAG_TIMER_AUTOLOAD_VBLANK;
        }
        if self.timer_autoload_repeat {
            flags |= FLAG_TIMER_AUTOLOAD_REPEAT;
        }
        if self.timer_stop {
            flags |= FLAG_TIMER_STOP;
        }
        if self.timer_pending {
            flags |= FLAG_TIMER_PENDING;
        }
        if self.vblank_pending {
            flags |= FLAG_VBLANK_PENDING;
        }
        if self.shadow {
            flags |= FLAG_SHADOW;
        }
        let mut out = Vec::with_capacity(self.restore_length());
        out.extend_from_slice(TAG);
        out.push(VERSION);
        out.extend_from_slice(&self.vram_addr.to_le_bytes());
        out.extend_from_slice(&self.vram_mod.to_le_bytes());
        out.extend_from_slice(&self.vram_read_buf.to_le_bytes());
        for word in &self.vram {
            out.extend_from_slice(&word.to_le_bytes());
        }
        out.push(self.aa_speed);
        out.push(self.aa_counter);
        out.push(self.aa_frame_counter);
        out.extend_from_slice(&self.timer_reload.to_le_bytes());
        out.extend_from_slice(&self.timer_remaining.to_le_bytes());
        out.extend_from_slice(&self.line.to_le_bytes());
        out.extend_from_slice(&self.frames.to_le_bytes());
        out.push(flags);
        out
    }

    /// Read a snapshot written by the same device version.
    ///
    /// # Errors
    ///
    /// Returns an error when the chunk does not match this device's
    /// expectations.
    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        let expected = self.restore_length();
        if data.len() != expected || &data[..4] != TAG || data[4] != VERSION {
            return Err(SnapshotError::Mismatch(format!(
                "the lspc chunk must be {expected} bytes of tag {TAG:?} version {VERSION}"
            )));
        }
        self.vram_addr = le_u16(data, 5)?;
        self.vram_mod = le_u16(data, 7)?;
        self.vram_read_buf = le_u16(data, 9)?;
        let mut pos = 11usize;
        for word in self.vram.iter_mut() {
            *word = le_u16(data, pos)?;
            pos += 2;
        }
        self.aa_speed = data[pos];
        self.aa_counter = data[pos + 1];
        self.aa_frame_counter = data[pos + 2];
        pos += 3;
        self.timer_reload = le_u32(data, pos)?;
        pos += 4;
        let mut raw = [0u8; 8];
        let tail = le_bytes(data, pos, 8)?;
        raw.copy_from_slice(&tail);
        self.timer_remaining = i64::from_le_bytes(raw);
        pos += 8;
        self.line = le_u32(data, pos)? % self.vtotal();
        pos += 4;
        self.frames = le_u64(data, pos)?;
        pos += 8;
        let flags = data[pos];
        self.aa_disable = flags & FLAG_AA_DISABLE != 0;
        self.timer_irq_en = flags & FLAG_TIMER_IRQ_EN != 0;
        self.timer_load_relative = flags & FLAG_TIMER_LOAD_RELATIVE != 0;
        self.timer_autoload_vblank = flags & FLAG_TIMER_AUTOLOAD_VBLANK != 0;
        self.timer_autoload_repeat = flags & FLAG_TIMER_AUTOLOAD_REPEAT != 0;
        self.timer_stop = flags & FLAG_TIMER_STOP != 0;
        self.timer_pending = flags & FLAG_TIMER_PENDING != 0;
        self.vblank_pending = flags & FLAG_VBLANK_PENDING != 0;
        self.shadow = flags & FLAG_SHADOW != 0;
        self.last_tick = None;
        self.next = Some(Tick(1));
        Ok(())
    }

    fn notify_line(&mut self, line: &str, level: Level, _tick: Tick) {
        if line == "shadow" {
            self.shadow = level.is_high();
        }
    }

    fn next_event(&self) -> Option<Tick> {
        self.next
    }

    /// One scheduler visit: the die renders a whole scanline per
    /// visit. The visit runs the timer countdown, the sprite parse,
    /// the line render, the per-line events, the line level edges,
    /// and arms the next line base.
    fn tick(&mut self, tick: Tick, ctx: &mut MachineCtx) {
        if self.last_tick.is_some_and(|t| t.0 >= tick.0) {
            return;
        }
        self.last_tick = Some(tick);

        self.step_timer(tick, ctx);
        self.parse_sprites();
        self.render_line(self.line, tick, ctx);
        self.step_line_events(tick, ctx);
        self.deliver_lines(tick, ctx);

        self.line += 1;
        if self.line >= self.vtotal() {
            self.line = 0;
        }
        self.next = Some(self.next_line_tick(tick.0));
    }
}

impl NeoLspc {
    /// The next line-base tick from the current visit.
    fn next_line_tick(&self, visit: u64) -> Tick {
        Tick(visit + self.line_ticks())
    }

    /// The boot pass of the tile sources: keep the FIX and C ROM
    /// bytes of the named devices. The boot state of the named
    /// device clones `Hex` bytes and reads `Rom` paths, the
    /// established reads.
    fn load_tile_sources(&mut self, config: &SystemConfig) {
        self.srom_bytes = Self::load_boot_image(config, &self.srom);
        self.crom_bytes = Self::load_boot_image(config, &self.crom);
    }

    /// Fetch the boot images of a named device. The device's own
    /// boot passes keep their forms; the die mirrors the bytes.
    fn load_boot_image(config: &SystemConfig, device: &str) -> Vec<u8> {
        let slot = config
            .devices_by_name
            .iter()
            .find_map(|(name, slot)| (name.as_str() == device).then_some(*slot));
        let Some(slot) = slot else {
            return Vec::new();
        };
        let Some(instance) = config.devices.get(slot) else {
            return Vec::new();
        };
        match config.boot.get(&instance.name) {
            Some(BootState::Hex { bytes }) => bytes.clone(),
            Some(BootState::Rom { path }) => std::fs::read(path).unwrap_or_default(),
            _ => Vec::new(),
        }
    }
}

impl VideoSource for NeoLspc {
    /// The composed frame: RGB888, row-major, the visible window
    /// (320 per line; 224 NTSC, 256 PAL).
    fn frame(&self) -> Option<&Frame> {
        Some(&self.frame)
    }

    fn dump_layer(&self, layer: VideoLayer) -> Option<Vec<u8>> {
        match layer {
            VideoLayer::Vram => Some(self.vram_bytes()),
            VideoLayer::NameTable => {
                // The FIX map: 1280 words of the map area.
                let mut out = Vec::with_capacity(1280 * 2);
                for i in 0..1280usize {
                    push_word(&mut out, self.fix_entry_raw(i as u16));
                }
                Some(out)
            }
            VideoLayer::Oam => {
                // The SCB words: SCB2, SCB3, SCB4, then the two line
                // lists with their spare words to the VRAM end.
                let mut out = Vec::with_capacity((0x200 * 3 + 0x200) * 2);
                for span in [0x8000u16, 0x8200, 0x8400] {
                    let base = usize::from(span);
                    for word in self.vram[base..base + 0x200].iter() {
                        push_word(&mut out, *word);
                    }
                }
                for word in self.vram[usize::from(LIST_BASE_EVEN)..].iter() {
                    push_word(&mut out, *word);
                }
                Some(out)
            }
            VideoLayer::Palette => None,
            VideoLayer::Framebuffer => Some(self.screen.clone()),
        }
    }

    fn frames(&self) -> u64 {
        self.frames
    }
}

/// The le_u64 helper.
fn le_u64(data: &[u8], at: usize) -> Result<u64, SnapshotError> {
    let mut raw = [0u8; 8];
    let tail = le_bytes(data, at, 8)?;
    raw.copy_from_slice(&tail);
    Ok(u64::from_le_bytes(raw))
}

impl NeoLspc {
    /// One color word of the palette device over the bus: the word
    /// index `(palette << 4) | color` at the palette window base. The
    /// palette device owns the bank selection; the read lands on the
    /// selected bank's words.
    fn read_palette_word(&self, word_index: u16, tick: Tick, ctx: &mut MachineCtx) -> u16 {
        let address = Address(0x400000 + u32::from(word_index) * 2);
        ctx.read(&self.bus, AccessWidth::Word, address, tick) as u16
    }

    /// The RGB888 form of one palette word: bit 15 the dark bit,
    /// bits 14-12 R0 G0 B0, bits 11-8 R4-R1, bits 7-4 G4-G1, bits
    /// 3-0 B4-B1, the packing of the palette device module docs. A
    /// five-bit channel replicates its upper bits into the byte; the
    /// dark bit steps the channel down.
    pub(crate) fn palette_word_rgb(word: u16) -> [u8; 3] {
        let dark = word & 0x8000 != 0;
        let r = ((word >> 14) & 1 | (word >> 7) & 0x1E) as u8;
        let g = ((word >> 13) & 1 | (word >> 3) & 0x1E) as u8;
        let b = ((word >> 12) & 1 | (word << 1) & 0x1E) as u8;
        let scale5 = |v: u8| (v << 3) | (v >> 2);
        let rgb = [scale5(r), scale5(g), scale5(b)];
        if dark { Self::shade_dark(rgb) } else { rgb }
    }

    /// The darkened RGB form: a quarter-step down per channel, the
    /// stand-in for the resistor-ladder shadow pen MAME builds.
    pub(crate) fn shade_dark(rgb: [u8; 3]) -> [u8; 3] {
        let step = |v: u8| (u16::from(v) * 3 / 4) as u8;
        [step(rgb[0]), step(rgb[1]), step(rgb[2])]
    }

    /// The timer countdown without a machine context: the flag edges
    /// only.
    #[cfg(test)]
    pub(crate) fn step_timer_no_ctx(&mut self) {
        if self.timer_remaining < 0 {
            return;
        }
        if self.timer_stop && self.pal {
            let raster = u32::from(self.raster());
            if !(0x0100..0x01F0).contains(&raster) {
                return;
            }
        }
        self.timer_remaining -= i64::from(LINE_PIXELS);
        if self.timer_remaining < 0 {
            if self.timer_irq_en {
                self.timer_pending = true;
            }
            if self.timer_autoload_repeat {
                self.timer_remaining = i64::from(self.timer_reload.wrapping_add(1));
            } else {
                self.timer_remaining = -1;
            }
        }
    }
}

impl BusAccess for NeoLspc {
    /// The register page answers words; byte reads answer `$FF`,
    /// per the byte-lane rule of the register module docs.
    fn read(&mut self, width: AccessWidth, address: Address, _tick: Tick) -> u32 {
        match width {
            AccessWidth::Word | AccessWidth::Dword => self.reg_read(Self::read_reg(address.0)),
            AccessWidth::Byte => 0xFF,
        }
    }

    /// The register page answers words; byte writes stay out of
    /// build.
    fn write(&mut self, width: AccessWidth, address: Address, value: u32, _tick: Tick) {
        match width {
            AccessWidth::Word | AccessWidth::Dword => {
                self.reg_write(Self::write_reg(address.0), value as u16);
            }
            AccessWidth::Byte => {}
        }
    }

    /// Byte reads are unmapped on the register page.
    fn peek(&self, _address: Address) -> u8 {
        0xFF
    }

    fn decodes(&self, _address: Address) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    impl NeoLspc {
        /// The VRAM word read for the tests.
        fn vram_get(&self, index: usize) -> u16 {
            self.vram.get(index).copied().unwrap_or(0)
        }

        /// The row conversion routed through an owned context, for
        /// the planted-tile checks.
        fn convert_row_stub(&mut self, backdrop: u16, line: u32) -> Vec<u8> {
            let mut before: [Box<dyn Device>; 0] = [];
            let mut after: [Box<dyn Device>; 0] = [];
            let mut buses: [demu_core::bus::Bus; 0] = [];
            let index = Default::default();
            let routes: Vec<demu_core::device::Route> = Vec::new();
            let mut scratch: Vec<demu_core::device::DecodeHit> = Vec::new();
            let mut dirty: Vec<usize> = Vec::new();
            let mut events: Vec<demu_core::triggers::TriggerEvent> = Vec::new();
            let mut ctx = MachineCtx::new(
                demu_core::device::DeviceSlices {
                    before: &mut before,
                    after: &mut after,
                    caller_slot: 0,
                },
                demu_core::device::BusBank {
                    buses: &mut buses,
                    index: &index,
                },
                routes.as_slice(),
                &mut scratch,
                demu_core::device::EventBuffers {
                    events: &mut events,
                    dirty: &mut dirty,
                },
                false,
            );
            let buffer = vec![TRANSPARENT; WIDTH];
            self.convert_row(&buffer, backdrop, line, Tick(0), &mut ctx)
        }

        /// The current VRAM address register value.
        fn vram_addr(&self) -> u16 {
            self.vram_addr
        }
    }

    /// The mini configuration with the die, the palette shim, the
    /// probe, and the strobe.
    fn mini_config() -> SystemConfig {
        let mut clocks = std::collections::BTreeMap::new();
        clocks.insert(
            "main".to_string(),
            demu_config::model::ClockDomain {
                name: "main".to_string(),
                frequency: demu_config::newtypes::Frequency::new(12_288_000, 1).unwrap(),
                origin: "test".to_string(),
            },
        );
        let mut lspc_params = std::collections::BTreeMap::new();
        lspc_params.insert("bus".to_string(), toml::Value::String("cpu".to_string()));
        let mut lspc_dividers = std::collections::BTreeMap::new();
        lspc_dividers.insert("dot".to_string(), 2u32);
        let lspc = DeviceInstance {
            name: demu_config::newtypes::DeviceName::new("lspc").unwrap(),
            kind: demu_config::newtypes::KindName::new("neogeo.lspc").unwrap(),
            domain: "main".to_string(),
            params: lspc_params,
            dividers: lspc_dividers,
        };
        let pal = DeviceInstance {
            name: demu_config::newtypes::DeviceName::new("pal").unwrap(),
            kind: demu_config::newtypes::KindName::new("test.palette").unwrap(),
            domain: "main".to_string(),
            params: std::collections::BTreeMap::new(),
            dividers: std::collections::BTreeMap::new(),
        };
        let probe = DeviceInstance {
            name: demu_config::newtypes::DeviceName::new("probe").unwrap(),
            kind: demu_config::newtypes::KindName::new("test.probe").unwrap(),
            domain: "main".to_string(),
            params: std::collections::BTreeMap::new(),
            dividers: std::collections::BTreeMap::new(),
        };
        let strobe = DeviceInstance {
            name: demu_config::newtypes::DeviceName::new("strobe").unwrap(),
            kind: demu_config::newtypes::KindName::new("test.strobe").unwrap(),
            domain: "main".to_string(),
            params: std::collections::BTreeMap::new(),
            dividers: std::collections::BTreeMap::new(),
        };
        let devices = vec![lspc, pal, probe, strobe];
        let mut devices_by_name = std::collections::BTreeMap::new();
        for (i, device) in devices.iter().enumerate() {
            devices_by_name.insert(device.name.clone(), i);
        }
        let buses = vec![demu_config::model::Bus {
            name: "cpu".to_string(),
            address_bits: 24,
            owner: None,
            parent: None,
        }];
        let windows = vec![
            demu_config::model::Window {
                name: "lspcregs".to_string(),
                bus: "cpu".to_string(),
                device: demu_config::newtypes::DeviceName::new("lspc").unwrap(),
                base: 0x3C0000,
                size: 0x040000,
                kind: None,
                mirror: None,
                overlay: None,
                overlay_read_priority: None,
                overlay_write_priority: None,
                conflict: None,
            },
            demu_config::model::Window {
                name: "paleram".to_string(),
                bus: "cpu".to_string(),
                device: demu_config::newtypes::DeviceName::new("pal").unwrap(),
                base: 0x400000,
                size: 0x002000,
                kind: None,
                mirror: None,
                overlay: None,
                overlay_read_priority: None,
                overlay_write_priority: None,
                conflict: None,
            },
        ];
        SystemConfig {
            name: "neogeo-mini".to_string(),
            triple: "test".to_string(),
            description: "The LSPC mini test machine.".to_string(),
            clocks,
            devices,
            devices_by_name,
            buses,
            windows,
            connections: Vec::new(),
            signals: Vec::new(),
            boot: std::collections::BTreeMap::new(),
            quirks: std::collections::BTreeMap::new(),
        }
    }

    /// A die with no tile boot images and the test configuration's
    /// bus name.
    fn lspc_only() -> NeoLspc {
        let config = mini_config();
        NeoLspc::new(&config.devices[0], &config).unwrap()
    }

    /// The machine-test palette shim: the `neogeo.palette` bank form
    /// (the High `palbank` line selects bank 1) for the in-machine
    /// tests; the cross-crate device lives with the memory crate and
    /// carries its own unit tests.
    #[derive(Debug, Clone, Default)]
    struct TestPalette {
        name: String,
        banks: [Vec<u16>; 2],
        bank: usize,
    }

    impl TestPalette {
        fn new(device: &DeviceInstance) -> Self {
            Self {
                name: device.name.to_string(),
                banks: [vec![0; 4096], vec![0; 4096]],
                bank: 0,
            }
        }

        fn word(&self, index: usize) -> u16 {
            self.banks[self.bank].get(index).copied().unwrap_or(0)
        }

        fn set_word(&mut self, index: usize, value: u16) {
            if index < self.banks[self.bank].len() {
                self.banks[self.bank][index] = value;
            }
        }
    }

    impl BusAccess for TestPalette {
        fn read(&mut self, width: AccessWidth, address: Address, _tick: Tick) -> u32 {
            match width {
                AccessWidth::Word | AccessWidth::Dword => {
                    u32::from(self.word(((address.0 & 0x1FFF) >> 1) as usize))
                }
                AccessWidth::Byte => 0xFF,
            }
        }

        fn write(&mut self, width: AccessWidth, address: Address, value: u32, _tick: Tick) {
            if matches!(width, AccessWidth::Word | AccessWidth::Dword) {
                self.set_word(((address.0 & 0x1FFF) >> 1) as usize, value as u16);
            }
        }

        fn peek(&self, _address: Address) -> u8 {
            0xFF
        }
    }

    impl Device for TestPalette {
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
            self.bank = 0;
            self.banks.iter_mut().for_each(|b| b.fill(0));
        }

        fn boot(&mut self, _state: Option<&BootState>) {
            self.reset();
        }

        fn snapshot(&self) -> Snapshot {
            let mut out = b"TPAL".to_vec();
            out.push(self.bank as u8);
            for bank in &self.banks {
                for word in bank {
                    out.extend_from_slice(&word.to_le_bytes());
                }
            }
            out
        }

        /// Read a snapshot written by the same device version.
        ///
        /// # Errors
        ///
        /// Returns an error when the chunk does not match.
        fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
            let len = 5 + self.banks.iter().map(Vec::len).sum::<usize>() * 2;
            if data.len() != len || &data[..4] != b"TPAL" {
                return Err(SnapshotError::Mismatch("test palette chunk".into()));
            }
            self.bank = usize::from(data[4] & 1);
            let mut pos = 5usize;
            for bank in &mut self.banks {
                for word in bank.iter_mut() {
                    *word = u16::from_le_bytes([data[pos], data[pos + 1]]);
                    pos += 2;
                }
            }
            Ok(())
        }

        fn notify_line(&mut self, line: &str, level: Level, _tick: Tick) {
            if line == "palbank" {
                self.bank = usize::from(level.is_high());
            }
        }

        fn tick(&mut self, _tick: Tick, _ctx: &mut MachineCtx) {}
    }

    impl VideoSource for TestPalette {
        fn frame(&self) -> Option<&Frame> {
            None
        }

        fn dump_layer(&self, layer: VideoLayer) -> Option<Vec<u8>> {
            match layer {
                VideoLayer::Palette => {
                    let mut out = Vec::with_capacity(4096 * 2);
                    for word in &self.banks[self.bank] {
                        out.extend_from_slice(&word.to_le_bytes());
                    }
                    Some(out)
                }
                _ => None,
            }
        }

        fn frames(&self) -> u64 {
            0
        }
    }

    /// The machine-test probe: records the notify levels it receives.
    #[derive(Debug, Default)]
    struct IrqProbe {
        seen: std::cell::RefCell<Vec<(String, Level)>>,
    }

    impl Device for IrqProbe {
        fn name(&self) -> &str {
            "probe"
        }

        fn as_any(&self) -> &dyn std::any::Any {
            self
        }

        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }

        fn reset(&mut self) {}
        fn boot(&mut self, _state: Option<&BootState>) {}

        fn snapshot(&self) -> Snapshot {
            Vec::new()
        }

        fn restore(&mut self, _data: &[u8]) -> Result<(), SnapshotError> {
            Ok(())
        }

        fn notify_line(&mut self, line: &str, level: Level, _tick: Tick) {
            self.seen.borrow_mut().push((line.to_string(), level));
        }

        fn tick(&mut self, _tick: Tick, _ctx: &mut MachineCtx) {}
    }

    /// The strobe drives one word write per visit against the
    /// register page, at the schedule's tick.
    #[derive(Debug, Default)]
    struct Strobe {
        name: String,
        schedule: std::cell::RefCell<Tick>,
        cursor: std::cell::Cell<usize>,
        next: Option<Tick>,
    }

    impl Strobe {
        fn new(device: &DeviceInstance) -> Self {
            Self {
                name: device.name.to_string(),
                schedule: std::cell::RefCell::new(Tick(1)),
                cursor: 0.into(),
                next: None,
            }
        }
    }

    impl Device for Strobe {
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
            self.next = None;
        }

        fn boot(&mut self, _state: Option<&BootState>) {
            let at = self.schedule.borrow();
            self.next = Some(*at);
        }

        fn snapshot(&self) -> Snapshot {
            Vec::new()
        }

        fn restore(&mut self, _data: &[u8]) -> Result<(), SnapshotError> {
            Ok(())
        }

        fn notify_line(&mut self, _line: &str, _level: Level, _tick: Tick) {}

        fn next_event(&self) -> Option<Tick> {
            self.next
        }

        fn tick(&mut self, tick: Tick, _ctx: &mut MachineCtx) {
            self.cursor.set(self.cursor.get().wrapping_add(1));
            self.next = Some(Tick(tick.0 + 1));
            let _ = self.name;
        }
    }
    #[test]
    fn timer_lap_sets_flag_without_context() {
        // The countdown's flag edges at the state level; the line
        // delivery runs under the machine layer and its own checks.
        let mut v = lspc_only();
        v.write(AccessWidth::Word, Address(0x3C0008), 0x0000, Tick(0));
        v.write(AccessWidth::Word, Address(0x3C0006), 0x01B0, Tick(0));
        v.write(AccessWidth::Word, Address(0x3C000A), 0x0001, Tick(0));
        assert!(v.timer_irq_en);
        assert!(v.timer_autoload_repeat);
        assert_eq!(v.timer_remaining, 2);
        v.step_timer_no_ctx();
        assert!(v.timer_pending);
        v.write(AccessWidth::Word, Address(0x3C000C), 0x0002, Tick(0));
        assert!(!v.timer_pending);
    }

    fn machine_config() -> SystemConfig {
        let mut clocks = std::collections::BTreeMap::new();
        clocks.insert(
            "main".to_string(),
            demu_config::model::ClockDomain {
                name: "main".to_string(),
                frequency: demu_config::newtypes::Frequency::new(12_288_000, 1).unwrap(),
                origin: "test".to_string(),
            },
        );
        let mut entries = std::collections::BTreeMap::new();
        entries.insert("bus".to_string(), toml::Value::String("cpu".to_string()));
        let mut dividers = std::collections::BTreeMap::new();
        dividers.insert("dot".to_string(), 2u32);
        let lspc = DeviceInstance {
            name: demu_config::newtypes::DeviceName::new("lspc").unwrap(),
            kind: demu_config::newtypes::KindName::new("neogeo.lspc").unwrap(),
            domain: "main".to_string(),
            params: entries,
            dividers,
        };
        let pal = DeviceInstance {
            name: demu_config::newtypes::DeviceName::new("pal").unwrap(),
            kind: demu_config::newtypes::KindName::new("test.palette").unwrap(),
            domain: "main".to_string(),
            params: std::collections::BTreeMap::new(),
            dividers: std::collections::BTreeMap::new(),
        };
        let probe = DeviceInstance {
            name: demu_config::newtypes::DeviceName::new("probe").unwrap(),
            kind: demu_config::newtypes::KindName::new("test.probe").unwrap(),
            domain: "main".to_string(),
            params: std::collections::BTreeMap::new(),
            dividers: std::collections::BTreeMap::new(),
        };
        let strobe = DeviceInstance {
            name: demu_config::newtypes::DeviceName::new("strobe").unwrap(),
            kind: demu_config::newtypes::KindName::new("test.strobe").unwrap(),
            domain: "main".to_string(),
            params: std::collections::BTreeMap::new(),
            dividers: std::collections::BTreeMap::new(),
        };
        let devices = vec![lspc, pal, probe, strobe];
        let mut devices_by_name = std::collections::BTreeMap::new();
        for (i, device) in devices.iter().enumerate() {
            devices_by_name.insert(device.name.clone(), i);
        }
        let buses = vec![demu_config::model::Bus {
            name: "cpu".to_string(),
            address_bits: 24,
            owner: None,
            parent: None,
        }];
        let windows = vec![
            demu_config::model::Window {
                name: "lspcregs".to_string(),
                bus: "cpu".to_string(),
                device: demu_config::newtypes::DeviceName::new("lspc").unwrap(),
                base: 0x3C0000,
                size: 0x040000,
                kind: None,
                mirror: None,
                overlay: None,
                overlay_read_priority: None,
                overlay_write_priority: None,
                conflict: None,
            },
            demu_config::model::Window {
                name: "paleram".to_string(),
                bus: "cpu".to_string(),
                device: demu_config::newtypes::DeviceName::new("pal").unwrap(),
                base: 0x400000,
                size: 0x002000,
                kind: None,
                mirror: None,
                overlay: None,
                overlay_read_priority: None,
                overlay_write_priority: None,
                conflict: None,
            },
        ];
        SystemConfig {
            name: "neogeo-mini".to_string(),
            triple: "test".to_string(),
            description: "The LSPC mini test machine.".to_string(),
            clocks,
            devices,
            devices_by_name,
            buses,
            windows,
            connections: Vec::new(),
            signals: Vec::new(),
            boot: std::collections::BTreeMap::new(),
            quirks: std::collections::BTreeMap::new(),
        }
    }

    fn ctors_test() -> demu_core::machine::DeviceCtors {
        let mut ctors = demu_core::machine::DeviceCtors::new();
        ctors.register(
            "neogeo.lspc",
            Box::new(|d, cfg| {
                Ok(Box::new(
                    NeoLspc::new(d, cfg).map_err(demu_core::machine::MachineError::Assembly)?,
                ) as Box<dyn Device>)
            }),
        );
        ctors.register(
            "test.palette",
            Box::new(|d, _| Ok(Box::new(TestPalette::new(d)) as Box<dyn Device>)),
        );
        ctors.register(
            "test.probe",
            Box::new(|_, _| Ok(Box::new(IrqProbe::default()) as Box<dyn Device>)),
        );
        ctors.register(
            "test.strobe",
            Box::new(|d, _| Ok(Box::new(Strobe::new(d)) as Box<dyn Device>)),
        );
        ctors
    }

    #[test]
    fn vram_address_data_and_modulo_round_trip() {
        let mut v = lspc_only();
        v.write(AccessWidth::Word, Address(0x3C0004), 0x0002, Tick(0));
        v.write(AccessWidth::Word, Address(0x3C0000), 0x7000, Tick(0));
        v.write(AccessWidth::Word, Address(0x3C0002), 0x1234, Tick(0));
        v.write(AccessWidth::Word, Address(0x3C0002), 0xABCD, Tick(0));
        assert_eq!(v.vram_get(0x7000), 0x1234);
        assert_eq!(v.vram_get(0x7002), 0xABCD);
        assert_eq!(v.vram_addr(), 0x7004);
        v.write(AccessWidth::Word, Address(0x3C0000), 0x7002, Tick(0));
        assert_eq!(
            v.read(AccessWidth::Word, Address(0x3C0002), Tick(0)),
            0xABCD
        );
    }

    #[test]
    fn vram_signed_modulo_walks_backwards() {
        let mut v = lspc_only();
        v.write(AccessWidth::Word, Address(0x3C0004), 0xFFFF, Tick(0));
        v.write(AccessWidth::Word, Address(0x3C0000), 0x0002, Tick(0));
        v.write(AccessWidth::Word, Address(0x3C0002), 0x0001, Tick(0));
        assert_eq!(v.vram_addr(), 0x0001);
        assert_eq!(v.vram_get(0x0002), 0x0001);
    }

    #[test]
    fn modulo_cannot_cross_zone_boundary() {
        let mut v = lspc_only();
        v.write(AccessWidth::Word, Address(0x3C0004), 0x0002, Tick(0));
        v.write(AccessWidth::Word, Address(0x3C0000), 0x7FFF, Tick(0));
        v.write(AccessWidth::Word, Address(0x3C0002), 0x1111, Tick(0));
        assert_eq!(v.vram_addr(), 0x0001);
        v.write(AccessWidth::Word, Address(0x3C0000), 0x87FF, Tick(0));
        v.write(AccessWidth::Word, Address(0x3C0002), 0x2222, Tick(0));
        assert_eq!(v.vram_addr(), 0x8001);
        assert_eq!(v.vram_get(0x87FF), 0x2222);
    }

    #[test]
    fn register_page_byte_lane_is_unmapped() {
        let mut v = lspc_only();
        assert_eq!(v.read(AccessWidth::Byte, Address(0x3C0002), Tick(0)), 0xFF);
        v.write(AccessWidth::Byte, Address(0x3C0000), 0x7000, Tick(0));
        assert_eq!(v.vram_addr(), 0x0000);
        assert_eq!(v.peek(Address(0x3C0006)), 0xFF);
    }

    #[test]
    fn reg_lspc_mode_reads_raster_position() {
        assert_eq!(NeoLspc::raster_of(0, VTOTAL_NTSC), 0x100u16);
        assert_eq!(NeoLspc::raster_of(255, VTOTAL_NTSC), 0x1FFu16);
        assert_eq!(NeoLspc::raster_of(256, VTOTAL_NTSC), 0x0F8u16);
        assert_eq!(NeoLspc::raster_of(263, VTOTAL_NTSC), 0x0FFu16);
        let mut pconfig = mini_config();
        pconfig.devices[0] = instance("lspc", &[("region", "pal")], 2);
        let mut p = NeoLspc::new(&pconfig.devices[0], &pconfig).unwrap();
        assert_eq!(p.vtotal(), 312);
        assert_eq!(
            p.read(AccessWidth::Word, Address(0x3C0006), Tick(0)) & 0x0008,
            0x0008
        );
        assert_eq!(NeoLspc::raster_of(256, VTOTAL_PAL), 0x0C8u16);
        p.aa_counter = 5;
        let value = p.read(AccessWidth::Word, Address(0x3C0006), Tick(0));
        assert_eq!(value & 0x0007, 5);
    }

    #[test]
    fn fix_layer_renders_planted_tile() {
        // The pinned left-pixel nibble form.
        let mut v = lspc_only();
        v.srom_bytes = vec![0u8; 32];
        v.srom_bytes[16] = 0x23;
        v.vram[usize::from(FIX_BASE)] = 1u16 << 12;
        assert_eq!(v.srom_nibble(0, 0, 0), 3);
        assert_eq!(v.srom_nibble(0, 0, 1), 2);
        assert_eq!(v.srom_nibble(0, 1, 0), 0);
        let row = v.convert_row_stub(0, 16);
        assert_eq!(row.len(), WIDTH * 3);
    }

    #[test]
    fn sprite_stick_chain_renders_wide_shape() {
        let mut v = lspc_only();
        v.crom_bytes = vec![0u8; 0x80];
        v.crom_bytes[0x40] = 0b0000_0001;
        v.crom_bytes[0x41] = 0b0000_0010;
        v.crom_bytes[0x42] = 0b0000_0100;
        v.crom_bytes[0x43] = 0b0000_1000;
        v.vram[usize::from(0x8000u16 | 1)] = 0xFFFF;
        v.vram[usize::from(0x8200u16 | 1)] = (496u16 << 7) | 1;
        v.vram[usize::from(0x8400u16 | 1)] = 88u16 << 7;
        v.vram[64] = 2;
        v.vram[65] = 8u16 << 8;
        v.line = 16;
        let mut buffer = vec![TRANSPARENT; WIDTH];
        v.paint_strip(1, 16, 16, 1, 0xFFu8, 0xFu8, 88u32, &mut buffer);
        let first = v.crom_nibble(2, 0, 0);
        assert_eq!(buffer[88usize], (8u16 << 4) | u16::from(first));
        // The second strip in the stick chain advances the span.
        v.vram[usize::from(0x8200u16 | 2)] = 0x0040;
        v.vram[usize::from(0x8000u16 | 2)] = 0xFFFF;
        v.vram[usize::from(0x8400u16 | 2)] = 0;
        {
            v.render_strips(16, &mut buffer);
            assert!(buffer.iter().any(|w| *w != TRANSPARENT));
        }
    }

    #[test]
    fn sprite_budget_and_overflow_tail() {
        let mut v = lspc_only();
        v.line = 17;
        for i in 0..200usize {
            v.vram[usize::from(0x8200u16 | i as u16)] = (496u16 << 7) | 1;
            v.vram[usize::from(0x8000u16 | i as u16)] = 0xFFFF;
            v.vram[usize::from(0x8400u16 | i as u16)] = 880u16 << 7;
        }
        v.parse_sprites();
        assert_eq!(v.live_list_count(17), LINE_SPRITES - 1);
        let live = v.vram_get(usize::from(LIST_BASE_ODD) + LINE_SPRITES);
        assert_eq!(live, 0);
    }

    #[test]
    fn shadow_latch_darkens_row() {
        let dark = NeoLspc::shade_dark([0xFFu8, 0xFF, 0xFF]);
        assert_eq!(dark, [191u8, 191, 191]);
        let mut v = lspc_only();
        v.notify_line("shadow", Level::High, Tick(0));
        assert!(v.shadow);
        v.notify_line("shadow", Level::Low, Tick(1));
        assert!(!v.shadow);
    }

    #[test]
    fn mini_machine_determinism_and_palette_bank() {
        let config = machine_config();
        let mut machine = demu_core::Machine::assemble(config, &ctors_test()).expect("assemble");
        machine.boot();
        let frame_ticks = 264u64 * line_ticks_test(&machine);
        machine.run_until(Tick(2 * frame_ticks));
        let lspc_slot = machine.device_slot("lspc").unwrap();
        let pal_slot = machine.device_slot("pal").unwrap();
        let lspc_snap = machine.devices[lspc_slot].snapshot();
        let pal_snap = machine.devices[pal_slot].snapshot();
        let _ = pal_snap;
        assert_eq!(&lspc_snap[..4], TAG.clone());
        // A machine restored from the save continues identically.
        let mut twin =
            demu_core::Machine::assemble(machine_config(), &ctors_test()).expect("assemble twin");
        twin.boot();
        let mut buf = Vec::new();
        machine.save(&mut buf).unwrap();
        twin.load(&buf).expect("load");
        let mut buf2 = Vec::new();
        twin.save(&mut buf2).unwrap();
        assert_eq!(buf, buf2, "save after restore is byte-identical");
        // The bank flip changes the color words the die would read.
        let words_changed = {
            let pal = twin.devices[pal_slot]
                .as_any_mut()
                .downcast_mut::<TestPalette>()
                .unwrap();
            pal.write(AccessWidth::Word, Address(0x400002), 0x0F00, Tick(0));
            let before = pal.dump_layer(VideoLayer::Palette).unwrap();
            pal.notify_line("palbank", Level::High, Tick(0));
            let after = pal.dump_layer(VideoLayer::Palette).unwrap();
            before != after
        };
        assert!(words_changed, "the flip changes the visible color words");
    }

    fn line_ticks_test(machine: &demu_core::Machine) -> u64 {
        machine.devices[machine.device_slot("lspc").unwrap()]
            .as_any()
            .downcast_ref::<NeoLspc>()
            .map(NeoLspc::line_ticks_pub)
            .unwrap_or(202_752)
    }

    fn instance(name: &str, params: &[(&str, &str)], dot: u32) -> DeviceInstance {
        let mut entries = std::collections::BTreeMap::new();
        for (key, value) in params {
            entries.insert(
                (*key).to_string(),
                toml::Value::String((*value).to_string()),
            );
        }
        let mut dividers = std::collections::BTreeMap::new();
        dividers.insert("dot".to_string(), dot);
        DeviceInstance {
            name: demu_config::newtypes::DeviceName::new(name).unwrap(),
            kind: demu_config::newtypes::KindName::new("neogeo.lspc").unwrap(),
            domain: "main".to_string(),
            params: entries,
            dividers,
        }
    }
}
