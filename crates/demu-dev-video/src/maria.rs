//! The MARIA graphics processor of the 7800.
//!
//! MARIA renders each scanline by direct-memory access: a display
//! list list (DLL) in RAM names a display list per zone of scanlines,
//! and each display list entry is a four- or five-byte header that
//! names graphics data, a palette, a width, and a horizontal position.
//! Graphics data writes into an internal line RAM through DMA cycles,
//! which steal machine cycles from the CPU through the halt line.
//!
//! The register file sits at `$0020-$003F` in four-byte slots: within
//! each slot the offset selects the chip register and the palette
//! colors, one color per slot offset 1 through 3. BACKGRND is the
//! slot zero offset, WSYNC the slot one strobe, MSTAT the slot two
//! status, and the display-list pointers, the character base, the
//! legacy offset register, and CTRL fill the zero offsets of the rest.
//!
//! Display list list entries are three bytes: flags with the zone
//! count, then the display-list address high byte and low byte. The
//! walker reads one entry per zone and walks the named display list
//! on every raster the zone covers, one more than the count in the
//! flag byte low nibble. When the count expires the walker reads the
//! next entry; three zero bytes are the empty terminator, and the
//! walk parks until the frame re-arm puts the DLL pointer back. The
//! walk window opens one raster before the visible one, so the walk
//! of a raster fills the visible row that follows it. Mode
//! lines come in direct and indirect (character) forms, in 160 and
//! 320 pixel encodings; the pixel colors read the palette entries
//! whose 8-bit value carries chroma in bits 7-4 and luma in bits 3-0.
//!
//! DMA cost model: the machine counts MARIA cycles per line, and the
//! exact startup, header, and shutdown costs of the chip manual
//! become halt cycles on the CPU. The intra-line interleave of DMA
//! and CPU work is approximate; the technical design records this.

use demu_config::model::BootState;
use demu_core::device::{AccessWidth, BusAccess, Frame, MachineCtx, VideoLayer, VideoSource};
use demu_core::frequency::{Address, Tick};
use demu_core::signal::Level;
use demu_core::triggers::TriggerEvent;
use demu_core::{Device, Snapshot, SnapshotError};

use crate::tia_color_rgb;

/// MARIA cycles per scanline: 456 cycles of the 7.16 MHz pixel clock.
const LINE_CYCLES: u32 = 456;
/// Scanlines per frame.
const LINES: u32 = 262;
/// Visible rasters of the frame.
const FRAME_LINES: usize = 192;
/// The first raster of the visible window.
const TOP_RASTER: u32 = 41;
/// The raster the walk re-arms on: one raster before the visible
/// window, whose compose fills the window's first row.
const WALK_RASTER: u32 = TOP_RASTER - 1;
/// The line RAM columns: the 160-column positioning grid.
const LINE_COLS: usize = 160;

/// One display-list header: the data address low byte, the palette
/// and width control, the address high byte, and the position.
#[derive(Debug, Clone, Copy)]
struct DlHeader {
    addr_lo: u8,
    control: u8,
    addr_hi: u8,
    hpos: u8,
}

/// One displayed pixel: the palette number with the color code.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct LineRam {
    palette: u8,
    code: u8,
}

/// The MARIA graphics processor.
#[derive(Debug)]
pub struct Maria {
    name: String,
    fetch_bus: String,
    dot_div: u64,

    // The register file.
    palette: [u8; 24], // 8 palettes of three colors
    backgrnd: u8,
    dpph: u8,
    dppl: u8,
    charbase: u8,
    ctrl: u8,
    offset: u8,
    wsync_pending: bool,

    // The display-list walk. The frame re-arm sets `dl_active`; the
    // empty terminator of the DLL parks the walk until the next
    // re-arm.
    dl_active: bool,
    dll_addr: u16,
    dll_zone_left: u32, // rasters before the next DLL entry
    dl_addr: u16,
    offset_current: u8,
    dli_after_zone: bool,
    holey16: bool,
    holey8: bool,
    line_ram: Vec<LineRam>,

    // The scanline state.
    cycles: u32,
    raster: u32,
    screen: Vec<u8>,
    cached_frame: Frame,
    frames: u64,
    next: Option<Tick>,
    last_tick: Option<Tick>,
}

