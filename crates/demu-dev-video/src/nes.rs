//! The NES PPU: the RP2A03's 2C02 and the RP2A07's 2C07.
//!
//! One device visit walks one dot of one scanline. The domain tick is
//! the master crystal; one dot is the configured divider (4 on NTSC,
//! 5 on PAL). The frame walks 262 scanlines of 341 dots on the 2C02,
//! with one skipped dot on odd frames, and 312 scanlines of 341 dots
//! on the 2C07. The vblank flag and the NMI edge land at scanline
//! 241, dot 1, and the flags clear on the pre-render scanline.
//!
//! The background and objects compose per scanline from the scroll
//! register state at the scanline start, so per-scanline raster
//! effects are exact. Pattern bytes fetch through the PPU bus, so the
//! cartridge mapper answers with the mapped CHR banks. The two-kilobyte
//! name-table RAM and the palette live in the device, mirrored the way
//! the cartridge declares.

use demu_config::model::BootState;
use demu_core::device::{AccessWidth, BusAccess, Frame, MachineCtx, VideoLayer, VideoSource};
use demu_core::frequency::{Address, Tick};
use demu_core::signal::Level;
use demu_core::triggers::TriggerEvent;
use demu_core::{Device, Snapshot, SnapshotError};

/// The dots per scanline.
const LINE_DOTS: u16 = 341;
/// The space of the object attribute memory.
const OAM_SIZE: usize = 0x100;
/// The screen width.
const WIDTH: usize = 256;
/// The screen height.
const HEIGHT: usize = 240;
/// The end of the visible scanlines.
const VISIBLE_LINES: u16 = 240;
/// The tiles a scanline fetches: the visible width plus the two
/// prefetch tiles of the horizontal wrap.
const LINE_TILES: usize = 34;
/// One evaluated object of one scanline.
#[derive(Debug, Clone, Copy)]
struct LineSprite {
    /// The left screen column minus eight.
    screen_x: i32,
    tile: u8,
    attrs: u8,
    row: u8,
    is_sprite_zero: bool,
}

/// One fetched background tile of a scanline.
#[derive(Debug, Default, Clone, Copy)]
struct BgTile {
    lo: u8,
    hi: u8,
    /// The two palette bits of the tile's attribute quadrant.
    attrs: u8,
}

/// The fixed master palette of the 2C02, three bytes per entry.
const PPU_PALETTE: &[[u8; 3]; 0x40] = &[
    [0x66, 0x66, 0x66],
    [0x00, 0x2A, 0x88],
    [0x14, 0x12, 0xA7],
    [0x3B, 0x00, 0xA4],
    [0x5C, 0x00, 0x7E],
    [0x6E, 0x00, 0x40],
    [0x6C, 0x06, 0x00],
    [0x56, 0x1D, 0x00],
    [0x33, 0x35, 0x00],
    [0x11, 0x4B, 0x00],
    [0x00, 0x4F, 0x00],
    [0x00, 0x49, 0x1B],
    [0x00, 0x3C, 0x55],
    [0x00, 0x00, 0x00],
    [0x00, 0x00, 0x00],
    [0x00, 0x00, 0x00],
    [0xA9, 0xA9, 0xA9],
    [0x15, 0x5F, 0xD9],
    [0x42, 0x40, 0xFF],
    [0x71, 0x23, 0xF6],
    [0x96, 0x00, 0xC6],
    [0xA4, 0x00, 0x38],
    [0x74, 0x2E, 0x00],
    [0x67, 0x4F, 0x00],
    [0x33, 0x68, 0x00],
    [0x0F, 0x82, 0x00],
    [0x00, 0x8B, 0x15],
    [0x00, 0x85, 0x5D],
    [0x00, 0x7F, 0xAD],
    [0x00, 0x00, 0x00],
    [0x00, 0x00, 0x00],
    [0x00, 0x00, 0x00],
    [0xEC, 0xEC, 0xEC],
    [0x4B, 0x9B, 0xFF],
    [0x7A, 0x7A, 0xFF],
    [0xA5, 0x7A, 0xFF],
    [0xD9, 0x3E, 0xFF],
    [0xDA, 0x4B, 0x9B],
    [0xC3, 0x74, 0x00],
    [0xA9, 0x96, 0x00],
    [0x7B, 0xA9, 0x00],
    [0x3B, 0xB5, 0x00],
    [0x1E, 0xB5, 0x3B],
    [0x2E, 0xB1, 0x82],
    [0x4C, 0xA9, 0xD9],
    [0x00, 0x00, 0x00],
    [0x00, 0x00, 0x00],
    [0x00, 0x00, 0x00],
    [0xFF, 0xFF, 0xFF],
    [0x87, 0xD0, 0xFF],
    [0xA9, 0xA9, 0xFF],
    [0xCE, 0xB9, 0xFF],
    [0xFA, 0xA1, 0xFF],
    [0xEA, 0x87, 0xDA],
    [0xE5, 0xA9, 0x56],
    [0xF2, 0xC5, 0x00],
    [0xEC, 0xDE, 0x00],
    [0x9C, 0xF2, 0x51],
    [0x70, 0xF2, 0x96],
    [0x70, 0xF2, 0xDE],
    [0x77, 0xEF, 0xFF],
    [0x00, 0x00, 0x00],
    [0x00, 0x00, 0x00],
    [0x00, 0x00, 0x00],
];

