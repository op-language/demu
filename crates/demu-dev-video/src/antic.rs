//! The ANTIC display-list processor together with the CTIA/GTIA
//! register file of the 5200.
//!
//! One device carries both, because the pixel output is internal to
//! the die pair: ANTIC walks the display list, fetches scanline data
//! with DMA through the CPU bus, and paints the playfield; the GTIA
//! register file supplies the colors, the player and missile graphics,
//! and the collision latches. The register decode: the GTIA file
//! answers `$C000-$C1FF` (the low five bits of the address), the ANTIC
//! file answers `$D400-$D5FF` (the low four bits).
//!
//! The scanline runs 228 color clocks; one dot of this device is one
//! color clock, so the dot divider in the config is the master crystal
//! divided by two. The frame runs 262 scanlines. Display-list DMA
//! happens at the line start as a block of stolen machine cycles,
//! delivered as one halt pulse; the exact intra-line interleave of
//! ANTIC DMA cycles is approximate, which the technical design
//! records. VCOUNT reads the scanline number halved, NMIEN gates the
//! display-list and vertical-blank interrupts, and NMIST carries the
//! raised sources until a write to NMIRES.
//!
//! Players support the GRAFP direct pattern with HPOSP positioning and
//! the SIZEP widths; the display-list modes cover both character and
//! bitmap forms, with the high-resolution modes decoding two pixels
//! per color clock.

use demu_config::model::BootState;
use demu_core::device::{
    AccessWidth, BusAccess, Frame, InputSink, MachineCtx, VideoLayer, VideoSource,
};
use demu_core::frequency::{Address, Tick};
use demu_core::signal::Level;
use demu_core::triggers::TriggerEvent;
use demu_core::{Device, Snapshot, SnapshotError};

use crate::tia_color_rgb;

/// Color clocks per scanline.
const LINE_CLOCKS: u32 = 228;
/// Wide playfield pixels per line.
const PWIDE: u32 = 384;
/// Scanlines per frame.
const LINES: u32 = 262;
/// The first picture row.
const TOP_LINE: u32 = 8;
/// Picture rows of the frame.
const FRAME_LINES: usize = 192;
/// The display data bytes carried per scanline; mode four stores
/// 80 bytes.
const DATA_LEN: usize = 80;

/// One display-list mode line's shape: the DMA bytes per scanline.
const MODE_BYTES: [u32; 14] = [40, 40, 40, 40, 20, 20, 10, 10, 20, 20, 20, 40, 80, 40];

/// The ANTIC and GTIA pair of the 5200.
#[derive(Debug)]
pub struct Antic {
    name: String,
    /// The name of the bus the DMA fetches walk.
    fetch_bus: String,
    /// Master ticks per color clock.
    dot_div: u64,
    /// Raster lines per frame: 262 NTSC, 312 PAL.
    lines: u32,

    // The ANTIC register file.
    dmactl: u8,
    chactl: u8,
    dlist: u16,
    hscrol: u8,
    vscrol: u8,
    pmbase: u8,
    chbase: u8,
    nmien: u8,
    nmist: u8,
    vcount_latch: u8,
    wsync_pending: bool,
    nmi_pending: bool,

    // The GTIA register file.
    hposp: [u8; 4],
    hposm: [u8; 4],
    sizep: [u8; 4],
    sizem: u8,
    grafp: [u8; 4],
    grafm: u8,
    colpm: [u8; 4],
    colpf: [u8; 4],
    colbk: u8,
    prior: u8,
    vdelay: u8,
    gractl: u8,
    consol: u8,
    /// Trigger values: one = released, zero = pressed.
    trig: [u8; 4],
    /// The collision latch bytes in read-side order: missiles versus
    /// playfields, players versus playfields, missiles versus players,
    /// players versus players, then the playfield pairs.
    collisions: [u8; 16],

