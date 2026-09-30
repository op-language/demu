//! The VIC-II of the Commodore 64.
//!
//! One device visit covers one whole raster line: the visit advances the
//! raster counter, repaints the visible line, and runs the raster
//! compare. The timing model is line-oriented by design; badline CPU
//! stuns and per-cycle sprite fetches are not modeled, and every per-line
//! fetch lands as one block at the line start through the named fetch
//! bus.
//!
//! Register decode: the register file `$D000-$D02E` decodes by the low
//! six bits of the bus address; the offsets `$2F` and above read `$FF`.
//! The color RAM sits at `$D800-$DBFF` and mirrors through `$DFFF`; the
//! decode masks the offset to ten bits. Sprite coordinates hold in the
//! offset pairs `$00-$0F` with the ninth X bits in `$D010`.
//!
//! Deliberate simplifications of this model:
//!
//! - The visible window is rasters 50 through 249, mapped line for line
//!   over the 25 text rows, and the frame carries those 320x200 pixels
//!   only. When the window shrinks (`RSEL` 0 in `$D011` or `CSEL` 0 in
//!   `$D016`), the whole window paints the border color.
//! - The scroll registers sample the content inside the window: the
//!   vertical scroll selects the tile sub-row with the wrap-around
//!   shift, and the horizontal scroll shifts the sample column without
//!   a 41st cell.
//! - Sprite X uses window coordinates 0..=511 (the MSBs of `$D010`)
//!   clipped at the window edge; the real left-edge margin is not
//!   modeled. A sprite shows from the line after its Y compare.
//! - The chip fetches the video matrix, the char or bitmap bytes, the
//!   sprite pointers, and the sprite data through `fetch_bus`. The
//!   `fetch_base` parameter carries the 16 KiB base of the current VIC
//!   bank; the CIA2 PRA bits that drive VA14/VA15 are not wired here, a
//!   config changes the bank through the parameter.
//! - The light pen registers are readout stubs; the unused register
//!   offsets read `$FF`.
//!
//! Colors convert through one fixed 16-entry palette table, so the
//! compose stays integer-only and byte-deterministic.

use demu_config::model::BootState;
use demu_core::device::{AccessWidth, BusAccess, Frame, MachineCtx, VideoLayer, VideoSource};
use demu_core::frequency::{Address, Tick};
use demu_core::signal::Level;
use demu_core::triggers::TriggerEvent;
use demu_core::{Device, Snapshot, SnapshotError};

/// The visible-frame width in pixels.
const WIDTH: usize = 320;
/// The visible-frame height in pixels.
const HEIGHT: usize = 200;
/// The first raster line of the visible window.
const FIRST_LINE: u16 = 50;
/// The last raster line of the visible window.
const LAST_LINE: u16 = FIRST_LINE + HEIGHT as u16 - 1;
/// The cell height in pixels.
const CELL: usize = 8;
/// The character cells per line.
const COLS: usize = 40;
/// The byte size of the color RAM.
const COLOR_RAM: usize = 0x400;
/// The used register bytes of the file.
const REG_BYTES: usize = 0x2F;

/// The sprite X/Y coordinate pairs start here: `$D000-$D00F`.
const REG_SPR_XY: usize = 0x00;
/// The sprite X MSBs: `$D010`.
const REG_SPR_MSB: usize = 0x10;
/// The control byte: raster MSB, ECM, BMM, DEN, RSEL, y-scroll: `$D011`.
const REG_CTRL: usize = 0x11;
/// The raster compare low byte and the raster readback: `$D012`.
const REG_RASTER: usize = 0x12;
/// The light pen X value: `$D013`.
const REG_LPX: usize = 0x13;
/// The light pen Y value: `$D014`.
const REG_LPY: usize = 0x14;
/// The sprite enable byte: `$D015`.
const REG_SPR_EN: usize = 0x15;
/// The scroll and mode byte: x-scroll, CSEL, MCM: `$D016`.
const REG_SCROLL: usize = 0x16;
/// The sprite Y-expand byte: `$D017`.
const REG_SPR_YEXP: usize = 0x17;
/// The video matrix and char base byte: `$D018`.
const REG_BASE: usize = 0x18;
/// The interrupt flags: `$D019`.
const REG_IRQFLAGS: usize = 0x19;
/// The interrupt mask: `$D01A`.
const REG_IRQMASK: usize = 0x1A;
/// The sprite behind-foreground byte: `$D01B`.
const REG_BG_PRI: usize = 0x1B;
/// The per-sprite multicolor byte: `$D01C`.
const REG_SPR_MCM: usize = 0x1C;
/// The sprite X-expand byte: `$D01D`.
const REG_SPR_XEXP: usize = 0x1D;
/// The sprite-sprite collision latch: `$D01E`.
const REG_SPR_SPR: usize = 0x1E;
/// The sprite-data collision latch: `$D01F`.
const REG_SPR_BG: usize = 0x1F;
/// The border color: `$D020`.
const REG_BORDER: usize = 0x20;
/// The four background colors: `$D021-$D024`.
const REG_BG: usize = 0x21;
/// The sprite multicolor 0: `$D025`.
const REG_SPR_MC0: usize = 0x25;
/// The sprite multicolor 1: `$D026`.
const REG_SPR_MC1: usize = 0x26;
/// The eight sprite colors: `$D027-$D02E`.
const REG_SPR_COLOR: usize = 0x27;