/// The pixel-processing unit of the NES.
#[derive(Debug)]
pub struct NesPpu {
    name: String,
    /// The bus the pattern fetches walk.
    pattern_bus: String,
    /// True on the PAL 2C07.
    pal: bool,
    /// Master-domain ticks per dot: 4 on the 2C02, 5 on the 2C07.
    dot_div: u64,
    /// The scanlines per frame: 262 on the 2C02, 312 on the 2C07.
    lines: u16,
    /// The mirror mode: 0 horizontal, 1 vertical.
    mirroring: u8,
    /// The two-kilobyte name-table RAM.
    ntram: Box<[u8; 0x800]>,
    /// The palette RAM.
    palette: [u8; 0x20],
    /// The object attribute memory.
    oam: Box<[u8; OAM_SIZE]>,
    /// The control register $2000.
    ctrl: u8,
    /// The mask register $2001.
    mask: u8,
    /// The status register $2002, high bits only.
    status_v: u8,
    /// The object address register $2003.
    oam_addr: u8,
    /// The buffered read data of the VRAM data port.
    data_buffer: u8,
    /// The loopy scroll register.
    v: u16,
    /// The loopy scroll latch.
    t: u16,
    /// The fine X scroll.
    fine_x: u8,
    /// The two-write toggle of the scroll registers.
    write_toggle: bool,
    /// The NMI line state this device drives.
    nmi_asserted: bool,
    /// The tick of the last status-register read, for the race window.
    status_read_tick: Option<Tick>,
    /// The pattern-bus writes the data-port writes left pending, in
    /// order. A queue, because several data-port writes can land
    /// between two dot visits.
    pending_writes: Vec<(u16, u8)>,
    /// The data-port read whose buffer fill is pending.
    pending_read: Option<u16>,
    /// The NMI state follows the register updates on the next visit.
    nmi_update_pending: bool,
    // The dot pipeline.
    dot: u64,
    /// The scanline of the current dot.
    line: u16,
    /// The objects evaluated for the current scanline.
    line_sprites: Vec<LineSprite>,
    /// True when sprite zero joined the current scanline.
    sprite_zero_on_line: bool,
    /// The composed frame in RGB888.
    screen: Vec<u8>,
    /// The frame for the shell's screenshot commands.
    cached_frame: Frame,
    /// The complete frames since boot.
    frames: u64,
    last_tick: Option<Tick>,
    next: Option<Tick>,
}

impl NesPpu {
    /// A NES PPU from its configuration entry.
    ///
    /// The kind selects the region: `rp2A07.ppu` or the `pal`
    /// parameter selects the PAL timing. The `dot` divider entry
    /// overrides the dot divider when present.
    pub fn new(device: &demu_config::model::DeviceInstance) -> Self {
        let pal = device.kind.as_str().starts_with("rp2A07")
            || device
                .params
                .get("pal")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
        let pattern_bus = device
            .params
            .get("pattern_bus")
            .and_then(|v| v.as_str())
            .unwrap_or("ppu");
        Self {
            name: device.name.to_string(),
            pattern_bus: pattern_bus.to_string(),
            pal,
            dot_div: if pal { 5 } else { 4 },
            lines: if pal { 312 } else { 262 },
            mirroring: 0,
            ntram: Box::new([0; 0x800]),
            palette: [0; 0x20],
            oam: Box::new([0; OAM_SIZE]),
            ctrl: 0,
            mask: 0,
            status_v: 0,
            oam_addr: 0,
            data_buffer: 0,
            v: 0,
            t: 0,
            fine_x: 0,
            write_toggle: false,
            nmi_asserted: false,
            status_read_tick: None,
            pending_writes: Vec::with_capacity(8),
            pending_read: None,
            nmi_update_pending: false,
            dot: 0,
            line: 0,
            line_sprites: Vec::with_capacity(9),
            sprite_zero_on_line: false,
            screen: vec![0; WIDTH * HEIGHT * 3],
            cached_frame: Frame {
                width: WIDTH as u32,
                height: HEIGHT as u32,
                pixels: vec![0; WIDTH * HEIGHT * 3],
            },
            frames: 0,
            last_tick: None,
            next: None,
        }
    }

