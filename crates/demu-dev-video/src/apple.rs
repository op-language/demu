//! The Apple II video generator.
//!
//! One device visit is one dot of the video clock. With the domain set
//! to `315000000/44` (the 7.159090 MHz video tick) and the `dot`
//! divider at 1, the device walks 455 dots per scanline and 262
//! scanlines per frame. The display window renders 280 dots wide and
//! 192 scanlines tall; the first picture dot sits 40 dots into the
//! line and the first picture line 40 lines into the frame, which
//! stands in for the real video blanking phase.
//!
//! The device owns the display memory: the text pages at
//! `$0400-$07FF` and `$0800-$0BFF` and the graphics pages at
//! `$2000-$3FFF` and `$4000-$5FFF` answer the bus as plain RAM. The
//! video form switches at `$C050-$C057` belong here too, because the
//! renderer reads them at every dot and the `a2.io` console device
//! must stay free of display state. Both reads and writes trigger the
//! switches, as the address-decoded switches do.
//!
//! The display byte addressing follows the interleaved layout of the
//! real pages: an eight-row band of 40 bytes repeats down each page,
//! so the address of row `r`, column `c` in a page is
//! `128 * (r & 7) + 40 * (r >> 3) + c`. Text uses 24 character rows,
//! hi-res uses 192 pixel rows, and LORES pairs two four-line blocks
//! over each eight-line text row.
//!
//! ## Simplifications
//!
//! The character generator ROM is not modeled: the text form renders
//! each six-bit character code as a fixed synthetic glyph, whose seven
//! glyph rows are the five-bit rotations of the code, with the eighth
//! row blank. The high bit of a text byte inverts the glyph. HIRES
//! colors follow the two-phase NTSC model: a lit dot paints green or
//! purple per the phase of the dot with the byte's high bit flipping
//! the phase, and a pair of neighboring lit dots within a byte paints
//! white. The horizontal color shift of the real odd and even columns
//! and the vertical color phase between the lores block halves are not
//! modeled. LORES shows the high nibble over the upper four scanlines
//! of each eight-line row and the low nibble over the lower four; the
//! real one-scanline nibble phase between the two halves is not
//! modeled. DOUBLE HIRES and the monitor color controls do not exist
//! here. The mode switches read back zero.

use demu_config::model::BootState;
use demu_core::device::{AccessWidth, BusAccess, Frame, MachineCtx, VideoLayer, VideoSource};
use demu_core::frequency::{Address, Tick};
use demu_core::signal::Level;
use demu_core::triggers::TriggerEvent;
use demu_core::{Device, Snapshot, SnapshotError};

use crate::tia_color_rgb;

/// Dots per scanline: 7 dots per CPU cycle over a 65-cycle line.
const LINE_DOTS: u32 = 455;
/// Scanlines per frame.
const LINES: u32 = 262;
/// The picture columns of the frame.
const HGR_W: usize = 280;
/// The picture rows of the frame.
const FRAME_LINES: usize = 192;
/// The first picture dot of the line.
const LEFT_DOT: u32 = 40;
/// The first picture line of the frame.
const TOP_LINE: u32 = 40;

/// The video generator, the display RAM, and the form switches.
#[derive(Debug)]
pub struct AppleVideo {
    name: String,
    /// Domain ticks per dot, from the `dot` divider entry.
    dot_div: u64,
    /// Text page 1 at `$0400-$07FF`.
    text1: Box<[u8; 1024]>,
    /// Text page 2 at `$0800-$0BFF`.
    text2: Box<[u8; 1024]>,
    /// Hi-res page 1 at `$2000-$3FFF`.
    hgr1: Box<[u8; 8192]>,
    /// Hi-res page 2 at `$4000-$5FFF`.
    hgr2: Box<[u8; 8192]>,
    /// True in the graphics form; false is the text form.
    graphics: bool,
    /// True while the four text rows at the bottom stay in text.
    mixed: bool,
    /// True in the hi-res form; false is the lores form.
    hires: bool,
    /// True while the pages land in page 2.
    page2: bool,
    /// The current dot inside the scanline.
    cc: u32,
    /// The current scanline.
    line: u32,
    /// The frame scan of the composed picture.
    screen: Vec<u8>,
    /// The view the shell reports.
    cached_frame: Frame,
    frames: u64,
    next: Option<Tick>,
    last_tick: Option<Tick>,
}

