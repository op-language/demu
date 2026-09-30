//! The SM83 PPU.
//!
//! The pixel processing unit walks one dot per domain tick, 70224 dots
//! per frame across 154 lines. Modes 2, 3, and 0 share the visible
//! lines; mode 1 fills the rest. Mode 3 length follows the scroll,
//! window, and object penalties of the real pipeline, and the line
//! renders from the register state at its start, so per-line raster
//! effects are exact. The CGB adds the second VRAM bank, the color
//! palettes, and the object priority mode.

use demu_config::model::BootState;
use demu_core::device::{AccessWidth, BusAccess, Frame, MachineCtx, VideoLayer, VideoSource};
use demu_core::frequency::{Address, Tick};
use demu_core::signal::Level;
use demu_core::{Device, Snapshot, SnapshotError};

/// The dots per scanline.
const LINE_DOTS: u64 = 456;
/// The visible lines.
const VISIBLE: u64 = 144;
/// The total lines per frame.
const LINES: u64 = 154;
/// The LCD width.
const WIDTH: usize = 160;
/// The LCD height.
const HEIGHT: usize = 144;

/// One evaluated object line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LineSprite {
    /// The OAM index.
    index: u8,
    /// The row inside the object tile.
    row: u8,
    /// The leftmost screen column minus eight.
    x: i32,
    /// The tile index.
    tile: u8,
    /// The attributes byte.
    attrs: u8,
}

/// The pixel-processing unit.
pub struct Ppu {
    name: String,
    cgb: bool,
    /// The DMG OAM corruption bug applies.
    pub oam_bug: bool,
    /// The two 4 KiB VRAM banks.
    vram: Vec<Box<[u8]>>,
    /// The VRAM bank select.
    vram_bank: usize,
    /// The object attribute memory.
    oam: Box<[u8; 0xA0]>,
    // Registers.
    lcdc: u8,
    stat: u8,
    scy: u8,
    scx: u8,
    lyc: u8,
    bgp: u8,
    obp: [u8; 2],
    wy: u8,
    wx: u8,
    /// The CGB background palette index and data.
    bgpi: u8,
    bgpd: [u8; 0x40],
    /// The object palette index and data.
    obpi: u8,
    obpd: [u8; 0x20],
    /// The object priority mode.
    opri: u8,
    /// The monochrome compatibility mode from `KEY0`.
    dmg_compat: bool,
    // The dot pipeline.
    /// The dots since boot.
    dot: u64,
    mode_dot: u32,
    mode_len: u32,
    /// The window's internal line counter.
    window_line: u8,
    /// The window renders on the current line.
    window_on_line: bool,
    /// The screen column where the window starts.
    window_start: usize,
    /// The evaluated objects on the current line, leftmost first.
    line_sprites: Vec<LineSprite>,
    /// The active PPU mode; transitions fire once.
    mode: u8,
    /// The penalty dots of the current line.
    line_penalty: u32,
    /// The composed line, by screen column.
    line_pixels: [u8; WIDTH],
    /// The composed frame in RGB888.
    screen: Vec<u8>,
    /// The view for the shell's screenshot commands.
    cached_frame: Frame,
    frames: u64,
    last_tick: Option<Tick>,
    next: Option<Tick>,
}

