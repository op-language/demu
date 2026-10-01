//! The SNES S-PPU1 and S-PPU2 pair as one device.
//!
//! One die carries both chips: PPU1 owns the background pipelines of
//! modes 0 to 6, the object engine, and the VRAM/OAM ports; PPU2 owns
//! CGRAM, the color math, and the output counters. The model composes
//! one scanline per device visit; the visit stride is 341 PPU dots
//! times the configured `dot` divider, so a config with the dot divider
//! 4 visits every 1364 master ticks of the 236250000/11 Hz crystal and
//! reproduces the 59.9 Hz NTSC and the 50.3 Hz PAL frame rates.
//!
//! The frame is 512x448 progressive for both regions. Every visible
//! scanline paints two rows, so a 224-line picture fills the 448 rows;
//! the `pal` device parameter changes only the scanline count and the
//! PAL status flag.
//!
//! Register decode: the B-bus register file `$2100-$213F` decodes by
//! `raw & 0x1FF`. Offsets `$100-$135` accept writes and carry the data
//! ports; `$136-$13F` is read-only; offsets outside return `$FF`
//! because the other B-bus registers belong to other devices.
//!
//! Deliberate simplifications of this model, recorded per subsystem:
//!
//! - Timing is line-oriented: one composed line per visit over 341
//!   nominal dots. Per-dot raster effects, mode 5/6 interlacing, the
//!   forced-blank glitch windows, and the object engine's cycle budget
//!   are not modeled beyond the 34-sprite-per-line cap.
//! - The visible window is lines 0 through the vblank line, mapped to
//!   the frame rows with no overscan or blanking rows. The PAL variant
//!   paints the same 448 rows; picture lines past row 447 paint nothing.
//! - VRAM, CGRAM, and OAM accesses store their data immediately, during
//!   active display included, where the real chip ignores them.
//! - VRAM reads alternate the low and high bytes of one prefetched
//!   word; completing a pair steps the address by the `$2115`
//!   increment and refills the buffer from the new address. The VMADD
//!   store prefetches `VRAM[VMADD]` and rests the alternation at the
//!   low half, so a read pair returns the word at the set address.
//! - The `$210D-$2114` scroll file and the `$211B-$2120` mode 7 file
//!   keep the hardware second-write reroute: the second byte of a
//!   two-byte write lands in the register the pair started with, even
//!   when the bus address advanced past the first byte. `$211C`
//!   composes its fixed-point value on every write and records the
//!   signed 8-bit factor that `$2134-$36` multiplies.
//! - Mode 7 occupies the first VRAM half with its 128x128 map and the
//!   second half with its 256 tiles; `$2107`/`$210B` are ignored there.
//!   The texel color is the low byte of the tile's word. `M7SEL`
//!   applies the repeat flag (bit 7), the fill flag (bit 6), and the
//!   screen flip bits, which mirror at 256-wide and 256-line
//!   coordinates. The transform adds the 8.8 matrix terms and shifts
//!   the sum right by eight.
//! - EXTBG mode 7 gives BG2 one layer with the texel's bit 7 as
//!   per-pixel priority and texel zero transparent; BG1 stays at
//!   priority zero.
//! - Offset-per-tile (modes 2, 4, 6) reads the optimization map entry
//!   and applies its low byte as a signed horizontal pixel shift and
//!   bits 8-14 as a signed vertical tilemap-row shift. The addend
//!   polarity is the model's own convention, and the optimization
//!   priority bit is not applied.
//! - Modes 5 and 6 render one doubled 8x8 character per 16-column
//!   cell. The 16-tall forms align the second character row at
//!   `+0x10`. The `SETINI` pseudo-hires form (modes 0-4) renders the
//!   main-screen merge on the even columns and the subscreen merge on
//!   the odd columns, with no composite blur.
//! - Tile attributes: bits 10-12 select the palette, bit 13 the
//!   per-tile priority, bit 14 the horizontal flip, and bit 15 the
//!   vertical flip. 8bpp layers ignore the palette bits, since the
//!   +256 palette offset aliases into CGRAM's 256 entries, and use
//!   bit 13 as their priority. Palettes: mode 0 layers map at
//!   32-entry strides; the other 2bpp layers use `palette*4 + pixel`;
//!   4bpp layers use `palette*16 + pixel`; 8bpp and mode 7 use the
//!   raw pixel. Subtraction color math clamps underflow to zero
//!   instead of wrapping.
//! - The backdrop of both screens is CGRAM entry 0. Color windows,
//!   direct color, the mode 7 direct-color form, window masking logic,
//!   and the `$2103` priority-rotation flag are decoded but never
//!   applied.
//! - Objects: 4bpp characters at the `$2101` name base, with the
//!   classic 0x2000-byte gap for names `$E0-$FF`, widened by the
//!   name-select field in 8 KiB steps. The base character aligns to
//!   the sprite width; characters step one per 8-pixel column and
//!   `0x10` per 16-pixel row. Palette, priority, and the flips apply
//!   per pixel; the per-object mosaic bit is decoded but not applied.
//!   Object pixels past logical position 255 do not paint. Sprites
//!   evaluate in OAM order; a scanline that sees more than the first
//!   34 matching objects keeps the first 34 and latches `$213E`
//!   bit 7. The range-over flag never latches.
//! - `$213E` reports its die-version nibble as zero and $213F reports
//!   the retail S-PPU2 version 3. The vblank OAM address reload and
//!   the OAM auto-rewrite are not modeled; the `$213C`/`$213D`
//!   latched counts approximate position with the model's line walk.

use demu_config::model::BootState;
use demu_core::device::{AccessWidth, BusAccess, Frame, MachineCtx, VideoLayer, VideoSource};
use demu_core::frequency::{Address, Tick};
use demu_core::signal::Level;
use demu_core::triggers::TriggerEvent;
use demu_core::{Device, Snapshot, SnapshotError};

/// dots per scanline.
const LINE_DOTS: u32 = 341;
/// Scanlines per frame, NTSC.
const NTSC_LINES: u16 = 263;
/// Scanlines per frame, PAL.
const PAL_LINES: u16 = 313;
/// The scanline where vblank starts, NTSC.
const NTSC_VBLANK: u16 = 224;
/// The scanline where vblank starts, PAL.
const PAL_VBLANK: u16 = 274;
/// Composed-frame width.
const WIDTH: usize = 512;
/// Composed-frame height.
const HEIGHT: usize = 448;
/// VRAM bytes (32K words).
const VRAM_BYTES: usize = 0x10000;
/// CGRAM bytes (256 words).
const CGRAM_BYTES: usize = 0x200;
/// OAM bytes: 128 objects of four bytes plus 32 attribute bytes.
const OAM_BYTES: usize = 0x220;
/// The sprite count a scanline evaluates.
const SPRITES_PER_LINE: usize = 34;
/// The mode 7 map size in tiles.
const M7_MAP: u32 = 128;
/// The mode 7 texel space size.
const M7_TEXELS: i32 = 1024;

/// Sprite size pairs, small and large, per `$2101` bits 7-5. Each
/// entry holds `[[w, h] small, [w, h] large]`.
const OBJ_SIZES: [[[u8; 2]; 2]; 8] = [
    [[8, 8], [16, 16]],
    [[8, 8], [32, 32]],
    [[8, 8], [64, 64]],
    [[16, 16], [32, 32]],
    [[16, 16], [64, 64]],
    [[32, 32], [64, 64]],
    [[16, 32], [32, 64]],
    [[16, 32], [32, 32]],
];

/// Priority slots `(layer, priority)` front to back. Layer 4 is the
/// object plane, layers 0-3 are BG1-BG4, and the backdrop is the merge
/// fallback; layer ids match the `$2131` color-math bit order.
const SLOTS_MODE0: [(u8, u8); 12] = [
    (4, 3),
    (0, 1),
    (1, 1),
    (4, 2),
    (0, 0),
    (1, 0),
    (4, 1),
    (2, 1),
    (3, 1),
    (4, 0),
    (2, 0),
    (3, 0),
];
/// Priority slots for mode 1 with the BG3 priority bit clear.
const SLOTS_MODE1: [(u8, u8); 10] = [
    (4, 3),
    (0, 1),
    (1, 1),
    (4, 2),
    (0, 0),
    (1, 0),
    (4, 1),
    (2, 1),
    (4, 0),
    (2, 0),
];
/// Priority slots for mode 1 with the BG3 priority bit set.
const SLOTS_MODE1_P: [(u8, u8); 10] = [
    (2, 1),
    (4, 3),
    (0, 1),
    (1, 1),
    (4, 2),
    (0, 0),
    (1, 0),
    (4, 1),
    (4, 0),
    (2, 0),
];
/// Priority slots for modes 2, 3, 4, and 5.
const SLOTS_MODE2: [(u8, u8); 8] = [
    (4, 3),
    (0, 1),
    (4, 2),
    (1, 1),
    (4, 1),
    (0, 0),
    (4, 0),
    (1, 0),
];
/// Priority slots for mode 6.
const SLOTS_MODE6: [(u8, u8); 6] = [(4, 3), (0, 1), (4, 2), (4, 1), (0, 0), (4, 0)];
/// Priority slots for mode 7 without EXTBG.
const SLOTS_MODE7: [(u8, u8); 5] = [(4, 3), (4, 2), (4, 1), (0, 0), (4, 0)];
/// Priority slots for mode 7 with EXTBG.
const SLOTS_MODE7_EXT: [(u8, u8); 7] = [(4, 3), (4, 2), (1, 1), (4, 1), (0, 0), (4, 0), (1, 0)];

/// The merge fallback layer id, the backdrop.
const LAYER_BACKDROP: u8 = 5;

/// Snapshot bytes before the three memory arrays; both `snapshot` and
/// `restore` derive from this count.
const SNAP_FIXED: usize = 112;

/// The signed 13-bit form of one mode 7 center register.
fn sign13(raw: u16) -> i32 {
    let v = (raw & 0x1FFF) as i32;
    if v & 0x1000 != 0 { v - 0x2000 } else { v }
}

/// One object of the current scanline's evaluation.
#[derive(Debug, Clone, Copy)]
struct ObjEval {
    /// The nine-bit X of the object plane.
    x: u32,
    /// The display row inside the object at this line.
    row: u8,
    /// The character name byte.
    name: u8,
    /// The attribute byte: palette, priority, flips, mosaic flag.
    attr: u8,
    /// The width in pixels.
    w: u8,
    /// The height in pixels.
    h: u8,
}

/// The S-PPU1 and S-PPU2 pair.
#[derive(Debug)]
pub struct SnesPpu {
    name: String,
    /// Domain ticks per scanline visit: 341 dots times the `dot` divider.
    line_div: u64,
    /// PPU dots per domain tick, from the `dot` divider.
    dot_div: u64,
    /// Scanlines per frame: 263 NTSC, 313 PAL.
    lines: u16,
    /// The scanline where vblank starts.
    vblank_line: u16,
    /// The PAL flag, reported in `$213F` bit 4.
    pal: bool,