impl AppleVideo {
    /// A device from its configuration entry. The `dot` divider steps
    /// one dot per visit; the configs of the video domain set it to 2
    /// when the domain runs at the half dot rate.
    pub fn new(device: &demu_config::model::DeviceInstance) -> Self {
        Self {
            name: device.name.to_string(),
            dot_div: u64::from(device.dividers.get("dot").copied().unwrap_or(2)).max(1),
            text1: Box::new([0; 1024]),
            text2: Box::new([0; 1024]),
            hgr1: Box::new([0; 8192]),
            hgr2: Box::new([0; 8192]),
            graphics: false,
            mixed: false,
            hires: false,
            page2: false,
            cc: 0,
            line: 0,
            screen: vec![0; HGR_W * FRAME_LINES * 3],
            cached_frame: Frame {
                width: HGR_W as u32,
                height: FRAME_LINES as u32,
                pixels: vec![0; HGR_W * FRAME_LINES * 3],
            },
            frames: 0,
            next: None,
            last_tick: None,
        }
    }

    /// The text page of the current page select.
    fn text_page(&self) -> &[u8] {
        if self.page2 {
            self.text2.as_slice()
        } else {
            self.text1.as_slice()
        }
    }

    /// The hi-res page of the current page select.
    fn hgr_page(&self) -> &[u8] {
        if self.page2 {
            self.hgr2.as_slice()
        } else {
            self.hgr1.as_slice()
        }
    }

    /// The offset of one interleaved row and column inside a page.
    fn row_offset(row: usize, col: usize) -> usize {
        128 * (row & 0x07) + 40 * (row >> 3) + col
    }

    /// The lores color entries: color 0 stays black and the 15 chroma
    /// entries reuse the house NTSC YIQ table at a mid luma.
    fn lores_rgb(v: u8) -> [u8; 3] {
        if v == 0 {
            [0, 0, 0]
        } else {
            tia_color_rgb((v << 4) | 0x0A, 4)
        }
    }

    /// One dot of the pipeline: compose the visible pixel.
    fn step(&mut self, _tick: Tick, ctx: &mut MachineCtx) {
        self.cc += 1;
        if self.cc == LINE_DOTS {
            self.cc = 0;
            self.line = (self.line + 1) % LINES;
        } else {
            let row_line = self.line.wrapping_sub(TOP_LINE);
            if row_line < FRAME_LINES as u32
                && self.cc >= LEFT_DOT
                && self.cc < LEFT_DOT + HGR_W as u32
            {
                self.paint_dot(row_line, self.cc - LEFT_DOT);
            }
        }
        if self.line == LINES - 1 && self.cc == LINE_DOTS - 1 {
            self.end_frame();
            ctx.emit(TriggerEvent::Vblank);
        }
    }

    /// Close the frame: bump the counter and refresh the cached view.
    fn end_frame(&mut self) {
        self.frames += 1;
        self.cached_frame.pixels = self.screen.clone();
    }

    /// The RGB bytes of one picture dot from the mode state.
    fn paint_dot(&mut self, row_line: u32, col: u32) {
        let y = row_line as usize;
        let x = col as usize;
        let text = !self.graphics || (self.mixed && y >= 160);
        let rgb = if text {
            let t = y / 8;
            let sub = y % 8;
            let byte = self.text_page()[Self::row_offset(t, x / 7)];
            self.text_pixel(byte, x % 7, sub)
        } else if self.hires {
            let byte = self.hgr_page()[Self::row_offset(y, x / 7)];
            self.hgr_pixel(byte, x % 7)
        } else {
            let t = y / 8;
            let byte = self.text_page()[Self::row_offset(t, x / 7)];
            let nibble = if (y % 8) < 4 { byte >> 4 } else { byte & 0x0F };
            Self::lores_rgb(nibble)
        };
        let base = (y * HGR_W + x) * 3;
        self.screen[base] = rgb[0];
        self.screen[base + 1] = rgb[1];
        self.screen[base + 2] = rgb[2];
    }

    /// The text glyph pixel of one dot: the synthetic glyph rows are
    /// the five-bit rotations of the character code.
    fn text_pixel(&self, byte: u8, col: usize, sub: usize) -> [u8; 3] {
        let code = byte & 0x3F;
        let inverse = byte & 0x80 != 0;
        let col_val = if col >= 5 || sub == 7 {
            0
        } else {
            let bits = ((code << sub) | (code >> (6 - sub))) & 0x1F;
            (bits >> (4 - col)) & 1
        };
        let on = if inverse { col_val == 0 } else { col_val == 1 };
        if on { Self::lores_rgb(15) } else { [0, 0, 0] }
    }