impl Ppu {
    /// A pixel-processing unit from its configuration entry.
    pub fn new(device: &demu_config::model::DeviceInstance) -> Self {
        let cgb = device
            .params
            .get("cgb")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let vram = if cgb {
            vec![
                vec![0; 0x2000].into_boxed_slice(),
                vec![0; 0x2000].into_boxed_slice(),
            ]
        } else {
            vec![vec![0; 0x2000].into_boxed_slice()]
        };
        Self {
            name: device.name.to_string(),
            cgb,
            oam_bug: true,
            vram,
            vram_bank: 0,
            oam: Box::new([0u8; 0xA0]),
            lcdc: 0x91,
            stat: 0x85,
            scy: 0,
            scx: 0,
            lyc: 0,
            bgp: 0xFC,
            obp: [0xFF, 0xFF],
            wy: 0,
            wx: 0,
            bgpi: 0,
            bgpd: [0xFF; 0x40],
            obpi: 0,
            obpd: [0xFF; 0x20],
            opri: 0,
            dmg_compat: false,
            dot: 0,
            mode_dot: 0,
            mode_len: 4560,
            window_line: 0,
            window_on_line: false,
            window_start: WIDTH,
            line_sprites: Vec::with_capacity(11),
            mode: 1,
            line_penalty: 0,
            line_pixels: [0; WIDTH],
            screen: vec![0; WIDTH * HEIGHT * 3],
            cached_frame: Frame {
                width: WIDTH as u32,
                height: HEIGHT as u32,
                pixels: vec![255; WIDTH * HEIGHT * 3],
            },
            frames: 0,
            last_tick: None,
            next: None,
        }
    }

    /// The line of one dot, honoring the short line 153.
    fn ly_of(dot: u64) -> u8 {
        let line = (dot / LINE_DOTS) % LINES;
        // The last line reads 153 for four dots and zero afterwards.
        if line == 153 && dot % LINE_DOTS >= 4 {
            0
        } else {
            line as u8
        }
    }

    /// The current line.
    fn ly(&self) -> u8 {
        Self::ly_of(self.dot)
    }

    /// True while the LCD is enabled.
    fn lcd_on(&self) -> bool {
        self.lcdc & 0x80 != 0
    }

    /// The dots into the current line.
    fn line_dot(&self) -> u32 {
        (self.dot % LINE_DOTS) as u32
    }

    /// The reported STAT mode bits.
    fn reported_mode(&self) -> u8 {
        if !self.lcd_on() {
            return 0;
        }
        if self.ly() >= VISIBLE as u8 {
            1
        } else if self.line_dot() < 80 {
            2
        } else if self.line_dot() < 80 + self.mode_len {
            3
        } else {
            0
        }
    }

    /// Re-evaluate the `LYC == LY` condition and its interrupt.
    fn check_lyc(&mut self, tick: Tick, ctx: &mut MachineCtx) {
        let flag = self.lyc == self.ly();
        let was = self.stat & 0x04 != 0;
        if flag {
            self.stat |= 0x04;
            if !was && self.stat & 0x40 != 0 {
                ctx.notify("stat", Level::High, tick);
            }
        } else {
            self.stat &= !0x04;
        }
    }

    /// Raise one mode interrupt on entry, plus the vblank and hblank
    /// edges.
    fn enter_mode(&mut self, mode: u8, tick: Tick, ctx: &mut MachineCtx) {
        let int_bit = match mode {
            2 => 0x20,
            1 => 0x10,
            0 => 0x08,
            _ => 0,
        };
        if int_bit != 0 && self.stat & int_bit != 0 {
            ctx.notify("stat", Level::High, tick);
        }
        if mode == 1 {
            ctx.notify("vblank", Level::High, tick);
            self.refresh_cached_frame();
            self.frames += 1;
        }
        if mode == 0 {
            ctx.notify("hblank", Level::High, tick);
        }
    }

    /// The mode-3 length of the current line in dots.
    fn mode3_len(&self) -> u32 {
        172 + u32::from(self.scx % 8) + self.line_penalty + u32::from(self.window_on_line) * 6
    }

    /// Evaluate the objects on the current line, leftmost first.
    fn scan_sprites(&mut self) {
        self.line_sprites.clear();
        self.line_penalty = 0;
        let height = if self.lcdc & 0x04 != 0 { 16 } else { 8 };
        let ly = self.ly();
        let mut found: Vec<LineSprite> = Vec::with_capacity(11);
        for i in 0..40u8 {
            let y = self.oam[usize::from(i) * 4];
            let row = i32::from(ly) + 16 - i32::from(y);
            if row < 0 || row >= height {
                continue;
            }
            found.push(LineSprite {
                index: i,
                row: row as u8,
                x: i32::from(self.oam[usize::from(i) * 4 + 1]) - 8,
                tile: self.oam[usize::from(i) * 4 + 2],
                attrs: self.oam[usize::from(i) * 4 + 3],
            });
        }
        // The CGB sorts by OAM index; the DMG sorts by X position.
        let dmg_style = !self.cgb || self.opri & 1 == 1;
        if dmg_style {
            found.sort_by_key(|s| (s.x.max(0) as u8, s.index));
        } else {
            found.sort_by_key(|s| (s.index, s.x.max(0) as u8));
        }
        found.truncate(10);
        self.line_sprites = found;
        // The object penalties: a flat six dots per drawn object and
        // eleven for the objects with X position zero.
        for sprite in &self.line_sprites {
            self.line_penalty += if sprite.x <= -8 { 11 } else { 6 };
        }
    }