    // PPU1 register state.
    /// `$2100` INIDISP: force blank bit 7 over brightness bits 3-0.
    inidisp: u8,
    /// `$2101` OBJSEL.
    objsel: u8,
    /// `$2105` BGMODE.
    bgmode: u8,
    /// `$2106` MOSAIC.
    mosaic: u8,
    /// `$2107-$210A` BGnSC.
    bg_sc: [u8; 4],
    /// `$210B`/`$210C` BGnNBA.
    nba: [u8; 2],
    /// `$210D`/`$210F`/`$2111`/`$2113` BGnHOFS.
    bg_hofs: [u16; 4],
    /// `$210E`/`$2110`/`$2112`/`$2114` BGnVOFS.
    bg_vofs: [u16; 4],
    /// The mode 7 form of `$210D`, 13-bit.
    m7h: u16,
    /// The mode 7 form of `$210E`, 13-bit.
    m7v: u16,
    /// `$2115` VMAIN.
    vmain: u8,
    /// `$2116`/`$2117` VMADD word address, 15 bits.
    vaddr: u16,
    /// VRAM, 32K words little-endian.
    vram: Vec<u8>,
    /// The VRAM read prefetch buffer.
    vbuffer: u16,
    /// False when the low byte of the prefetch buffer is pending.
    vread_low: bool,
    /// `$211A` M7SEL.
    m7sel: u8,
    /// `$211B-$211E`: M7A, M7B, M7C, M7D as 16-bit values.
    m7: [u16; 4],
    /// The signed 8-bit factor last written through `$211C`.
    m7b_mult: i8,
    /// `$211F` M7X, 13-bit.
    m7x: u16,
    /// `$2120` M7Y, 13-bit.
    m7y: u16,
    /// The shared low-byte latch of the `$210D-$2114` scroll file.
    bgofs_latch: u8,
    /// The keeper of the low three bits of BG horizontal scroll.
    bghofs_latch: u8,
    /// The shared low-byte latch of the mode 7 register file.
    mode7_latch: u8,
    /// The second-write reroute of the scroll file, as the register
    /// offset inside `$210D-$2114`.
    scroll_prev: Option<u8>,
    /// The second-write reroute of the mode 7 file, as the register
    /// offset inside `$211B-$2120`.
    m7_prev: Option<u8>,

    // PPU2 register state.
    /// `$2121` CGADD byte address, wrapping at 0x200.
    cgadd: u16,
    /// CGRAM, 256 words little-endian.
    cgram: Vec<u8>,
    /// The pending CGDATA write byte.
    cg_latch: u8,
    /// `$2123-$2125` window selection bytes (stored only).
    wsel: [u8; 3],
    /// `$2126-$2129` window positions (stored only).
    wpos: [u8; 4],
    /// `$212A`/`$212B` window mask logic (stored only).
    wlog: [u8; 2],
    /// `$212C` TM.
    tm: u8,
    /// `$212D` TS.
    ts: u8,
    /// `$212E` TMW (stored only).
    tmw: u8,
    /// `$212F` TSW (stored only).
    tsw: u8,
    /// `$2130` CGWSEL.
    cgwsel: u8,
    /// `$2131` CGADSUB.
    cgadsub: u8,
    /// The fixed color assembled by `$2132` COLDATA writes.
    fixed: u16,
    /// `$2133` SETINI.
    setini: u8,

    // Object memory.
    /// The 544-byte OAM space.
    oam: Vec<u8>,
    /// `$2102`/`$2103` OAMADD: bits 0-8 the word address, bit 15 the
    /// priority-rotation flag.
    oamadd: u16,
    /// The internal OAM byte address, wrapping at 0x220.
    oamaddr: usize,
    /// The pending OAMDATA write byte.
    oam_latch: u8,

    // Line walk state.
    /// The current scanline counter.
    line: u16,
    /// The domain tick of the next line visit.
    line_due: u64,
    /// Complete frames since boot.
    frames: u64,
    /// The latched horizontal counter.
    latch_h: u16,
    /// The latched vertical counter.
    latch_v: u16,
    /// False when the low byte of `$213C` is pending.
    hbyte: bool,
    /// False when the low byte of `$213D` is pending.
    vbyte: bool,
    /// `$213F` bit 6: the counters hold a software latch.
    counter_latch: bool,
    /// `$213E` bit 7: an object line hit the sprite limit.
    time_over: bool,

    // Compose scratch, refilled per scanline.
    obj_on: [bool; WIDTH],
    obj_val: [u16; WIDTH],
    obj_prio: [u8; WIDTH],
    /// The composed frame in RGB888.
    screen: Vec<u8>,
    /// The frame view the shell serves.
    cached_frame: Frame,
    next: Option<Tick>,
    last_tick: Option<Tick>,
}

impl SnesPpu {
    /// An S-PPU pair from its configuration entry.
    pub fn new(device: &demu_config::model::DeviceInstance) -> Self {
        let pal = device
            .params
            .get("pal")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let dot_div = u64::from(device.dividers.get("dot").copied().unwrap_or(4)).max(1);
        Self {
            name: device.name.to_string(),
            line_div: u64::from(LINE_DOTS) * dot_div,
            dot_div,
            lines: if pal { PAL_LINES } else { NTSC_LINES },
            vblank_line: if pal { PAL_VBLANK } else { NTSC_VBLANK },
            pal,
            inidisp: 0x80,
            objsel: 0,
            bgmode: 0,
            mosaic: 0,
            bg_sc: [0; 4],
            nba: [0; 2],
            bg_hofs: [0; 4],
            bg_vofs: [0; 4],
            m7h: 0,
            m7v: 0,
            vmain: 0,
            vaddr: 0,
            vram: vec![0; VRAM_BYTES],
            vbuffer: 0,
            vread_low: true,
            m7sel: 0,
            m7: [0; 4],
            m7b_mult: 0,
            m7x: 0,
            m7y: 0,
            bgofs_latch: 0,
            bghofs_latch: 0,
            mode7_latch: 0,
            scroll_prev: None,
            m7_prev: None,
            cgadd: 0,
            cgram: vec![0; CGRAM_BYTES],
            cg_latch: 0,
            wsel: [0; 3],
            wpos: [0; 4],
            wlog: [0; 2],
            tm: 0,
            ts: 0,
            tmw: 0,
            tsw: 0,
            cgwsel: 0,
            cgadsub: 0,
            fixed: 0,
            setini: 0,
            oam: vec![0; OAM_BYTES],
            oamadd: 0,
            oamaddr: 0,
            oam_latch: 0,
            line: 0,
            line_due: 0,
            frames: 0,
            latch_h: 0,
            latch_v: 0,
            hbyte: false,
            vbyte: false,
            counter_latch: false,
            time_over: false,
            obj_on: [false; WIDTH],
            obj_val: [0; WIDTH],
            obj_prio: [0; WIDTH],
            screen: vec![0; WIDTH * HEIGHT * 3],
            cached_frame: Frame {
                width: WIDTH as u32,
                height: HEIGHT as u32,
                pixels: vec![0; WIDTH * HEIGHT * 3],
            },
            next: None,
            last_tick: None,
        }
    }

    /// The step the VRAM address advances per VMAIN bits 1-0.
    fn vram_step(&self) -> u16 {
        match self.vmain & 0x03 {
            1 => 32,
            2 | 3 => 128,
            _ => 1,
        }
    }

    /// The VRAM word at a 32K-word address, little-endian.
    fn vram_word(&self, w: u32) -> u16 {
        let w = (w & 0x7FFF) as usize;
        u16::from(self.vram[w * 2]) | (u16::from(self.vram[w * 2 + 1]) << 8)
    }

    /// The VRAM byte at a 32K-word address, low or high.
    fn vram_byte(&self, w: u32, low: bool) -> u8 {
        self.vram[(w & 0x7FFF) as usize * 2 + usize::from(!low)]
    }

    /// The CGRAM byte at a 512-byte-address offset.
    fn cgram_byte(&self, b: usize) -> u8 {
        self.cgram[b & 0x1FF]
    }

    /// The CGRAM word of one 256-entry palette address.
    fn cgram_word(&self, e: u16) -> u16 {
        u16::from(self.cgram_byte(usize::from(e) * 2))
            | (u16::from(self.cgram_byte(usize::from(e) * 2 + 1)) << 8)
    }

    /// Re-derive the internal OAM byte address from OAMADD.
    fn set_oamaddr(&mut self) {
        let w = self.oamadd & 0x01FF;
        self.oamaddr = if w >= 0x100 {
            0x200 + usize::from(w & 0x0F) * 2
        } else {
            usize::from(w & 0xFF) * 2
        };
    }

    /// Apply one write to the `$210D-$2114` scroll file. `idx` counts
    /// from `$210D`; even offsets are horizontal.
    fn scroll_write(&mut self, idx: u8, v: u8) {
        if let Some(prev) = self.scroll_prev.take() {
            let high = u16::from(v);
            if prev & 1 == 0 {
                // Horizontal: the low byte is the shared latch, with
                // its low three bits in the H keeper.
                let low = u16::from(self.bgofs_latch & !0x07) | u16::from(self.bghofs_latch & 7);
                self.bg_hofs[usize::from(prev >> 1)] = (high << 8 | low) & 0x3FF;
                if prev >> 1 == 0 {
                    self.m7h = (high << 8 | u16::from(self.mode7_latch)) & 0x1FFF;
                }
            } else {
                self.bg_vofs[usize::from(prev >> 1)] =
                    (high << 8 | u16::from(self.bgofs_latch)) & 0x3FF;
                if prev >> 1 == 0 {
                    self.m7v = (high << 8 | u16::from(self.mode7_latch)) & 0x1FFF;
                }
            }
        } else {
            self.scroll_prev = Some(idx & 7);
        }
        // Every scroll write refreshes the shared latches.
        self.bgofs_latch = v;
        if idx & 1 == 0 {
            self.bghofs_latch = v;
        }
        if (idx & 7) <= 1 {
            // `$210D`/`$210E` share the mode 7 low-byte latch.
            self.mode7_latch = v;
        }
    }

    /// Apply one write to the `$211B-$2120` mode 7 file, where `idx`
    /// counts from `$211B`.
    fn m7_write(&mut self, idx: u8, v: u8) {
        let high = u16::from(v);
        let low = u16::from(self.mode7_latch);
        if let Some(prev) = self.m7_prev.take() {
            match prev {
                0 => self.m7[0] = (high << 8) | low,
                1 => {
                    self.m7[1] = (high << 8) | low;
                    self.m7b_mult = v as i8;
                }
                2 => self.m7[2] = (high << 8) | low,
                3 => self.m7[3] = (high << 8) | low,
                4 => self.m7x = (high << 8 | low) & 0x1FFF,
                _ => self.m7y = (high << 8 | low) & 0x1FFF,
            }
        } else {
            if idx == 1 {
                // The 8-bit multiplier write of `$211C` composes the
                // register and records its signed factor.
                self.m7[1] = (high << 8) | low;
                self.m7b_mult = v as i8;
            }
            self.m7_prev = Some(idx);
        }
        self.mode7_latch = v;
    }

    /// Write the VRAM data ports `$2118` (low) and `$2119` (high); the
    /// increment follows the VMAIN half the port matches.
    fn vram_write(&mut self, high: bool, v: u8) {
        let w = usize::from(self.vaddr & 0x7FFF);
        self.vram[w * 2 + usize::from(high)] = v;
        if (self.vmain & 0x80 != 0) == high {
            self.vaddr = (self.vaddr + self.vram_step()) & 0x7FFF;
        }
    }

    /// Write the CGRAM data port `$2122` with its two-stage latch; the
    /// byte address advances by one per write.
    fn cgram_write(&mut self, v: u8) {
        if self.cgadd & 1 == 1 {
            let base = usize::from(self.cgadd) & !1;
            self.cgram[base] = self.cg_latch;
            self.cgram[base + 1] = v;
        }
        self.cg_latch = v;
        self.cgadd = (self.cgadd + 1) & 0x1FF;
    }

    /// Write the OAM data port `$2104` with its two-stage latch; the
    /// region word addresses below 0x200 hold pairs.
    fn oam_write(&mut self, v: u8) {
        let a = self.oamaddr;
        if a & 1 == 0 {
            self.oam_latch = v;
            if a >= 0x200 {
                self.oam[a] = v;
            }
        } else if a < 0x200 {
            self.oam[a - 1] = self.oam_latch;
            self.oam[a] = v;
        } else {
            self.oam[a] = v;
        }
        self.oamaddr = (a + 1) % OAM_BYTES;
    }