/// The flag bit of `$D019`: the raster compare.
const FLAG_RASTER: u8 = 0x01;
/// The flag bit of `$D019`: a sprite-sprite collision.
const FLAG_SPR_SPR: u8 = 0x02;
/// The flag bit of `$D019`: a sprite-data collision.
const FLAG_SPR_BG: u8 = 0x04;
/// The nibble of write-set flag bits and the used flag bits.
const FLAG_WRITE_SET: u8 = 0x10;

/// The 16-color display palette of the chip: the published integer RGB
/// values, fixed at compile time so the compose stays exact.
const COLORS: [[u8; 3]; 16] = [
    [0x00, 0x00, 0x00],
    [0xFF, 0xFF, 0xFF],
    [0x68, 0x37, 0x2B],
    [0x70, 0xA4, 0xB2],
    [0x6F, 0x3D, 0x86],
    [0x58, 0x8D, 0x67],
    [0x35, 0x28, 0x79],
    [0xB8, 0xC7, 0x6F],
    [0x6F, 0x4F, 0x25],
    [0x43, 0x39, 0x00],
    [0x9A, 0x67, 0x59],
    [0x44, 0x44, 0x44],
    [0x6C, 0x6C, 0x6C],
    [0x9A, 0xD2, 0x84],
    [0x6C, 0x5E, 0xB5],
    [0x95, 0x95, 0x95],
];

/// The RGB triple of one color nibble.
fn rgb(index: u8) -> [u8; 3] {
    COLORS[usize::from(index & 0x0F)]
}

/// The video chip of the Commodore 64.
#[derive(Debug)]
pub struct Vic {
    name: String,
    /// The bus the per-line fetch walk uses.
    fetch_bus: String,
    /// The 16 KiB base of the current VIC bank within the fetch bus.
    fetch_base: u32,
    /// Domain ticks per raster line: one visit covers one line.
    line_div: u64,
    /// The raster lines per frame: 263 NTSC, 312 PAL.
    lines: u16,
    /// The register file of `$D000-$D02E`.
    regs: [u8; REG_BYTES],
    /// The 1K color RAM the chip samples for `$D800-$DFFF`.
    color_ram: Box<[u8; COLOR_RAM]>,
    /// The raster line counter.
    raster: u16,
    /// The tick of the next line walk.
    line_due: u64,
    // The per-line fetch buffers; the tick path fills them, the compose
    // reads them.
    matrix: [u8; COLS],
    gen_data: [u8; COLS],
    spr_ptr: [u8; 8],
    spr_data: [[u8; 3]; 8],
    // The per-line compose state.
    line_px: [u8; WIDTH],
    line_idx: [u8; WIDTH],
    spr_layer: [u16; WIDTH],
    /// The composed frame in RGB888.
    screen: Vec<u8>,
    /// The view the shell serves.
    cached_frame: Frame,
    frames: u64,
    /// The interrupt level currently driven on the line.
    irq_out: bool,
    next: Option<Tick>,
    last_tick: Option<Tick>,
}