    /// Render the current line into the frame.
    ///
    /// The background and window layers compose per column, and the
    /// objects overlay in priority order.
    fn render_line(&mut self, ly: u8) {
        let window_on_line = self.lcdc & 0x20 != 0
            && u32::from(self.wy) <= u32::from(ly)
            && self.wx <= 166
            && self.wx.wrapping_sub(7) < WIDTH as u8;
        self.window_on_line = window_on_line;
        self.window_start = if self.wx >= 7 {
            usize::from(self.wx) - 7
        } else {
            0
        };
        for x in 0..WIDTH {
            let entry = self.compose_pixel(x, ly);
            self.line_pixels[x] = entry;
            let rgb = self.screen_rgb(entry, ly, x);
            let base = (usize::from(ly) * WIDTH + x) * 3;
            self.screen[base..base + 3].copy_from_slice(&rgb);
        }
        // The window's internal line advances once per rendered line.
        if self.window_on_line {
            self.window_line = self.window_line.wrapping_add(1);
        }
    }

    /// The VRAM byte in the active bank.
    fn vram_byte(&self, addr: usize) -> u8 {
        let bank = if self.cgb { self.vram_bank } else { 0 };
        self.vram[bank][addr & 0x1FFF]
    }

    /// A VRAM byte from one bank.
    fn vram_bank_byte(&self, bank: usize, addr: usize) -> u8 {
        self.vram[bank.min(self.vram.len() - 1)][addr & 0x1FFF]
    }

    /// The two-bit color of one tile pixel.
    fn tile_color(&self, entry: u8, row: u8, col: u8, bank: usize, attrs: u8) -> u8 {
        let col = if self.cgb && attrs & 0x20 != 0 {
            7 - col
        } else {
            col
        };
        let row = if self.cgb && attrs & 0x40 != 0 {
            7 - row
        } else {
            row
        };
        let signed = self.lcdc & 0x10 == 0 && !self.cgb;
        let base = if signed {
            0x9000usize.wrapping_add_signed(isize::from(entry as i8) * 16)
        } else {
            0x8000 + usize::from(entry) * 16
        };
        let addr = base + usize::from(row) * 2;
        let lo = self.vram_bank_byte(bank, addr);
        let hi = self.vram_bank_byte(bank, addr.wrapping_add(1));
        let bit = 7 - col;
        ((hi >> bit) & 1) << 1 | ((lo >> bit) & 1)
    }

    /// The DMG shade of one two-bit color through a palette byte.
    fn shade(&self, palette: u8, color: u8) -> u8 {
        (palette >> (color * 2)) & 0x03
    }

    /// The map entry of one background or window column, from bank 0.
    fn map_entry_at(&self, x: usize, ly: u8) -> u8 {
        let index = self.map_index_at(x, ly);
        self.vram_bank_byte(0, self.map_base_at(x) + index)
    }

    /// The attributes byte of the map entry at one column, from bank 1.
    fn map_attrs_at(&self, x: usize, _ly: u8) -> u8 {
        if !self.cgb {
            return 0;
        }
        self.vram_bank_byte(1, self.map_base_at(x) + self.map_index_at(x, _ly))
    }

    /// The map base for one column.
    fn map_base_at(&self, x: usize) -> usize {
        let window = self.window_on_line && x >= self.window_start;
        if (window && self.lcdc & 0x40 != 0) || (!window && self.lcdc & 0x08 != 0) {
            0x1C00
        } else {
            0x1800
        }
    }