    /// Apply one COLDATA write, updating the fixed-color channels the
    /// byte selects.
    fn coldata_write(&mut self, v: u8) {
        if v & 0x20 != 0 {
            self.fixed = (self.fixed & 0x7FE0) | u16::from(v & 0x1F);
        }
        if v & 0x40 != 0 {
            self.fixed = (self.fixed & 0x7C1F) | (u16::from(v & 0x1F) << 5);
        }
        if v & 0x80 != 0 {
            self.fixed = (self.fixed & 0x03FF) | (u16::from(v & 0x1F) << 10);
        }
    }

    /// The B-bus register write, offset `$100-$135`.
    fn reg_write(&mut self, r: u8, v: u8) {
        match r {
            0x00 => self.inidisp = v,
            0x01 => self.objsel = v,
            0x02 => {
                self.oamadd = (self.oamadd & 0xFF00) | u16::from(v);
                self.set_oamaddr();
            }
            0x03 => {
                self.oamadd = (u16::from(v & 0x81) << 8) | (self.oamadd & 0x00FF);
                self.set_oamaddr();
            }
            0x04 => self.oam_write(v),
            0x05 => self.bgmode = v,
            0x06 => self.mosaic = v,
            0x07..=0x0A => self.bg_sc[usize::from(r) - 0x07] = v,
            0x0B | 0x0C => self.nba[usize::from(r) - 0x0B] = v,
            0x0D..=0x14 => self.scroll_write(r - 0x0D, v),
            0x15 => self.vmain = v,
            0x16 | 0x17 => {
                if r == 0x16 {
                    self.vaddr = ((self.vaddr & 0xFF00) | u16::from(v)) & 0x7FFF;
                } else {
                    self.vaddr = (self.vaddr & 0x00FF) | (u16::from(v & 0x7F) << 8);
                }
                self.vbuffer = self.vram_word(u32::from(self.vaddr));
                self.vread_low = true;
            }
            0x18 => self.vram_write(false, v),
            0x19 => self.vram_write(true, v),
            0x1A => self.m7sel = v,
            0x1B..=0x20 => self.m7_write(r - 0x1B, v),
            0x21 => self.cgadd = u16::from(v) << 1,
            0x22 => self.cgram_write(v),
            0x23..=0x25 => self.wsel[usize::from(r) - 0x23] = v,
            0x26..=0x29 => self.wpos[usize::from(r) - 0x26] = v,
            0x2A | 0x2B => self.wlog[usize::from(r) - 0x2A] = v,
            0x2C => self.tm = v,
            0x2D => self.ts = v,
            0x2E => self.tmw = v,
            0x2F => self.tsw = v,
            0x30 => self.cgwsel = v,
            0x31 => self.cgadsub = v,
            0x32 => self.coldata_write(v),
            0x33 => self.setini = v,
            _ => {}
        }
    }

    /// The side-effect-free read of one register offset, for `peek`.
    fn peek_reg(&self, r: u8) -> u8 {
        match r {
            0x00 => self.inidisp,
            0x01 => self.objsel,
            0x02 => self.oamadd as u8,
            0x03 => ((self.oamadd >> 8) & 0x81) as u8,
            0x05 => self.bgmode,
            0x06 => self.mosaic,
            0x07..=0x0A => self.bg_sc[usize::from(r) - 0x07],
            0x0B | 0x0C => self.nba[usize::from(r) - 0x0B],
            0x15 => self.vmain,
            0x16 => self.vaddr as u8,
            0x17 => (self.vaddr >> 8) as u8,
            0x21 => (self.cgadd >> 1) as u8,
            0x23..=0x25 => self.wsel[usize::from(r) - 0x23],
            0x26..=0x29 => self.wpos[usize::from(r) - 0x26],
            0x2A | 0x2B => self.wlog[usize::from(r) - 0x2A],
            0x2C => self.tm,
            0x2D => self.ts,
            0x2E => self.tmw,
            0x2F => self.tsw,
            0x30 => self.cgwsel,
            0x31 => self.cgadsub,
            0x33 => self.setini,
            0x34 => self.m7_product() as u8,
            0x35 => (self.m7_product() >> 8) as u8,
            0x36 => (self.m7_product() >> 16) as u8,
            0x38 => self.oam[self.oamaddr],
            0x39 | 0x3A => {
                if r == 0x39 {
                    self.vbuffer as u8
                } else {
                    (self.vbuffer >> 8) as u8
                }
            }
            0x3B => self.cgram_byte(usize::from(self.cgadd & 0x1FF)),
            0x3C => (self.latch_h & 0xFF) as u8 | (u8::from(self.hbyte) << 7),
            0x3D => (self.latch_v & 0xFF) as u8 | (u8::from(self.vbyte) << 7),
            0x3E => u8::from(self.time_over) << 7,
            0x3F => {
                u8::from(self.frames & 1 == 1) << 7
                    | u8::from(self.counter_latch) << 6
                    | 0x20
                    | u8::from(self.pal) << 4
                    | 0x03
            }
            _ => 0xFF,
        }
    }

    /// The signed 24-bit `$2134-$36` product of `$211B` and the last
    /// `$211C` byte.
    fn m7_product(&self) -> i32 {
        i32::from(self.m7[0] as i16) * i32::from(self.m7b_mult)
    }

    /// The B-bus register read, offset `$100-$13F`.
    fn reg_read(&mut self, r: u8, tick: Tick) -> u8 {
        match r {
            // The software latch of `$2137`: the counters latch and the
            // read returns open bus.
            0x37 => {
                self.latch_counters(tick);
                0xFF
            }
            // The OAM data read advances the internal byte address.
            0x38 => {
                let v = self.oam[self.oamaddr];
                self.oamaddr = (self.oamaddr + 1) % OAM_BYTES;
                v
            }
            // The VRAM reads alternate the halves of the prefetched
            // word; the completed pair steps the address and refills.
            0x39 | 0x3A => {
                let v = if self.vread_low {
                    self.vbuffer & 0xFF
                } else {
                    self.vbuffer >> 8
                };
                if !self.vread_low {
                    self.vaddr = (self.vaddr + self.vram_step()) & 0x7FFF;
                    self.vbuffer = self.vram_word(u32::from(self.vaddr));
                }
                self.vread_low = !self.vread_low;
                v as u8
            }
            // The CGRAM read alternates the halves and advances.
            0x3B => {
                let b = usize::from(self.cgadd & 0x1FF);
                let v = self.cgram_byte(b);
                self.cgadd = (self.cgadd + 1) & 0x1FF;
                v
            }
            // The counter reads carry the byte toggle; the `$213F`
            // read clears the counter latch and both toggles.
            0x3C | 0x3D => {
                let (h, pend) = if r == 0x3C {
                    (self.latch_h, self.hbyte)
                } else {
                    (self.latch_v, self.vbyte)
                };
                let v = if pend { (h >> 8) as u8 } else { h as u8 };
                if r == 0x3C {
                    self.hbyte = !self.hbyte;
                } else {
                    self.vbyte = !self.vbyte;
                }
                v
            }
            0x3E => u8::from(self.time_over) << 7,
            0x3F => {
                let field = if self.setini & 0x01 != 0 {
                    u8::from(self.frames & 1 == 1) << 7
                } else {
                    0
                };
                let out = field
                    | u8::from(self.counter_latch) << 6
                    | 0x20
                    | u8::from(self.pal) << 4
                    | 0x03;
                self.counter_latch = false;
                self.hbyte = false;
                self.vbyte = false;
                out
            }
            _ => self.peek_reg(r),
        }
    }

    /// Latch the H/V counters for `$213C`/`$213D` reads. The latched
    /// horizontal count is the model's dot estimate of the current
    /// line and the vertical count is the line counter.
    fn latch_counters(&mut self, tick: Tick) {
        let anchor = self.line_due.saturating_sub(self.line_div);
        let dot =
            (tick.0.saturating_sub(anchor) / self.dot_div).min(u64::from(LINE_DOTS - 1)) as u16;
        self.latch_h = dot;
        self.latch_v = self.line;
        self.counter_latch = true;
    }

    /// The 5-bit channels `[r, g, b]` of one CGRAM entry.
    fn cg_channels(&self, entry: u16) -> [u8; 3] {
        let v = self.cgram_word(entry) & 0x7FFF;
        [
            v as u8 & 0x1F,
            (v >> 5) as u8 & 0x1F,
            (v >> 10) as u8 & 0x1F,
        ]
    }

    /// The 5-bit channels of the fixed color.
    fn fixed_channels(&self) -> [u8; 3] {
        let f = self.fixed & 0x7FFF;
        [
            f as u8 & 0x1F,
            (f >> 5) as u8 & 0x1F,
            (f >> 10) as u8 & 0x1F,
        ]
    }

    /// Expand 5-bit channels to RGB888 with bit replication.
    fn expand555(c: [u8; 3]) -> [u8; 3] {
        [
            (c[0] << 3) | (c[0] >> 2),
            (c[1] << 3) | (c[1] >> 2),
            (c[2] << 3) | (c[2] >> 2),
        ]
    }

    /// Scale RGB888 by the INIDISP brightness, integer truncated.
    fn brightness(&self, c: [u8; 3]) -> [u8; 3] {
        let level = usize::from(self.inidisp & 0x0F);
        [
            ((usize::from(c[0]) * level) / 15) as u8,
            ((usize::from(c[1]) * level) / 15) as u8,
            ((usize::from(c[2]) * level) / 15) as u8,
        ]
    }

    /// The map entry of one background layer at a map position.
    fn map_entry(&self, layer: usize, col: u32, row: u32) -> u16 {
        let sc = self.bg_sc[layer];
        let base = u32::from(sc >> 2) << 10;
        let col = col & (if sc & 0x01 != 0 { 0x3F } else { 0x1F });
        let row = row & (if sc & 0x02 != 0 { 0x3F } else { 0x1F });
        let addr =
            base + (row & 0x1F) * 32 + (col & 0x1F) + (col / 32) * 0x400 + (row / 32) * 0x800;
        self.vram_word(addr)
    }

    /// The optimization map entry the offset-per-tile forms read.
    fn opt_entry(&self, layer: usize, x: u32, line: u32) -> u16 {
        let shift = if usize::from(self.bgmode & 7) == 6 {
            4
        } else {
            3
        };
        let hofs = u32::from(self.bg_hofs[layer] & 0x3FF);
        let vofs = u32::from(self.bg_vofs[layer] & 0x3FF);
        self.map_entry(layer, (x + hofs) >> shift, (line + vofs) >> 3)
    }

    /// The character base of one layer, in words.
    fn chr_base(&self, layer: usize) -> u32 {
        let nba = self.nba[layer / 2];
        let nib = if layer.is_multiple_of(2) {
            nba & 0x0F
        } else {
            nba >> 4
        };
        (u32::from(nib) << 12) & 0x7FFF
    }

    /// The decode of one character at a depth, returning the palette
    /// value. `char_addr` is a 32K-word address; `px`/`py` are 0..7.
    fn tile_value(&self, depth: u8, char_addr: u32, px: u8, py: u8) -> u8 {
        let bit = 7 - (px & 7);
        match depth {
            // One word holds one 2bpp row pair.
            2 => {
                let w = ((char_addr & 0x7FFF) + u32::from(py)) as usize;
                let b_lo = self.vram[w * 2];
                let b_hi = self.vram[w * 2 + 1];
                ((b_hi >> bit) & 1) << 1 | (b_lo >> bit) & 1
            }
            // Two words per row pair; the second tile half holds the
            // planes of the two high color bits.
            4 => {
                let w = ((char_addr & 0x7FFF) + u32::from(py) * 2) as usize;
                let p0 = self.vram[w * 2];
                let p1 = self.vram[w * 2 + 1];
                let p2 = self.vram[(w + 8) * 2];
                let p3 = self.vram[(w + 8) * 2 + 1];
                ((p3 >> bit) & 1) << 3
                    | ((p2 >> bit) & 1) << 2
                    | ((p1 >> bit) & 1) << 1
                    | (p0 >> bit) & 1
            }
            // Four words per row, one plane byte per pixel bit.
            8 => {
                let mut v = 0u8;
                let w = ((char_addr & 0x7FFF) + u32::from(py) * 4) as usize;
                for p in 0..8usize {
                    v |= ((self.vram[w * 2 + p] >> bit) & 1) << p;
                }
                v
            }
            _ => 0,
        }
    }