impl Vic {
    /// A video chip from its configuration entry.
    pub fn new(device: &demu_config::model::DeviceInstance) -> Self {
        let pal = device
            .params
            .get("pal")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let region_lines = if pal { 312u16 } else { 263u16 };
        let lines = device
            .params
            .get("lines")
            .and_then(|v| v.as_integer())
            .and_then(|v| u32::try_from(v).ok())
            .and_then(|v| u16::try_from(v).ok())
            .filter(|v| *v > LAST_LINE)
            .unwrap_or(region_lines);
        let fetch_bus = device
            .params
            .get("fetch_bus")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| "cpu".to_string());
        let fetch_base = device
            .params
            .get("fetch_base")
            .and_then(|v| v.as_integer())
            .and_then(|v| u32::try_from(v).ok())
            .unwrap_or(0);
        Self {
            name: device.name.to_string(),
            fetch_bus,
            fetch_base,
            line_div: u64::from(device.dividers.get("line").copied().unwrap_or(65)).max(1),
            lines,
            regs: {
                let mut regs = [0u8; REG_BYTES];
                // The power-up state KERNAL setups expect: display
                // enabled with the full window and the scroll at zero.
                regs[REG_CTRL] = 0x1B;
                regs[REG_SCROLL] = 0x08;
                regs
            },
            color_ram: Box::new([0; COLOR_RAM]),
            raster: 0,
            line_due: 0,
            matrix: [0; COLS],
            gen_data: [0; COLS],
            spr_ptr: [0; 8],
            spr_data: [[0; 3]; 8],
            line_px: [0; WIDTH],
            line_idx: [0; WIDTH],
            spr_layer: [0; WIDTH],
            screen: vec![0; WIDTH * HEIGHT * 3],
            cached_frame: Frame {
                width: WIDTH as u32,
                height: HEIGHT as u32,
                pixels: vec![0; WIDTH * HEIGHT * 3],
            },
            frames: 0,
            irq_out: false,
            next: None,
            last_tick: None,
        }
    }

    /// The 9-bit raster compare: `$D011` bit 7 over `$D012`.
    fn raster_compare(&self) -> u32 {
        u32::from(self.regs[REG_CTRL] & 0x80) << 1 | u32::from(self.regs[REG_RASTER])
    }

    /// True when any unmasked interrupt source latches.
    fn irq_wanted(&self) -> bool {
        (self.regs[REG_IRQFLAGS] & self.regs[REG_IRQMASK] & 0x0F) != 0
    }

    /// Arm a re-seeded visit so the interrupt line follows a register
    /// write immediately instead of waiting for the next line walk.
    fn reseed_level(&mut self, tick: Tick) {
        if self.irq_wanted() != self.irq_out {
            self.next = Some(Tick(tick.0));
            self.last_tick = None;
        }
    }

    /// The 9-bit X coordinate of one sprite: the low byte plus the MSB
    /// of `$D010`.
    fn spr_x(&self, i: usize) -> u32 {
        u32::from(self.regs[REG_SPR_XY + i * 2])
            | (u32::from((self.regs[REG_SPR_MSB] >> i) & 1) << 8)
    }

    /// The data row a sprite shows at this raster line; `None` when the
    /// sprite does not display now. The display starts the line after
    /// the Y compare, and the Y-expand stretches every data row over
    /// two lines.
    fn spr_drow(&self, i: usize, raster: u32) -> Option<usize> {
        let y = u32::from(self.regs[REG_SPR_XY + 1 + i * 2]);
        let expand = self.regs[REG_SPR_YEXP] >> i & 1 == 1;
        let tall = if expand { 42u32 } else { 21 };
        let d = raster.checked_sub(y + 1)?;
        if d >= tall {
            return None;
        }
        Some(if expand { (d / 2) as usize } else { d as usize })
    }

    /// Advance the raster counter one line and set the raster flag on a
    /// compare hit. Returns true when the just-finished line ended the
    /// visible window; the caller turns that into the frame commit and
    /// the vblank event.
    fn advance_line(&mut self) -> bool {
        let old = self.raster;
        self.raster = (self.raster + 1) % self.lines;
        if u32::from(self.raster) == self.raster_compare() {
            self.regs[REG_IRQFLAGS] |= FLAG_RASTER;
        }
        old == LAST_LINE
    }

    /// Fetch the video matrix, the char or bitmap bytes, the sprite
    /// pointers, and the sprite data of one visible line through the
    /// fetch bus. The scroll values pick the tile sub-row the line
    /// shows.
    fn fetch_line(&mut self, raster: u32, tick: Tick, ctx: &mut MachineCtx) {
        let win_row = (raster - u32::from(FIRST_LINE)) as usize;
        let cell_row = win_row / CELL;
        let rline = (win_row % CELL + CELL - usize::from(self.regs[REG_CTRL] & 0x07)) & 7;
        let d018 = self.regs[REG_BASE];
        let vm = self.fetch_base + (u32::from(d018 >> 4) << 10);
        let mat = vm + (cell_row as u32) * COLS as u32;
        for i in 0..COLS {
            self.matrix[i] = self.bus_byte(tick, ctx, mat + i as u32);
        }
        for i in 0..8 {
            self.spr_ptr[i] = self.bus_byte(tick, ctx, vm + 0x3F8 + i as u32);
        }
        // The char base and the bitmap base share the same three bits
        // of `$D018` (the bit 0 is ignored both ways).
        let gen_base = self.fetch_base + (u32::from((d018 >> 1) & 0x07) << 11);
        if self.regs[REG_CTRL] & 0x20 != 0 {
            // The bitmap modes: the row lands on byte line
            // `(cell_row * 8 + rline)` times 40 bytes.
            let off = ((cell_row as u32) * 8 + rline as u32) * COLS as u32;
            for i in 0..COLS {
                self.gen_data[i] = self.bus_byte(tick, ctx, gen_base + off + i as u32);
            }
        } else {
            // The character modes: the generator data of each cell at
            // the tile sub-row.
            for i in 0..COLS {
                let addr = gen_base + u32::from(self.matrix[i]) * 8 + rline as u32;
                self.gen_data[i] = self.bus_byte(tick, ctx, addr);
            }
        }
        for i in 0..8 {
            let Some(drow) = self.spr_drow(i, raster) else {
                continue;
            };
            let base = self.fetch_base + (u32::from(self.spr_ptr[i]) << 6) + (drow as u32) * 3;
            self.spr_data[i] = [
                self.bus_byte(tick, ctx, base),
                self.bus_byte(tick, ctx, base + 1),
                self.bus_byte(tick, ctx, base + 2),
            ];
        }
    }

    /// One fetch bus byte.
    fn bus_byte(&self, tick: Tick, ctx: &mut MachineCtx, addr: u32) -> u8 {
        ctx.read(&self.fetch_bus, AccessWidth::Byte, Address(addr), tick) as u8
    }

    /// The char-cell and bitmap pixel at one window column: the data
    /// index (0 = background and transparent to sprites) and the color
    /// nibble. The vertical scroll belongs to the fetch: it picks the
    /// tile sub-row the line reads; only the horizontal scroll samples
    /// inside the window.
    fn interior_pixel(&self, x: usize, cell_row: usize) -> (u8, u8) {
        let d011 = self.regs[REG_CTRL];
        let d016 = self.regs[REG_SCROLL];
        // The horizontal scroll: the sample sits to the window pixel's
        // right, wrapped within the 40 cells instead of a split cell.
        let src = x + usize::from(d016 & 0x07);
        let cell = (src / 8) % COLS;
        let subbit = src % 8;
        let code = self.matrix[cell];
        let color_byte = self.color_ram[cell_row * COLS + cell];
        let bg0 = self.regs[REG_BG] & 0x0F;
        if d011 & 0x40 != 0 {
            // The extra color mode ($D011 bit 6): the cell paints the
            // background color the char bits 6-7 pick; no data pixels.
            let c = self.regs[REG_BG + ((code >> 6) & 3) as usize] & 0x0F;
            return (0, c);
        }
        if d011 & 0x20 != 0 {
            // The bitmap modes ($D011 bit 5): one bit per pixel, or a
            // two-bit pixel with `$D016` bit 4.
            if d016 & 0x10 != 0 {
                let b = self.gen_data[cell];
                let pair = (b >> (6 - ((subbit / 2) * 2))) & 3;
                let idx = match pair {
                    0 => (0, bg0),
                    1 => (1, self.regs[REG_BG + 1] & 0x0F),
                    2 => (2, color_byte >> 4 & 0x0F),
                    _ => (3, color_byte & 0x0F),
                };
                return idx;
            }
            let b = self.gen_data[cell];
            let bit = (b >> (7 - subbit)) & 1;
            if bit == 1 {
                return (1, code & 0x0F);
            }
            return (0, code >> 4);
        }
        // The character modes. Multicolor characters carry bit 1 of the
        // code ($D016 bit 4); the rest show one bit per pixel.
        if d016 & 0x10 != 0 && code & 0x02 != 0 {
            let b = self.gen_data[cell];
            let pair = (b >> (6 - ((subbit / 2) * 2))) & 3;
            let idx = match pair {
                0 => (0, bg0),
                1 => (1, self.regs[REG_BG + 1] & 0x0F),
                2 => (2, self.regs[REG_BG + 2] & 0x0F),
                _ => (3, color_byte & 0x0F),
            };
            return idx;
        }
        let b = self.gen_data[cell];
        if (b >> (7 - subbit)) & 1 == 1 {
            (1, color_byte & 0x0F)
        } else {
            (0, bg0)
        }
    }

    /// Compose one visible line into the window row of the frame: the
    /// interior cells, then the sprites, then the RGB paint.
    fn compose_line(&mut self, raster: u32) {
        let win_row = (raster - u32::from(FIRST_LINE)) as usize;
        let cell_row = win_row / CELL;
        let d011 = self.regs[REG_CTRL];
        let d016 = self.regs[REG_SCROLL];
        let border = self.regs[REG_BORDER] & 0x0F;
        // The shrunk window (RSEL or CSEL zero) paints the border only.
        let suppressed = d011 & 0x08 == 0 || d016 & 0x08 == 0;
        self.line_px = [border; WIDTH];
        self.line_idx = [0; WIDTH];
        self.spr_layer = [0; WIDTH];
        if !suppressed {
            for x in 0..WIDTH {
                let (idx, color) = self.interior_pixel(x, cell_row);
                self.line_idx[x] = idx;
                self.line_px[x] = color;
            }
        }
        self.compose_sprites(raster);
        let base = win_row * WIDTH * 3;
        for x in 0..WIDTH {
            let [r, g, b] = rgb(self.line_px[x]);
            let o = base + x * 3;
            self.screen[o] = r;
            self.screen[o + 1] = g;
            self.screen[o + 2] = b;
        }
    }

    /// Overlay the enabled sprites of the line and latch the collision
    /// bits. Sprite 0 paints first, so the lower-numbered sprite keeps
    /// the pixel where they overlap.
    fn compose_sprites(&mut self, raster: u32) {
        let d015 = self.regs[REG_SPR_EN];
        for i in 0..8usize {
            if d015 >> i & 1 == 0 {
                continue;
            }
            if self.spr_drow(i, raster).is_none() {
                continue;
            }
            let data = self.spr_data[i];
            let mcm = self.regs[REG_SPR_MCM] >> i & 1 == 1;
            let xpand = self.regs[REG_SPR_XEXP] >> i & 1 == 1;
            let behind = self.regs[REG_BG_PRI] >> i & 1 == 1;
            let mc0 = self.regs[REG_SPR_MC0] & 0x0F;
            let mc1 = self.regs[REG_SPR_MC1] & 0x0F;
            let own = self.regs[REG_SPR_COLOR + i] & 0x0F;
            let x0 = self.spr_x(i);
            // A sprite pixel covers two window pixels with the
            // X-expand; multicolor pixels are two bits wide already,
            // so their stretch factor doubles as well.
            let base_per: usize = if mcm { 2 } else { 1 };
            let per = if xpand { base_per * 2 } else { base_per };
            let wide = if xpand { 48 } else { 24 };
            for k in 0..wide {
                let src = k / per;
                let color = if mcm {
                    let byte = data[src / 4];
                    match (byte >> (6 - ((src % 4) * 2))) & 3 {
                        0 => None,
                        1 => Some(mc0),
                        2 => Some(own),
                        _ => Some(mc1),
                    }
                } else {
                    let byte = data[src / 8];
                    if byte >> (7 - (src % 8)) & 1 == 1 {
                        Some(own)
                    } else {
                        None
                    }
                };
                let Some(color) = color else { continue };
                let x0k = x0 + k as u32;
                if x0k >= WIDTH as u32 {
                    break;
                }
                let x = x0k as usize;
                // The sprite-sprite overlap: any earlier sprite already
                // painted this column latches both bits of the pair.
                let others = self.spr_layer[x];
                if others & !(1u16 << i) != 0 {
                    let was = self.regs[REG_SPR_SPR];
                    for j in 0..8usize {
                        if others >> j & 1 == 1 {
                            self.regs[REG_SPR_SPR] |= (1 << i) | (1 << j);
                        }
                    }
                    if self.regs[REG_SPR_SPR] != was {
                        self.regs[REG_IRQFLAGS] |= FLAG_SPR_SPR;
                    }
                }
                // The sprite-data overlap: the video data pixel is not
                // the background.
                if self.line_idx[x] != 0 {
                    self.regs[REG_SPR_BG] |= 1 << i;
                    self.regs[REG_IRQFLAGS] |= FLAG_SPR_BG;
                }
                // The paint: the behind-foreground sprites show only
                // over background pixels, and an earlier sprite keeps
                // the pixel.
                if self.spr_layer[x] == 0 && (!behind || self.line_idx[x] == 0) {
                    self.line_px[x] = color;
                }
                self.spr_layer[x] |= 1 << i;
            }
        }
    }

    /// The register read at the address's low six bits, with the read
    /// side effects the chip has: the flag, collision, and latch reads
    /// clear themselves.
    fn reg_read(&mut self, r: usize) -> u8 {
        match r {
            REG_RASTER => self.raster as u8,
            REG_CTRL => self.regs[REG_CTRL] & 0x7F | u8::from(self.raster & 0x0100 != 0) << 7,
            REG_LPX | REG_LPY => 0,
            REG_IRQFLAGS => {
                // The flag bits read and clear; the unused high bits sit.
                let out = self.regs[REG_IRQFLAGS] | 0xF0;
                self.regs[REG_IRQFLAGS] &= !0x0F;
                out
            }
            REG_IRQMASK => self.regs[REG_IRQMASK] | 0xF0,
            REG_SPR_SPR => {
                let out = self.regs[REG_SPR_SPR];
                self.regs[REG_SPR_SPR] = 0;
                out
            }
            REG_SPR_BG => {
                let out = self.regs[REG_SPR_BG];
                self.regs[REG_SPR_BG] = 0;
                out
            }
            r if r < REG_BYTES => self.regs[r],
            _ => 0xFF,
        }
    }

    /// The register write at the address's low six bits.
    fn reg_write(&mut self, r: usize, v: u8) {
        match r {
            REG_IRQFLAGS => {
                // Bit 4 set: the write sets the given flags. Otherwise
                // the written bits acknowledge (clear).
                if v & FLAG_WRITE_SET != 0 {
                    self.regs[REG_IRQFLAGS] |= v & 0x0F;
                } else {
                    self.regs[REG_IRQFLAGS] &= !(v & 0x0F);
                }
            }
            REG_IRQMASK => self.regs[REG_IRQMASK] = v & 0x0F,
            REG_LPX | REG_LPY | REG_SPR_SPR | REG_SPR_BG => {
                // Read-only and read-to-clear latches ignore writes.
            }
            REG_SCROLL => self.regs[REG_SCROLL] = v & 0x1F,
            r if r < REG_BYTES => self.regs[r] = v,
            _ => {}
        }
    }

    /// The pure register view for debug commands, without the read
    /// side effects.
    fn reg_peek(&self, r: usize) -> u8 {
        match r {
            REG_RASTER => self.raster as u8,
            REG_CTRL => self.regs[REG_CTRL] & 0x7F | u8::from(self.raster & 0x0100 != 0) << 7,
            REG_IRQFLAGS | REG_IRQMASK => self.regs[r] | 0xF0,
            REG_SPR_SPR | REG_SPR_BG | REG_LPX | REG_LPY => self.regs[r],
            r if r < REG_BYTES => self.regs[r],
            _ => 0xFF,
        }
    }
}