    /// The region: true on the PAL 2C07.
    pub fn pal(&self) -> bool {
        self.pal
    }

    /// The dots of one frame, after the NTSC odd-frame skip.
    pub fn dots_per_frame(&self) -> u64 {
        let dots = u64::from(self.lines) * u64::from(LINE_DOTS);
        if self.pal { dots } else { dots - 1 }
    }

    /// Set the mirror mode from the cartridge header bit 0: 0
    /// horizontal, 1 vertical. A four-screen cartridge falls back on
    /// the vertical form; the four-kilobyte expansion is not part of
    /// the current mapper set.
    pub fn set_mirroring(&mut self, mode: u8) {
        self.mirroring = mode & 0x01;
    }

    /// The mask register $2001 for the debug commands.
    pub fn nes_mask(&self) -> u8 {
        self.mask
    }

    /// The pre-render scanline, the last one of the frame.
    fn pre_render(&self) -> u16 {
        self.lines - 1
    }

    /// True while either rendering layer runs at all.
    fn rendering(&self) -> bool {
        self.mask & 0x18 != 0
    }

    /// True on a scanline the pipeline works on: the visible lines
    /// and the pre-render line.
    fn rendered_scanline(&self) -> bool {
        self.line < VISIBLE_LINES || self.line == self.pre_render()
    }

    /// The internal name-table index of a bus address in $2000-$2FFF:
    /// the vertical case banks on address bit 10, the horizontal case
    /// on bit 11 folded into the second half of the RAM.
    pub(crate) fn nt_index(&self, addr: u16) -> usize {
        match self.mirroring {
            1 => (addr & 0x0400 | (addr & 0x03FF)) as usize,
            _ => (((addr & 0x0800) >> 1) | (addr & 0x03FF)) as usize,
        }
    }

    /// Read a name-table byte through the internal mirroring.
    fn nt_read(&self, addr: u16) -> u8 {
        self.ntram[self.nt_index(addr)]
    }

    /// Write a name-table byte through the internal mirroring.
    fn nt_write(&mut self, addr: u16, value: u8) {
        let index = self.nt_index(addr);
        self.ntram[index] = value;
    }

    /// The palette byte of one internal index, with the mirrored
    /// universal entries.
    pub(crate) fn palette_byte(&self, index: u8) -> u8 {
        self.palette[palette_index(index)]
    }

    /// The palette RAM for the debug commands.
    pub fn palette_ram(&self) -> &[u8] {
        &self.palette
    }

    /// The object attribute memory for the debug commands.
    pub fn oam_ram(&self) -> &[u8] {
        self.oam.as_slice()
    }

    /// The name-table RAM for the debug commands.
    pub fn ntram(&self) -> &[u8] {
        self.ntram.as_slice()
    }