    // The scanline state.
    cc: u32,
    line: u32,
    /// True while the display list still walks; JVB ends it.
    dl_active: bool,
    /// Blank-mode repeats left before the next instruction.
    blank_left: u32,
    /// The current display-list instruction byte, when one is active;
    /// its low nibble is the mode and its bit 7 the interrupt flag.
    dl_mode: Option<u8>,
    /// The memory-scan pointer of the display data.
    mem_scan: u16,
    /// The fetched data bytes of the current scanline.
    dl_data: Vec<u8>,
    /// The playfield colors of the current scanline.
    line_px: Vec<u8>,
    screen: Vec<u8>,
    cached_frame: Frame,
    frames: u64,
    next: Option<Tick>,
    last_tick: Option<Tick>,
}

impl Antic {
    /// A device from its configuration entry.
    pub fn new(device: &demu_config::model::DeviceInstance) -> Self {
        let fetch_bus = device
            .params
            .get("fetch_bus")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| "cpu".to_string());
        let pal = device
            .params
            .get("pal")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        Self {
            name: device.name.to_string(),
            fetch_bus,
            dot_div: u64::from(device.dividers.get("dot").copied().unwrap_or(2)).max(1),
            lines: if pal { 312 } else { LINES },
            dmactl: 0,
            chactl: 0,
            dlist: 0,
            hscrol: 0,
            vscrol: 0,
            pmbase: 0,
            chbase: 0xF8,
            nmien: 0,
            nmist: 0,
            vcount_latch: 0,
            wsync_pending: false,
            nmi_pending: false,
            hposp: [0; 4],
            hposm: [0; 4],
            sizep: [0; 4],
            sizem: 0,
            grafp: [0; 4],
            grafm: 0,
            colpm: [0; 4],
            colpf: [0; 4],
            colbk: 0,
            prior: 0,
            vdelay: 0,
            gractl: 0,
            consol: 0x03,
            trig: [1; 4],
            collisions: [0; 16],
            cc: 0,
            line: 0,
            dl_active: false,
            blank_left: 0,
            dl_mode: None,
            mem_scan: 0,
            dl_data: Vec::new(),
            line_px: vec![0; PWIDE as usize],
            screen: vec![0; PWIDE as usize * FRAME_LINES * 3],
            cached_frame: Frame {
                width: PWIDE,
                height: FRAME_LINES as u32,
                pixels: vec![0; PWIDE as usize * FRAME_LINES * 3],
            },
            frames: 0,
            next: None,
            last_tick: None,
        }
    }

    /// The DMA control register for the debug commands.
    pub fn dmactl(&self) -> u8 {
        self.dmactl
    }

    /// The display-list pointer for the debug commands.
    pub fn dlist(&self) -> u16 {
        self.dlist
    }

    /// The memory-scan pointer for the debug commands.
    pub fn mem_scan(&self) -> u16 {
        self.mem_scan
    }

    /// The display-list mode currently active, for the debug commands.
    pub fn dl_mode(&self) -> Option<u8> {
        self.dl_mode
    }

    /// The playfield width in pixels: narrow 256, normal 320, wide 384.
    fn playfield_width(&self) -> u32 {
        match self.dmactl & 0x03 {
            1 => 256,
            2 => 320,
            3 => 384,
            _ => 0,
        }
    }

    /// Consume at most one display-list instruction at the line start.
    /// Blank instructions hold repeats over their counted scanlines;
    /// jumps and the JVB move or end the walk; mode lines fetch their
    /// display data through the bus.
    fn dl_step(&mut self, tick: Tick, ctx: &mut MachineCtx) {
        if self.blank_left > 0 {
            self.blank_left -= 1;
            self.dl_mode = None;
            self.dl_data.clear();
            return;
        }
        if !self.dl_active {
            self.dl_mode = None;
            return;
        }
        let raw = self.dl_fetch(tick, ctx);
        match raw & 0x0F {
            0 => {
                // Blank lines: the count sits in the upper three bits;
                // bit 7 is the interrupt flag and does not count.
                self.blank_left = u32::from((raw >> 4) & 0x07);
                self.dl_mode = None;
                self.dl_data.clear();
            }
            1 => {
                let lo = if raw == 0x41 {
                    // JVB stores the frame-end pointer and ends the
                    // walk until the next frame.
                    self.dl_active = false;
                    let lo = self.dl_fetch(tick, ctx);
                    let hi = self.dl_fetch(tick, ctx);
                    self.dlist = u16::from_le_bytes([lo, hi]);
                    self.dl_mode = None;
                    return;
                } else {
                    self.dl_fetch(tick, ctx)
                };
                let hi = self.dl_fetch(tick, ctx);
                self.dlist = u16::from_le_bytes([lo, hi]);
                self.dl_mode = None;
            }
            m => {
                if raw & 0x40 != 0 {
                    let lo = self.dl_fetch(tick, ctx);
                    let hi = self.dl_fetch(tick, ctx);
                    self.mem_scan = u16::from_le_bytes([lo, hi]);
                }
                self.dl_mode = Some(raw);
                let bytes = MODE_BYTES.get(usize::from(m - 2)).copied().unwrap_or(40) as usize;
                self.dl_data.clear();
                for i in 0..bytes {
                    let b = ctx.read(
                        &self.fetch_bus,
                        AccessWidth::Byte,
                        Address(u32::from(self.mem_scan.wrapping_add(i as u16))),
                        tick,
                    ) as u8;
                    self.dl_data.push(b);
                }
                self.mem_scan = self.mem_scan.wrapping_add(bytes as u16);
            }
        }
    }

    /// Fetch one display-list byte through the bus.
    fn dl_fetch(&mut self, tick: Tick, ctx: &mut MachineCtx) -> u8 {
        let byte = ctx.read(
            &self.fetch_bus,
            AccessWidth::Byte,
            Address(u32::from(self.dlist)),
            tick,
        ) as u8;
        self.dlist = self.dlist.wrapping_add(1);
        byte
    }

    /// Render one scanline into the screen with the playfield, the
    /// players, and the missiles.
    fn render_line(&mut self) {
        let width = self.playfield_width();
        let start = ((PWIDE - width) / 2) as usize;
        let border = self.colbk & 0xFE;
        self.line_px.fill(border);
        for i in 0..width as usize {
            let x = start + i;
            self.line_px[x] = self.playfield_pixel(i as u32);
        }
        self.draw_players();
        let row_line = self.line.wrapping_sub(TOP_LINE);
        if row_line < FRAME_LINES as u32 {
            let y = row_line as usize;
            for x in 0..PWIDE as usize {
                let [r, g, b] = tia_color_rgb(self.line_px[x], 4);
                let base = (y * PWIDE as usize + x) * 3;
                self.screen[base] = r;
                self.screen[base + 1] = g;
                self.screen[base + 2] = b;
            }
        }
    }

    /// The playfield color byte of one narrow pixel.
    fn playfield_pixel(&self, i: u32) -> u8 {
        let Some(m) = self.dl_mode else {
            return self.colbk & 0xFE;
        };
        let m = m & 0x0F;
        let hi_res = matches!(m, 0x02 | 0x03 | 0x0F);
        if self.dl_data.is_empty() {
            return self.colbk & 0xFE;
        }
        if hi_res {
            let byte = self.dl_data[(i as usize / 8) % self.dl_data.len()];
            let bit = i & 0x07;
            match self.chactl & 0x06 {
                0 => {
                    if byte >> bit & 1 == 1 {
                        self.colpf[1] & 0xFE
                    } else {
                        self.colbk & 0xFE
                    }
                }
                _ => {
                    // Inverse video forms flip the data bit.
                    if byte >> bit & 1 == 0 {
                        self.colpf[1] & 0xFE
                    } else {
                        self.colbk & 0xFE
                    }
                }
            }
        } else {
            // The four-color bitmap form: two bits per pixel.
            let byte = self.dl_data[(i as usize / 4) % self.dl_data.len()];
            let pair = (byte >> (6 - (i & 0x03) * 2)) & 0x03;
            match pair {
                0 => self.colbk & 0xFE,
                1 => self.colpf[0] & 0xFE,
                2 => self.colpf[1] & 0xFE,
                _ => self.colpf[2] & 0xFE,
            }
        }
    }

    /// Overlay the players and the missiles on the line.
    fn draw_players(&mut self) {
        if self.gractl & 0x01 == 0 {
            return;
        }
        for (i, hp) in self.hposp.iter().enumerate() {
            if i32::from(self.grafp[i]) == 0 {
                continue;
            }
            let size = match self.sizep[i] & 0x03 {
                1 => 2,
                3 => 4,
                _ => 1,
            };
            let x0 = (i32::from(*hp) * 2).clamp(0, PWIDE as i32);
            let x1 = (i32::from(*hp) * 2 + 8).clamp(0, PWIDE as i32);
            for x in (x0 as usize..x1 as usize).step_by(size as usize) {
                self.line_px[x] = self.colpm[i] & 0xFE;
            }
        }
        // Missiles draw from the GRAFM bits at the missile positions.
        if self.gractl & 0x02 == 0 {
            return;
        }
        for (i, hp) in self.hposm.iter().enumerate() {
            for b in 0..8usize {
                if self.grafm >> b & 1 == 1 {
                    let x =
                        (i32::from(*hp) * 2 + i32::try_from(b).unwrap_or(0)).clamp(0, PWIDE as i32);
                    self.line_px[x as usize] = self.colpm[i] & 0xFE;
                }
            }
        }
    }
}