impl Maria {
    /// A device from its configuration entry.
    pub fn new(device: &demu_config::model::DeviceInstance) -> Self {
        let fetch_bus = device
            .params
            .get("fetch_bus")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| "cpu".to_string());
        Self {
            name: device.name.to_string(),
            fetch_bus,
            dot_div: u64::from(device.dividers.get("dot").copied().unwrap_or(1)).max(1),
            palette: [0; 24],
            backgrnd: 0,
            dpph: 0,
            dppl: 0,
            charbase: 0,
            ctrl: 0,
            offset: 0,
            wsync_pending: false,
            dl_active: false,
            dll_addr: 0,
            dll_zone_left: 0,
            dl_addr: 0,
            offset_current: 0,
            dli_after_zone: false,
            holey16: false,
            holey8: false,
            line_ram: vec![LineRam::default(); LINE_COLS],
            cycles: 0,
            raster: 0,
            screen: vec![0; LINE_COLS * 2 * FRAME_LINES * 3],
            cached_frame: Frame {
                width: LINE_COLS as u32 * 2,
                height: FRAME_LINES as u32,
                pixels: vec![0; (LINE_COLS * 2) * FRAME_LINES * 3],
            },
            frames: 0,
            next: None,
            last_tick: None,
        }
    }

    /// The DMA control of CTRL bits 6-5: two = normal DMA.
    fn dma_on(&self) -> bool {
        (self.ctrl & 0x60) >> 5 == 2
    }

    /// The read mode of CTRL bits 1-0.
    fn read_mode(&self) -> u8 {
        self.ctrl & 0x03
    }

    /// Fetch a byte through the bus with the holey mask applied.
    fn fetch(&self, addr: u16, ctx: &mut MachineCtx, tick: Tick) -> u8 {
        let raw = u32::from(addr);
        if self.holey16 && raw >= 0x8000 && raw & 0x1000 != 0 {
            return 0;
        }
        if self.holey8 && raw >= 0x8000 && raw & 0x0800 != 0 {
            return 0;
        }
        ctx.read(&self.fetch_bus, AccessWidth::Byte, Address(raw), tick) as u8
    }

    /// Clear line RAM for one raster.
    fn clear_line_ram(&mut self) {
        self.line_ram.fill(LineRam::default());
    }

    /// Run one raster's DMA: read the next DLL entry when the zone
    /// count expired, then walk the zone's display list. Returns the
    /// MARIA cycles the DMA consumed.
    fn dma_raster(&mut self, tick: Tick, ctx: &mut MachineCtx) -> u32 {
        self.clear_line_ram();
        if !self.dma_on() {
            return 0;
        }
        let mut cycles = 7u32; // The startup wait, not stolen.
        // The zone count ran out: read the next display list list
        // entry. Three zero bytes park the walk for the frame.
        if self.dl_active && self.dll_zone_left == 0 {
            cycles += 6;
            self.read_next_zone(tick, ctx);
        }
        // Walk the display list of the zone: the four-byte headers
        // carry the data address low byte, the palette and width, the
        // address high byte, and the screen position.
        if self.dl_active {
            let mut header_at = self.dl_addr;
            for _ in 0..64 {
                let b0 = self.fetch(header_at, ctx, tick);
                let b1 = self.fetch(header_at.wrapping_add(1), ctx, tick);
                if b1 == 0 {
                    cycles += 8;
                    break;
                }
                let b2 = self.fetch(header_at.wrapping_add(2), ctx, tick);
                let b3 = self.fetch(header_at.wrapping_add(3), ctx, tick);
                header_at = header_at.wrapping_add(4);
                if b1 & 0x40 != 0 && b1 & 0x1F == 0 {
                    // Nothing to paint: skip but keep walking.
                    cycles += 8;
                    continue;
                }
                cycles += 8;
                self.dl_entry(
                    DlHeader {
                        addr_lo: b0,
                        control: b1,
                        addr_hi: b2,
                        hpos: b3,
                    },
                    tick,
                    ctx,
                    &mut cycles,
                );
            }
        }
        // Shutdown cost of the line; the zone's last raster pays more.
        cycles += if self.dll_zone_left == 1 { 11 } else { 5 };
        cycles
    }

    /// One display-list entry: palette, width, address, and the
    /// horizontal position; the graphics copy into line RAM.
    fn dl_entry(&mut self, header: DlHeader, tick: Tick, ctx: &mut MachineCtx, cycles: &mut u32) {
        let palette = (header.control >> 5) & 0x07;
        // The width is stored two's complement: zero names 32 bytes,
        // one to 31 otherwise.
        let width = match header.control & 0x1F {
            0 => 32,
            n => 0x20 - n,
        } as usize;
        // The hpos sits on the 160-column positioning grid.
        let hpos = usize::from(header.hpos) % LINE_COLS;
        // The data address: the offset register joins the page high
        // byte.
        let page = u16::from(header.addr_hi).wrapping_add(u16::from(self.offset_current));
        let data_addr = (page << 8) | u16::from(header.addr_lo);
        for i in 0..width {
            let byte = self.fetch(data_addr.wrapping_add(i as u16), ctx, tick);
            *cycles += 3;
            self.paint_byte(palette, byte, hpos + i * 4);
        }
    }

    /// Paint one graphics byte into line RAM in the 160A form.
    fn paint_byte(&mut self, palette: u8, byte: u8, start: usize) {
        for (i, pair) in [6u8, 4, 2, 0].iter().enumerate() {
            let code = (byte >> pair) & 0x03;
            let col = (start + i) % LINE_COLS;
            if code != 0 {
                self.line_ram[col] = LineRam { palette, code };
            }
        }
    }

    /// Read the next display list list entry and arm the zone. All
    /// three entry bytes zero are the empty terminator: the walk
    /// parks until the next frame re-arms it.
    fn read_next_zone(&mut self, tick: Tick, ctx: &mut MachineCtx) {
        let b0 = self.fetch(self.dll_addr, ctx, tick);
        let hi = self.fetch(self.dll_addr.wrapping_add(1), ctx, tick);
        let lo = self.fetch(self.dll_addr.wrapping_add(2), ctx, tick);
        self.dll_addr = self.dll_addr.wrapping_add(3);
        if b0 == 0 && hi == 0 && lo == 0 {
            self.dl_active = false;
            return;
        }
        self.dli_after_zone = b0 & 0x80 != 0;
        self.holey16 = b0 & 0x40 != 0;
        self.holey8 = b0 & 0x20 != 0;
        let offset = b0 & 0x0F;
        // The zone covers one more raster than the count names.
        self.dll_zone_left = u32::from(offset) + 1;
        self.offset_current = offset;
        self.dl_addr = (u16::from(hi) << 8) | u16::from(lo);
    }

    /// Compose the displayed raster from line RAM into the screen.
    /// The walk of raster `n` fills the visible row `n`-WALK_RASTER:
    /// the DMA runs one raster before the line it displays.
    fn compose_raster(&mut self) {
        let row = self.raster.wrapping_sub(WALK_RASTER);
        if row >= FRAME_LINES as u32 {
            return;
        }
        let y = row as usize;
        let bg = self.backgrnd;
        let rm = self.read_mode();
        let kangaroo = self.ctrl & 0x04 != 0;
        for (col, entry) in self.line_ram.iter().enumerate() {
            let color = if entry.code == 0 {
                bg
            } else {
                let pal_entry = usize::from(entry.palette) * 3 + usize::from(entry.code - 1);
                self.palette[pal_entry]
            };
            let [r, g, b] = tia_color_rgb(color, 4);
            // The 320 read modes split each column into two pixels.
            let splits = if rm != 0 { 1 } else { 2 };
            for sub in 0..splits {
                let x = col * 2 + sub;
                let base = (y * (LINE_COLS * 2) + x) * 3;
                let v = if splits == 2 || sub == 0 || kangaroo {
                    [r, g, b]
                } else {
                    // The right sub-pixel mirrors the entry.
                    let [r2, g2, b2] = tia_color_rgb(bg, 4);
                    [r2, g2, b2]
                };
                self.screen[base] = v[0];
                self.screen[base + 1] = v[1];
                self.screen[base + 2] = v[2];
            }
        }
    }
}