    /// One dot of the pipeline: line transitions, object evaluation,
    /// composition, and the vblank edges.
    fn step(&mut self, tick: Tick, ctx: &mut MachineCtx) {
        self.dot += 1;
        let line_dot = self.dot % u64::from(LINE_DOTS);
        if line_dot == 0 {
            self.line = if self.line + 1 >= self.lines {
                0
            } else {
                self.line + 1
            };
            // One scanline edge per line for the trigger engine and
            // the scanline-counting mapper circuits.
            ctx.emit(TriggerEvent::Scanline(u32::from(self.line)));
            ctx.notify("scanline", Level::High, tick);
        }
        if self.line == self.pre_render() && line_dot == 1 {
            // The flags clear and the scroll re-loads at the
            // pre-render scanline.
            self.status_v &= !0xE0;
            if self.nmi_asserted {
                self.nmi_asserted = false;
                ctx.notify("nmi", Level::Low, tick);
            }
            self.oam_addr = 0;
        }
        if self.rendered_scanline() && self.rendering() {
            match line_dot {
                256 => self.increment_y(),
                257 => self.v = (self.v & 0x041F) | (self.t & 0x7BE0),
                320 if self.line == self.pre_render() => self.v = self.t,
                _ => {}
            }
        }
        if self.line < VISIBLE_LINES && line_dot == 1 {
            // Evaluate the objects of this scanline at its start.
            self.scan_sprites();
        }
        if self.line < VISIBLE_LINES && line_dot == u64::try_from(WIDTH).unwrap_or(257) + 1 {
            // The composed line lands after the last background
            // fetch, from the register state of the scanline start.
            self.render_line(tick, ctx);
        }
        if self.line == 241 && line_dot == 1 {
            self.enter_vblank(tick, ctx);
        }
        if !self.pal
            && self.rendering()
            && self.line == self.pre_render()
            && line_dot == 339
            && self.frames % 2 == 1
        {
            // The odd-frame skip on the NTSC form: the odd frame's
            // pre-render scanline carries no 341st dot. The counter
            // jumps past it, the scanline transition fires, and the
            // next visit lands on the scanline-zero dot one, so the
            // frame is one dot shorter.
            self.dot += 2;
            self.line = 0;
            ctx.emit(TriggerEvent::Scanline(0));
            ctx.notify("scanline", Level::High, tick);
        }
    }

    /// The vertical scroll increment at the end of a rendered line.
    fn increment_y(&mut self) {
        if self.v & 0x7000 != 0x7000 {
            self.v += 0x1000;
        } else {
            self.v &= !0x7000;
            let coarse_y = (self.v >> 5) & 0x1F;
            if coarse_y == 29 {
                self.v &= !0x03E0;
                self.v ^= 0x0800;
            } else if coarse_y == 31 {
                self.v &= !0x03E0;
            } else {
                self.v += 0x0020;
            }
        }
    }

    /// Enter vblank: the frame count increments and the cached frame
    /// refreshes; the flag and the NMI follow the enable bit, with
    /// the same-dot race suppressing both. The write-suppression of
    /// the power-on warm-up ends here.
    fn enter_vblank(&mut self, tick: Tick, ctx: &mut MachineCtx) {
        self.frames += 1;
        self.cached_frame.pixels = self.screen.clone();
        // A status read within one dot of this set suppresses the
        // flag and the NMI; hardware sets the flag one dot later.
        let raced = self
            .status_read_tick
            .is_some_and(|r: Tick| r.0 + self.dot_div > tick.0);
        if raced {
            return;
        }
        self.status_v |= 0x80;
        ctx.emit(TriggerEvent::Vblank);
        ctx.notify("vblank", Level::High, tick);
        self.update_nmi(tick, ctx);
    }

    /// Set or release the NMI line to match the enable bit and the
    /// vblank flag.
    fn update_nmi(&mut self, tick: Tick, ctx: &mut MachineCtx) {
        let wanted = self.ctrl & 0x80 != 0 && self.status_v & 0x80 != 0;
        if wanted != self.nmi_asserted {
            self.nmi_asserted = wanted;
            ctx.notify("nmi", if wanted { Level::High } else { Level::Low }, tick);
        }
    }

    /// Evaluate the objects for one scanline from the object table,
    /// with the eight-object limit of the secondary OAM.
    fn scan_sprites(&mut self) {
        self.line_sprites.clear();
        self.sprite_zero_on_line = false;
        let height: i32 = if self.ctrl & 0x20 != 0 { 16 } else { 8 };
        for i in 0..64u8 {
            let y = self.oam[i as usize * 4];
            let row = i32::from(self.line as u8).wrapping_sub(i32::from(y));
            if !(0..height).contains(&row) {
                continue;
            }
            if self.line_sprites.len() == 8 {
                // The ninth matching object sets the overflow flag.
                self.status_v |= 0x20;
                break;
            }
            self.line_sprites.push(LineSprite {
                screen_x: i32::from(self.oam[i as usize * 4 + 1]).wrapping_sub(8),
                tile: self.oam[i as usize * 4 + 2],
                attrs: self.oam[i as usize * 4 + 3],
                row: row as u8,
                is_sprite_zero: i == 0,
            });
        }
        self.sprite_zero_on_line = self.line_sprites.iter().any(|s| s.is_sprite_zero);
    }