impl BusAccess for Antic {
    fn read(&mut self, _width: AccessWidth, address: Address, _tick: Tick) -> u32 {
        let raw = address.0;
        if raw & 0xF000 == 0xC000 {
            // The GTIA file: read-side collisions, triggers, console.
            let r = (raw & 0x1F) as usize;
            match r {
                0..=0x0F => u32::from(self.collisions[r]),
                0x10..=0x13 => u32::from(self.trig[r - 0x10]),
                0x14 => 0, // The PAL flag: NTSC clears it.
                0x1F => u32::from(self.consol),
                _ => 0xFF,
            }
        } else if raw & 0xFE00 == 0xD400 {
            // The ANTIC file.
            match raw & 0x0F {
                0x0B => u32::from(self.vcount_latch),
                0x0F => u32::from(self.nmist),
                _ => 0xFF,
            }
        } else {
            0xFF
        }
    }

    fn write(&mut self, _width: AccessWidth, address: Address, value: u32, _tick: Tick) {
        let raw = address.0;
        let v = value as u8;
        if raw & 0xF000 == 0xC000 {
            let r = raw & 0x1F;
            match r {
                0..=3 => self.hposp[r as usize] = v,
                4..=7 => self.hposm[(r - 4) as usize] = v,
                8..=0xB => self.sizep[(r - 8) as usize] = v,
                0x0C => self.sizem = v,
                0x0D..=0x10 => self.grafp[(r - 0x0D) as usize] = v,
                0x11 => self.grafm = v,
                0x12..=0x15 => self.colpm[(r - 0x12) as usize] = v & 0xFE,
                0x16..=0x19 => self.colpf[(r - 0x16) as usize] = v & 0xFE,
                0x1A => self.colbk = v & 0xFE,
                0x1B => self.prior = v,
                0x1C => self.vdelay = v,
                0x1D => self.gractl = v,
                0x1E => self.collisions = [0; 16],
                0x1F => self.consol = v,
                _ => {}
            }
        } else if raw & 0xFE00 == 0xD400 {
            match raw & 0x0F {
                0x00 => {
                    self.dmactl = v;
                    // The display-list walk arms with the DMA bit.
                    self.dl_active = v & 0x20 != 0;
                }
                0x01 => self.chactl = v,
                0x02 => self.dlist = (self.dlist & 0xFF00) | u16::from(v),
                0x03 => self.dlist = (self.dlist & 0x00FF) | (u16::from(v) << 8),
                0x04 => self.hscrol = v,
                0x05 => self.vscrol = v,
                0x07 => self.pmbase = v,
                0x09 => self.chbase = v,
                0x0A => self.wsync_pending = true,
                0x0E => self.nmien = v,
                0x0F => self.nmist = 0, // NMIRES resets the sources.
                _ => {}
            }
        }
    }