impl BusAccess for Vic {
    fn read(&mut self, _width: AccessWidth, address: Address, _tick: Tick) -> u32 {
        let raw = address.0;
        if (0xD000..=0xD3FF).contains(&raw) {
            return u32::from(self.reg_read((raw & 0x3F) as usize));
        }
        if (0xD800..=0xDFFF).contains(&raw) {
            return u32::from(self.color_ram[(raw & 0x3FF) as usize]);
        }
        0xFF
    }

    fn write(&mut self, _width: AccessWidth, address: Address, value: u32, tick: Tick) {
        let raw = address.0;
        let v = value as u8;
        if (0xD000..=0xD3FF).contains(&raw) {
            self.reg_write((raw & 0x3F) as usize, v);
            self.reseed_level(tick);
            return;
        }
        if (0xD800..=0xDFFF).contains(&raw) {
            self.color_ram[(raw & 0x3FF) as usize] = v & 0x0F;
        }
    }

    fn peek(&self, address: Address) -> u8 {
        let raw = address.0;
        if (0xD000..=0xD3FF).contains(&raw) {
            return self.reg_peek((raw & 0x3F) as usize);
        }
        if (0xD800..=0xDFFF).contains(&raw) {
            return self.color_ram[(raw & 0x3FF) as usize];
        }
        0xFF
    }
}