impl BusAccess for Maria {
    fn read(&mut self, _width: AccessWidth, address: Address, _tick: Tick) -> u32 {
        let dec = (address.0 & 0x1F) as u8;
        let slot = (dec >> 2) & 0x07;
        let ofs = dec & 0x03;
        if ofs == 0 {
            match slot {
                2 => u32::from(self.vblank_on()) << 7, // MSTAT
                6 => u32::from(self.offset),           // OFFSET reads back
                _ => 0,
            }
        } else {
            // Palette reads through absolute,index addressing only;
            // the plain reads return zero.
            0
        }
    }

    fn write(&mut self, _width: AccessWidth, address: Address, value: u32, _tick: Tick) {
        let v = value as u8;
        let dec = (address.0 & 0x1F) as u8;
        let slot = (dec >> 2) & 0x07;
        let ofs = dec & 0x03;
        if ofs == 0 {
            match slot {
                0 => self.backgrnd = v & 0x7E,
                1 => self.wsync_pending = true, // WSYNC strobe
                3 => self.dpph = v,
                4 => self.dppl = v,
                5 => self.charbase = v,
                6 => self.offset = v,
                7 => self.ctrl = v,
                _ => {}
            }
            return;
        }
        self.palette[usize::from(slot) * 3 + usize::from(ofs - 1)] = v;
    }