    /// The fetched pattern byte through the PPU bus.
    fn fetch(&mut self, ctx: &mut MachineCtx, addr: u16, tick: Tick) -> u8 {
        ctx.read(
            &self.pattern_bus,
            AccessWidth::Byte,
            Address(u32::from(addr & 0x3FFF)),
            tick,
        ) as u8
    }

    /// Render the visible scanline into the frame buffer from the
    /// scroll state of the scanline start.
    ///
    /// The background fetches walk one tile at a time like the
    /// hardware pipeline; the objects overlay after the background
    /// with their priority and the left-column masks.
    fn render_line(&mut self, tick: Tick, ctx: &mut MachineCtx) {
        let line = usize::from(self.line);
        let bg_on = self.mask & 0x08 != 0;
        let sp_on = self.mask & 0x10 != 0;
        let left_bg = self.mask & 0x02 != 0;
        let left_sp = self.mask & 0x04 != 0;
        let greyscale = self.mask & 0x01 != 0;
        let mut tiles = [BgTile::default(); LINE_TILES];
        if bg_on && self.rendering() {
            tiles = self.fetch_background(tick, ctx);
        }
        // The object patterns fetch once per object, in eval order.
        let mut sprite_rows: Vec<(u8, u8)> = Vec::with_capacity(8);
        if sp_on && self.rendering() {
            let count = self.line_sprites.len();
            let mut i = 0;
            while i < count {
                let sprite = self.line_sprites[i];
                sprite_rows.push(self.sprite_bytes(&sprite, tick, ctx));
                i += 1;
            }
        }
        let mut sprite_zero_hit = false;
        for x in 0..WIDTH {
            let mut entry = self.palette[0];
            let mut bg_color = 0u8;
            if bg_on && self.rendering() && (left_bg || x >= 8) {
                let fine = usize::from(self.fine_x) + x;
                let tile = tiles[fine / 8];
                let bit = 7 - (fine % 8) as u8;
                let color = (tile.lo >> bit & 1) | ((tile.hi >> bit & 1) << 1);
                bg_color = color;
                if color != 0 {
                    entry = self.palette_byte((tile.attrs << 2) | color);
                }
            }
            if sp_on && self.rendering() && (left_sp || x >= 8) {
                for (i, (lo, hi)) in sprite_rows.iter().enumerate() {
                    let sprite = self.line_sprites[i];
                    let left = sprite.screen_x;
                    if left > x as i32 || left + 8 <= x as i32 {
                        continue;
                    }
                    let mut col = x as i32 - left;
                    if sprite.attrs & 0x40 != 0 {
                        col = 7 - col;
                    }
                    let bit = 7 - col as u8;
                    let color = ((*lo >> bit) & 1) | ((*hi >> bit & 1) << 1);
                    if color == 0 {
                        continue;
                    }
                    let behind = sprite.attrs & 0x20 != 0;
                    if !(behind && bg_color != 0) {
                        entry = self.palette_byte(0x10 | ((sprite.attrs & 0x03) << 2) | color);
                    }
                    if sprite.is_sprite_zero && bg_color != 0 && bg_on {
                        sprite_zero_hit = true;
                    }
                    break;
                }
            }
            let rgb = pixel_rgb(entry, greyscale, self.mask);
            let at = (line * WIDTH + x) * 3;
            self.screen[at..at + 3].copy_from_slice(&rgb);
        }
        if sprite_zero_hit {
            self.status_v |= 0x40;
        }
    }