impl VideoSource for Vic {
    fn frame(&self) -> Option<&Frame> {
        Some(&self.cached_frame)
    }

    fn dump_layer(&self, layer: VideoLayer) -> Option<Vec<u8>> {
        match layer {
            VideoLayer::Framebuffer => Some(self.screen.clone()),
            VideoLayer::Palette => {
                // The color nibbles of `$D020-$D026` and the sprite
                // colors: 15 bytes plus one pad.
                let mut out = Vec::with_capacity(16);
                for r in REG_BORDER..=REG_SPR_MC1 {
                    out.push(self.regs[r] & 0x0F);
                }
                for r in REG_SPR_COLOR..=REG_SPR_COLOR + 7 {
                    out.push(self.regs[r] & 0x0F);
                }
                out.push(0);
                Some(out)
            }
            _ => None,
        }
    }

    fn frames(&self) -> u64 {
        self.frames
    }
}

impl Device for Vic {
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
        self.regs = {
            let mut regs = [0u8; REG_BYTES];
            regs[REG_CTRL] = 0x1B;
            regs[REG_SCROLL] = 0x08;
            regs
        };
        self.color_ram.iter_mut().for_each(|b| *b = 0);
        self.raster = 0;
        self.matrix = [0; COLS];
        self.gen_data = [0; COLS];
        self.spr_ptr = [0; 8];
        self.spr_data = [[0; 3]; 8];
        self.line_px = [0; WIDTH];
        self.line_idx = [0; WIDTH];
        self.spr_layer = [0; WIDTH];
        self.frames = 0;
        self.irq_out = false;
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
        let mut out = Vec::with_capacity(5 + REG_BYTES + COLOR_RAM + 2 + 8 + 8 + 1);
        out.extend_from_slice(b"VIC2");
        out.push(1);
        out.extend_from_slice(&self.regs);
        out.extend_from_slice(self.color_ram.as_ref());
        out.extend_from_slice(&self.raster.to_le_bytes());
        out.extend_from_slice(&self.frames.to_le_bytes());
        out.extend_from_slice(&self.line_due.to_le_bytes());
        out.push(u8::from(self.irq_out));
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        let expected = 5 + REG_BYTES + COLOR_RAM + 2 + 8 + 8 + 1;
        if data.len() != expected || &data[..4] != b"VIC2" || data[4] != 1 {
            return Err(SnapshotError::Mismatch(format!(
                "vic chunk must be {expected} bytes"
            )));
        }
        let mut pos = 5usize;
        self.regs.copy_from_slice(&data[pos..pos + REG_BYTES]);
        pos += REG_BYTES;
        self.color_ram.copy_from_slice(&data[pos..pos + COLOR_RAM]);
        pos += COLOR_RAM;
        self.raster =
            u16::from_le_bytes(data[pos..pos + 2].try_into().expect("2 bytes")) % self.lines;
        pos += 2;
        self.frames = u64::from_le_bytes(data[pos..pos + 8].try_into().expect("8 bytes"));
        pos += 8;
        self.line_due = u64::from_le_bytes(data[pos..pos + 8].try_into().expect("8 bytes"));
        pos += 8;
        self.irq_out = data[pos] != 0;
        self.next = None;
        self.last_tick = None;
        Ok(())
    }

    fn notify_line(&mut self, _line: &str, _level: Level, _tick: Tick) {
        // No signal lines enter this device; the bank select rides the
        // `fetch_base` parameter until the config wires it.
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
            self.line_step(tick, ctx);
            self.line_due += self.line_div;
        }
        let want = self.irq_wanted();
        if want != self.irq_out {
            self.irq_out = want;
            ctx.notify("irq", if want { Level::High } else { Level::Low }, tick);
        }
        self.next = Some(Tick(self.line_due));
    }
}