    /// The background depth in bits per pixel for one layer.
    fn bg_depth(&self, layer: usize) -> u8 {
        match usize::from(self.bgmode & 7) {
            0 => 2,
            1 => {
                if layer < 2 {
                    4
                } else {
                    2
                }
            }
            2 => 4,
            3 => {
                if layer == 0 {
                    8
                } else {
                    4
                }
            }
            4 => {
                if layer == 0 {
                    8
                } else {
                    2
                }
            }
            5 => {
                if layer == 0 {
                    4
                } else {
                    2
                }
            }
            6 => 4,
            _ => 8,
        }
    }

    /// True when the current mode renders the layer.
    fn bg_present(&self, layer: usize) -> bool {
        match usize::from(self.bgmode & 7) {
            0 => true,
            1 => layer < 3,
            2..=5 => layer < 2,
            6 | 7 => layer == 0,
            _ => false,
        }
    }

    /// The pixel of one normal (non mode 7) background layer: the
    /// CGRAM entry and the tile priority, or `None` when transparent.
    fn bg_pixel(&self, layer: usize, x: u32, line: u32, wide: bool) -> Option<(u16, u8)> {
        let mode = usize::from(self.bgmode & 7);
        let depth = self.bg_depth(layer);

        // Offset-per-tile addends of modes 2, 4, and 6.
        let (hadd, vadd) = if matches!(mode, 2 | 4) || mode == 6 {
            let entry = self.opt_entry(if mode == 6 { 1 } else { 2 }, x, line);
            (i32::from((entry & 0xFF) as u8 as i8), {
                let raw = ((entry >> 8) & 0x7F) as i32;
                if raw & 0x40 != 0 { raw - 0x80 } else { raw }
            })
        } else {
            (0, 0)
        };
        let hofs = u32::from(self.bg_hofs[layer] & 0x3FF);
        let vofs = u32::from(self.bg_vofs[layer] & 0x3FF);
        let msz = u32::from(self.mosaic & 0x0F) + 1;
        let m_on = self.mosaic & (0x01 << layer) != 0;
        let sx = ((if m_on { x / msz * msz } else { x }) + hofs) & 0x3FF;
        let sy = ((if m_on { line / msz * msz } else { line }) + vofs) & 0x3FF;
        let big = !wide && self.bgmode & (0x10 << layer) != 0;
        let cw: u32 = if big || wide { 16 } else { 8 };
        let ch: u32 = if big { 16 } else { 8 };

        // The map position. The offset-per-tile V addend shifts the
        // tilemap row and the H addend shifts the sample inside the
        // cell; the doubled characters of the wide forms sample every
        // second column.
        let col = (sx >> if cw == 16 { 4 } else { 3 })
            & (if self.bg_sc[layer] & 0x01 != 0 {
                0x3F
            } else {
                0x1F
            });
        let row = (((sy >> if ch == 16 { 4 } else { 3 }) as i32 + vadd)
            & (if self.bg_sc[layer] & 0x02 != 0 {
                0x3F
            } else {
                0x1F
            })) as u32;
        let entry = self.map_entry(layer, col, row);
        let mut px = (((sx as i32 + hadd) & (cw as i32 - 1)) as u32) >> if wide { 1 } else { 0 };
        let mut py = (sy & (ch - 1)) as u8;
        if entry & 0x4000 != 0 {
            px = (cw - 1) - px;
        }
        if entry & 0x8000 != 0 {
            py = ((ch - 1) - u32::from(py)) as u8;
        }
        let tile = u32::from(entry & 0xFF);
        let char_num = if big {
            (tile & !1) + (px >> 3) + u32::from(py >> 3) * 0x10
        } else {
            tile
        };
        let char_bytes = u32::from(depth) * 4; // one character in words
        let v = self.tile_value(
            depth,
            self.chr_base(layer) + char_num * char_bytes,
            px as u8,
            py & 7,
        );
        if v == 0 {
            return None;
        }
        let entry16 = match depth {
            8 => u16::from(v),
            2 => {
                let layer_pal = if mode == 0 { 32 * layer as u16 } else { 0 };
                layer_pal + (((entry >> 10) & 7) << 2) + u16::from(v)
            }
            _ => (((entry >> 10) & 7) << 4) + u16::from(v),
        };
        Some((entry16, ((entry >> 13) & 1) as u8))
    }

    /// The pixel of one mode 7 plane: the CGRAM entry and priority, or
    /// `None` when transparent.
    fn mode7_pixel(&self, plane: usize, x: u32, line: u32) -> Option<(u16, u8)> {
        let mut px = x;
        let mut py = line;
        if self.m7sel & 0x1 != 0 {
            px = 255 - px;
        }
        if self.m7sel & 0x2 != 0 {
            py = 255 - py;
        }
        let a = i32::from(self.m7[0] as i16);
        let b = i32::from(self.m7[1] as i16);
        let c = i32::from(self.m7[2] as i16);
        let d = i32::from(self.m7[3] as i16);
        let dx = i32::try_from(px).unwrap_or(0) - sign13(self.m7x);
        let dy = i32::try_from(py).unwrap_or(0) - sign13(self.m7y);
        let vx = (a * dx + b * dy) >> 8;
        let vy = (c * dx + d * dy) >> 8;
        let in_map = (0..M7_TEXELS).contains(&vx) && (0..M7_TEXELS).contains(&vy);
        let repeat = self.m7sel & 0x80 == 0;
        if !in_map && !repeat {
            if self.m7sel & 0x40 == 0 {
                return None;
            }
            // Character-zero fill beyond the map bounds.
            return self.m7_texel(plane, (vx & 7) as u32, (vy & 7) as u32);
        }
        self.m7_texel(
            plane,
            (vx & (M7_TEXELS - 1)) as u32,
            (vy & (M7_TEXELS - 1)) as u32,
        )
    }

    /// One mode 7 pixel from its tilemap position. Mode 7 occupies the
    /// first VRAM half with its 128x128 map and the second half with
    /// its 256 tiles.
    fn m7_texel(&self, plane: usize, tx: u32, ty: u32) -> Option<(u16, u8)> {
        let entry = self.vram_word(((tx >> 3) * M7_MAP + (ty >> 3)) & 0x7FFF);
        let texel = self.vram_byte(
            0x4000 + u32::from(entry & 0xFF) * 64 + (ty & 7) * 8 + (tx & 7),
            true,
        );
        if texel == 0 {
            return None;
        }
        if plane == 0 {
            Some((u16::from(texel), 0))
        } else {
            Some((u16::from(texel), (texel >> 7) & 1))
        }
    }

    /// The priority slot list for the current mode.
    fn slots(&self) -> &'static [(u8, u8)] {
        match usize::from(self.bgmode & 7) {
            0 => &SLOTS_MODE0,
            1 => {
                if self.bgmode & 0x08 != 0 {
                    &SLOTS_MODE1_P
                } else {
                    &SLOTS_MODE1
                }
            }
            2..=5 => &SLOTS_MODE2,
            6 => &SLOTS_MODE6,
            _ => {
                if self.setini & 0x40 != 0 {
                    &SLOTS_MODE7_EXT
                } else {
                    &SLOTS_MODE7
                }
            }
        }
    }

    /// Compose the main-screen or subscreen merge of one column: the
    /// CGRAM entry and the winning layer id.
    fn screen_pixel(&self, col: usize, line: u32, mask: u8) -> (u16, u8) {
        let mode = usize::from(self.bgmode & 7);
        let wide = matches!(mode, 5 | 6);
        let x = (if wide { col } else { col / 2 }) as u32;
        let mut cand = [None, None, None, None];
        if mode == 7 {
            cand[0] = self.mode7_pixel(0, x, line);
            if self.setini & 0x40 != 0 {
                cand[1] = self.mode7_pixel(1, x, line);
            }
        } else {
            for (layer, slot) in cand.iter_mut().enumerate() {
                if self.bg_present(layer) {
                    *slot = self.bg_pixel(layer, x, line, wide);
                }
            }
        }
        for &(layer, prio) in self.slots() {
            if layer == 4 {
                if mask & 0x10 != 0 && self.obj_on[col] && self.obj_prio[col] == prio {
                    return (self.obj_val[col], 4);
                }
                continue;
            }
            if mask >> layer & 1 == 0 {
                continue;
            }
            if let Some((e, p)) = cand[usize::from(layer)]
                && p == prio
            {
                return (e, layer);
            }
        }
        (self.cgram_word(0), LAYER_BACKDROP)
    }

    /// The pixel color of one column: the main-screen entry optionally
    /// through color math, expanded to RGB888, then scaled by the
    /// INIDISP brightness.
    fn color_out(&self, col: usize, line: u32, entry: u16, layer: u8) -> [u8; 3] {
        let mut m = self.cg_channels(entry);
        let math = self.cgadsub >> layer & 1 == 1 && (layer != 4 || (entry >> 4) & 7 >= 4);
        if math {
            let s = if self.cgwsel & 0x01 != 0 {
                self.cg_channels(self.screen_pixel(col, line, self.ts).0)
            } else {
                self.fixed_channels()
            };
            let half = self.cgadsub & 0x40 != 0;
            let sub = self.cgadsub & 0x80 != 0;
            m = [
                Self::math_channel(m[0], s[0], sub, half),
                Self::math_channel(m[1], s[1], sub, half),
                Self::math_channel(m[2], s[2], sub, half),
            ];
        }
        self.brightness(Self::expand555(m))
    }

    /// One additive or subtractive color-math channel, clamped.
    fn math_channel(m: u8, s: u8, sub: bool, half: bool) -> u8 {
        if !sub {
            let v = usize::from(m) + usize::from(s);
            (if half { v / 2 } else { v }).min(31) as u8
        } else {
            let v = if usize::from(m) >= usize::from(s) {
                usize::from(m) - usize::from(s)
            } else {
                0
            };
            (if half { (v + 31) / 2 } else { v }).min(31) as u8
        }
    }

    /// Evaluate the objects of one line and paint their pixels into
    /// the object columns. Objects render in OAM order; later objects
    /// keep a column only with higher priority.
    fn eval_sprites(&mut self, line: u16) {
        self.obj_on[..].fill(false);
        self.obj_val[..].fill(0);
        self.obj_prio[..].fill(0);

        let sel = usize::from((self.objsel >> 5) & 7);
        let base_words = (u32::from(self.objsel & 0x07) << 12) & 0x7FFF;
        let gap_words = (u32::from((self.objsel >> 3) & 3) + 1) << 12;
        let mut found: Vec<ObjEval> = Vec::with_capacity(SPRITES_PER_LINE);
        for i in 0..128usize {
            let pair = (self.oam[0x200 + i / 4] >> ((i % 4) * 2)) & 3;
            let size = OBJ_SIZES[sel][usize::from(pair & 1)];
            let row = (u32::from(line) + 256 - u32::from(self.oam[i * 4])) & 0xFF;
            if row >= u32::from(size[1]) {
                continue;
            }
            found.push(ObjEval {
                x: u32::from(self.oam[i * 4 + 1]) | (u32::from(pair >> 1) << 8),
                row: row as u8,
                name: self.oam[i * 4 + 2],
                attr: self.oam[i * 4 + 3],
                w: size[0],
                h: size[1],
            });
        }
        self.time_over = found.len() > SPRITES_PER_LINE;
        for s in found.iter().take(SPRITES_PER_LINE) {
            let src_row = if s.attr & 0x80 != 0 {
                s.h - 1 - s.row
            } else {
                s.row
            };
            let ty = u32::from(src_row >> 3);
            let py = u32::from(src_row & 7);
            for k in 0..u32::from(s.w) {
                let px = (s.x + k) & 0x1FF;
                if px >= 256 {
                    continue;
                }
                let src_x = if s.attr & 0x40 != 0 {
                    u32::from(s.w) - 1 - k
                } else {
                    k
                };
                let align = u32::from((s.w >> 3) - 1);
                let tile = (u32::from(s.name) & !align)
                    + (src_x >> 3)
                    + (ty & 1) * 0x10
                    + (ty >> 1) * 0x20;
                let addr =
                    base_words + tile * 16 + py * 2 + if s.name >= 0xE0 { gap_words } else { 0 };
                let bit = 7 - (src_x & 7);
                let p0 = self.vram_byte(addr, true);
                let p1 = self.vram_byte(addr, false);
                let p2 = self.vram_byte(addr + 8, true);
                let p3 = self.vram_byte(addr + 8, false);
                let v = ((p3 >> bit) & 1) << 3
                    | ((p2 >> bit) & 1) << 2
                    | ((p1 >> bit) & 1) << 1
                    | (p0 >> bit) & 1;
                if v == 0 {
                    continue;
                }
                // Sprite palettes are the upper eight: entry
                // 0x80 plus palette bits 4..7 of the attribute.
                let entry = 0x80 + ((u16::from(s.attr & 0x7)) << 4) + u16::from(v);
                let col = 2 * px as usize;
                let prio = (s.attr >> 4) & 3;
                for c in [col, col + 1] {
                    if !self.obj_on[c] || prio > self.obj_prio[c] {
                        self.obj_on[c] = true;
                        self.obj_val[c] = entry;
                        self.obj_prio[c] = prio;
                    }
                }
            }
        }
    }

    /// Compose one visible scanline into the screen rows `line*2` and
    /// `line*2+1`.
    fn compose_line(&mut self, line: u16) {
        let row = usize::from(line) * 2;
        if row + 2 > HEIGHT {
            return;
        }
        if self.inidisp & 0x80 != 0 {
            self.screen[row * WIDTH * 3..(row + 2) * WIDTH * 3].fill(0);
            return;
        }
        self.eval_sprites(line);
        let line_u32 = u32::from(line);
        let mode = usize::from(self.bgmode & 7);
        let hires = matches!(mode, 5 | 6) || self.setini & 0x08 != 0;
        let mut out = [0u8; WIDTH * 3];
        for col in 0..WIDTH {
            let rgb = if hires && mode < 5 {
                // Pseudo-hires: the main screen on the even columns and
                // the subscreen on the odd columns.
                if col & 1 == 0 {
                    let (entry, layer) = self.screen_pixel(col, line_u32, self.tm);
                    self.color_out(col, line_u32, entry, layer)
                } else {
                    let (s_entry, _) = self.screen_pixel(col, line_u32, self.ts);
                    Self::expand555(self.cg_channels(s_entry))
                }
            } else {
                let (entry, layer) = self.screen_pixel(col, line_u32, self.tm);
                self.color_out(col, line_u32, entry, layer)
            };
            out[col * 3..col * 3 + 3].copy_from_slice(&rgb);
        }
        let span = WIDTH * 3;
        self.screen[row * span..(row + 1) * span].copy_from_slice(&out);
        self.screen[(row + 1) * span..(row + 2) * span].copy_from_slice(&out);
    }

    /// One line visit: notify triggers, drive the NMI level, paint the
    /// line, advance the counter, and commit the frame at the wrap.
    fn line_walk(&mut self, tick: Tick, ctx: &mut MachineCtx) {
        let line = self.line;
        ctx.emit(TriggerEvent::Scanline(u32::from(line)));
        ctx.notify("scanline", Level::High, tick);
        if line == self.vblank_line {
            ctx.notify("nmi", Level::High, tick);
        } else if line == 0 {
            ctx.notify("nmi", Level::Low, tick);
        }
        if line < self.vblank_line && usize::from(line) * 2 < HEIGHT {
            self.compose_line(line);
        }
        self.line = (line + 1) % self.lines;
        if self.line == 0 {
            self.frames += 1;
            self.cached_frame.pixels.clone_from(&self.screen);
            ctx.emit(TriggerEvent::Vblank);
        }
    }
}