    /// The background tiles of the scanline, fetched through the PPU
    /// bus from the scroll register of the line start.
    fn fetch_background(&mut self, tick: Tick, ctx: &mut MachineCtx) -> [BgTile; LINE_TILES] {
        let coarse_y = (self.v >> 5) & 0x1F;
        let fine_y = (self.v >> 12) & 0x07;
        let nt_y_bit = (self.v >> 11) & 0x01;
        let coarse_x = self.v & 0x1F;
        let nt_x_bit = (self.v >> 10) & 0x01;
        // The scrolled Y of this scanline in the repeated 240-pixel
        // field; the vertical table bit flips at the wrap.
        let y_scroll = u32::from(nt_y_bit) * 240 + u32::from(coarse_y) * 8 + u32::from(fine_y);
        let y_flip = ((y_scroll / 240) & 0x01) as u16;
        let y_field = (y_scroll % 240) as u16;
        let tile_row = y_field / 8;
        let row = y_field % 8;
        let col_start = u32::from(nt_x_bit) * 32 + u32::from(coarse_x);
        let pattern_base = u16::from(self.ctrl & 0x10) << 12;
        let mut tiles = [BgTile::default(); LINE_TILES];
        for (i, tile_slot) in tiles.iter_mut().enumerate() {
            let col_abs = col_start + i as u32;
            let x_nt = ((col_abs / 32) & 0x01) as u16;
            let col = (col_abs % 32) as u16;
            let base = 0x2000 | (y_flip << 11) | (x_nt << 10) | (tile_row << 5) | col;
            let tile = self.nt_read(base);
            let attr_addr = 0x23C0 | (base & 0x0C00) | ((tile_row >> 2) << 3) | ((col >> 2) & 0x07);
            let attrs = self.nt_read(attr_addr);
            let quadrant = ((tile_row & 0x02) << 1) | (col & 0x02);
            let bits = (attrs >> quadrant) & 0x03;
            let pattern_addr = pattern_base | (u16::from(tile) << 4) | row;
            let lo = self.fetch(ctx, pattern_addr, tick);
            let hi = self.fetch(ctx, pattern_addr + 8, tick);
            *tile_slot = BgTile {
                lo,
                hi,
                attrs: bits,
            };
        }
        tiles
    }

    /// The two pattern bytes of one evaluated object, with the row
    /// flip and the 8-by-16 half selection.
    fn sprite_bytes(&mut self, sprite: &LineSprite, tick: Tick, ctx: &mut MachineCtx) -> (u8, u8) {
        let height16 = self.ctrl & 0x20 != 0;
        let flipped = sprite.attrs & 0x80 != 0;
        let row = if flipped {
            (if height16 { 15 } else { 7 }) - sprite.row
        } else {
            sprite.row
        };
        let addr = if height16 {
            // The tile number's low bit picks the pattern table, and
            // the row half picks the tile.
            let table = u16::from(sprite.tile & 0x01) << 12;
            let tile = sprite.tile & 0xFE;
            if row < 8 {
                table | u16::from(tile) << 4 | u16::from(row)
            } else {
                table | u16::from(tile + 1) << 4 | u16::from(row - 8)
            }
        } else {
            let table = u16::from(self.ctrl & 0x08) << 12;
            table | u16::from(sprite.tile) << 4 | u16::from(row)
        };
        let lo = self.fetch(ctx, addr, tick);
        let hi = self.fetch(ctx, addr + 8, tick);
        (lo, hi)
    }

    /// The pending data-port operations drain at the next visit,
    /// where the context routes through the PPU bus.
    fn drain_pending(&mut self, tick: Tick, ctx: &mut MachineCtx) {
        for (addr, value) in self.pending_writes.drain(..) {
            ctx.write(
                &self.pattern_bus,
                AccessWidth::Byte,
                Address(u32::from(addr)),
                u32::from(value),
                tick,
            );
        }
        if let Some(addr) = self.pending_read.take() {
            self.data_buffer = if addr < 0x2000 {
                self.fetch(ctx, addr, tick)
            } else {
                self.nt_read(addr)
            };
        }
        if self.nmi_update_pending {
            self.nmi_update_pending = false;
            self.update_nmi(tick, ctx);
        }
    }
}

/// The palette index of one raw index: the color-zero entries of each
/// column read the shared background color.
fn palette_index(index: u8) -> usize {
    if index & 0x03 == 0 {
        (index & 0x0F) as usize
    } else {
        (index & 0x1F) as usize
    }
}

/// The RGB888 bytes of one palette entry, with the greyscale mask and
/// the integer approximation of the emphasis dimming.
fn pixel_rgb(entry: u8, greyscale: bool, mask: u8) -> [u8; 3] {
    let mut rgb = PPU_PALETTE[usize::from(entry & 0x3F)];
    if greyscale {
        let green = rgb[1];
        rgb = [green, green, green];
    }
    let emphasis = (mask >> 5) & 0x07;
    if emphasis & 0x01 != 0 {
        rgb[1] = rgb[1] * 6 / 8;
        rgb[2] = rgb[2] * 6 / 8;
    }
    if emphasis & 0x02 != 0 {
        rgb[0] = rgb[0] * 6 / 8;
        rgb[2] = rgb[2] * 6 / 8;
    }
    if emphasis & 0x04 != 0 {
        rgb[0] = rgb[0] * 6 / 8;
        rgb[1] = rgb[1] * 6 / 8;
    }
    rgb
}