    fn peek(&self, address: Address) -> u8 {
        let raw = address.0;
        if raw & 0xF000 == 0xC000 {
            match raw & 0x1F {
                0x10..=0x13 => self.trig[((raw & 0x1F) - 0x10) as usize],
                0x1F => self.consol,
                _ => 0,
            }
        } else {
            match raw & 0x0F {
                0x0B => self.vcount_latch,
                0x0F => self.nmist,
                _ => 0,
            }
        }
    }
}

impl VideoSource for Antic {
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

impl Device for Antic {
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

    fn input_mut(&mut self) -> Option<&mut dyn InputSink> {
        Some(self)
    }

    fn reset(&mut self) {
        self.dmactl = 0;
        self.chactl = 0;
        self.dlist = 0;
        self.hscrol = 0;
        self.vscrol = 0;
        self.pmbase = 0;
        self.chbase = 0xF8;
        self.nmien = 0;
        self.nmist = 0;
        self.wsync_pending = false;
        self.nmi_pending = false;
        self.hposp = [0; 4];
        self.hposm = [0; 4];
        self.sizep = [0; 4];
        self.sizem = 0;
        self.grafp = [0; 4];
        self.grafm = 0;
        self.colpm = [0; 4];
        self.colpf = [0; 4];
        self.colbk = 0;
        self.prior = 0;
        self.vdelay = 0;
        self.gractl = 0;
        self.consol = 0x03;
        self.trig = [1; 4];
        self.collisions = [0; 16];
        self.cc = 0;
        self.line = 0;
        self.dl_mode = None;
        self.mem_scan = 0;
        self.dl_data.clear();
        self.frames = 0;
        self.next = None;
        self.last_tick = None;
    }