    /// The hi-res pixel of one dot: the phase of the dot picks green
    /// or purple, the byte's high bit flips it, and neighboring lit
    /// dots paint white.
    fn hgr_pixel(&self, byte: u8, dot: usize) -> [u8; 3] {
        let bit = dot % 7;
        let lit = byte >> bit & 1 == 1;
        if !lit {
            return [0, 0, 0];
        }
        let left = bit > 0 && byte >> (bit - 1) & 1 == 1;
        let right = bit < 6 && byte >> (bit + 1) & 1 == 1;
        if left || right {
            return Self::lores_rgb(15);
        }
        let odd = bit & 1 == 1;
        let flip = byte & 0x80 != 0;
        if odd == flip {
            Self::lores_rgb(1)
        } else {
            Self::lores_rgb(12)
        }
    }
}

impl BusAccess for AppleVideo {
    fn read(&mut self, _width: AccessWidth, address: Address, _tick: Tick) -> u32 {
        let a = address.0;
        match a {
            0x0400..=0x07FF => u32::from(self.text1[(a - 0x0400) as usize]),
            0x0800..=0x0BFF => u32::from(self.text2[(a - 0x0800) as usize]),
            0x2000..=0x3FFF => u32::from(self.hgr1[(a - 0x2000) as usize]),
            0x4000..=0x5FFF => u32::from(self.hgr2[(a - 0x4000) as usize]),
            0xC050 => {
                self.graphics = true;
                0
            }
            0xC051 => {
                self.graphics = false;
                0
            }
            0xC052 => {
                self.mixed = false;
                0
            }
            0xC053 => {
                self.mixed = true;
                0
            }
            0xC054 => {
                self.page2 = false;
                0
            }
            0xC055 => {
                self.page2 = true;
                0
            }
            0xC056 => {
                self.hires = true;
                0
            }
            0xC057 => {
                self.hires = false;
                0
            }
            _ => 0xFF,
        }
    }

    fn write(&mut self, _width: AccessWidth, address: Address, value: u32, _tick: Tick) {
        let a = address.0;
        let v = value as u8;
        match a {
            0x0400..=0x07FF => self.text1[(a - 0x0400) as usize] = v,
            0x0800..=0x0BFF => self.text2[(a - 0x0800) as usize] = v,
            0x2000..=0x3FFF => self.hgr1[(a - 0x2000) as usize] = v,
            0x4000..=0x5FFF => self.hgr2[(a - 0x4000) as usize] = v,
            0xC050 => self.graphics = true,
            0xC051 => self.graphics = false,
            0xC052 => self.mixed = false,
            0xC053 => self.mixed = true,
            0xC054 => self.page2 = false,
            0xC055 => self.page2 = true,
            0xC056 => self.hires = true,
            0xC057 => self.hires = false,
            _ => {}
        }
    }

    fn peek(&self, address: Address) -> u8 {
        let a = address.0;
        match a {
            0x0400..=0x07FF => self.text1[(a - 0x0400) as usize],
            0x0800..=0x0BFF => self.text2[(a - 0x0800) as usize],
            0x2000..=0x3FFF => self.hgr1[(a - 0x2000) as usize],
            0x4000..=0x5FFF => self.hgr2[(a - 0x4000) as usize],
            _ => 0,
        }
    }

    /// The display RAM and the form switches always answer.
    fn decodes(&self, address: Address) -> bool {
        matches!(
            address.0,
            0x0400..=0x0BFF | 0x2000..=0x5FFF | 0xC050..=0xC057
        )
    }
}

impl VideoSource for AppleVideo {
    fn frame(&self) -> Option<&Frame> {
        Some(&self.cached_frame)
    }

    fn dump_layer(&self, layer: VideoLayer) -> Option<Vec<u8>> {
        match layer {
            VideoLayer::Framebuffer => Some(self.screen.clone()),
            VideoLayer::Vram => Some(
                self.text1
                    .iter()
                    .chain(self.text2.iter())
                    .chain(self.hgr1.iter())
                    .chain(self.hgr2.iter())
                    .copied()
                    .collect(),
            ),
            VideoLayer::NameTable => Some(self.text_page().to_vec()),
            _ => None,
        }
    }

    fn frames(&self) -> u64 {
        self.frames
    }
}