impl BusAccess for NesPpu {
    /// The register read. The status port clears the vblank flag; the
    /// NMI release lands on the next dot visit, because the bus
    /// access path holds no context.
    fn read(&mut self, _width: AccessWidth, address: Address, tick: Tick) -> u32 {
        match address.0 & 0x07 {
            0x02 => {
                let value = (self.status_v & 0xE0) | (self.data_buffer & 0x1F);
                self.status_v &= !0x80;
                self.write_toggle = false;
                self.status_read_tick = Some(tick);
                self.nmi_update_pending = true;
                u32::from(value)
            }
            0x04 => u32::from(self.oam[usize::from(self.oam_addr)]),
            0x07 => {
                let addr = self.v & 0x3FFF;
                let value = if addr & 0x3F00 == 0x3F00 {
                    self.palette_byte((addr & 0x1F) as u8)
                } else {
                    self.data_buffer
                };
                // The buffered fetch of the next read is pending; the
                // next visit fills the buffer. A palette read refills
                // from the name table byte the address mirrors.
                if addr & 0x3F00 == 0x3F00 {
                    self.pending_read = Some(0x2000 | (addr & 0x03FF));
                } else {
                    self.pending_read = Some(addr);
                }
                self.v = self
                    .v
                    .wrapping_add(if self.ctrl & 0x04 != 0 { 32 } else { 1 })
                    & 0x7FFF;
                u32::from(value)
            }
            _ => 0,
        }
    }

    fn write(&mut self, _width: AccessWidth, address: Address, value: u32, _tick: Tick) {
        let v = value as u8;
        match address.0 & 0x07 {
            0x00 => {
                self.ctrl = v;
                self.t = (self.t & 0x73FF) | (u16::from(v & 0x03) << 10);
                self.nmi_update_pending = true;
                self.write_toggle = false;
            }
            0x01 => self.mask = v,
            0x03 => self.oam_addr = v,
            0x04 => {
                self.oam[usize::from(self.oam_addr)] = v;
                self.oam_addr = self.oam_addr.wrapping_add(1);
            }
            0x05 => match self.write_toggle {
                false => {
                    self.fine_x = v & 0x07;
                    self.t = (self.t & 0x7FE0) | u16::from(v >> 3);
                    self.write_toggle = true;
                }
                _ => {
                    self.t = (self.t & 0x0C1F)
                        | (u16::from(v & 0x07) << 12)
                        | (u16::from(v & 0xF8) << 2);
                    self.write_toggle = false;
                }
            },
            0x06 => match self.write_toggle {
                false => {
                    self.t = (self.t & 0x00FF) | (u16::from(v & 0x3F) << 8);
                    self.write_toggle = true;
                }
                _ => {
                    self.t = (self.t & 0x7F00) | u16::from(v);
                    self.v = self.t;
                    self.write_toggle = false;
                }
            },
            0x07 => {
                let addr = self.v & 0x3FFF;
                match addr {
                    0x0000..=0x1FFF => {
                        self.pending_writes.push((addr, v));
                    }
                    0x2000..=0x3EFF => self.nt_write(addr, v),
                    _ => self.palette[palette_index((addr & 0x1F) as u8)] = v,
                }
                self.v = self
                    .v
                    .wrapping_add(if self.ctrl & 0x04 != 0 { 32 } else { 1 })
                    & 0x7FFF;
            }
            _ => {}
        }
    }

    /// The side-effect-free register read of the debug path.
    fn peek(&self, address: Address) -> u8 {
        match address.0 & 0x07 {
            0x02 => (self.status_v & 0xE0) | (self.data_buffer & 0x1F),
            0x04 => self.oam[usize::from(self.oam_addr)],
            _ => 0,
        }
    }
}

impl VideoSource for NesPpu {
    fn frame(&self) -> Option<&Frame> {
        Some(&self.cached_frame)
    }

    fn dump_layer(&self, layer: VideoLayer) -> Option<Vec<u8>> {
        match layer {
            VideoLayer::Vram => Some(self.ntram.to_vec()),
            VideoLayer::NameTable => Some(self.ntram.to_vec()),
            VideoLayer::Oam => Some(self.oam.to_vec()),
            VideoLayer::Palette => Some(self.palette.to_vec()),
            VideoLayer::Framebuffer => Some(self.screen.clone()),
        }
    }