impl BusAccess for SnesPpu {
    fn read(&mut self, _width: AccessWidth, address: Address, tick: Tick) -> u32 {
        let b = address.0 & 0x1FF;
        if (0x100..=0x13F).contains(&b) {
            return u32::from(self.reg_read((b - 0x100) as u8, tick));
        }
        0xFF
    }

    fn write(&mut self, _width: AccessWidth, address: Address, value: u32, _tick: Tick) {
        let b = address.0 & 0x1FF;
        if (0x100..=0x135).contains(&b) {
            self.reg_write((b - 0x100) as u8, value as u8);
        }
    }

    fn peek(&self, address: Address) -> u8 {
        let b = address.0 & 0x1FF;
        if (0x100..=0x13F).contains(&b) {
            self.peek_reg((b - 0x100) as u8)
        } else {
            0xFF
        }
    }
}

impl VideoSource for SnesPpu {
    fn frame(&self) -> Option<&Frame> {
        Some(&self.cached_frame)
    }

    fn dump_layer(&self, layer: VideoLayer) -> Option<Vec<u8>> {
        match layer {
            VideoLayer::Vram => Some(self.vram.clone()),
            VideoLayer::NameTable => {
                // The BG1 tilemap region, word by word through the
                // wrap-aware word read.
                let sc = self.bg_sc[0];
                let cols = if sc & 0x01 != 0 { 64 } else { 32 };
                let rows = if sc & 0x02 != 0 { 64 } else { 32 };
                let base = u32::from(sc >> 2) << 10;
                let mut out = Vec::with_capacity(cols as usize * rows as usize * 2);
                for row in 0..rows {
                    for col in 0..cols {
                        let word = self.vram_word(base + row * 32 + col);
                        out.extend_from_slice(&word.to_le_bytes());
                    }
                }
                Some(out)
            }
            VideoLayer::Oam => Some(self.oam.clone()),
            VideoLayer::Palette => Some(self.cgram.clone()),
            VideoLayer::Framebuffer => Some(self.screen.clone()),
        }
    }

    fn frames(&self) -> u64 {
        self.frames
    }
}