impl Device for AppleVideo {
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
        self.text1.iter_mut().for_each(|b| *b = 0);
        self.text2.iter_mut().for_each(|b| *b = 0);
        self.hgr1.iter_mut().for_each(|b| *b = 0);
        self.hgr2.iter_mut().for_each(|b| *b = 0);
        self.graphics = false;
        self.mixed = false;
        self.hires = false;
        self.page2 = false;
        self.cc = 0;
        self.line = 0;
        self.frames = 0;
        self.next = None;
        self.last_tick = None;
    }

    fn boot(&mut self, state: Option<&BootState>) {
        self.reset();
        if let Some(BootState::Ram { pattern }) = state {
            demu_core::boot::fill_ram(self.text1.as_mut(), *pattern);
            demu_core::boot::fill_ram(self.text2.as_mut(), *pattern);
            demu_core::boot::fill_ram(self.hgr1.as_mut(), *pattern);
            demu_core::boot::fill_ram(self.hgr2.as_mut(), *pattern);
        }
        // The video walk starts at once: the scheduler visits only
        // devices that arm a queued tick.
        self.next = Some(Tick(0));
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(5 + 4 + 18 * 1024 + 12);
        out.extend_from_slice(b"A2VD");
        out.push(1);
        out.push(u8::from(self.graphics));
        out.push(u8::from(self.mixed));
        out.push(u8::from(self.hires));
        out.push(u8::from(self.page2));
        out.extend_from_slice(&self.cc.to_le_bytes());
        out.extend_from_slice(&self.line.to_le_bytes());
        out.extend_from_slice(&self.frames.to_le_bytes());
        out.extend_from_slice(&self.text1[..]);
        out.extend_from_slice(&self.text2[..]);
        out.extend_from_slice(&self.hgr1[..]);
        out.extend_from_slice(&self.hgr2[..]);
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        let expected = 4 + 1 + 4 + 4 + 4 + 8 + 2 * 1024 + 2 * 8192;
        if data.len() != expected || &data[..4] != b"A2VD" || data[4] != 1 {
            return Err(SnapshotError::Mismatch(format!(
                "apple video chunk must be {expected} bytes"
            )));
        }
        let mut pos = 5usize;
        self.graphics = data[pos] != 0;
        pos += 1;
        self.mixed = data[pos] != 0;
        pos += 1;
        self.hires = data[pos] != 0;
        pos += 1;
        self.page2 = data[pos] != 0;
        pos += 1;
        self.cc = u32::from_le_bytes(data[pos..pos + 4].try_into().expect("4 bytes")) % LINE_DOTS;
        pos += 4;
        self.line = u32::from_le_bytes(data[pos..pos + 4].try_into().expect("4 bytes")) % LINES;
        pos += 4;
        self.frames = u64::from_le_bytes(data[pos..pos + 8].try_into().expect("8 bytes"));
        pos += 8;
        self.text1.copy_from_slice(&data[pos..pos + 1024]);
        pos += 1024;
        self.text2.copy_from_slice(&data[pos..pos + 1024]);
        pos += 1024;
        self.hgr1.copy_from_slice(&data[pos..pos + 8192]);
        pos += 8192;
        self.hgr2.copy_from_slice(&data[pos..pos + 8192]);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The screen RGB of one picture pixel.
    fn px(v: &AppleVideo, y: usize, x: usize) -> [u8; 3] {
        let b = (y * HGR_W + x) * 3;
        [v.screen[b], v.screen[b + 1], v.screen[b + 2]]
    }

    fn instance() -> demu_config::model::DeviceInstance {
        demu_config::model::DeviceInstance {
            name: demu_config::newtypes::DeviceName::new("a2vid").expect("name"),
            kind: demu_config::newtypes::KindName::new("a2.video").expect("kind"),
            domain: "main".to_string(),
            params: std::collections::BTreeMap::new(),
            dividers: [("dot".to_string(), 1)].into_iter().collect(),
        }
    }

    #[test]
    fn row_offsets_follow_the_interleave() {
        assert_eq!(AppleVideo::row_offset(0, 0), 0);
        assert_eq!(AppleVideo::row_offset(1, 0), 128);
        assert_eq!(AppleVideo::row_offset(8, 0), 40);
        assert_eq!(AppleVideo::row_offset(23, 39), 7 * 128 + 2 * 40 + 39);
        assert_eq!(AppleVideo::row_offset(191, 39), 7 * 128 + 23 * 40 + 39);
    }

    #[test]
    fn ram_windows_answer_the_bus() {
        let mut v = AppleVideo::new(&instance());
        v.write(AccessWidth::Byte, Address(0x0401), 0x37, Tick(0));
        assert_eq!(v.peek(Address(0x0401)), 0x37);
        v.write(AccessWidth::Byte, Address(0x0850), 0x11, Tick(0));
        assert_eq!(v.peek(Address(0x0850)), 0x11);
        v.write(AccessWidth::Byte, Address(0x2005), 0x42, Tick(0));
        assert_eq!(v.peek(Address(0x2005)), 0x42);
        v.write(AccessWidth::Byte, Address(0x45FF), 0x99, Tick(0));
        assert_eq!(v.peek(Address(0x45FF)), 0x99);
        assert_eq!(v.read(AccessWidth::Byte, Address(0x0400), Tick(0)), 0);
        assert!(!v.decodes(Address(0x0D00)));
        assert!(v.decodes(Address(0x2AAA)));
    }

    #[test]
    fn form_switches_flip_on_reads_and_writes() {
        let mut v = AppleVideo::new(&instance());
        assert!(!v.graphics, "reset leaves the text form");
        // The switch addresses act on a read as on a write.
        assert_eq!(v.read(AccessWidth::Byte, Address(0xC050), Tick(0)), 0);
        assert!(v.graphics);
        v.write(AccessWidth::Byte, Address(0xC051), 0, Tick(0));
        assert!(!v.graphics);
        v.write(AccessWidth::Byte, Address(0xC053), 0, Tick(0));
        assert!(v.mixed);
        v.write(AccessWidth::Byte, Address(0xC055), 0, Tick(0));
        assert!(v.page2);
        v.write(AccessWidth::Byte, Address(0xC056), 0, Tick(0));
        assert!(v.hires);
        v.write(AccessWidth::Byte, Address(0xC057), 0, Tick(0));
        assert!(!v.hires);
        v.write(AccessWidth::Byte, Address(0xC052), 0, Tick(0));
        assert!(!v.mixed);
        assert_eq!(v.read(AccessWidth::Byte, Address(0xC05F), Tick(0)), 0xFF);
    }

    #[test]
    fn hires_colors_follow_phase_and_flip() {
        let mut v = AppleVideo::new(&instance());
        v.write(AccessWidth::Byte, Address(0xC050), 0, Tick(0));
        v.write(AccessWidth::Byte, Address(0xC056), 0, Tick(0));
        // 0x2A lights the odd dots with the phase of green: the lit
        // bits 1, 3, 5 sit between unlit neighbors.
        v.write(AccessWidth::Byte, Address(0x2000), 0x2A, Tick(0));
        assert_eq!(px(&v, 0, 0), [0, 0, 0], "the unlit dot stays black");
        v.paint_dot(0, 1);
        assert_eq!(
            px(&v, 0, 1),
            AppleVideo::lores_rgb(12),
            "an odd lit dot of a phase-zero byte paints green"
        );
        // The high bit flips the phase to purple.
        v.write(AccessWidth::Byte, Address(0x2000), 0xAA, Tick(0));
        v.paint_dot(0, 1);
        assert_eq!(px(&v, 0, 1), AppleVideo::lores_rgb(1));
        // Neighboring lit dots merge to white.
        v.write(AccessWidth::Byte, Address(0x2000), 0xEF, Tick(0));
        v.paint_dot(0, 0);
        assert_eq!(px(&v, 0, 0), AppleVideo::lores_rgb(15));
    }

    #[test]
    fn text_glyphs_render_deterministically() {
        let mut v = AppleVideo::new(&instance());
        // $0480 is the first column of text row 1; code 0x41 has the
        // glyph bit only on the fifth column of the first glyph row.
        v.write(AccessWidth::Byte, Address(0x0480), 0x41, Tick(0));
        v.paint_dot(8, 4);
        assert_eq!(px(&v, 8, 4), AppleVideo::lores_rgb(15));
        v.paint_dot(8, 0);
        assert_eq!(px(&v, 8, 0), [0, 0, 0]);
        v.paint_dot(15, 4);
        assert_eq!(px(&v, 15, 4), [0, 0, 0], "the eighth glyph row stays blank");
        // The high bit inverts the glyph.
        v.write(AccessWidth::Byte, Address(0x0480), 0xC1, Tick(0));
        v.paint_dot(8, 4);
        assert_eq!(px(&v, 8, 4), [0, 0, 0]);
        v.paint_dot(8, 0);
        assert_eq!(px(&v, 8, 0), AppleVideo::lores_rgb(15));
    }

    #[test]
    fn lores_nibbles_stack_over_the_row() {
        let mut v = AppleVideo::new(&instance());
        v.write(AccessWidth::Byte, Address(0xC050), 0, Tick(0));
        // $05D0 is the first column of lores row 19; the high nibble
        // covers the upper four scanlines of the row and the low
        // nibble the lower four.
        v.write(AccessWidth::Byte, Address(0x05D0), 0x80, Tick(0));
        v.paint_dot(152, 0);
        assert_eq!(px(&v, 152, 0), AppleVideo::lores_rgb(8));
        v.paint_dot(156, 0);
        assert_eq!(
            px(&v, 156, 0),
            [0, 0, 0],
            "the low nibble answers the lower half"
        );
        v.write(AccessWidth::Byte, Address(0x05D0), 0x08, Tick(0));
        v.paint_dot(156, 0);
        assert_eq!(px(&v, 156, 0), AppleVideo::lores_rgb(8));
    }

    #[test]
    fn mixed_mode_keeps_text_at_the_four_bottom_rows() {
        let mut v = AppleVideo::new(&instance());
        v.write(AccessWidth::Byte, Address(0xC050), 0, Tick(0));
        v.write(AccessWidth::Byte, Address(0xC053), 0, Tick(0)); // mixed on
        // Scanline 152 still renders lores; 160 renders text.
        v.write(AccessWidth::Byte, Address(0x05D0), 0x80, Tick(0));
        v.paint_dot(152, 0);
        assert_eq!(px(&v, 152, 0), AppleVideo::lores_rgb(8));
        // $0750 = $0400 + 848 = text row 22, column 0; code 0x41 has
        // the glyph bit on the fifth column of the first glyph row.
        v.write(AccessWidth::Byte, Address(0x0750), 0x41, Tick(0));
        v.paint_dot(176, 4);
        assert_eq!(px(&v, 176, 4), AppleVideo::lores_rgb(15));
    }

    #[test]
    fn page_two_switches_the_display_pages() {
        let mut v = AppleVideo::new(&instance());
        v.write(AccessWidth::Byte, Address(0xC055), 0, Tick(0));
        // $0A80 = $0800 + 640 = page-2 text row 4, column 0.
        v.write(AccessWidth::Byte, Address(0x0A80), 0x41, Tick(0));
        assert_eq!(v.peek(Address(0x0A80)), 0x41);
        assert_eq!(v.peek(Address(0x0400)), 0, "page 1 memory is untouched");
        // $5228 = $5000 + 552 = page-2 hi-res row 12, column 4.
        v.write(AccessWidth::Byte, Address(0x5228), 0x33, Tick(0));
        v.write(AccessWidth::Byte, Address(0xC056), 0, Tick(0)); // hi-res page 2
        assert_eq!(v.peek(Address(0x5228)), 0x33);
    }

    #[test]
    fn frame_end_counts_and_caches() {
        let mut v = AppleVideo::new(&instance());
        v.write(AccessWidth::Byte, Address(0x2000), 0x5A, Tick(0));
        v.line = LINES - 1;
        v.cc = LINE_DOTS - 1;
        v.end_frame();
        assert_eq!(v.frames, 1);
        assert_eq!(v.cached_frame.width, HGR_W as u32);
        assert_eq!(v.cached_frame.height, FRAME_LINES as u32);
        assert_eq!(v.cached_frame.pixels.len(), HGR_W * FRAME_LINES * 3);
    }

    #[test]
    fn snapshot_round_trip() {
        let mut v = AppleVideo::new(&instance());
        v.boot(None);
        v.write(AccessWidth::Byte, Address(0xC053), 0, Tick(0));
        v.write(AccessWidth::Byte, Address(0xC055), 0, Tick(0));
        v.write(AccessWidth::Byte, Address(0x0402), 0x37, Tick(0));
        v.write(AccessWidth::Byte, Address(0x2ABC), 0x77, Tick(0));
        let snapshot = v.snapshot();
        let mut twin = AppleVideo::new(&instance());
        twin.restore(&snapshot).expect("restore");
        assert_eq!(twin.snapshot(), snapshot);
        assert_eq!(twin.peek(Address(0x2ABC)), 0x77);
        assert!(twin.mixed && twin.page2);
    }

    #[test]
    fn restore_rejects_a_bad_chunk() {
        let mut v = AppleVideo::new(&instance());
        let snapshot = v.snapshot();
        let mut broken = snapshot.clone();
        broken[0] = b'X';
        assert!(v.restore(&broken).is_err());
        assert!(v.restore(&[0; 12]).is_err());
    }
}