    fn frames(&self) -> u64 {
        self.frames
    }
}

impl Device for NesPpu {
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
        self.ctrl = 0;
        self.mask = 0;
        self.status_v = 0;
        self.oam_addr = 0;
        self.data_buffer = 0;
        self.v = 0;
        self.t = 0;
        self.fine_x = 0;
        self.write_toggle = false;
        self.nmi_asserted = false;
        self.status_read_tick = None;
        self.pending_writes.clear();
        self.pending_read = None;
        self.nmi_update_pending = false;
        self.dot = 0;
        self.line = 0;
        self.line_sprites.clear();
        self.sprite_zero_on_line = false;
        self.frames = 0;
        self.next = None;
        self.last_tick = None;
    }

    fn boot(&mut self, state: Option<&BootState>) {
        let _ = state;
        self.reset();
        self.next = Some(Tick(0));
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(0x8C4);
        out.extend_from_slice(b"NPPU");
        out.push(1);
        out.push(self.ctrl);
        out.push(self.mask);
        out.push(self.status_v & 0xE0);
        out.push(self.oam_addr);
        out.push(self.data_buffer);
        out.extend_from_slice(&self.v.to_le_bytes());
        out.extend_from_slice(&self.t.to_le_bytes());
        out.push(self.fine_x);
        out.push(u8::from(self.write_toggle));
        out.push(u8::from(self.nmi_asserted));
        out.extend_from_slice(&self.dot.to_le_bytes());
        out.extend_from_slice(&self.line.to_le_bytes());
        out.push(self.mirroring);
        out.extend_from_slice(&self.frames.to_le_bytes());
        out.extend_from_slice(&self.oam[..]);
        out.extend_from_slice(&self.palette);
        out.extend_from_slice(&self.ntram[..]);
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        let expected = 4 + 1 + 5 + 4 + 3 + 10 + 9 + OAM_SIZE + 0x20 + 0x800;
        if data.len() != expected || &data[..4] != b"NPPU" || data[4] != 1 {
            return Err(SnapshotError::Mismatch("bad nes ppu chunk".into()));
        }
        let mut pos = 5usize;
        self.ctrl = data[pos];
        pos += 1;
        self.mask = data[pos];
        pos += 1;
        self.status_v = data[pos] & 0xE0;
        pos += 1;
        self.oam_addr = data[pos];
        pos += 1;
        self.data_buffer = data[pos];
        pos += 1;
        self.v = u16::from_le_bytes(data[pos..pos + 2].try_into().expect("2 bytes"));
        pos += 2;
        self.t = u16::from_le_bytes(data[pos..pos + 2].try_into().expect("2 bytes"));
        pos += 2;
        self.fine_x = data[pos];
        pos += 1;
        self.write_toggle = data[pos] != 0;
        pos += 1;
        self.nmi_asserted = data[pos] != 0;
        pos += 1;
        self.dot = u64::from_le_bytes(data[pos..pos + 8].try_into().expect("8 bytes"));
        pos += 8;
        self.line = u16::from_le_bytes(data[pos..pos + 2].try_into().expect("2 bytes"));
        pos += 2;
        self.mirroring = data[pos] & 0x01;
        pos += 1;
        self.frames = u64::from_le_bytes(data[pos..pos + 8].try_into().expect("8 bytes"));
        pos += 8;
        self.oam.copy_from_slice(&data[pos..pos + OAM_SIZE]);
        pos += OAM_SIZE;
        self.palette.copy_from_slice(&data[pos..pos + 0x20]);
        pos += 0x20;
        self.ntram.copy_from_slice(&data[pos..pos + 0x800]);
        self.line_sprites.clear();
        self.sprite_zero_on_line = false;
        self.status_read_tick = None;
        self.pending_writes.clear();
        self.pending_read = None;
        self.nmi_update_pending = false;
        self.next = None;
        self.last_tick = None;
        Ok(())
    }

    fn notify_line(&mut self, line: &str, _level: Level, _tick: Tick) {
        let _ = line;
    }

    fn next_event(&self) -> Option<Tick> {
        self.next
    }

    fn tick(&mut self, tick: Tick, ctx: &mut MachineCtx) {
        if self.last_tick.is_some_and(|t| t.0 >= tick.0) {
            return;
        }
        self.last_tick = Some(tick);
        self.drain_pending(tick, ctx);
        self.step(tick, ctx);
        self.next = Some(Tick(tick.0 + self.dot_div));
    }
}