    fn boot(&mut self, _state: Option<&BootState>) {
        self.reset();
        self.next = Some(Tick(0));
    }

    fn snapshot(&self) -> Snapshot {
        let mut out =
            Vec::with_capacity(6 + 9 + 24 + 4 + 4 + 1 + 1 + 2 + 2 + 2 + 1 + 1 + 1 + 4 + 16 + 8 + 8);
        out.extend_from_slice(b"ANTA");
        out.push(1);
        for r in [
            self.dmactl,
            self.chactl,
            self.hscrol,
            self.vscrol,
            self.pmbase,
            self.chbase,
            self.nmien,
            self.nmist,
            self.vcount_latch,
        ] {
            out.push(r);
        }
        out.extend_from_slice(&self.dlist.to_le_bytes());
        out.extend_from_slice(&self.hposp[..]);
        out.extend_from_slice(&self.hposm[..]);
        out.extend_from_slice(&self.sizep[..]);
        out.push(self.sizem);
        out.extend_from_slice(&self.grafp[..]);
        out.push(self.grafm);
        out.extend_from_slice(&self.colpm[..]);
        out.extend_from_slice(&self.colpf[..]);
        out.push(self.colbk);
        out.push(self.prior);
        out.push(self.vdelay);
        out.push(self.gractl);
        out.push(self.consol);
        out.extend_from_slice(&self.trig[..]);
        out.extend_from_slice(&self.collisions[..]);
        out.extend_from_slice(&self.cc.to_le_bytes());
        out.extend_from_slice(&self.line.to_le_bytes());
        out.extend_from_slice(&self.mem_scan.to_le_bytes());
        out.push(u8::from(self.dl_active));
        out.extend_from_slice(&self.blank_left.to_le_bytes());
        out.push(self.dl_mode.unwrap_or(0xFF));
        out.push(u8::from(self.wsync_pending));
        out.push(u8::from(self.nmi_pending));
        out.extend_from_slice(&self.frames.to_le_bytes());
        let mut padded = [0u8; DATA_LEN];
        for (i, b) in self.dl_data.iter().take(DATA_LEN).enumerate() {
            padded[i] = *b;
        }
        out.extend_from_slice(&padded);
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        let expected = 5
            + 9
            + 2
            + 4
            + 4
            + 4
            + 1
            + 4
            + 1
            + 4
            + 4
            + 1
            + 1
            + 1
            + 1
            + 1
            + 4
            + 16
            + 4
            + 4
            + 2
            + 1 // the walk flag
            + 4 // the blank holds left
            + 1 // the mode instruction byte
            + 1
            + 1
            + 8
            + DATA_LEN;
        if data.len() != expected || &data[..4] != b"ANTA" || data[4] != 1 {
            return Err(SnapshotError::Mismatch(format!(
                "antic chunk must be {expected} bytes"
            )));
        }
        let mut pos = 5usize;
        self.dmactl = data[pos];
        pos += 1;
        self.chactl = data[pos];
        pos += 1;
        self.hscrol = data[pos];
        pos += 1;
        self.vscrol = data[pos];
        pos += 1;
        self.pmbase = data[pos];
        pos += 1;
        self.chbase = data[pos];
        pos += 1;
        self.nmien = data[pos];
        pos += 1;
        self.nmist = data[pos];
        pos += 1;
        self.vcount_latch = data[pos];
        pos += 1;
        self.dlist = u16::from_le_bytes(data[pos..pos + 2].try_into().expect("2 bytes"));
        pos += 2;
        self.hposp.copy_from_slice(&data[pos..pos + 4]);
        pos += 4;
        self.hposm.copy_from_slice(&data[pos..pos + 4]);
        pos += 4;
        self.sizep.copy_from_slice(&data[pos..pos + 4]);
        pos += 4;
        self.sizem = data[pos];
        pos += 1;
        self.grafp.copy_from_slice(&data[pos..pos + 4]);
        pos += 4;
        self.grafm = data[pos];
        pos += 1;
        self.colpm.copy_from_slice(&data[pos..pos + 4]);
        pos += 4;
        self.colpf.copy_from_slice(&data[pos..pos + 4]);
        pos += 4;
        self.colbk = data[pos];
        pos += 1;
        self.prior = data[pos];
        pos += 1;
        self.vdelay = data[pos];
        pos += 1;
        self.gractl = data[pos];
        pos += 1;
        self.consol = data[pos];
        pos += 1;
        self.trig.copy_from_slice(&data[pos..pos + 4]);
        pos += 4;
        self.collisions.copy_from_slice(&data[pos..pos + 16]);
        pos += 16;
        self.cc = u32::from_le_bytes(data[pos..pos + 4].try_into().expect("4 bytes")) % LINE_CLOCKS;
        pos += 4;
        self.line =
            u32::from_le_bytes(data[pos..pos + 4].try_into().expect("4 bytes")) % self.lines;
        pos += 4;
        self.mem_scan = u16::from_le_bytes(data[pos..pos + 2].try_into().expect("2 bytes"));
        pos += 2;
        self.dl_active = data[pos] != 0;
        pos += 1;
        self.blank_left = u32::from_le_bytes(data[pos..pos + 4].try_into().expect("4 bytes"));
        pos += 4;
        let mode = data[pos];
        self.dl_mode = if mode == 0xFF { None } else { Some(mode) };
        pos += 1;
        self.wsync_pending = data[pos] != 0;
        pos += 1;
        self.nmi_pending = data[pos] != 0;
        pos += 1;
        self.frames = u64::from_le_bytes(data[pos..pos + 8].try_into().expect("8 bytes"));
        pos += 8;
        self.dl_data.clear();
        self.dl_data.extend_from_slice(&data[pos..pos + DATA_LEN]);
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
        self.cc += 1;
        if self.cc >= LINE_CLOCKS {
            self.cc %= LINE_CLOCKS;
            self.line = (self.line + 1) % self.lines;
            // The WSYNC hold releases at the next line's blank start.
            ctx.notify("halt", Level::Low, tick);
            self.vcount_latch = (self.line / 2) as u8;
            if self.dmactl & 0x20 != 0 {
                self.dl_step(tick, ctx);
            }
            self.render_line();
            if self.line == self.lines - 1 {
                self.frames += 1;
                self.cached_frame.pixels = self.screen.clone();
                ctx.emit(TriggerEvent::Vblank);
            }
            // The walk arms one line before the display window so the
            // first instruction fetch lands on the window's first line.
            if self.line == TOP_LINE - 1 {
                self.dl_active = self.dmactl & 0x20 != 0;
            }
            // The display-list interrupt lands on the last line of a
            // flagged mode line; the vertical blank on the frame end.
            if self.line == self.lines - 1 && self.nmien & 0x40 != 0 {
                self.nmist |= 0x40;
                self.nmi_pending = true;
            } else if self.dl_mode.is_some_and(|m| m & 0x80 != 0)
                && self.blank_left == 0
                && self.nmien & 0x80 != 0
            {
                self.nmist |= 0x80;
                self.nmi_pending = true;
            }
            if self.nmi_pending {
                self.nmi_pending = false;
                ctx.notify("nmi", Level::High, tick);
                // The NMI level releases after the pulse.
                ctx.notify("nmi", Level::Low, tick);
            }
        }
        self.next = Some(Tick(tick.0 + self.dot_div));
    }
}

impl InputSink for Antic {
    fn inject(&mut self, event: &str) -> bool {
        // Grammar: `trigger<n>` presses (the value reads zero) and
        // `trigger<n>-release` lifts it.
        let form = event.trim().to_ascii_lowercase();
        let (release, rest) = match form.strip_suffix("-release") {
            Some(r) => (true, r),
            None => (false, form.as_str()),
        };
        let Some(n) = rest
            .strip_prefix("trigger")
            .and_then(|v| v.parse::<usize>().ok())
        else {
            return false;
        };
        if n >= 4 {
            return false;
        }
        self.trig[n] = u8::from(!release);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn antic() -> Antic {
        Antic {
            name: "antic".to_string(),
            fetch_bus: "cpu".to_string(),
            dot_div: 2,
            lines: 262,
            dmactl: 0,
            chactl: 0,
            dlist: 0,
            hscrol: 0,
            vscrol: 0,
            pmbase: 0,
            chbase: 0xF8,
            nmien: 0,
            nmist: 0,
            vcount_latch: 0,
            wsync_pending: false,
            nmi_pending: false,
            hposp: [0; 4],
            hposm: [0; 4],
            sizep: [0; 4],
            sizem: 0,
            grafp: [0; 4],
            grafm: 0,
            colpm: [0; 4],
            colpf: [0; 4],
            colbk: 0,
            prior: 0,
            vdelay: 0,
            gractl: 0,
            consol: 0x03,
            trig: [1; 4],
            collisions: [0; 16],
            cc: 0,
            line: 0,
            dl_active: false,
            blank_left: 0,
            dl_mode: None,
            mem_scan: 0,
            dl_data: Vec::new(),
            line_px: vec![0; PWIDE as usize],
            screen: vec![0; PWIDE as usize * FRAME_LINES * 3],
            cached_frame: Frame {
                width: PWIDE,
                height: FRAME_LINES as u32,
                pixels: vec![0; PWIDE as usize * FRAME_LINES * 3],
            },
            frames: 0,
            next: None,
            last_tick: None,
        }
    }

    #[test]
    fn gtia_register_file_round_trip() {
        let mut a = antic();
        a.write(AccessWidth::Byte, Address(0xC000), 0x40, Tick(0));
        assert_eq!(a.hposp[0], 0x40);
        a.write(AccessWidth::Byte, Address(0xC016), 0x28, Tick(0));
        assert_eq!(a.colpf[0], 0x28);
        a.write(AccessWidth::Byte, Address(0xC01E), 0, Tick(0));
        assert_eq!(a.collisions, [0; 16]);
    }

    #[test]
    fn antic_registers_decode() {
        let mut a = antic();
        a.write(AccessWidth::Byte, Address(0xD402), 0x34, Tick(0));
        a.write(AccessWidth::Byte, Address(0xD403), 0x12, Tick(0));
        assert_eq!(a.dlist, 0x1234);
        a.write(AccessWidth::Byte, Address(0xD40E), 0x40, Tick(0));
        assert_eq!(a.nmien, 0x40);
    }

    #[test]
    fn snapshot_round_trip() {
        let mut a = antic();
        a.dlist = 0x42AA;
        a.colbk = 0x54;
        let snap = a.snapshot();
        let mut twin = antic();
        twin.restore(&snap).expect("restore");
        assert_eq!(twin.snapshot(), snap);
        assert_eq!(twin.dlist, 0x42AA);
    }
}