    /// The map-entry index shared by the entry and attribute reads.
    fn map_index_at(&self, x: usize, ly: u8) -> usize {
        let window = self.window_on_line && x >= self.window_start;
        let (row, col): (u32, u32) = if window {
            (
                u32::from(self.window_line) / 8,
                u32::try_from(x - self.window_start).unwrap_or(0) / 8,
            )
        } else {
            let scroll_x = u32::from(self.scx) + x as u32;
            ((u32::from(ly) + u32::from(self.scy)) / 8, scroll_x / 8)
        };
        ((row % 32) * 32 + (col % 32)) as usize & 0x3FF
    }

    /// Compose one screen column: background, window, then objects in
    /// priority order.
    fn compose_pixel(&mut self, x: usize, ly: u8) -> u8 {
        let entry = self.map_entry_at(x, ly);
        let window = self.window_on_line && x >= self.window_start;
        let attrs = self.map_attrs_at(x, ly);
        let bank = if self.cgb && attrs & 0x08 != 0 { 1 } else { 0 };
        let (row, col) = if window {
            (
                self.window_line % 8,
                u8::try_from((x - self.window_start) % 8).unwrap_or(0),
            )
        } else {
            let scroll_x = u32::from(self.scx) + x as u32;
            let scy = u32::from(self.scy);
            (((u32::from(ly) + scy) % 8) as u8, (scroll_x % 8) as u8)
        };
        let bg_color = self.tile_color(entry, row, col, bank, attrs);
        let bg_index = self.shade(self.bgp, bg_color);
        if self.lcdc & 0x02 != 0 {
            for sprite in &self.line_sprites {
                let left = sprite.x;
                if left > x as i32 || left + 8 <= x as i32 {
                    continue;
                }
                let color = self.obj_color_at(sprite, self.obj_col_for(sprite, x));
                if color == 0 {
                    continue;
                }
                if self.cgb && sprite.attrs & 0x80 != 0 && bg_color != 0 {
                    continue;
                }
                if self.cgb && !self.dmg_compat {
                    // The composed line carries the palette entry index;
                    // the frame conversion applies the RGB555 color.
                    let base = usize::from(sprite.attrs & 0x07) * 8 + usize::from(color) * 2;
                    return u8::try_from(0x80 + base / 2).unwrap_or(0);
                }
                let palette = self.obp[usize::from((sprite.attrs >> 2) & 1)];
                return self.shade(palette, color);
            }
        }
        bg_index
    }

    /// The two-bit color of one object pixel.
    fn obj_color_at(&self, sprite: &LineSprite, col: u8) -> u8 {
        let bank = if self.cgb && sprite.attrs & 0x08 != 0 {
            1
        } else {
            0
        };
        let base = 0x8000 + usize::from(sprite.tile) * 16;
        let addr = base + usize::from(sprite.row) * 2;
        let lo = self.vram_bank_byte(bank, addr);
        let hi = self.vram_bank_byte(bank, addr.wrapping_add(1));
        let bit = 7 - col;
        ((hi >> bit) & 1) << 1 | ((lo >> bit) & 1)
    }