impl Device for SnesPpu {
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
        self.inidisp = 0x80;
        self.objsel = 0;
        self.bgmode = 0;
        self.mosaic = 0;
        self.bg_sc = [0; 4];
        self.nba = [0; 2];
        self.bg_hofs = [0; 4];
        self.bg_vofs = [0; 4];
        self.m7h = 0;
        self.m7v = 0;
        self.vmain = 0;
        self.vaddr = 0;
        self.vram.iter_mut().for_each(|b| *b = 0);
        self.vbuffer = 0;
        self.vread_low = true;
        self.m7sel = 0;
        self.m7 = [0; 4];
        self.m7b_mult = 0;
        self.m7x = 0;
        self.m7y = 0;
        self.bgofs_latch = 0;
        self.bghofs_latch = 0;
        self.mode7_latch = 0;
        self.scroll_prev = None;
        self.m7_prev = None;
        self.cgadd = 0;
        self.cgram.iter_mut().for_each(|b| *b = 0);
        self.cg_latch = 0;
        self.wsel = [0; 3];
        self.wpos = [0; 4];
        self.wlog = [0; 2];
        self.tm = 0;
        self.ts = 0;
        self.tmw = 0;
        self.tsw = 0;
        self.cgwsel = 0;
        self.cgadsub = 0;
        self.fixed = 0;
        self.setini = 0;
        self.oam.iter_mut().for_each(|b| *b = 0);
        self.oamadd = 0;
        self.oamaddr = 0;
        self.oam_latch = 0;
        self.line = 0;
        self.frames = 0;
        self.latch_h = 0;
        self.latch_v = 0;
        self.hbyte = false;
        self.vbyte = false;
        self.counter_latch = false;
        self.time_over = false;
        self.next = None;
        self.last_tick = None;
    }

    fn boot(&mut self, state: Option<&BootState>) {
        let _ = state;
        self.reset();
        self.line_due = 0;
        self.next = Some(Tick(0));
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(SNAP_FIXED + VRAM_BYTES + CGRAM_BYTES + OAM_BYTES);
        out.extend_from_slice(b"SNPP");
        out.push(1);
        for f in [
            self.inidisp,
            self.objsel,
            self.bgmode,
            self.mosaic,
            self.vmain,
            self.m7sel,
            self.tm,
            self.ts,
            self.tmw,
            self.tsw,
            self.cgwsel,
            self.cgadsub,
            self.setini,
        ] {
            out.push(f);
        }
        out.extend_from_slice(&self.bg_sc);
        out.extend_from_slice(&self.nba);
        for v in self.bg_hofs.iter().chain(self.bg_vofs.iter()) {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&self.m7h.to_le_bytes());
        out.extend_from_slice(&self.m7v.to_le_bytes());
        out.extend_from_slice(&self.vaddr.to_le_bytes());
        out.extend_from_slice(&self.vbuffer.to_le_bytes());
        out.push(u8::from(self.vread_low));
        out.push(self.m7b_mult as u8);
        for v in &self.m7 {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&self.m7x.to_le_bytes());
        out.extend_from_slice(&self.m7y.to_le_bytes());
        out.push(self.bgofs_latch);
        out.push(self.bghofs_latch);
        out.push(self.mode7_latch);
        out.push(self.scroll_prev.unwrap_or(0xFF));
        out.push(self.m7_prev.unwrap_or(0xFF));
        out.extend_from_slice(&self.wsel);
        out.extend_from_slice(&self.wpos);
        out.extend_from_slice(&self.wlog);
        out.extend_from_slice(&self.fixed.to_le_bytes());
        out.extend_from_slice(&self.cgadd.to_le_bytes());
        out.push(self.cg_latch);
        out.extend_from_slice(&self.oamadd.to_le_bytes());
        out.extend_from_slice(&(self.oamaddr as u16).to_le_bytes());
        out.push(self.oam_latch);
        out.extend_from_slice(&self.line.to_le_bytes());
        out.extend_from_slice(&self.line_due.to_le_bytes());
        out.extend_from_slice(&self.frames.to_le_bytes());
        out.extend_from_slice(&self.latch_h.to_le_bytes());
        out.extend_from_slice(&self.latch_v.to_le_bytes());
        out.push(u8::from(self.hbyte));
        out.push(u8::from(self.vbyte));
        out.push(u8::from(self.counter_latch));
        out.push(u8::from(self.time_over));
        out.extend_from_slice(&self.vram);
        out.extend_from_slice(&self.cgram);
        out.extend_from_slice(&self.oam);
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        let expected = SNAP_FIXED + VRAM_BYTES + CGRAM_BYTES + OAM_BYTES;
        if data.len() != expected || &data[..4] != b"SNPP" || data[4] != 1 {
            return Err(SnapshotError::Mismatch(format!(
                "snes ppu chunk must be {expected} bytes"
            )));
        }
        let mut pos = 5usize;
        for f in [
            &mut self.inidisp,
            &mut self.objsel,
            &mut self.bgmode,
            &mut self.mosaic,
            &mut self.vmain,
            &mut self.m7sel,
            &mut self.tm,
            &mut self.ts,
            &mut self.tmw,
            &mut self.tsw,
            &mut self.cgwsel,
            &mut self.cgadsub,
            &mut self.setini,
        ] {
            *f = data[pos];
            pos += 1;
        }
        self.bg_sc.copy_from_slice(&data[pos..pos + 4]);
        pos += 4;
        self.nba.copy_from_slice(&data[pos..pos + 2]);
        pos += 2;
        for v in self.bg_hofs.iter_mut().chain(self.bg_vofs.iter_mut()) {
            *v = u16::from_le_bytes(data[pos..pos + 2].try_into().unwrap_or([0; 2]));
            pos += 2;
        }
        self.m7h = u16::from_le_bytes(data[pos..pos + 2].try_into().unwrap_or([0; 2]));
        pos += 2;
        self.m7v = u16::from_le_bytes(data[pos..pos + 2].try_into().unwrap_or([0; 2]));
        pos += 2;
        self.vaddr = u16::from_le_bytes(data[pos..pos + 2].try_into().unwrap_or([0; 2])) & 0x7FFF;
        pos += 2;
        self.vbuffer = u16::from_le_bytes(data[pos..pos + 2].try_into().unwrap_or([0; 2]));
        pos += 2;
        self.vread_low = data[pos] != 0;
        pos += 1;
        self.m7b_mult = data[pos] as i8;
        pos += 1;
        for v in self.m7.iter_mut() {
            *v = u16::from_le_bytes(data[pos..pos + 2].try_into().unwrap_or([0; 2]));
            pos += 2;
        }
        self.m7x = u16::from_le_bytes(data[pos..pos + 2].try_into().unwrap_or([0; 2])) & 0x1FFF;
        pos += 2;
        self.m7y = u16::from_le_bytes(data[pos..pos + 2].try_into().unwrap_or([0; 2])) & 0x1FFF;
        pos += 2;
        self.bgofs_latch = data[pos];
        pos += 1;
        self.bghofs_latch = data[pos];
        pos += 1;
        self.mode7_latch = data[pos];
        pos += 1;
        let sp = data[pos];
        self.scroll_prev = if sp == 0xFF { None } else { Some(sp & 7) };
        pos += 1;
        let mp = data[pos];
        self.m7_prev = if mp == 0xFF { None } else { Some(mp & 7) };
        pos += 1;
        self.wsel.copy_from_slice(&data[pos..pos + 3]);
        pos += 3;
        self.wpos.copy_from_slice(&data[pos..pos + 4]);
        pos += 4;
        self.wlog.copy_from_slice(&data[pos..pos + 2]);
        pos += 2;
        self.fixed = u16::from_le_bytes(data[pos..pos + 2].try_into().unwrap_or([0; 2])) & 0x7FFF;
        pos += 2;
        self.cgadd = u16::from_le_bytes(data[pos..pos + 2].try_into().unwrap_or([0; 2])) & 0x1FF;
        pos += 2;
        self.cg_latch = data[pos];
        pos += 1;
        self.oamadd = u16::from_le_bytes(data[pos..pos + 2].try_into().unwrap_or([0; 2]));
        pos += 2;
        self.oamaddr = usize::from(u16::from_le_bytes(
            data[pos..pos + 2].try_into().unwrap_or([0; 2]),
        )) % OAM_BYTES;
        pos += 2;
        self.oam_latch = data[pos];
        pos += 1;
        self.line =
            u16::from_le_bytes(data[pos..pos + 2].try_into().unwrap_or([0; 2])) % self.lines;
        pos += 2;
        self.line_due = u64::from_le_bytes(data[pos..pos + 8].try_into().unwrap_or([0; 8]));
        pos += 8;
        self.frames = u64::from_le_bytes(data[pos..pos + 8].try_into().unwrap_or([0; 8]));
        pos += 8;
        self.latch_h = u16::from_le_bytes(data[pos..pos + 2].try_into().unwrap_or([0; 2]));
        pos += 2;
        self.latch_v = u16::from_le_bytes(data[pos..pos + 2].try_into().unwrap_or([0; 2]));
        pos += 2;
        self.hbyte = data[pos] != 0;
        pos += 1;
        self.vbyte = data[pos] != 0;
        pos += 1;
        self.counter_latch = data[pos] != 0;
        pos += 1;
        self.time_over = data[pos] != 0;
        pos += 1;
        self.vram.copy_from_slice(&data[pos..pos + VRAM_BYTES]);
        pos += VRAM_BYTES;
        self.cgram.copy_from_slice(&data[pos..pos + CGRAM_BYTES]);
        pos += CGRAM_BYTES;
        self.oam.copy_from_slice(&data[pos..pos + OAM_BYTES]);
        self.next = None;
        self.last_tick = None;
        Ok(())
    }

    fn notify_line(&mut self, _line: &str, _level: Level, _tick: Tick) {
        // No signal lines enter the S-PPU pair.
    }

    fn next_event(&self) -> Option<Tick> {
        self.next
    }

    fn tick(&mut self, tick: Tick, ctx: &mut MachineCtx) {
        if self.last_tick.is_some_and(|t| t.0 >= tick.0) {
            return;
        }
        self.last_tick = Some(tick);
        if tick.0 >= self.line_due {
            self.line_walk(tick, ctx);
            self.line_due += self.line_div;
        }
        self.next = Some(Tick(self.line_due));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn instance(pal: bool) -> demu_config::model::DeviceInstance {
        let mut dividers = BTreeMap::new();
        dividers.insert("dot".to_string(), 4u32);
        let mut params = BTreeMap::new();
        if pal {
            params.insert("pal".to_string(), toml::Value::Boolean(true));
        }
        demu_config::model::DeviceInstance {
            name: demu_config::newtypes::DeviceName::new("snesppu").expect("name"),
            kind: demu_config::newtypes::KindName::new("snes.ppu").expect("kind"),
            domain: "main".to_string(),
            params,
            dividers,
        }
    }

    fn ppu() -> SnesPpu {
        SnesPpu::new(&instance(false))
    }

    fn wr(v: &mut SnesPpu, raw: u32, val: u8) {
        v.write(AccessWidth::Byte, Address(raw), u32::from(val), Tick(0));
    }

    fn rd(v: &mut SnesPpu, raw: u32) -> u8 {
        v.read(AccessWidth::Byte, Address(raw), Tick(0)) as u8
    }

    /// The pixel bytes at frame column `x` of row `y`.
    fn px(v: &SnesPpu, x: usize, y: usize) -> [u8; 3] {
        let o = (y * WIDTH + x) * 3;
        [v.screen[o], v.screen[o + 1], v.screen[o + 2]]
    }

    #[test]
    fn registers_decode_mirror_and_readback() {
        let mut v = ppu();
        wr(&mut v, 0x2105, 0x0B);
        assert_eq!(v.bgmode, 0x0B);
        assert_eq!(rd(&mut v, 0x2105), 0x0B);
        // A high-bank copy lands on the same register through raw & 0x1FF.
        wr(&mut v, 0x402105, 0x03);
        assert_eq!(v.bgmode, 0x03);
        // The offsets outside $100-$13F read open bus and ignore writes.
        assert_eq!(rd(&mut v, 0x420000), 0xFF);
        wr(&mut v, 0x420000, 0xFF);
        // The read-only group ignores writes.
        wr(&mut v, 0x213E, 0xFF);
        assert_eq!(rd(&mut v, 0x213E), 0x00);
        // The control bytes read back.
        wr(&mut v, 0x2100, 0x0F);
        assert_eq!(rd(&mut v, 0x2100), 0x0F);
        wr(&mut v, 0x2116, 0x34);
        wr(&mut v, 0x2117, 0x12);
        assert_eq!(rd(&mut v, 0x2116), 0x34);
        assert_eq!(rd(&mut v, 0x2117), 0x12);
        // The scroll write ports read open bus.
        assert_eq!(rd(&mut v, 0x210D), 0xFF);
    }

    #[test]
    fn vram_ports_write_read_prefetch_and_step() {
        let mut v = ppu();
        // The step of one word with the increment after the high write.
        wr(&mut v, 0x2115, 0x80);
        wr(&mut v, 0x2116, 0x00);
        wr(&mut v, 0x2117, 0x10);
        wr(&mut v, 0x2118, 0x34);
        assert_eq!(v.vram[0x2000], 0x34);
        assert_eq!(v.vaddr, 0x1000, "the low write does not step");
        wr(&mut v, 0x2119, 0x12);
        assert_eq!(v.vram[0x2001], 0x12);
        assert_eq!(v.vaddr, 0x1001, "the high write steps once");
        // Reads: the pair returns the word at the set address, then the
        // next pair the following word.
        wr(&mut v, 0x2116, 0x00);
        wr(&mut v, 0x2117, 0x10);
        assert_eq!(v.vbuffer, 0x1234);
        assert_eq!(rd(&mut v, 0x2139), 0x34);
        assert_eq!(rd(&mut v, 0x213A), 0x12);
        assert_eq!(rd(&mut v, 0x2139), v.vram[0x2002]);
        assert_eq!(v.vaddr, 0x1001);
        // The 32-word step form.
        wr(&mut v, 0x2115, 0x81);
        wr(&mut v, 0x2116, 0x00);
        wr(&mut v, 0x2117, 0x10);
        wr(&mut v, 0x2118, 0x55);
        wr(&mut v, 0x2119, 0x66);
        assert_eq!(v.vaddr, 0x1020);
    }

    #[test]
    fn cgram_write_read_and_rgb_decode() {
        let mut v = ppu();
        wr(&mut v, 0x2121, 0x0F);
        wr(&mut v, 0x2122, 0x1F);
        wr(&mut v, 0x2122, 0x7C);
        assert_eq!(v.cgram[30], 0x1F);
        assert_eq!(v.cgram[31], 0x7C);
        assert_eq!(v.cgadd, 0x20, "two writes step to the next word");
        let channels = v.cg_channels(0x0F);
        assert_eq!(channels, [31, 0, 31], "BGR555 channel split");
        assert_eq!(SnesPpu::expand555([31, 16, 1]), [255, 132, 8]);
        // Reads alternate the halves and advance with them.
        wr(&mut v, 0x2121, 0x0F);
        assert_eq!(v.cgadd, 0x1E);
        assert_eq!(rd(&mut v, 0x213B), 0x1F);
        assert_eq!(rd(&mut v, 0x213B), 0x7C);
        assert_eq!(v.cgadd, 0x20);
    }

    #[test]
    fn oam_two_stage_latch_commit_and_upper_region() {
        let mut v = ppu();
        // The pairs below 0x200: the even write latches, the odd commits.
        wr(&mut v, 0x2104, 0x20);
        wr(&mut v, 0x2104, 0x08);
        wr(&mut v, 0x2104, 0x04);
        wr(&mut v, 0x2104, 0x10);
        assert_eq!(&v.oam[0..4], &[0x20, 0x08, 0x04, 0x10]);
        assert_eq!(v.oamaddr, 4);
        // The upper attribute region applies every byte immediately.
        wr(&mut v, 0x2103, 0x01);
        assert_eq!(v.oamaddr, 0x200);
        wr(&mut v, 0x2104, 0x03);
        assert_eq!(v.oam[0x200], 0x03);
        // Reads walk the bytes in order.
        wr(&mut v, 0x2102, 0x00);
        wr(&mut v, 0x2103, 0x00);
        assert_eq!(rd(&mut v, 0x2138), 0x20);
        assert_eq!(rd(&mut v, 0x2138), 0x08);
        assert_eq!(rd(&mut v, 0x2138), 0x04);
    }

    #[test]
    fn scroll_pairs_share_latch_and_reroute_second_write() {
        let mut v = ppu();
        // Two writes to $210D compose the 10-bit 0x234 and also the
        // wider mode 7 form.
        wr(&mut v, 0x210D, 0x34);
        assert_eq!(v.bg_hofs[0], 0, "the first write only latches");
        wr(&mut v, 0x210D, 0x02);
        assert_eq!(v.bg_hofs[0], 0x234);
        assert_eq!(v.m7h, 0x0234);
        // The vertical pair.
        wr(&mut v, 0x210E, 0x56);
        wr(&mut v, 0x210E, 0x01);
        assert_eq!(v.bg_vofs[0], 0x156);
        // The second byte of a pair lands in the pair's register even
        // when the bus address advanced to the next register.
        wr(&mut v, 0x210F, 0x78);
        assert_eq!(v.bg_hofs[1], 0, "the write only armed the pair");
        wr(&mut v, 0x2110, 0x02);
        assert_eq!(v.bg_hofs[1], 0x278, "the second write rerouted to BG2HOFS");
        assert_eq!(v.bg_vofs[1], 0, "BG2VOFS untouched");
    }

    #[test]
    fn m7_matrix_pair_reroute_and_product_register() {
        let mut v = ppu();
        // The pair to M7A completes although the second byte's bus
        // address is $211C.
        wr(&mut v, 0x211B, 0x34);
        wr(&mut v, 0x211C, 0x0A);
        assert_eq!(v.m7[0], 0x0A34);
        // A single 8-bit multiplier write records the factor.
        wr(&mut v, 0x211C, 0x05);
        assert_eq!(v.m7b_mult, 5);
        assert_eq!(rd(&mut v, 0x2134), 0x04);
        assert_eq!(rd(&mut v, 0x2135), 0x33);
        assert_eq!(rd(&mut v, 0x2136), 0x00);
        // Complete the armed M7B pair to disarm the reroute.
        wr(&mut v, 0x211C, 0x00);
        // The M7A pair with -1 lands through two $211B writes.
        wr(&mut v, 0x211B, 0xFF);
        wr(&mut v, 0x211B, 0xFF);
        assert_eq!(v.m7[0], 0xFFFF);
        // The factor -3: the product is 3.
        wr(&mut v, 0x211C, 0xFD);
        assert_eq!(rd(&mut v, 0x2134), 0x03);
        assert_eq!(rd(&mut v, 0x2135), 0x00);
        assert_eq!(rd(&mut v, 0x2136), 0x00);
    }

    #[test]
    fn mode1_composes_map_tile_and_palette() {
        let mut v = ppu();
        wr(&mut v, 0x2100, 0x0F);
        wr(&mut v, 0x2105, 0x01);
        wr(&mut v, 0x2107, 0x00);
        wr(&mut v, 0x210B, 0x01);
        wr(&mut v, 0x212C, 0x01);
        // The BG1 map word 0: tile 1, palette 2, low priority.
        v.vram[0] = 0x01;
        v.vram[1] = 0x08;
        // The tile 1 rows 0 and 5: value 3 in the leftmost pixel.
        v.vram[0x2020] = 0x80;
        v.vram[0x2021] = 0x80;
        v.vram[0x2034] = 0x80;
        v.vram[0x2035] = 0x80;
        // Palette 2 color 3 = entry 35: full red.
        v.cgram[70] = 0x1F;
        v.cgram[71] = 0x00;
        v.compose_line(5);
        assert_eq!(px(&v, 0, 10), [255, 0, 0]);
        assert_eq!(px(&v, 1, 10), [255, 0, 0], "the logical pixel doubles");
        // The transparent pixel 1 falls to the backdrop.
        assert_eq!(px(&v, 2, 10), [0, 0, 0]);
    }

    #[test]
    fn mode0_layers_map_palette_strides() {
        let mut v = ppu();
        wr(&mut v, 0x2100, 0x0F);
        wr(&mut v, 0x2105, 0x00);
        wr(&mut v, 0x2108, 0x04); // The BG2 map at words 0x400.
        wr(&mut v, 0x212C, 0x02);
        v.vram[0x800] = 0x02;
        v.vram[0x801] = 0x00;
        // The BG2 tile 0's 2bpp rows: plane bytes 32..48.
        for i in (32..48).step_by(2) {
            v.vram[i] = 0xC0;
            v.vram[i + 1] = 0x40;
        }
        // BG2 palettes live at entries 32-63 in the mode 0 layout.
        v.cgram[66] = 0x03;
        v.cgram[67] = 0x00;
        v.compose_line(1);
        assert_eq!(px(&v, 0, 2), [24, 0, 0]);
    }

    #[test]
    fn obj_priority_against_background_layers() {
        let mut v = ppu();
        wr(&mut v, 0x2100, 0x0F);
        wr(&mut v, 0x2105, 0x01);
        wr(&mut v, 0x2107, 0x00);
        wr(&mut v, 0x210B, 0x02);
        wr(&mut v, 0x212C, 0x11);
        // The BG1 map: tile 1, palette 2, high priority.
        v.vram[0] = 0x01;
        v.vram[1] = 0x28;
        // The BG tile at the words 0x2010 and the object tile at 0x10.
        v.vram[0x4020] = 0x80;
        v.vram[0x4021] = 0x80;
        v.vram[32] = 0x80;
        v.vram[33] = 0x80;
        v.oam[0] = 0x00;
        v.oam[1] = 0x00;
        v.oam[2] = 0x01;
        v.oam[3] = 0x11;
        // The BG palette 2 color 3 = entry 35 (red) and the object
        // palette 1 color 3 = entry 0x93.
        v.cgram[70] = 0x1F;
        v.cgram[71] = 0x00;
        v.cgram[294] = 0x03;
        v.cgram[295] = 0x7C;
        v.compose_line(0);
        assert_eq!(px(&v, 0, 0), [255, 0, 0], "BG1 high priority wins");
        assert_eq!(px(&v, 1, 0), [255, 0, 0]);
        // With BG1 off the main screen the object pixel surfaces.
        wr(&mut v, 0x212C, 0x10);
        v.compose_line(0);
        assert_eq!(px(&v, 0, 0), [24, 0, 255]);
    }

    #[test]
    fn inidisp_blank_black_and_brightness() {
        let mut v = ppu();
        wr(&mut v, 0x2105, 0x01);
        wr(&mut v, 0x210B, 0x01);
        wr(&mut v, 0x212C, 0x01);
        v.vram[0] = 0x01;
        v.vram[1] = 0x08;
        v.vram[64] = 0x01;
        v.vram[65] = 0x08;
        // The compose at line 10 samples py = 2 of the character: the
        // row's plane byte pair sits at 0x2028/0x2029.
        v.vram[0x2028] = 0x80;
        v.vram[0x2029] = 0x80;
        v.cgram[70] = 0x1F;
        v.cgram[71] = 0x00;
        wr(&mut v, 0x2100, 0x0F);
        v.compose_line(10);
        assert_eq!(px(&v, 0, 20), [255, 0, 0]);
        // Force blank paints the whole line black.
        wr(&mut v, 0x2100, 0x80);
        v.compose_line(10);
        assert_eq!(px(&v, 0, 20), [0, 0, 0], "force blank paints black");
        // Brightness 8 of 15 scales the channels.
        wr(&mut v, 0x2100, 0x08);
        v.compose_line(10);
        assert_eq!(px(&v, 0, 20), [136, 0, 0], "brightness 8 of 15");
    }

    #[test]
    fn mode7_identity_and_scale_transform() {
        let mut v = ppu();
        wr(&mut v, 0x2100, 0x0F);
        wr(&mut v, 0x2105, 0x07);
        wr(&mut v, 0x212C, 0x01);
        // The identity: A and D hold 1.0 in the 8.8 form.
        wr(&mut v, 0x211B, 0x00);
        wr(&mut v, 0x211B, 0x01);
        wr(&mut v, 0x211E, 0x00);
        wr(&mut v, 0x211E, 0x01);
        assert_eq!(v.m7[0], 0x0100);
        assert_eq!(v.m7[3], 0x0100);
        // The map entry (0,0) = tile 1 in the first VRAM half.
        v.vram[0] = 0x01;
        v.vram[1] = 0x00;
        // The tile 1 occupies words 0x4040-0x407f; the texel (3,2) is
        // word 0x4040 + 2*8 + 3 = 0x4053.
        v.vram[0x80A6] = 5;
        // The palette entry 5: pure blue rides the high byte.
        v.cgram[10] = 0x00;
        v.cgram[11] = 0x7C;
        v.compose_line(2);
        assert_eq!(px(&v, 6, 4), [0, 0, 255], "the identity transform");
        // The scale of two: pixel (3,2) samples texel (6,2), word 0x4056.
        wr(&mut v, 0x211B, 0x00);
        wr(&mut v, 0x211B, 0x02);
        v.vram[0x80AC] = 6;
        // The palette entry 6: mid green.
        v.cgram[12] = 0x60;
        v.cgram[13] = 0x00;
        v.compose_line(2);
        assert_eq!(px(&v, 6, 4), [0, 24, 0], "the 2x scale transform");
    }

    #[test]
    fn object_limit_latches_time_over() {
        let mut v = ppu();
        wr(&mut v, 0x2100, 0x0F);
        wr(&mut v, 0x212C, 0x10);
        // The tile 0 with every pixel at color 1.
        for b in &mut v.vram[0..32] {
            *b = 0xFF;
        }
        v.cgram[128] = 0x13;
        v.cgram[129] = 0x00;
        for i in 0..36usize {
            v.oam[i * 4] = 0x00;
            v.oam[i * 4 + 1] = (i as u8) * 7;
            v.oam[i * 4 + 2] = 0x00;
            v.oam[i * 4 + 3] = 0x00;
        }
        v.compose_line(0);
        assert!(v.time_over, "36 sprites latch the time-over flag");
        assert_eq!(rd(&mut v, 0x213E), 0x80);
        // The objects sit 7 pixels apart, 8 wide. Objects 0..33 paint;
        // the 35th (x 238) and the 36th (x 245) stay invisible, so
        // their exclusive pixels at 242 and 250 never turn on.
        assert!(v.obj_on[2 * 231], "the 34th object painted");
        assert!(!v.obj_on[2 * 242], "the 35th object dropped");
        assert!(!v.obj_on[2 * 250], "the 36th object dropped");
        // Fewer objects clear the flag again: objects 2..127 leave the
        // line (a zeroed OAM puts every object at y 0, which would
        // still be found).
        for i in 2..128usize {
            v.oam[i * 4] = 200;
        }
        v.compose_line(0);
        assert!(!v.time_over);
        assert_eq!(rd(&mut v, 0x213E), 0x00);
    }

    #[test]
    fn counter_reads_toggle_bytes_and_stat78_clears() {
        let mut v = ppu();
        assert_eq!(rd(&mut v, 0x213F), 0x23, "bit 5 set, version 3, NTSC");
        assert_eq!(rd(&mut v, 0x213E), 0x00);
        // The $2137 strobe sets the counter-latch flag; $213F clears it.
        let _ = rd(&mut v, 0x2137);
        assert_eq!(rd(&mut v, 0x213F), 0x63);
        assert_eq!(rd(&mut v, 0x213F), 0x23, "the read cleared the latch flag");
        // The counter reads alternate the low and high bytes.
        v.line = 0x123;
        let _ = rd(&mut v, 0x2137);
        v.latch_v = v.line;
        assert_eq!(rd(&mut v, 0x213D), 0x23);
        assert_eq!(rd(&mut v, 0x213D), 0x01);
    }

    #[test]
    fn color_math_add_sub_and_subscreen_addend() {
        let mut v = ppu();
        wr(&mut v, 0x2100, 0x0F);
        wr(&mut v, 0x2105, 0x01);
        wr(&mut v, 0x2107, 0x00);
        wr(&mut v, 0x210B, 0x01);
        wr(&mut v, 0x212C, 0x01);
        // The BG1 map: the tile 1, the palette 2; the value 1 at the px 0.
        v.vram[0] = 0x01;
        v.vram[1] = 0x08;
        v.vram[0x2020] = 0xFF;
        v.vram[0x2021] = 0x00;
        // The main color: full red at the palette 2's entry 33.
        v.cgram[66] = 0x1F;
        v.cgram[67] = 0x00;
        // The fixed color: full blue.
        wr(&mut v, 0x2132, 0x80 | 0x1F);
        assert_eq!(v.fixed, 0x7C00);
        // The add with the fixed color: the red and the blue.
        wr(&mut v, 0x2131, 0x01);
        v.compose_line(0);
        assert_eq!(px(&v, 0, 0), [255, 0, 255]);
        // The subtract: the red stays, the blue clamps to zero.
        wr(&mut v, 0x2131, 0x81);
        v.compose_line(0);
        assert_eq!(px(&v, 0, 0), [255, 0, 0]);
        // The halved add halves every channel: red joins at half strength.
        wr(&mut v, 0x2131, 0x40 | 0x01);
        v.compose_line(0);
        assert_eq!(px(&v, 0, 0), [123, 0, 123]);
        // The subscreen addend: BG2 on the subscreen supplies green.
        wr(&mut v, 0x2131, 0x01);
        wr(&mut v, 0x2130, 0x01);
        wr(&mut v, 0x2108, 0x40); // The BG2 map at the words 0x4000.
        wr(&mut v, 0x212D, 0x02);
        v.vram[0x8000] = 0x01;
        v.vram[0x8001] = 0x00; // The tile 1, the palette 0.
        v.vram[0x20] = 0x80;
        v.vram[0x21] = 0xC0; // The BG2's tile 1: the value 3.
        v.cgram[6] = 0xE0;
        v.cgram[7] = 0x03; // The entry 3 = pure green.
        v.compose_line(0);
        assert_eq!(
            px(&v, 0, 0),
            [255, 255, 0],
            "the subscreen addend blends red+green"
        );
    }

    #[test]
    fn mosaic_holds_background_pixels() {
        let mut v = ppu();
        wr(&mut v, 0x2100, 0x0F);
        wr(&mut v, 0x2105, 0x01);
        wr(&mut v, 0x2107, 0x00);
        wr(&mut v, 0x210B, 0x01);
        wr(&mut v, 0x212C, 0x01);
        // Four map tiles with a distinct color per tile row.
        for (i, c) in [0x04u8, 0x08, 0x0C, 0x10].iter().enumerate() {
            let w = (i as u32) * 2;
            v.vram[(w * 2) as usize] = *c;
        }
        v.compose_line(0);
        // Mosaic 4 enabled for BG1: all pixels hold the leftmost column's.
        wr(&mut v, 0x2106, 0x13);
        v.compose_line(0);
        for x in 0..WIDTH {
            assert_eq!(px(&v, x, 0), px(&v, 0, 0), "the mosaic holds at {x}");
        }
    }

    #[test]
    fn offset_per_tile_mode2_shifts_row() {
        let mut v = ppu();
        wr(&mut v, 0x2100, 0x0F);
        wr(&mut v, 0x2105, 0x02);
        wr(&mut v, 0x2107, 0x00);
        wr(&mut v, 0x2109, 0x08); // The BG3 map at words 0x800.
        wr(&mut v, 0x212C, 0x01);
        // The BG1 map: the cells (0,0) and (0,1) = the tile 1, palette 2.
        v.vram[0] = 0x01;
        v.vram[1] = 0x08;
        v.vram[64] = 0x01;
        v.vram[65] = 0x08;
        // The BG's tile 1 sits at words 16-31 with the default
        // character base 0: rows 0, 1, and 4 use these byte pairs.
        for f in [0x20usize, 0x24, 0x30] {
            v.vram[f] = 0x80;
            v.vram[f + 1] = 0x80;
        }
        v.cgram[70] = 0x1F;
        v.cgram[71] = 0x00;
        // The optimization entry (0,0): the V addend 0x7f, the 7-bit
        // form of -1, shifts the tilemap row down by one tile.
        v.vram[0x1000] = 0x00;
        v.vram[0x1001] = 0x7F;
        v.compose_line(4);
        assert_eq!(px(&v, 0, 8), [0, 0, 0], "map row 0 shifted to row 31");
        v.compose_line(12);
        assert_eq!(px(&v, 0, 24), [255, 0, 0], "map row 1 shifted to row 0");
    }

    #[test]
    fn dump_layers_cover_vram_oam_palette_frame() {
        let mut v = ppu();
        wr(&mut v, 0x2100, 0x0F);
        v.compose_line(3);
        let vram = v.dump_layer(VideoLayer::Vram).expect("vram");
        assert_eq!(vram.len(), VRAM_BYTES);
        let oam = v.dump_layer(VideoLayer::Oam).expect("oam");
        assert_eq!(oam.len(), OAM_BYTES);
        let pal = v.dump_layer(VideoLayer::Palette).expect("palette");
        assert_eq!(pal.len(), CGRAM_BYTES);
        let nt = v.dump_layer(VideoLayer::NameTable).expect("nametable");
        assert_eq!(nt.len(), 32 * 32 * 2);
        let fb = v.dump_layer(VideoLayer::Framebuffer).expect("framebuffer");
        assert_eq!(fb.len(), WIDTH * HEIGHT * 3);
        assert!(v.dump_layer(VideoLayer::Palette).is_some());
    }

    #[test]
    fn snapshot_round_trips_state() {
        let mut v = ppu();
        wr(&mut v, 0x2100, 0x0F);
        wr(&mut v, 0x2105, 0x03);
        wr(&mut v, 0x211B, 0x34);
        wr(&mut v, 0x211C, 0x12);
        wr(&mut v, 0x2121, 0x20);
        wr(&mut v, 0x2122, 0x11);
        wr(&mut v, 0x2122, 0x22);
        v.time_over = true;
        v.counter_latch = true;
        v.line = 40;
        v.frames = 3;
        let snap = v.snapshot();
        assert_eq!(&snap[..4], b"SNPP");
        let mut twin = ppu();
        twin.restore(&snap).expect("restore");
        assert_eq!(twin.snapshot(), snap, "the round trip is byte-identical");
        assert_eq!(twin.bgmode, 0x03);
        assert_eq!(twin.m7[0], 0x1234);
        assert_eq!(twin.cgram[64], 0x11);
        assert_eq!(twin.frames, 3);
    }

    #[test]
    fn pal_param_changes_lines_and_flag() {
        let mut v = SnesPpu::new(&instance(true));
        assert_eq!(v.lines, 313);
        assert_eq!(v.vblank_line, 274);
        assert_eq!(rd(&mut v, 0x213F), 0x33, "the PAL flag set");
        let mut ntsc = ppu();
        assert_eq!(ntsc.lines, 263);
        assert_eq!(rd(&mut ntsc, 0x213F), 0x23);
    }

    #[derive(Default)]
    struct Probe {
        seen: std::cell::RefCell<Vec<(String, Level)>>,
    }

    impl Device for Probe {
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
        fn next_event(&self) -> Option<Tick> {
            None
        }
        fn tick(&mut self, _tick: Tick, _ctx: &mut MachineCtx) {}
    }

    #[test]
    fn line_walk_emits_scanline_nmi_and_vblank() {
        use demu_config::model::{
            Bus, ClockDomain, Connection, ConnectionDirection, ConnectionSemantics,
        };
        use demu_config::newtypes::{Frequency, LineName, LinePath};

        let mut clocks = BTreeMap::new();
        clocks.insert(
            "main".to_string(),
            ClockDomain {
                name: "main".to_string(),
                frequency: Frequency::new(236250000, 11).expect("frequency"),
                origin: "test".to_string(),
            },
        );
        let devices = vec![
            demu_config::model::DeviceInstance {
                name: demu_config::newtypes::DeviceName::new("snesppu").expect("name"),
                kind: demu_config::newtypes::KindName::new("snes.ppu").expect("kind"),
                domain: "main".to_string(),
                params: BTreeMap::new(),
                dividers: [("dot".to_string(), 4u32)].into_iter().collect(),
            },
            demu_config::model::DeviceInstance {
                name: demu_config::newtypes::DeviceName::new("probe").expect("name"),
                kind: demu_config::newtypes::KindName::new("test.probe").expect("kind"),
                domain: "main".to_string(),
                params: BTreeMap::new(),
                dividers: BTreeMap::new(),
            },
        ];
        let buses = vec![Bus {
            name: "cpu".to_string(),
            address_bits: 24,
            owner: None,
            parent: None,
        }];
        let mut connections = Vec::new();
        for (from_line, to_line) in [("scanline", "line"), ("nmi", "nmi")] {
            connections.push(Connection {
                from: LinePath::new(
                    demu_config::newtypes::DeviceName::new("snesppu").expect("name"),
                    LineName::new(from_line).expect("line"),
                ),
                to: LinePath::new(
                    demu_config::newtypes::DeviceName::new("probe").expect("name"),
                    LineName::new(to_line).expect("line"),
                ),
                semantics: ConnectionSemantics::DisplaySync,
                direction: ConnectionDirection::Forward,
            });
        }
        let config = demu_config::model::SystemConfig {
            name: "snes-ppu-emit-test".to_string(),
            triple: "test".to_string(),
            description: "The S-PPU line-walk emit test.".to_string(),
            clocks,
            devices_by_name: [
                (
                    demu_config::newtypes::DeviceName::new("snesppu").expect("name"),
                    0,
                ),
                (
                    demu_config::newtypes::DeviceName::new("probe").expect("name"),
                    1,
                ),
            ]
            .into_iter()
            .collect(),
            devices,
            buses,
            windows: Vec::new(),
            connections,
            signals: Vec::new(),
            boot: BTreeMap::new(),
            quirks: BTreeMap::new(),
        };
        let mut ctors = demu_core::DeviceCtors::new();
        ctors.register(
            "snes.ppu",
            Box::new(|d, _| Ok(Box::new(SnesPpu::new(d)) as Box<dyn Device>)),
        );
        ctors.register(
            "test.probe",
            Box::new(|_, _| Ok(Box::new(Probe::default()) as Box<dyn Device>)),
        );
        let mut machine = demu_core::Machine::assemble(config, &ctors).expect("assemble");
        machine.boot();

        let events: std::cell::RefCell<Vec<TriggerEvent>> = std::cell::RefCell::new(Vec::new());
        let line = 264 * u64::from(LINE_DOTS) * 4;
        machine.run_hook(
            Tick(line),
            &mut |e| {
                events.borrow_mut().push(e.clone());
                false
            },
            true,
        );

        let seen = events.borrow();
        // One scanline event per visit. The target's tick is itself a
        // due instant, so the walk at the exact target also runs: 265
        // visits, lines 0..=262 and the first line of the next frame.
        let scanlines: Vec<u32> = seen
            .iter()
            .filter_map(|e| match e {
                TriggerEvent::Scanline(n) => Some(*n),
                _ => None,
            })
            .collect();
        assert_eq!(scanlines.len(), 265, "one scanline event per visit");
        assert_eq!(scanlines[0], 0);
        assert_eq!(scanlines[262], 262);
        assert_eq!(scanlines[263], 0, "the wrap restarts the line counter");
        assert_eq!(scanlines[264], 1);
        // The probe received the scanline edge per visit and the NMI
        // level edges: the line 0 low twice, the line 224 high once.
        let probe = {
            let dev = machine.device_by_name("probe").expect("probe device");
            dev.as_any()
                .downcast_ref::<Probe>()
                .expect("probe")
                .seen
                .borrow()
                .len()
        };
        assert_eq!(probe, 265 + 3, "265 scanline edges plus three NMI edges");
        // One vblank per frame walked.
        assert_eq!(
            seen.iter()
                .filter(|e| matches!(**e, TriggerEvent::Vblank))
                .count(),
            1
        );
    }
}