impl Vic {
    /// One line walk: the raster advance, the scanline notification,
    /// and the visible-line fetch and compose.
    fn line_step(&mut self, tick: Tick, ctx: &mut MachineCtx) {
        let frame_end = self.advance_line();
        if frame_end {
            self.frames += 1;
            self.cached_frame.pixels = self.screen.clone();
            ctx.emit(TriggerEvent::Vblank);
        }
        // One scanline edge per line for the trigger engine and the
        // scanline-counting circuits.
        ctx.emit(TriggerEvent::Scanline(u32::from(self.raster)));
        ctx.notify("scanline", Level::High, tick);
        let raster = self.raster;
        if (FIRST_LINE..=LAST_LINE).contains(&raster) {
            self.fetch_line(u32::from(raster), tick, ctx);
            self.compose_line(u32::from(raster));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instance() -> demu_config::model::DeviceInstance {
        demu_config::model::DeviceInstance {
            name: demu_config::newtypes::DeviceName::new("vic").expect("name"),
            kind: demu_config::newtypes::KindName::new("c64.vic").expect("kind"),
            domain: "main".to_string(),
            params: std::collections::BTreeMap::new(),
            dividers: [("line".to_string(), 65)].into_iter().collect(),
        }
    }

    fn wr(v: &mut Vic, addr: u32, val: u8) {
        v.write(AccessWidth::Byte, Address(addr), u32::from(val), Tick(0));
    }

    fn rd(v: &mut Vic, addr: u32) -> u8 {
        v.read(AccessWidth::Byte, Address(addr), Tick(0)) as u8
    }

    #[test]
    fn registers_decode_with_mirrors() {
        let mut v = Vic::new(&instance());
        wr(&mut v, 0xD018, 0x15);
        assert_eq!(v.regs[REG_BASE], 0x15);
        // The mirror 64 bytes later lands in the same register.
        wr(&mut v, 0xD040, 0x77);
        assert_eq!(v.regs[REG_SPR_XY], 0x77);
        assert_eq!(v.regs[REG_SPR_XY + 1], 0);
        // The unused offsets at and above $2F read one.
        assert_eq!(rd(&mut v, 0xD02F), 0xFF);
        assert_eq!(rd(&mut v, 0xD2FF), 0xFF);
        // The color RAM decodes over the $D800-$DFFF range.
        wr(&mut v, 0xD800, 0x02);
        assert_eq!(v.color_ram[0], 0x02);
        wr(&mut v, 0xDC01, 0x42);
        assert_eq!(
            rd(&mut v, 0xDC01),
            0x02,
            "the high nibble of color RAM drops"
        );
    }

    #[test]
    fn raster_compare_latches_and_clears() {
        let mut v = Vic::new(&instance());
        wr(&mut v, 0xD012, 0x0A);
        wr(&mut v, 0xD01A, 0x01);
        v.raster = 9;
        assert!(!v.irq_wanted());
        assert!(!v.advance_line(), "raster 10 is not the window end");
        assert!(v.irq_wanted(), "the compare of 10 latched the raster flag");
        // The flag reads with the high bits set and the read clears it.
        assert_eq!(rd(&mut v, 0xD019), 0xF1);
        assert!(!v.irq_wanted());
        // The 9-bit compare rides $D011 bit 7.
        wr(&mut v, 0xD011, 0x80);
        wr(&mut v, 0xD012, 0x04);
        assert_eq!(v.raster_compare(), 0x104);
        while v.raster != 0x104 {
            v.advance_line();
        }
        assert!(v.regs[REG_IRQFLAGS] & FLAG_RASTER != 0);
        // The counter readback carries the raster bits.
        assert_eq!(rd(&mut v, 0xD012), 0x04);
        assert_eq!(
            rd(&mut v, 0xD011),
            0x80,
            "the control bits read back plus raster bit 8"
        );
        // The counter wraps at the region size.
        v.raster = 262;
        v.advance_line();
        assert_eq!(v.raster, 0);
    }

    #[test]
    fn window_paints_char_row_and_border() {
        let mut v = Vic::new(&instance());
        // One character cell with data pixels and color memory color.
        wr(&mut v, 0xD800 + 43, 0x07);
        v.matrix[3] = 0x41;
        v.gen_data[3] = 0xF0;
        v.compose_line(60);
        // Raster 60 maps to window row 10: the left 4 pixels of cell 3
        // show the color RAM entry, the right 4 the background.
        assert_eq!(v.line_px[24], 0x07);
        assert_eq!(v.line_px[31], 0x00, "the zero bit shows background");
        assert_eq!(v.line_idx[24], 1);
        assert_eq!(v.line_idx[31], 0);
        let base = 10 * WIDTH * 3 + 24 * 3;
        assert_eq!(v.screen[base..base + 3], COLORS[7]);
        // The shrunk window paints border everywhere: RSEL zero.
        wr(&mut v, 0xD011, 0x10);
        wr(&mut v, 0xD020, 0x06);
        wr(&mut v, 0xD016, 0x08);
        v.compose_line(60);
        assert!(v.line_px.iter().all(|c| *c == 0x06));
    }

    #[test]
    fn sprites_paint_and_latch_collisions() {
        let mut v = Vic::new(&instance());
        wr(&mut v, 0xD015, 0x03);
        // Both sprites start the line after Y 49: visible at 50..=70.
        v.raster = 60;
        wr(&mut v, 0xD001, 0x31);
        wr(&mut v, 0xD003, 0x31);
        // Y 0x31 = 49; at raster 60 the data row is 60 - 49 - 1 = 10.
        assert_eq!(v.spr_drow(0, 60), Some(10));
        v.spr_data[0] = [0xFF, 0, 0];
        v.spr_data[1] = [0xFF, 0, 0];
        // Some video data under the sprites: cell 2 covers x 16..23.
        v.matrix[2] = 0x65;
        v.gen_data[2] = 0xFF;
        wr(&mut v, 0xD800 + 40 + 2, 0x03);
        wr(&mut v, 0xD027, 0x01);
        wr(&mut v, 0xD028, 0x0A);
        // X 10 and 12 overlap.
        wr(&mut v, 0xD000, 0x0A);
        wr(&mut v, 0xD002, 0x0C);
        v.compose_line(60);
        assert_eq!(v.line_px[10], 0x01, "sprite 0 shows its own color");
        assert_eq!(v.line_px[12], 0x01, "the earlier sprite keeps the pixel");
        assert_eq!(v.regs[REG_SPR_SPR], 0x03, "the pair latched in $D01E");
        assert_eq!(v.regs[REG_SPR_BG], 0x03, "both sprites hit the video data");
        assert_eq!(v.regs[REG_IRQFLAGS] & (FLAG_SPR_SPR | FLAG_SPR_BG), 0x06);
        // The latch reads clear themselves.
        assert_eq!(rd(&mut v, 0xD01E), 0x03);
        assert_eq!(v.regs[REG_SPR_SPR], 0);
        // The MSB of $D010 extends the X coordinate out of the window.
        wr(&mut v, 0xD000, 0x40);
        wr(&mut v, 0xD010, 0x01);
        assert_eq!(v.spr_x(0), 0x140);
        // Back inside the window: the moved sprite paints at x 64.
        wr(&mut v, 0xD010, 0x00);
        v.compose_line(60);
        assert_eq!(v.line_px[64], 0x01);
    }

    #[test]
    fn snapshot_round_trip() {
        let mut v = Vic::new(&instance());
        wr(&mut v, 0xD018, 0x15);
        wr(&mut v, 0xD012, 0x40);
        wr(&mut v, 0xD020, 0x02);
        wr(&mut v, 0xD800, 0x03);
        v.raster = 66;
        v.advance_line();
        v.frames = 9;
        v.gen_data[0] = 0x55;
        let snap = v.snapshot();
        assert_eq!(snap.len(), 5 + REG_BYTES + COLOR_RAM + 2 + 8 + 8 + 1);
        let mut twin = Vic::new(&instance());
        twin.restore(&snap).expect("restore");
        assert_eq!(twin.snapshot(), snap, "the round trip is byte-identical");
        assert_eq!(twin.raster, 67);
        assert_eq!(twin.frames, 9);
        assert_eq!(twin.color_ram[0], 0x03);
    }
}