    /// The object pixel column, honoring the horizontal flip.
    fn obj_col_for(&self, sprite: &LineSprite, x: usize) -> u8 {
        let raw = x as i32 - sprite.x.max(0);
        let col = u8::try_from(raw.max(0)).unwrap_or(0);
        if sprite.attrs & 0x20 != 0 {
            7 - col
        } else {
            col
        }
    }
}
impl Ppu {
    /// The RGB888 pixel of one CGB palette entry, with bit replication
    /// from 5-bit channels. Entries carry the background palettes and,
    /// from $80, the object palettes.
    fn cgb_entry(&self, entry: usize) -> [u8; 3] {
        if entry >= 0x80 {
            let lo = self.obpd[entry - 0x80];
            let hi = self.obpd[entry - 0x80 + 1];
            let value = u16::from(lo) | (u16::from(hi) << 8);
            let r = (value & 0x1F) as u8;
            let g = ((value >> 5) & 0x1F) as u8;
            let b = ((value >> 10) & 0x1F) as u8;
            return [
                (r << 3) | (r >> 2),
                (g << 3) | (g >> 2),
                (b << 3) | (b >> 2),
            ];
        }
        let lo = self.bgpd[entry * 2];
        let hi = self.bgpd[entry * 2 + 1];
        let value = u16::from(lo) | (u16::from(hi) << 8);
        let r = (value & 0x1F) as u8;
        let g = ((value >> 5) & 0x1F) as u8;
        let b = ((value >> 10) & 0x1F) as u8;
        [
            (r << 3) | (r >> 2),
            (g << 3) | (g >> 2),
            (b << 3) | (b >> 2),
        ]
    }

    /// Copy the composed screen into the cached frame for the shell.
    fn refresh_cached_frame(&mut self) {
        self.cached_frame.pixels = self.screen.clone();
    }

    /// The RGB888 bytes of one composed line entry.
    fn screen_rgb(&self, entry: u8, _ly: u8, _x: usize) -> [u8; 3] {
        if self.cgb && !self.dmg_compat {
            return self.cgb_entry(usize::from(entry) & 0x3F);
        }
        let shade = entry & 0x03;
        let gray = 255 - shade * 85;
        [gray, gray, gray]
    }
}

impl Ppu {
    /// The tick visit: advance the dot, walk the mode machine.
    fn step(&mut self, tick: Tick, ctx: &mut MachineCtx) {
        self.dot += 1;
        self.mode_dot += 1;
        if !self.lcd_on() {
            // The LCD reads zero and the pipeline rests; the write to
            // LCDC that re-enables it re-seeds the schedule.
            self.next = None;
            return;
        }
        match self.ly() {
            0..=143 => match self.line_dot() {
                0 => {
                    self.scan_sprites();
                    self.enter_mode(2, tick, ctx);
                }
                80 => {
                    self.render_line(self.ly());
                    self.mode_len = self.mode3_len();
                    self.enter_mode(3, tick, ctx);
                }
                d if d >= 80 + self.mode_len => {
                    self.enter_mode(0, tick, ctx);
                }
                _ => {}
            },
            144 => {
                self.enter_mode(1, tick, ctx);
            }
            _ => {}
        }
        if self.line_dot() == 0 {
            // A new line begins: update LY and the compare flag.
            self.check_lyc(tick, ctx);
        }
    }
}