    fn peek(&self, address: Address) -> u8 {
        let dec = (address.0 & 0x1F) as u8;
        let slot = (dec >> 2) & 0x07;
        let ofs = dec & 0x03;
        match (slot, ofs) {
            (2, 0) => u8::from(self.vblank_on()) << 7,
            _ => self
                .palette
                .get(usize::from(slot) * 3 + usize::from(ofs.saturating_sub(1)))
                .copied()
                .unwrap_or(self.backgrnd),
        }
    }
}

impl Maria {
    /// MSTAT bit 7: the vertical blank of the current raster.
    fn vblank_on(&self) -> bool {
        !(self.raster >= WALK_RASTER && self.raster <= 258)
    }
}

impl VideoSource for Maria {
    fn frame(&self) -> Option<&Frame> {
        Some(&self.cached_frame)
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

impl Device for Maria {
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
        self.palette = [0; 24];
        self.backgrnd = 0;
        self.dpph = 0;
        self.dppl = 0;
        self.charbase = 0;
        self.ctrl = 0;
        self.offset = 0;
        self.wsync_pending = false;
        self.dl_active = false;
        self.dll_addr = 0;
        self.dll_zone_left = 0;
        self.dl_addr = 0;
        self.offset_current = 0;
        self.dli_after_zone = false;
        self.holey16 = false;
        self.holey8 = false;
        self.clear_line_ram();
        self.cycles = 0;
        self.raster = 0;
        self.frames = 0;
        self.next = None;
        self.last_tick = None;
    }

    fn boot(&mut self, _state: Option<&BootState>) {
        self.reset();
        self.next = Some(Tick(0));
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(40);
        out.extend_from_slice(b"MARI");
        out.push(2);
        out.extend_from_slice(&self.palette[..]);
        out.push(self.backgrnd);
        out.push(self.dpph);
        out.push(self.dppl);
        out.push(self.charbase);
        out.push(self.ctrl);
        out.push(self.offset);
        out.push(u8::from(self.wsync_pending));
        out.push(u8::from(self.dl_active));
        out.extend_from_slice(&self.dll_addr.to_le_bytes());
        out.extend_from_slice(&self.dll_zone_left.to_le_bytes());
        out.extend_from_slice(&self.dl_addr.to_le_bytes());
        out.push(self.offset_current);
        out.push(u8::from(self.dli_after_zone));
        out.push(u8::from(self.holey16));
        out.push(u8::from(self.holey8));
        for e in &self.line_ram {
            out.push(e.palette);
            out.push(e.code);
        }
        out.extend_from_slice(&self.cycles.to_le_bytes());
        out.extend_from_slice(&self.raster.to_le_bytes());
        out.extend_from_slice(&self.frames.to_le_bytes());
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        let expected = 5 + 24 + 6 + 1 + 1 + 2 + 4 + 2 + 1 + 1 + 1 + 1 + LINE_COLS * 2 + 4 + 4 + 8;
        if data.len() != expected || &data[..4] != b"MARI" || data[4] != 2 {
            return Err(SnapshotError::Mismatch(format!(
                "maria chunk must be {expected} bytes"
            )));
        }
        let mut pos = 5usize;
        self.palette.copy_from_slice(&data[pos..pos + 24]);
        pos += 24;
        self.backgrnd = data[pos];
        pos += 1;
        self.dpph = data[pos];
        pos += 1;
        self.dppl = data[pos];
        pos += 1;
        self.charbase = data[pos];
        pos += 1;
        self.ctrl = data[pos];
        pos += 1;
        self.offset = data[pos];
        pos += 1;
        self.wsync_pending = data[pos] != 0;
        pos += 1;
        self.dl_active = data[pos] != 0;
        pos += 1;
        self.dll_addr = u16::from_le_bytes(data[pos..pos + 2].try_into().expect("2 bytes"));
        pos += 2;
        self.dll_zone_left = u32::from_le_bytes(data[pos..pos + 4].try_into().expect("4 bytes"));
        pos += 4;
        self.dl_addr = u16::from_le_bytes(data[pos..pos + 2].try_into().expect("2 bytes"));
        pos += 2;
        self.offset_current = data[pos];
        pos += 1;
        self.dli_after_zone = data[pos] != 0;
        pos += 1;
        self.holey16 = data[pos] != 0;
        pos += 1;
        self.holey8 = data[pos] != 0;
        pos += 1;
        for e in self.line_ram.iter_mut() {
            e.palette = data[pos];
            pos += 1;
            e.code = data[pos];
            pos += 1;
        }
        self.cycles =
            u32::from_le_bytes(data[pos..pos + 4].try_into().expect("4 bytes")) % LINE_CYCLES;
        pos += 4;
        self.raster = u32::from_le_bytes(data[pos..pos + 4].try_into().expect("4 bytes")) % LINES;
        pos += 4;
        self.frames = u64::from_le_bytes(data[pos..pos + 8].try_into().expect("8 bytes"));
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
        if self.wsync_pending {
            self.wsync_pending = false;
            ctx.notify("halt", Level::High, tick);
        }
        self.cycles += 1;
        if self.cycles >= LINE_CYCLES {
            self.cycles %= LINE_CYCLES;
            // The previous raster ends: count the zone, raise the DLI
            // after the last line of the zone with the flag.
            if self.dll_zone_left > 0 {
                self.dll_zone_left -= 1;
                if self.dll_zone_left == 0 && self.dli_after_zone {
                    self.dli_after_zone = false;
                    ctx.notify("nmi", Level::High, tick);
                    ctx.notify("nmi", Level::Low, tick);
                }
            }
            self.raster = (self.raster + 1) % LINES;
            // The raster's own DMA happens one line before display;
            // the walk runs at the raster start, and the compose that
            // follows maps the walk of raster WALK_RASTER onto the
            // frame's first visible row.
            if self.raster >= WALK_RASTER && self.raster < WALK_RASTER + FRAME_LINES as u32 {
                let stolen = self.dma_raster(tick, ctx) / 4;
                if stolen > 0 {
                    ctx.notify("halt", Level::High, tick);
                }
            }
            ctx.notify("halt", Level::Low, tick);
            self.compose_raster();
            if self.raster == LINES - 1 {
                self.frames += 1;
                self.cached_frame.pixels = self.screen.clone();
                ctx.emit(TriggerEvent::Vblank);
                // A new frame re-reads the DLL pointer and re-arms the
                // display-list walk.
                self.dll_addr = (u16::from(self.dpph) << 8) | u16::from(self.dppl);
                self.dll_zone_left = 0;
                self.dl_active = true;
            }
        }
        self.next = Some(Tick(tick.0 + self.dot_div));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn maria() -> Maria {
        Maria {
            name: "maria".to_string(),
            fetch_bus: "cpu".to_string(),
            dot_div: 1,
            palette: [0; 24],
            backgrnd: 0,
            dpph: 0,
            dppl: 0,
            charbase: 0,
            ctrl: 0,
            offset: 0,
            wsync_pending: false,
            dl_active: false,
            dll_addr: 0,
            dll_zone_left: 0,
            dl_addr: 0,
            offset_current: 0,
            dli_after_zone: false,
            holey16: false,
            holey8: false,
            line_ram: vec![LineRam::default(); LINE_COLS],
            cycles: 0,
            raster: 0,
            screen: vec![0; (LINE_COLS * 2) * FRAME_LINES * 3],
            cached_frame: Frame {
                width: LINE_COLS as u32 * 2,
                height: FRAME_LINES as u32,
                pixels: vec![0; (LINE_COLS * 2) * FRAME_LINES * 3],
            },
            frames: 0,
            next: None,
            last_tick: None,
        }
    }

    #[test]
    fn palette_registers_decode_in_slots() {
        let mut m = maria();
        m.write(AccessWidth::Byte, Address(0x0021), 0x5A, Tick(0));
        assert_eq!(m.palette[0], 0x5A); // P0C1 at slot 0 offset 1.
        m.write(AccessWidth::Byte, Address(0x003D), 0x77, Tick(0));
        assert_eq!(m.palette[21], 0x77); // P7C1 at slot 7 offset 1.
        m.write(AccessWidth::Byte, Address(0x0020), 0x44, Tick(0));
        assert_eq!(m.backgrnd, 0x44);
    }

    #[test]
    fn mstat_reads_vblank() {
        let mut m = maria();
        m.raster = 0;
        assert_eq!(m.read(AccessWidth::Byte, Address(0x0028), Tick(0)), 0x80);
        m.raster = 100;
        assert_eq!(m.read(AccessWidth::Byte, Address(0x0028), Tick(0)), 0);
    }

    #[test]
    fn snapshot_round_trip() {
        let mut m = maria();
        m.backgrnd = 0x24;
        m.dpph = 0x18;
        let snap = m.snapshot();
        let mut twin = maria();
        twin.restore(&snap).expect("restore");
        assert_eq!(twin.snapshot(), snap);
        assert_eq!(twin.backgrnd, 0x24);
    }
}