impl Device for Ppu {
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
        self.lcdc = 0x91;
        self.stat = 0x85;
        self.scy = 0;
        self.scx = 0;
        self.lyc = 0;
        self.bgp = 0xFC;
        self.obp = [0xFF, 0xFF];
        self.wy = 0;
        self.wx = 0;
        self.dot = 0;
        self.mode_dot = 0;
        self.window_line = 0;
        self.frames = 0;
        self.next = None;
        self.last_tick = None;
    }

    fn boot(&mut self, state: Option<&BootState>) {
        let _ = state;
        self.reset();
        self.next = Some(Tick(1));
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(0x2200);
        out.extend_from_slice(b"PPU1");
        out.extend_from_slice(&self.dot.to_le_bytes());
        out.extend_from_slice(&self.mode_dot.to_le_bytes());
        out.extend_from_slice(&self.mode_len.to_le_bytes());
        out.push(self.window_line);
        out.push(self.vram_bank as u8);
        out.push(u8::from(self.window_on_line));
        out.extend_from_slice(&self.window_start.to_le_bytes());
        out.push(self.mode);
        out.push(self.lcdc);
        out.push(self.stat);
        out.push(self.scy);
        out.push(self.scx);
        out.push(self.lyc);
        out.push(self.bgp);
        out.extend_from_slice(&self.obp);
        out.push(self.wy);
        out.push(self.wx);
        out.push(self.bgpi);
        out.extend_from_slice(&self.bgpd);
        out.push(self.obpi);
        out.extend_from_slice(&self.obpd);
        out.push(self.opri);
        out.push(u8::from(self.dmg_compat));
        out.push(self.line_penalty as u8);
        out.extend_from_slice(&self.line_pixels);
        for bank in &self.vram {
            out.extend_from_slice(bank);
        }
        out.extend_from_slice(&self.oam[..]);
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        let expected = expected_size(self.cgb);
        if data.len() != expected || &data[..4] != b"PPU1" {
            return Err(SnapshotError::Mismatch("bad ppu chunk".into()));
        }
        let mut pos = 4usize;
        self.dot = u64::from_le_bytes(data[pos..pos + 8].try_into().expect("8 bytes"));
        pos += 8;
        self.mode_dot = u32::from_le_bytes(data[pos..pos + 4].try_into().expect("4 bytes"));
        pos += 4;
        self.mode_len = u32::from_le_bytes(data[pos..pos + 4].try_into().expect("4 bytes"));
        pos += 4;
        self.window_line = data[pos];
        pos += 1;
        self.vram_bank = usize::from(data[pos] & 1);
        pos += 1;
        self.window_on_line = data[pos] != 0;
        pos += 1;
        self.window_start = usize::try_from(u64::from_le_bytes(
            data[pos..pos + 8].try_into().expect("8 bytes"),
        ))
        .unwrap_or(WIDTH);
        pos += 8;
        self.mode = data[pos];
        pos += 1;
        self.lcdc = data[pos];
        pos += 1;
        self.stat = data[pos];
        pos += 1;
        self.scy = data[pos];
        pos += 1;
        self.scx = data[pos];
        pos += 1;
        self.lyc = data[pos];
        pos += 1;
        self.bgp = data[pos];
        pos += 1;
        self.obp.copy_from_slice(&data[pos..pos + 2]);
        pos += 2;
        self.wy = data[pos];
        pos += 1;
        self.wx = data[pos];
        pos += 1;
        self.bgpi = data[pos];
        pos += 1;
        self.bgpd.copy_from_slice(&data[pos..pos + 0x40]);
        pos += 0x40;
        self.obpi = data[pos];
        pos += 1;
        self.obpd.copy_from_slice(&data[pos..pos + 0x20]);
        pos += 0x20;
        self.opri = data[pos];
        pos += 1;
        self.dmg_compat = data[pos] != 0;
        pos += 1;
        self.line_penalty = u32::from(data[pos]);
        pos += 1;
        self.line_pixels.copy_from_slice(&data[pos..pos + WIDTH]);
        pos += WIDTH;
        for bank in &mut self.vram {
            bank.copy_from_slice(&data[pos..pos + 0x2000]);
            pos += 0x2000;
        }
        self.oam.copy_from_slice(&data[pos..pos + 0xA0]);
        self.next = None;
        self.last_tick = None;
        Ok(())
    }

    fn notify_line(&mut self, line: &str, _level: Level, _tick: Tick) {
        // The monochrome compatibility mode from KEY0.
        if line == "key0" {
            self.dmg_compat = true;
        }
        if line == "oam_bug" {
            self.corrupt_oam();
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
        self.step(tick, ctx);
        self.next = if self.lcd_on() {
            Some(Tick(tick.0 + 1))
        } else {
            None
        };
    }
}

fn expected_size(cgb: bool) -> usize {
    let banks = usize::from(cgb) + 1;
    let fixed = 4 + 8 + 4 + 4 + 1 + 1 + 1 + 8 + 1 + 8 + 3 + 0x40 + 1 + 0x20 + 1 + 1 + 1 + 0xA0;
    fixed + WIDTH + banks * 0x2000
}

impl Ppu {
    /// The object corruption of one OAM access during OAM scan.
    fn corrupt_oam(&mut self) {
        if !self.oam_bug || self.ly() >= VISIBLE as u8 || self.line_dot() >= 80 {
            return;
        }
        let row = self.line_dot() as usize / 4;
        if row == 0 {
            return;
        }
        let a = [self.oam[row * 8], self.oam[row * 8 + 1]];
        let b = [self.oam[(row - 1) * 8], self.oam[(row - 1) * 8 + 1]];
        let c = [self.oam[(row - 1) * 8 + 4], self.oam[(row - 1) * 8 + 5]];
        // The first word of the row: ((a ^ c) & (b ^ c)) ^ c per word.
        let first: [u8; 2] = std::array::from_fn(|i| ((a[i] ^ c[i]) & (b[i] ^ c[i])) ^ c[i]);
        self.oam[row * 8] = first[0];
        self.oam[row * 8 + 1] = first[1];
        for w in 1..4 {
            self.oam[row * 8 + w * 2] = self.oam[(row - 1) * 8 + w * 2];
            self.oam[row * 8 + w * 2 + 1] = self.oam[(row - 1) * 8 + w * 2 + 1];
        }
    }
}

impl BusAccess for Ppu {
    fn read(&mut self, _width: AccessWidth, address: Address, _tick: Tick) -> u32 {
        match address.0 {
            0x8000..=0x9FFF => {
                if self.vram_locked() {
                    return 0xFF;
                }
                u32::from(self.vram_byte(address.0 as usize & 0x1FFF))
            }
            0xFE00..=0xFE9F => {
                if self.oam_locked() {
                    return 0xFF;
                }
                u32::from(self.oam[(address.0 - 0xFE00) as usize])
            }
            0xFEA0..=0xFEFF => 0x00,
            0xFF40 => u32::from(self.lcdc),
            0xFF41 => u32::from(self.stat & !0x07 | self.reported_mode()),
            0xFF42 => u32::from(self.scy),
            0xFF43 => u32::from(self.scx),
            0xFF44 => u32::from(if self.lcd_on() { self.ly() } else { 0 }),
            0xFF45 => u32::from(self.lyc),
            0xFF47 => u32::from(self.bgp),
            0xFF48 => u32::from(self.obp[0]),
            0xFF49 => u32::from(self.obp[1]),
            0xFF4A => u32::from(self.wy),
            0xFF4B => u32::from(self.wx),
            0xFF4F => u32::from(0xFE | u8::from(self.vram_bank != 0)),
            0xFF68 => u32::from(self.bgpi) | 0x40,
            0xFF69 => u32::from(self.bgpd[usize::from(self.bgpi & 0x3F)]),
            0xFF6A => u32::from(self.obpi) | 0x40,
            0xFF6B => u32::from(self.obpd[usize::from(self.obpi & 0x1F)]),
            0xFF6C => u32::from(self.opri) | 0xFE,
            _ => 0xFF,
        }
    }

    fn write(&mut self, _width: AccessWidth, address: Address, value: u32, tick: Tick) {
        let _ = tick;
        let v = value as u8;
        match address.0 {
            0x8000..=0x9FFF => {
                if !self.vram_locked() {
                    let bank = if self.cgb { self.vram_bank } else { 0 };
                    self.vram[bank][address.0 as usize & 0x1FFF] = v;
                }
            }
            0xFE00..=0xFE9F => {
                if !self.oam_locked() {
                    self.oam[(address.0 - 0xFE00) as usize] = v;
                }
            }
            0xFF40 => {
                let was = self.lcd_on();
                self.lcdc = v;
                if v & 0x80 == 0 {
                    // The LCD stops: the line freezes at zero and the
                    // pipeline resets.
                    self.dot = 0;
                    self.mode_dot = 0;
                    self.window_line = 0;
                    self.next = None;
                    self.last_tick = None;
                } else if !was {
                    // The restart schedules from the write's tick.
                    self.dot = 0;
                    self.mode_dot = 0;
                    self.next = Some(Tick(tick.0 + 1));
                    self.last_tick = Some(tick);
                }
            }
            0xFF41 => {
                self.stat = (self.stat & 0x87) | (v & 0x78);
                if !self.cgb && self.lcd_on() && self.spurious_stat_write() {
                    self.stat |= 0x02;
                }
            }
            0xFF42 => self.scy = v,
            0xFF43 => self.scx = v,
            0xFF45 => self.lyc = v,
            0xFF47 => self.bgp = v,
            0xFF48 => self.obp[0] = v,
            0xFF49 => self.obp[1] = v,
            0xFF4A => self.wy = v,
            0xFF4B => self.wx = v,
            0xFF4F => self.vram_bank = usize::from(v & 0x01),
            0xFF68 => self.bgpi = v & 0xBF,
            0xFF69 => {
                let i = usize::from(self.bgpi & 0x3F);
                self.bgpd[i] = v;
                if self.bgpi & 0x80 != 0 {
                    self.bgpi = (self.bgpi & 0x3F).wrapping_add(1) & 0x3F | 0x80;
                }
            }
            0xFF6A => self.obpi = v & 0x9F,
            0xFF6B => {
                let i = usize::from(self.obpi & 0x1F);
                self.obpd[i] = v;
                if self.obpi & 0x80 != 0 {
                    self.obpi = ((self.obpi & 0x1F).wrapping_add(1) & 0x1F) | 0x80;
                }
            }
            0xFF6C => self.opri = v & 0x01,
            _ => {}
        }
    }

    fn peek(&self, address: Address) -> u8 {
        match address.0 {
            0x8000..=0x9FFF => self.vram_byte(address.0 as usize & 0x1FFF),
            0xFE00..=0xFE9F => self.oam[(address.0 - 0xFE00) as usize],
            0xFF40 => self.lcdc,
            0xFF41 => (self.stat & !0x07) | self.reported_mode(),
            0xFF42 => self.scy,
            0xFF43 => self.scx,
            0xFF44 => {
                if self.lcd_on() {
                    self.ly()
                } else {
                    0
                }
            }
            0xFF45 => self.lyc,
            0xFF47 => self.bgp,
            0xFF48 => self.obp[0],
            0xFF49 => self.obp[1],
            0xFF4A => self.wy,
            0xFF4B => self.wx,
            0xFF4F => 0xFE | u8::from(self.vram_bank != 0),
            0xFF68 => self.bgpi | 0x40,
            0xFF69 => self.bgpd[usize::from(self.bgpi & 0x3F)],
            0xFF6A => self.obpi | 0x40,
            0xFF6B => self.obpd[usize::from(self.obpi & 0x1F)],
            0xFF6C => self.opri | 0xFE,
            _ => 0xFF,
        }
    }

    fn decodes(&self, _address: Address) -> bool {
        true
    }
}

impl Ppu {
    /// The OAM is inaccessible during OAM scan and rendering.
    fn oam_locked(&self) -> bool {
        self.lcd_on() && self.ly() < VISIBLE as u8 && self.line_dot() < 80 + self.mode_len
    }

    /// The VRAM is inaccessible during rendering.
    fn vram_locked(&self) -> bool {
        self.lcd_on()
            && self.ly() < VISIBLE as u8
            && self.line_dot() >= 80
            && self.line_dot() < 80 + self.mode_len
    }

    /// True on a STAT write that requests the spurious monochrome
    /// interrupt.
    fn spurious_stat_write(&self) -> bool {
        self.ly() >= VISIBLE as u8 || self.line_dot() < 80 + self.mode_len || self.stat & 0x04 != 0
    }
}

impl VideoSource for Ppu {
    fn frame(&self) -> Option<&Frame> {
        Some(&self.cached_frame)
    }

    fn dump_layer(&self, layer: VideoLayer) -> Option<Vec<u8>> {
        match layer {
            VideoLayer::Vram => Some(self.vram.iter().flat_map(|b| b.iter().copied()).collect()),
            VideoLayer::NameTable => Some(
                self.vram
                    .iter()
                    .flat_map(|b| b[0x1800..0x2000].iter().copied())
                    .collect(),
            ),
            VideoLayer::Oam => Some(self.oam.to_vec()),
            VideoLayer::Palette => Some(self.bgpd.to_vec()),
            VideoLayer::Framebuffer => Some(self.screen.clone()),
        }
    }

    fn frames(&self) -> u64 {
        self.frames
    }
}
