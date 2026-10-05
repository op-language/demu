//! The Z80-machine display family: the TMS9918A Video Display
//! Processor and its Sega mode-4 descendants as the `tms9918.vdp`, the
//! `sms.vdp`, and the `gg.vdp` device kinds.
//!
//! One device struct covers the three forms. The `tms9918.vdp` kind
//! builds the bare TMS9918A form with the four TMS display modes; the
//! `sms.vdp` kind builds the mode-4 Sega form of the 315-5307 class;
//! the `gg.vdp` kind builds the mode-4 form with the Game Gear color
//! RAM and the 160x144 window over the 256x192 raster.
//!
//! # Schedule
//!
//! The host clock domain runs at 10738635 Hz, three times the NTSC
//! 3579545 Hz color burst. The `dot` divider names the domain ticks
//! per VDP dot and defaults to 2, so one device visit carries 684
//! ticks: one display line of 342 dots, exactly 228 CPU T-states at
//! the 3579545 Hz CPU clock. The frame holds 262 lines. Every visit
//! composes the pending line into the internal raster; the first line
//! past the active display is the frame boundary: it lands the frame
//! interrupt flag, emits a vblank trigger event, publishes the cached
//! frame, counts the frame, and reloads the mode-4 line counter.
//!
//! # Port protocol
//!
//! The device answers its I/O window over the machine bus; the
//! configuration maps the window over the `$BE`/`$BF` port pair and
//! the device dispatches on `address & 1`:
//!
//! | Address | Access |
//! |---------|--------|
//! | even | `$BE`: the data port, a VRAM or CRAM access at the auto-increment address, or the V counter read on the mode-4 forms |
//! | odd | `$BF`: the control port, the two-phase address and register latch, plus the status read |
//!
//! The control write takes two bytes. The first byte stores the low
//! byte of the VRAM address. The second byte decodes bits 6-7: code
//! `00` writes the first byte into the register named by bits 3-0 of
//! the second byte; codes `01`, `10`, and `11` load the VRAM address
//! pointer from bits 0-5 times 256 plus the stored low byte; the read
//! and write codes share the one pointer of this build.
//!
//! The data write stores at the pointer and steps it by one on every
//! form. On the Tms9918 form the data read returns the VRAM byte at
//! the pointer without a step; on the mode-4 forms the data read
//! returns the V counter of the pending line instead of a VRAM byte,
//! so the mode-4 forms carry no data-port VRAM read in this build. The
//! status read returns bit 7, the frame interrupt flag; bit
//! 6, the fifth-sprite flag on the Tms9918 form and the sprite
//! overflow flag on the mode-4 forms; bit 5, the sprite collision
//! flag; and bits 3-0, the recorded sprite index. The read clears the
//! frame flag, the line-interrupt pending flag, and the collision and
//! fifth-sprite flags.
//!
//! The mode-4 CRAM hides under the VRAM address space: data-port
//! addresses `$0000-$003F` reach the color RAM instead of the VRAM on
//! the Sms and GameGear forms, 32 one-byte entries on the Sms form
//! with addresses `$0020-$003F` mirrored onto `$0000-$001F`, and 32
//! two-byte entries on the GameGear form.
//!
//! # Registers
//!
//! | Register | Content |
//! |----------|---------|
//! | R0 | bit 7 vertical scroll lock (mode 4), bit 6 horizontal scroll lock (mode 4), bit 5 left column mask (mode 4), bit 4 line-interrupt enable IE1 (mode 4), bit 3 sprite shift-left-8 (mode 4), bit 2 mode-4 select, bit 1 Tms M3 select; other bits stored |
//! | R1 | bit 6 display enable, bit 5 frame-interrupt enable IE0, bit 4 Tms M1 select (Sms form: the 224-line select behind `extended_height`), bit 3 Tms M2 select (Sms form: the 240-line select behind `extended_height`), bit 1 sprite size select (Tms: 16x16, mode 4: 8x16), bit 0 magnify select (mode 4: the zoom doubler) |
//! | R2 | name table base = `(value & 0x0F) << 10`, one base form for every mode of this build |
//! | R3 | Tms color table base = `(value & 0xFF) << 6`; stored without effect on the mode-4 forms |
//! | R4 | Tms pattern table base = `(value & 0x07) << 11`; Graphics II uses bits 1-0 as the page slot of each 8 KiB area; the mode-4 forms store it without effect, the mode-4 pattern generator sits at VRAM `$0000` |
//! | R5 | sprite list base = `(value & 0x7F) << 7` |
//! | R6 | sprite pattern base = `(value & 0x07) << 11`; the mode-4 form adds `$2000` while bit 3 sets |
//! | R7 | Tms form: bits 3-0 the text foreground color, bits 7-4 the text backdrop color; mode 4: bits 3-0 the backdrop CRAM entry |
//! | R8 | mode-4 horizontal scroll; stored without effect on the Tms9918 form |
//! | R9 | mode-4 vertical scroll; stored without effect on the Tms9918 form |
//! | R10 | mode-4 line-interrupt counter reload value; values `$C0-$FF` disable the line interrupt |
//!
//! The Tms9918 form renders the four base modes and falls through the
//! stated invalid mode-bit combinations to Graphics I; the Sms and
//! GameGear forms always render mode 4, so the M4 select bit carries
//! no decision in this build.
//!
//! # Tms9918 display modes
//!
//! | Mode | Selection | Form |
//! |------|-----------|------|
//! | Graphics I | no mode bits set | 32x24 name entries, one-bit patterns, one color byte per eight characters |
//! | Text 40 | M1 | 40 six-pixel characters over the first 23 rows, the last row shows the text backdrop, columns `$F0-$FF` show the text backdrop |
//! | Graphics II | M3 | 32x24 name entries over three 8 KiB pattern and color areas |
//! | Multicolor | M2 | four 4x4 blocks per character, two color bits per block from the name byte |
//!
//! Every Tms9918 pixel color passes through the fixed palette; the
//! color 0 entry doubles as the transparent backdrop.
//!
//! # Palettes
//!
//! The Tms9918 form colors pixels through the fixed 16-entry TMS9918A
//! palette table, the openMSX reference values. The Sms form colors
//! pixels through 32 one-byte CRAM entries whose bits hold red in 1-0,
//! blue in 3-2, and green in 5-4. The GameGear form colors pixels
//! through 32 two-byte CRAM entries whose first byte holds blue in
//! bits 3-0 and green in bits 7-4 and whose second byte holds red in
//! bits 3-0. Two-bit channels scale by fourfold replication, four-bit
//! channels by twofold replication.
//!
//! # Mode 4 raster rules
//!
//! The background walks a 32x28 name table from R2; the vertical
//! coordinate `(line + R9) mod 224` names the tile row and the
//! horizontal coordinate `(column + R8) mod 256` names the tile
//! column, so the table row rolls over at 224 pixels. R0 bit 7 holds
//! the top 16 rows to the horizontal scroll 0, and R0 bit 6 holds the
//! left 8 columns to the vertical scroll 0. R0 bit 5 blanks the left 8
//! columns to the backdrop and drops their sprite pixels. Transparent
//! pixels of color 0 and the band past the active display show the
//! backdrop CRAM entry from R7 bits 3-0. The four plane bits name the
//! pixel color, so the background covers CRAM entries `$00-$0F`.
//!
//! The sprite layer lists 64 sprites until the `$D0` stored row value
//! ends the list; the stored value carries the top row minus one, so
//! line `top + 1` draws the first sprite row. Eight sprites per line
//! draw, and the ninth in list order sets the overflow flag with its
//! index in the status bits. Sprites paint in list order, the lowest
//! index wins, and two opaque sprite pixels on one column set the
//! collision flag. A sprite pixel takes its color from the four plane
//! bits into the CRAM entries `$10-$1F`. R1 bit 1 selects the 8x16
//! form whose lower half takes the pattern `n + 16`; R1 bit 0 doubles
//! every sprite axis; R0 bit 3 shifts every sprite eight pixels left.
//!
//! The Tms9918 form lists 32 sprites, draws the first four on a line,
//! and sets the fifth-sprite flag with the fifth index in the status
//! bits instead of the overflow flag. Attribute bits 3-0 hold the
//! single sprite color for the whole 8x8 or 16x16 form, color 0 stays
//! transparent, and attribute bit 7 shifts the sprite 32 pixels right
//! as the early clock. R1 bit 1 selects the 16x16 form whose two tiles
//! sit side by side under the even pattern number; R1 bit 0 doubles
//! both axes.
//!
//! # Interrupts
//!
//! Every visit recomputes the `irq` line: High while the frame flag
//! holds with IE0 set on R1 bit 5, or while the mode-4 line flag holds
//! with IE1 set on R0 bit 4; Low otherwise. An edge notifies `irq`,
//! and the `irq` line drops after the status read clears the flags.
//! The frame flag sets once per frame at the first line past the
//! active display and clears on the status read. The mode-4 line
//! counter loads R10 at line 0 and at the frame boundary, counts down
//! on the other lines, and fires on the underflow from zero toward
//! `$FF`; R10 values 0, 1, and 2 of the published form request every
//! line, every second line, and every third line; values `$C0` and up
//! disable, and the fired request reloads the counter.
//!
//! # Recorded approximations
//!
//! - The one pointer of the address latch stands for the separate read
//!   and write pointers of the published chip; the data port infers
//!   the direction.
//! - The mode-4 data read returns the V counter; the published split
//!   of the V counter onto port `$7E` and the VRAM data read on `$BE`
//!   needs the second window and stays out of build, so the mode-4
//!   forms offer no data-port VRAM reads here.
//! - The horizontal counter read lacks a mapped port; the published
//!   `$7E` window is recorded out of build.
//! - The Graphics II pattern and color tables sit at the register low
//!   two bits times `$800` plus the `$800` area offset per third; the
//!   published register-bit split is recorded as this simplified form.
//! - The Multicolor block color combines the two name bits of the
//!   block over the two pattern bits of the block row band; the
//!   published scheme keeps the same two color sources.
//! - The Tms9918 text mode keeps the right `$10`-column band on the
//!   backdrop instead of the published centered border and shows no
//!   sprite suppression in text mode.
//! - The mode-4 sprites keep no use for the attribute control byte 3;
//!   the published always-`$10` palette offset form matches this
//!   build, and the second pattern bank of R6 bit 3 moves the pattern
//!   block only.
//! - The mode-4 name table base uses `(R2 & 0x0F) << 10` on every
//!   form; the SMS1 mask-bit form is recorded as this single form.
//! - The `extended_height` parameter arms the 224- and 240-line R1
//!   forms of the Sms form: R1 bit 4 picks 224 lines and R1 bit 3
//!   picks 240 lines with bit 4 winning when both set. The GameGear
//!   form always renders 192 lines, and every form publishes 192
//!   lines without the parameter.
//! - The snapshot carries only the state the ports can change; the
//!   restore re-composes the active display from the restored
//!   registers and VRAM, so a restored frame reads one instant of the
//!   frame, and the restored flag values land after that re-compose
//!   pass.
//!
//! Sources: the TMS9918A Program Manual, the published Sega 8-bit VDP
//! documents, and the openMSX and SMS Power! reference
//! implementations, in particular the openMSX `VDPtms9918.cc` and
//! `VDP.cc` forms and the SMS Power! VDP register table.

use demu_config::model::{BootState, DeviceInstance, SystemConfig};
use demu_core::device::{AccessWidth, BusAccess, Frame, MachineCtx, VideoLayer, VideoSource};
use demu_core::frequency::{Address, Tick};
use demu_core::signal::Level;
use demu_core::triggers::TriggerEvent;
use demu_core::{Device, Snapshot, SnapshotError};

/// The interrupt line the display processor drives.
pub const IRQ_LINE: &str = "irq";

/// The display dots of one scanline: 684 domain ticks at the default
/// divider of 2, exactly 228 CPU T-states.
const LINE_DOTS: u64 = 342;
/// The lines of one frame.
const FRAME_LINES: u32 = 262;
/// The active display lines without the extended Sms forms.
const BASE_LINES: u32 = 192;
/// The tallest raster the internal buffer holds.
const MAX_LINES: u32 = 240;
/// The raster width of the internal buffer.
const WIDTH: usize = 256;
/// The VRAM size in bytes.
const VRAM_SIZE: usize = 0x4000;
/// The VRAM address mask.
const VRAM_MASK: usize = VRAM_SIZE - 1;
/// The first Game Gear window row on the raster.
const GG_CROP_Y: usize = 24;
/// The Game Gear window width.
const GG_W: usize = 160;
/// The Game Gear window height.
const GG_H: usize = 144;
/// The register count the control write accepts.
const REG_COUNT: usize = 11;
/// The sprite list entry count of mode 4.
const MODE4_SPRITES: usize = 64;
/// The sprite list entry count of the Tms9918 form.
const TMS_SPRITES: usize = 32;
/// The sprite entries that draw per mode-4 line.
const MODE4_PER_LINE: usize = 8;
/// The sprite entries that draw per Tms9918 line.
const TMS_PER_LINE: usize = 4;
/// The sprite list row value that ends the scan; the stored value
/// carries the top row minus one.
const SPRITE_STOP: u8 = 0xD0;
/// The mode-4 sprite palette offset into the CRAM.
const MODE4_SPRITE_COLOR: u8 = 0x10;
/// The default domain ticks per display dot.
const DEFAULT_DOT_DIV: u32 = 2;
/// The snapshot bytes after the tag, version, VRAM, and CRAM span:
/// eleven registers, both latch bytes, the pointer, the line, five
/// flag bytes, the line counter, and the frame counter.
const SNAP_TAIL: usize = 11 + 1 + 1 + 2 + 4 + 1 + 1 + 1 + 1 + 1 + 1 + 8;

/// The fixed 16-entry TMS9918A palette in the openMSX reference
/// values; entries 0 and 1 both read black.
const TMS_PALETTE: [[u8; 3]; 16] = [
    [0, 0, 0],
    [0, 0, 0],
    [33, 200, 66],
    [94, 220, 120],
    [84, 85, 237],
    [125, 118, 252],
    [212, 82, 77],
    [66, 235, 245],
    [252, 85, 84],
    [255, 121, 120],
    [212, 193, 84],
    [230, 206, 128],
    [33, 176, 59],
    [91, 110, 225],
    [159, 63, 53],
    [233, 238, 207],
];

/// Scale one two-bit channel to eight bits by replication.
fn scale2(v: u8) -> u8 {
    (v << 6) | (v << 4) | (v << 2) | v
}

/// Scale one four-bit channel to eight bits by replication.
fn scale4(v: u8) -> u8 {
    (v << 4) | v
}

/// One device form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VdpForm {
    /// The bare TMS9918A.
    Tms9918,
    /// The Sega mode-4 form of the 315-5307 class.
    Sms,
    /// The mode-4 form with the Game Gear color RAM and window.
    GameGear,
}

impl VdpForm {
    /// Pick the form from the device kind.
    fn from_kind(kind: &str) -> Self {
        if kind.starts_with("tms9918.vdp") {
            Self::Tms9918
        } else if kind.starts_with("gg.vdp") {
            Self::GameGear
        } else {
            // The sms.vdp class is the default of the mode-4 kinds.
            Self::Sms
        }
    }

    /// The form renders mode 4.
    fn mode4(&self) -> bool {
        *self != Self::Tms9918
    }

    /// The form owns the Game Gear window.
    fn gg(&self) -> bool {
        *self == Self::GameGear
    }

    /// The CRAM bytes the form keeps.
    fn cram_len(&self) -> usize {
        match self {
            Self::Tms9918 => 0,
            Self::Sms => 32,
            Self::GameGear => 64,
        }
    }

    /// The snapshot tag of the form.
    fn tag(&self) -> [u8; 4] {
        match self {
            Self::Tms9918 => *b"TMVD",
            Self::Sms => *b"SMVD",
            Self::GameGear => *b"GGVD",
        }
    }
}

/// A Tms9918 display mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TmsMode {
    /// The 32x24 two-color tile mode.
    Graphics1,
    /// The 40-column text mode.
    Text40,
    /// The three-area tile mode.
    Graphics2,
    /// The four-block color mode.
    Multicolor,
}

/// One scanned sprite entry of the pending line.
#[derive(Debug, Clone, Copy, Default)]
struct SprEntry {
    /// The OAM index.
    index: u8,
    /// The stored top row minus one.
    y: u8,
    /// The attribute left column.
    x: u8,
    /// The attribute pattern number.
    pat: u8,
    /// The attribute control byte: bits 3-0 (Tms) the color index with
    /// bit 7 the early clock, stored unused on the mode-4 forms.
    attr3: u8,
}

/// The display processor.
#[derive(Debug)]
pub struct Vdp {
    name: String,
    /// The device form picked from the kind.
    form: VdpForm,
    /// The domain ticks per display dot, from the `dot` divider.
    dot_div: u64,
    /// The Sms form answers the 224- and 240-line R1 forms.
    extended_height: bool,
    /// The 16 KiB VRAM.
    vram: Box<[u8; VRAM_SIZE]>,
    /// The mode-4 color RAM; the Tms9918 form leaves it idle.
    cram: Box<[u8; 64]>,
    /// The internal registers R0-R10.
    regs: [u8; REG_COUNT],
    /// The first control-write byte of the two-phase latch.
    addr_low: u8,
    /// The latch phase: 0 idle, 1 low byte latched.
    latch_phase: u8,
    /// The VRAM address pointer.
    addr: u16,
    /// The frame interrupt flag.
    frame_pending: bool,
    /// The fifth-sprite flag on the Tms9918 form, the sprite overflow
    /// flag on the mode-4 forms.
    five_flag: bool,
    /// The sprite collision flag.
    collision: bool,
    /// The recorded fifth-sprite or overflow index.
    five_index: u8,
    /// The mode-4 line-interrupt pending flag.
    line_pending: bool,
    /// The pending display line.
    line: u32,
    /// The mode-4 line-interrupt counter.
    line_counter: u8,
    /// The last `irq` level this device notified.
    line_irq_driven: bool,
    /// The complete frames since boot.
    frames: u64,
    /// The internal raster in RGB888, `WIDTH` by `MAX_LINES`.
    screen: Vec<u8>,
    /// The cached frame for the shell.
    frame: Frame,
    /// The scanned sprite entries of the pending line.
    spr_scan: Vec<SprEntry>,
    next: Option<Tick>,
    last_tick: Option<Tick>,
}

impl Vdp {
    /// A display processor from its configuration entry.
    ///
    /// The kind name picks the form. The `extended_height` parameter
    /// arms the 224- and 240-line R1 forms of the Sms kind. The `dot`
    /// divider defaults to 2.
    pub fn new(device: &DeviceInstance, _config: &SystemConfig) -> Self {
        let form = VdpForm::from_kind(device.kind.as_str());
        let dot_div = u64::from(
            device
                .dividers
                .get("dot")
                .copied()
                .unwrap_or(DEFAULT_DOT_DIV),
        )
        .max(1);
        let extended_height = device
            .params
            .get("extended_height")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        Self {
            name: device.name.to_string(),
            form,
            dot_div,
            extended_height,
            vram: Box::new([0; VRAM_SIZE]),
            cram: Box::new([0; 64]),
            regs: [0; REG_COUNT],
            addr_low: 0,
            latch_phase: 0,
            addr: 0,
            frame_pending: false,
            five_flag: false,
            collision: false,
            five_index: 0,
            line_pending: false,
            line: 0,
            line_counter: 0,
            line_irq_driven: false,
            frames: 0,
            screen: vec![0; WIDTH * MAX_LINES as usize * 3],
            frame: Frame {
                width: WIDTH as u32,
                height: BASE_LINES,
                pixels: vec![0; WIDTH * BASE_LINES as usize * 3],
            },
            spr_scan: Vec::with_capacity(MODE4_SPRITES),
            next: None,
            last_tick: None,
        }
    }

    /// The line cadence in domain ticks.
    fn line_step(&self) -> u64 {
        LINE_DOTS * self.dot_div
    }

    /// The active display lines of the current register form.
    fn active_lines(&self) -> u32 {
        match self.form {
            VdpForm::Sms if self.extended_height => {
                if self.regs[1] & 0x10 != 0 {
                    224
                } else if self.regs[1] & 0x08 != 0 {
                    240
                } else {
                    BASE_LINES
                }
            }
            _ => BASE_LINES,
        }
    }

    /// The display enable of R1 bit 6.
    fn display_on(&self) -> bool {
        self.regs[1] & 0x40 != 0
    }

    /// The V counter of the pending line: values 0-218 report direct
    /// and the fold reports values 213-255 past line 218.
    fn vcounter(&self) -> u8 {
        if self.line < 219 {
            self.line as u8
        } else {
            (self.line - 6) as u8
        }
    }

    /// The mode-4 backdrop RGB from the R7 CRAM entry; the Tms9918
    /// form holds the fixed palette entry 0.
    fn backdrop_rgb(&self) -> [u8; 3] {
        match self.form {
            VdpForm::Tms9918 => TMS_PALETTE[0],
            _ => {
                let i = usize::from(self.regs[7] & 0x0F);
                self.cram_rgb(i)
            }
        }
    }

    /// The RGB888 of one CRAM entry.
    ///
    /// The Sms byte holds red in bits 1-0, blue in bits 3-2, and green
    /// in bits 5-4; the GameGear pair holds blue in first-byte bits
    /// 3-0, green in first-byte bits 7-4, and red in second-byte bits
    /// 3-0.
    fn cram_rgb(&self, index: usize) -> [u8; 3] {
        match self.form {
            VdpForm::GameGear => {
                let i = index & 0x1F;
                let b = self.cram[i * 2] & 0x0F;
                let g = self.cram[i * 2] >> 4;
                let r = self.cram[i * 2 + 1] & 0x0F;
                [scale4(r), scale4(g), scale4(b)]
            }
            _ => {
                let v = self.cram[index & 0x1F];
                [scale2(v & 3), scale2((v >> 4) & 3), scale2((v >> 2) & 3)]
            }
        }
    }

    /// The RGB888 table of the 32 CRAM entries.
    fn cram_table(&self) -> [[u8; 3]; 32] {
        let mut out = [[0u8; 3]; 32];
        for (i, entry) in out.iter_mut().enumerate() {
            *entry = self.cram_rgb(i);
        }
        out
    }

    /// The status byte without side effects.
    fn status_bits(&self) -> u8 {
        (u8::from(self.frame_pending) << 7)
            | (u8::from(self.five_flag) << 6)
            | (u8::from(self.collision) << 5)
            | (self.five_index & 0x1F)
    }

    /// The status read: the flags out, then all flags cleared.
    fn status_read(&mut self) -> u8 {
        let out = self.status_bits();
        self.frame_pending = false;
        self.five_flag = false;
        self.collision = false;
        self.line_pending = false;
        out
    }

    /// The control write: the two-phase latch, then the code field.
    fn control_write(&mut self, v: u8) {
        if self.latch_phase == 0 {
            self.addr_low = v;
            self.latch_phase = 1;
            return;
        }
        self.latch_phase = 0;
        match (v >> 6) & 0x03 {
            0 => {
                let index = v & 0x1F;
                if usize::from(index) < REG_COUNT {
                    self.regs[usize::from(index)] = self.addr_low;
                    // The R10 write reloads the line counter.
                    if index == 10 && self.form.mode4() {
                        self.line_counter = self.addr_low;
                    }
                }
            }
            // The read and write codes share the one pointer here.
            _ => {
                self.addr = (u16::from(v & 0x3F) << 8) | u16::from(self.addr_low);
            }
        }
    }

    /// The data read: the V counter on the mode-4 forms, the VRAM
    /// byte on the Tms9918 form without a pointer step.
    fn data_read(&mut self) -> u8 {
        if self.form.mode4() {
            return self.vcounter();
        }
        self.vram[usize::from(self.addr) & VRAM_MASK]
    }

    /// The data write at the pointer with the auto-increment; the
    /// mode-4 forms route the addresses below `$40` to the CRAM.
    fn data_write(&mut self, v: u8) {
        let a = usize::from(self.addr) & VRAM_MASK;
        if self.form.mode4() && a < 0x40 {
            match self.form {
                VdpForm::GameGear => self.cram[a] = v,
                // The Sms CRAM mirrors `$0020-$003F` on `$0000-$001F`.
                _ => self.cram[a & 0x1F] = v,
            }
        } else {
            self.vram[a] = v;
        }
        self.addr = self.addr.wrapping_add(1);
    }

    /// The mode-4 line-interrupt countdown with the `R10` reloads.
    fn step_line_counter(&mut self) {
        let reload = self.regs[10];
        // The disable band of `$C0-$FF`.
        if reload >= 0xC0 {
            return;
        }
        let pre = self.line_counter;
        self.line_counter = pre.wrapping_sub(1);
        // The underflow from zero toward `$FF` fires and reloads.
        if pre == 0 {
            self.line_pending = true;
            self.line_counter = reload;
        }
    }

    /// Fill one raster row with one RGB triple.
    fn fill_row(&mut self, row: usize, rgb: [u8; 3]) {
        for x in 0..WIDTH {
            let o = row * WIDTH * 3 + x * 3;
            self.screen[o] = rgb[0];
            self.screen[o + 1] = rgb[1];
            self.screen[o + 2] = rgb[2];
        }
    }

    /// Compose one pending line into the internal raster.
    fn compose_line(&mut self, line: u32) {
        let row = line as usize;
        if row >= MAX_LINES as usize {
            return;
        }
        if !self.display_on() {
            // The blanked display shows the backdrop.
            let rgb = self.backdrop_rgb();
            self.fill_row(row, rgb);
            return;
        }
        if row >= self.active_lines() as usize {
            return;
        }
        if self.form.mode4() {
            self.compose_mode4_line(line);
        } else {
            self.compose_tms_line(line);
        }
    }

    /// Compose one mode-4 line: the background walk, the sprite walk,
    /// then the CRAM conversion.
    fn compose_mode4_line(&mut self, line: u32) {
        let mut colors = [0u8; WIDTH];
        let mut drew = [false; WIDTH];
        self.compose_mode4_bg(line, &mut colors);
        self.compose_mode4_sprites(line, &mut colors, &mut drew);
        let pal = self.cram_table();
        let backdrop = self.backdrop_rgb();
        let base = (line as usize) * WIDTH * 3;
        for x in 0..WIDTH {
            let rgb = if colors[x] == 0 {
                backdrop
            } else {
                // The sprite colors carry the `$10` CRAM offset.
                pal[usize::from(colors[x])]
            };
            let o = base + x * 3;
            self.screen[o] = rgb[0];
            self.screen[o + 1] = rgb[1];
            self.screen[o + 2] = rgb[2];
        }
    }

    /// The mode-4 background walk with the scroll and lock rules.
    fn compose_mode4_bg(&self, line: u32, colors: &mut [u8; WIDTH]) {
        let hscroll = u32::from(self.regs[8]);
        let vscroll = u32::from(self.regs[9]);
        let vlock = self.regs[0] & 0x80 != 0;
        let hlock = self.regs[0] & 0x40 != 0;
        let blank_left = self.regs[0] & 0x20 != 0;
        let name_base = (usize::from(self.regs[2] & 0x0F)) << 10;
        let pattern_base = (usize::from(self.regs[4] & 0x07)) << 11;
        for (x, slot) in colors.iter_mut().enumerate() {
            // R0 bit 5 blanks the left 8 columns to the backdrop.
            if blank_left && x < 8 {
                *slot = 0;
                continue;
            }
            // The left 8 columns hold the vertical scroll 0.
            let vy = (line + u32::from(!(hlock && x < 8)) * vscroll) % 224;
            // The top 16 rows hold the horizontal scroll 0.
            let hx = x as u32 + if vlock && line < 16 { 0 } else { hscroll };
            let name = self.vram[name_base + (vy >> 3) as usize * 32 + ((hx & 0xFF) >> 3) as usize];
            let pa = pattern_base + usize::from(name) * 32 + (vy & 7) as usize * 4;
            let bit = 7 - (x & 7);
            *slot = (self.vram[pa] >> bit) & 1
                | (((self.vram[pa + 1] >> bit) & 1) << 1)
                | (((self.vram[pa + 2] >> bit) & 1) << 2)
                | (((self.vram[pa + 3] >> bit) & 1) << 3);
        }
    }

    /// The mode-4 sprite walk: the list scan to the first `$D0` value,
    /// the eight-per-line limit with the overflow record, then the
    /// paint in list order with the lowest index winning.
    fn compose_mode4_sprites(
        &mut self,
        line: u32,
        colors: &mut [u8; WIDTH],
        drew: &mut [bool; WIDTH],
    ) {
        self.scan_sprites(line);
        if self.spr_scan.len() > MODE4_PER_LINE {
            let idx = self.spr_scan[MODE4_PER_LINE].index;
            self.five_flag = true;
            self.five_index = idx;
        }
        let pattern_base = ((usize::from(self.regs[6] & 0x07)) << 11)
            + usize::from(self.regs[6] & 0x08 != 0) * 0x2000;
        let tall = self.regs[1] & 0x02 != 0;
        let zoom = self.regs[1] & 0x01 != 0;
        let shift = i32::from(u8::from(self.regs[0] & 0x08 != 0)) * 8;
        let zp = usize::from(zoom);
        for i in 0..self.spr_scan.len().min(MODE4_PER_LINE) {
            let s = self.spr_scan[i];
            let x0 = i32::from(s.x) - shift;
            let width = 8usize << zp;
            let crow = (line as i32 - (i32::from(s.y) + 1)) as usize;
            let row8 = ((crow >> zp) & 7) * 4;
            let tile = usize::from(if tall {
                s.pat.wrapping_add(if crow >> zp >= 8 { 16 } else { 0 })
            } else {
                s.pat
            });
            for px in 0..width {
                let sx = (x0 + px as i32).rem_euclid(256) as usize;
                let addr = pattern_base + tile * 32 + row8;
                let bit = 7 - ((px >> zp) & 7);
                let pix = (self.vram[addr & VRAM_MASK] >> bit) & 1
                    | (((self.vram[(addr + 1) & VRAM_MASK] >> bit) & 1) << 1)
                    | (((self.vram[(addr + 2) & VRAM_MASK] >> bit) & 1) << 2)
                    | (((self.vram[(addr + 3) & VRAM_MASK] >> bit) & 1) << 3);
                if pix == 0 {
                    continue;
                }
                // The palette offset holds the sprite colors above the
                // background half of the CRAM.
                let color = pix | MODE4_SPRITE_COLOR;
                if drew[sx] {
                    self.collision = true;
                } else {
                    drew[sx] = true;
                    colors[sx] = color;
                }
            }
        }
    }

    /// The Tms9918 line compose through the mode of the registers.
    fn compose_tms_line(&mut self, line: u32) {
        let mut colors = [0u8; WIDTH];
        let mut drew = [false; WIDTH];
        match self.tms_mode() {
            TmsMode::Graphics1 => self.compose_g1(line, &mut colors),
            TmsMode::Graphics2 => self.compose_g2(line, &mut colors),
            TmsMode::Text40 => self.compose_text(line, &mut colors),
            TmsMode::Multicolor => self.compose_mc(line, &mut colors),
        }
        self.compose_tms_sprites(line, &mut colors, &mut drew);
        let base = (line as usize) * WIDTH * 3;
        for x in 0..WIDTH {
            let rgb = TMS_PALETTE[(colors[x] & 0x0F) as usize];
            let o = base + x * 3;
            self.screen[o] = rgb[0];
            self.screen[o + 1] = rgb[1];
            self.screen[o + 2] = rgb[2];
        }
    }

    /// The Tms9918 mode select from M1, M2, and M3; the unstated
    /// combinations fall through to Graphics I.
    fn tms_mode(&self) -> TmsMode {
        let m1 = self.regs[1] & 0x10 != 0;
        let m2 = self.regs[1] & 0x08 != 0;
        let m3 = self.regs[0] & 0x02 != 0;
        if m3 && !m1 && !m2 {
            TmsMode::Graphics2
        } else if m1 && !m2 && !m3 {
            TmsMode::Text40
        } else if m2 && !m1 && !m3 {
            TmsMode::Multicolor
        } else {
            TmsMode::Graphics1
        }
    }

    /// The Graphics I walk: one-bit patterns and one color byte per
    /// eight characters.
    fn compose_g1(&self, line: u32, colors: &mut [u8; WIDTH]) {
        let name_base = (usize::from(self.regs[2] & 0x0F)) << 10;
        let pattern_base = (usize::from(self.regs[4] & 0x07)) << 11;
        let color_base = usize::from(self.regs[3]) << 6;
        let row = (line as usize) / 8;
        let r = (line as usize) & 7;
        for (x, slot) in colors.iter_mut().enumerate() {
            let name = usize::from(self.vram[name_base + row * 32 + x / 8]);
            let pat = self.vram[(pattern_base + name * 8 + r) & VRAM_MASK];
            let colb = self.vram[(color_base + (name >> 3)) & VRAM_MASK];
            let bit = (pat >> (7 - (x & 7))) & 1;
            *slot = if bit != 0 { colb >> 4 } else { colb & 0x0F };
        }
    }

    /// The Graphics II walk over the three pattern and color areas.
    fn compose_g2(&self, line: u32, colors: &mut [u8; WIDTH]) {
        let name_base = (usize::from(self.regs[2] & 0x0F)) << 10;
        let pattern_base = (usize::from(self.regs[4] & 0x03)) << 11;
        let color_base = (usize::from(self.regs[3] & 0x03)) << 11;
        let area = ((line as usize) >> 6) << 11;
        let row = (line as usize) / 8;
        let r = (line as usize) & 7;
        for (x, slot) in colors.iter_mut().enumerate() {
            let name = usize::from(self.vram[name_base + row * 32 + x / 8]);
            let pat = self.vram[(pattern_base + area + name * 8 + r) & VRAM_MASK];
            let colb = self.vram[(color_base + area + name * 8 + r) & VRAM_MASK];
            let bit = (pat >> (7 - (x & 7))) & 1;
            *slot = if bit != 0 { colb >> 4 } else { colb & 0x0F };
        }
    }

    /// The Text 40 walk: six-pixel characters over the first 23 rows
    /// at the fixed R7 colors, the last row and the right band on the
    /// text backdrop.
    fn compose_text(&self, line: u32, colors: &mut [u8; WIDTH]) {
        let name_base = (usize::from(self.regs[2] & 0x0F)) << 10;
        let pattern_base = (usize::from(self.regs[4] & 0x07)) << 11;
        let fg = self.regs[7] & 0x0F;
        let bg = self.regs[7] >> 4;
        let row = (line as usize) / 8;
        let r = (line as usize) & 7;
        for (x, slot) in colors.iter_mut().enumerate() {
            if row >= 23 || x >= 240 {
                *slot = bg;
                continue;
            }
            let name = usize::from(self.vram[name_base + row * 40 + x / 6]);
            let pat = self.vram[(pattern_base + name * 8 + r) & VRAM_MASK];
            let bit = (pat >> (7 - (x % 6))) & 1;
            *slot = if bit != 0 { fg } else { bg };
        }
    }

    /// The Multicolor walk: four 4x4 blocks per character, each block
    /// color the two name bits over the two pattern bits of the block
    /// row band.
    fn compose_mc(&self, line: u32, colors: &mut [u8; WIDTH]) {
        let name_base = (usize::from(self.regs[2] & 0x0F)) << 10;
        let pattern_base = (usize::from(self.regs[4] & 0x07)) << 11;
        let row = (line as usize) / 8;
        let r = (line as usize) & 7;
        for (x, slot) in colors.iter_mut().enumerate() {
            let name = usize::from(self.vram[name_base + row * 32 + x / 8]);
            let by = r >> 2;
            let bx = (x & 7) >> 2;
            let block = by * 2 + bx;
            let pat = self.vram[(pattern_base + name * 8 + by * 4) & VRAM_MASK];
            *slot = (((name >> (block * 2)) & 3) as u8) << 2 | ((pat >> (6 - bx * 2)) & 3);
        }
    }

    /// The Tms9918 sprite walk: 32 entries, four per line, the fifth
    /// sets the flag with its index, the early clock shifts right, and
    /// the 16x16 form pairs the even pattern numbers side by side.
    fn compose_tms_sprites(
        &mut self,
        line: u32,
        colors: &mut [u8; WIDTH],
        drew: &mut [bool; WIDTH],
    ) {
        self.scan_sprites(line);
        if self.spr_scan.len() > TMS_PER_LINE {
            let idx = self.spr_scan[TMS_PER_LINE].index;
            self.five_flag = true;
            self.five_index = idx;
        }
        let pattern_base = (usize::from(self.regs[6] & 0x07)) << 11;
        let size16 = self.regs[1] & 0x02 != 0;
        let zp = usize::from(self.regs[1] & 0x01 != 0);
        for i in 0..self.spr_scan.len().min(TMS_PER_LINE) {
            let s = self.spr_scan[i];
            let x0 = i32::from(s.x) + i32::from(u8::from(s.attr3 & 0x80 != 0)) * 32;
            // The 16x16 form spans the two side-by-side tiles.
            let width = (if size16 { 16 } else { 8 }) << zp;
            let crow = (line as i32 - (i32::from(s.y) + 1)) as usize;
            let row8 = (crow >> zp) & 7;
            for px in 0..width {
                let sx = (x0 + px as i32).rem_euclid(256) as usize;
                let pat_x = px >> zp;
                let tile = if size16 {
                    usize::from(s.pat & 0xFE) + (pat_x >> 3)
                } else {
                    usize::from(s.pat)
                };
                let addr = pattern_base + tile * 8 + row8;
                let bit = 7 - (pat_x & 7);
                if (self.vram[addr & VRAM_MASK] >> bit) & 1 == 0 {
                    continue;
                }
                // Tms sprite color 0 stays transparent.
                let color = s.attr3 & 0x0F;
                if color == 0 {
                    continue;
                }
                if drew[sx] {
                    self.collision = true;
                } else {
                    drew[sx] = true;
                    colors[sx] = color;
                }
            }
        }
    }

    /// The sprite list scan of one line: the entries in list order
    /// until the first `$D0` value, each carrying its OAM index for
    /// the flag records.
    fn scan_sprites(&mut self, line: u32) {
        self.spr_scan.clear();
        let total = match self.form {
            VdpForm::Tms9918 => TMS_SPRITES,
            _ => MODE4_SPRITES,
        };
        let zoom = usize::from(self.regs[1] & 0x01 != 0);
        // The magnify select doubles the span of every sprite.
        let height = (if self.regs[1] & 0x02 != 0 {
            16i32
        } else {
            8i32
        }) << zoom;
        let sat_base = (usize::from(self.regs[5] & 0x7F)) << 7;
        for i in 0..total {
            let base = (sat_base + i * 4) & VRAM_MASK;
            let y = self.vram[base];
            if y == SPRITE_STOP {
                break;
            }
            let row = line as i32 - (i32::from(y) + 1);
            if row < 0 || row >= height {
                continue;
            }
            self.spr_scan.push(SprEntry {
                index: i as u8,
                y,
                x: self.vram[(base + 1) & VRAM_MASK],
                pat: self.vram[(base + 2) & VRAM_MASK],
                attr3: self.vram[(base + 3) & VRAM_MASK],
            });
        }
    }

    /// Publish the cached frame: the GameGear form crops the 160x144
    /// window of rows 24-167 and columns 48-207 from the raster, and
    /// the other forms publish the active rows whole.
    fn refresh_cached_frame(&mut self) {
        if self.form.gg() {
            let mut px = Vec::with_capacity(GG_W * GG_H * 3);
            for row in GG_CROP_Y..GG_CROP_Y + GG_H {
                for col in (WIDTH - GG_W) / 2..(WIDTH + GG_W) / 2 {
                    let o = row * WIDTH * 3 + col * 3;
                    px.extend_from_slice(&self.screen[o..o + 3]);
                }
            }
            self.frame.width = GG_W as u32;
            self.frame.height = GG_H as u32;
            self.frame.pixels = px;
        } else {
            let h = self.active_lines() as usize;
            self.frame.width = WIDTH as u32;
            self.frame.height = h as u32;
            self.frame.pixels = self.screen[..WIDTH * h * 3].to_vec();
        }
    }
}

impl BusAccess for Vdp {
    fn read(&mut self, _width: AccessWidth, address: Address, _tick: Tick) -> u32 {
        u32::from(match address.0 & 1 {
            0 => self.data_read(),
            _ => self.status_read(),
        })
    }

    fn write(&mut self, _width: AccessWidth, address: Address, value: u32, _tick: Tick) {
        let v = value as u8;
        match address.0 & 1 {
            0 => self.data_write(v),
            _ => self.control_write(v),
        }
    }

    fn peek(&self, address: Address) -> u8 {
        match address.0 & 1 {
            0 => {
                if self.form.mode4() {
                    self.vcounter()
                } else {
                    self.vram[usize::from(self.addr) & VRAM_MASK]
                }
            }
            _ => self.status_bits(),
        }
    }

    fn decodes(&self, _address: Address) -> bool {
        true
    }
}

/// The snapshot byte length of one form.
fn snapshot_len(form: VdpForm) -> usize {
    4 + 1 + VRAM_SIZE + form.cram_len() + SNAP_TAIL
}

impl Device for Vdp {
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
        self.regs = [0; REG_COUNT];
        self.vram.fill(0);
        self.cram.fill(0);
        self.addr_low = 0;
        self.latch_phase = 0;
        self.addr = 0;
        self.frame_pending = false;
        self.five_flag = false;
        self.collision = false;
        self.five_index = 0;
        self.line_pending = false;
        self.line = 0;
        self.line_counter = 0;
        self.line_irq_driven = false;
        self.spr_scan.clear();
        self.screen.fill(0);
        self.frames = 0;
        self.refresh_cached_frame();
        self.next = None;
        self.last_tick = None;
    }

    fn boot(&mut self, state: Option<&BootState>) {
        let _ = state;
        self.reset();
        self.next = Some(Tick(0));
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(snapshot_len(self.form));
        out.extend_from_slice(&self.form.tag());
        out.push(1);
        out.extend_from_slice(self.vram.as_ref());
        match self.form {
            VdpForm::Tms9918 => {}
            VdpForm::Sms => out.extend_from_slice(&self.cram[..32]),
            VdpForm::GameGear => out.extend_from_slice(self.cram.as_ref()),
        }
        out.extend_from_slice(&self.regs);
        out.push(self.addr_low);
        out.push(self.latch_phase);
        out.extend_from_slice(&self.addr.to_le_bytes());
        out.push(u8::from(self.frame_pending));
        out.push(u8::from(self.five_flag));
        out.push(u8::from(self.collision));
        out.push(self.five_index);
        out.push(u8::from(self.line_pending));
        out.extend_from_slice(&self.line.to_le_bytes());
        out.push(self.line_counter);
        out.extend_from_slice(&self.frames.to_le_bytes());
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        let want = snapshot_len(self.form);
        let tag = self.form.tag();
        if data.len() != want || data[0..4] != tag || data[4] != 1 {
            return Err(SnapshotError::Mismatch(format!(
                "the vdp chunk must be {want} bytes, tag {}, and version 1",
                String::from_utf8_lossy(&tag)
            )));
        }
        let mut pos = 5usize;
        self.vram.copy_from_slice(&data[pos..pos + VRAM_SIZE]);
        pos += VRAM_SIZE;
        match self.form {
            VdpForm::Tms9918 => {}
            VdpForm::Sms => self.cram[..32].copy_from_slice(&data[pos..pos + 32]),
            VdpForm::GameGear => self.cram.copy_from_slice(&data[pos..pos + 64]),
        }
        pos += self.form.cram_len();
        for (i, reg) in self.regs.iter_mut().enumerate() {
            *reg = data[pos + i];
        }
        pos += REG_COUNT;
        self.addr_low = data[pos];
        pos += 1;
        self.latch_phase = data[pos];
        pos += 1;
        self.addr = u16::from(data[pos]) | u16::from(data[pos + 1]) << 8;
        pos += 2;
        self.frame_pending = data[pos] != 0;
        pos += 1;
        self.five_flag = data[pos] != 0;
        pos += 1;
        self.collision = data[pos] != 0;
        pos += 1;
        self.five_index = data[pos];
        pos += 1;
        self.line_pending = data[pos] != 0;
        pos += 1;
        self.line = u32::from(data[pos])
            | u32::from(data[pos + 1]) << 8
            | u32::from(data[pos + 2]) << 16
            | u32::from(data[pos + 3]) << 24;
        pos += 4;
        self.line_counter = data[pos];
        pos += 1;
        self.frames = u64::from(data[pos])
            | u64::from(data[pos + 1]) << 8
            | u64::from(data[pos + 2]) << 16
            | u64::from(data[pos + 3]) << 24
            | u64::from(data[pos + 4]) << 32
            | u64::from(data[pos + 5]) << 40
            | u64::from(data[pos + 6]) << 48
            | u64::from(data[pos + 7]) << 56;
        // The sprite flags hold across the re-composition pass; the
        // sprite scan of the pass seeds them, and the restored values
        // land after it.
        let held_frame = self.frame_pending;
        let held_five = self.five_flag;
        let held_collide = self.collision;
        let held_index = self.five_index;
        let held_line = self.line_pending;
        let lines = self.active_lines();
        self.screen.fill(0);
        for line in 0..lines {
            self.compose_line(line);
        }
        self.frame_pending = held_frame;
        self.five_flag = held_five;
        self.collision = held_collide;
        self.five_index = held_index;
        self.line_pending = held_line;
        self.refresh_cached_frame();
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
        let boundary = self.active_lines();
        if self.line == boundary {
            // The first line past the display: the frame flag, the
            // vblank event, the publish, and the counter reload.
            if !self.frame_pending {
                self.frame_pending = true;
            }
            ctx.emit(TriggerEvent::Vblank);
            self.frames += 1;
            self.refresh_cached_frame();
            self.line_counter = self.regs[10];
        } else if self.line < boundary {
            self.compose_line(self.line);
            if self.form.mode4() {
                if self.line == 0 {
                    // The frame start loads the counter without a step.
                    self.line_counter = self.regs[10];
                } else {
                    self.step_line_counter();
                }
            }
        } else if self.form.mode4() {
            // The blank lines keep the line counter running.
            self.step_line_counter();
        }
        self.line += 1;
        if self.line >= FRAME_LINES {
            self.line = 0;
        }
        // The interrupt line recomputes from the flags and enables.
        let ie0 = self.regs[1] & 0x20 != 0;
        let ie1 = self.regs[0] & 0x10 != 0;
        let wanted = (self.frame_pending && ie0) || (self.line_pending && ie1);
        if wanted != self.line_irq_driven {
            self.line_irq_driven = wanted;
            ctx.notify(
                IRQ_LINE,
                if wanted { Level::High } else { Level::Low },
                tick,
            );
        }
        self.next = Some(Tick(tick.0 + self.line_step()));
    }
}

impl VideoSource for Vdp {
    fn frame(&self) -> Option<&Frame> {
        Some(&self.frame)
    }

    fn dump_layer(&self, layer: VideoLayer) -> Option<Vec<u8>> {
        match layer {
            VideoLayer::Vram => Some(self.vram.to_vec()),
            VideoLayer::NameTable => {
                // The mode-4 and Tms tile forms dump 768 entries, the
                // text form 960.
                let len = if self.form.mode4() || self.tms_mode() != TmsMode::Text40 {
                    768
                } else {
                    960
                };
                let base = (usize::from(self.regs[2] & 0x0F)) << 10;
                Some(self.vram[base..base + len].to_vec())
            }
            VideoLayer::Oam => {
                // The attribute table of 256 entries wraps the 16 KiB
                // VRAM at the R5 base.
                let base = (usize::from(self.regs[5] & 0x7F)) << 7;
                let mut out = Vec::with_capacity(256);
                for i in 0..256 {
                    out.push(self.vram[(base + i) & VRAM_MASK]);
                }
                Some(out)
            }
            VideoLayer::Palette => match self.form {
                // The Tms fixed palette dumps as RGB bytes.
                VdpForm::Tms9918 => Some(TMS_PALETTE.iter().flat_map(|c| c.to_vec()).collect()),
                VdpForm::Sms => Some(self.cram[..32].to_vec()),
                VdpForm::GameGear => Some(self.cram.to_vec()),
            },
            VideoLayer::Framebuffer => {
                let h = self.active_lines() as usize;
                Some(self.screen[..WIDTH * h * 3].to_vec())
            }
        }
    }

    fn frames(&self) -> u64 {
        self.frames
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use demu_config::model::{
        Bus as CBus, ClockDomain, Connection, ConnectionDirection, ConnectionSemantics,
        Window as CWindow,
    };
    use demu_config::newtypes::{DeviceName, Frequency, KindName, LinePath};
    use demu_core::machine::Machine;
    use std::collections::BTreeMap;

    /// The line cadence in domain ticks at the default dot divider.
    const LINE: u64 = 684;
    /// The frame cadence in domain ticks.
    const FRAMES: u64 = LINE * (FRAME_LINES as u64);
    /// The frame boundary tick of the 192-line form.
    const BOUND: u64 = LINE * (BASE_LINES as u64);

    /// A device under test with one empty configuration.
    fn unit(kind: &str) -> Vdp {
        Vdp::new(&demo_instance(kind, "vdp"), &demo_config())
    }

    /// The port-protocol control write on the `$BF` odd address.
    fn dctl(s: &mut Vdp, v: u8) {
        s.write(AccessWidth::Byte, Address(0xBF), u32::from(v), Tick(0));
    }

    /// The port-protocol data write on the `$BE` even address.
    fn ddat(s: &mut Vdp, v: u8) {
        s.write(AccessWidth::Byte, Address(0xBE), u32::from(v), Tick(0));
    }

    /// The port-protocol data read from the `$BE` even address.
    fn dd_rd(s: &mut Vdp) -> u8 {
        s.read(AccessWidth::Byte, Address(0xBE), Tick(0)) as u8
    }

    /// Set the VRAM pointer to a write address.
    fn dwa(s: &mut Vdp, addr: u16) {
        dctl(s, (addr & 0xFF) as u8);
        dctl(s, 0x80 | ((addr >> 8) as u8 & 0x3F));
    }

    /// Set the VRAM pointer to a read address.
    fn dra(s: &mut Vdp, addr: u16) {
        dctl(s, (addr & 0xFF) as u8);
        dctl(s, 0x40 | ((addr >> 8) as u8 & 0x3F));
    }

    /// Write one VRAM byte through the protocol.
    fn dpoke(s: &mut Vdp, addr: u16, v: u8) {
        dwa(s, addr);
        ddat(s, v);
    }

    /// Write one register through the protocol.
    fn dwr(s: &mut Vdp, index: u8, v: u8) {
        dctl(s, v);
        dctl(s, index & 0x1F);
    }

    /// A device instance with no parameters.
    fn demo_instance(kind: &str, name: &str) -> DeviceInstance {
        DeviceInstance {
            name: DeviceName::new(name).expect("name"),
            kind: KindName::new(kind).expect("kind"),
            domain: "main".to_string(),
            params: BTreeMap::new(),
            dividers: BTreeMap::new(),
        }
    }

    /// The smallest configuration shape for direct device tests.
    fn demo_config() -> SystemConfig {
        let mut clocks = BTreeMap::new();
        clocks.insert(
            "main".to_string(),
            ClockDomain {
                name: "main".to_string(),
                frequency: Frequency::new(10_738_635, 1).expect("freq"),
                origin: "test".to_string(),
            },
        );
        SystemConfig {
            name: "demo".to_string(),
            triple: "test/demo".to_string(),
            description: String::new(),
            clocks,
            devices: Vec::new(),
            devices_by_name: BTreeMap::new(),
            buses: Vec::new(),
            windows: Vec::new(),
            connections: Vec::new(),
            signals: Vec::new(),
            boot: BTreeMap::new(),
            quirks: BTreeMap::new(),
        }
    }

    /// A test-only device that records inbound line edges.
    struct LineRecorder {
        name: String,
        lines: Vec<(String, Level, u64)>,
    }

    impl LineRecorder {
        fn new(device: &DeviceInstance) -> Self {
            Self {
                name: device.name.to_string(),
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

        fn boot(&mut self, _state: Option<&BootState>) {}

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

    /// A machine with the display device, its `$00-$FF` port window on
    /// the `bus` bus, and the optional recorder on the `irq` route.
    fn machine_for(vdp: DeviceInstance, record: bool) -> Machine {
        let mut devices = vec![vdp.clone()];
        let mut by_name = BTreeMap::new();
        by_name.insert(vdp.name.clone(), 0usize);
        let mut connections = Vec::new();
        if record {
            let rec = demo_instance("test.lines", "lines");
            by_name.insert(rec.name.clone(), devices.len());
            devices.push(rec);
            connections.push(Connection {
                from: LinePath::parse("vdp.irq").expect("route"),
                to: LinePath::parse("lines.irq").expect("route"),
                semantics: ConnectionSemantics::Irq,
                direction: ConnectionDirection::Forward,
            });
        }
        let mut clocks = BTreeMap::new();
        clocks.insert(
            "main".to_string(),
            ClockDomain {
                name: "main".to_string(),
                frequency: Frequency::new(10_738_635, 1).expect("freq"),
                origin: "the NTSC master, triple the color burst".to_string(),
            },
        );
        let config = SystemConfig {
            name: "vdp-test".to_string(),
            triple: "test/vdp".to_string(),
            description: "the vdp test machine".to_string(),
            clocks,
            devices,
            devices_by_name: by_name,
            buses: vec![CBus {
                name: "bus".to_string(),
                address_bits: 16,
                owner: None,
                parent: None,
            }],
            windows: vec![CWindow {
                name: "vdp.ports".to_string(),
                bus: "bus".to_string(),
                device: vdp.name.clone(),
                base: 0x0000,
                size: 0x0100,
                kind: None,
                mirror: None,
                overlay: None,
                overlay_read_priority: None,
                overlay_write_priority: None,
                conflict: None,
            }],
            connections,
            signals: Vec::new(),
            boot: BTreeMap::new(),
            quirks: BTreeMap::new(),
        };
        let mut ctors = demu_core::smoke_ctors();
        ctors.register("tms9918.vdp", Box::new(|d, c| Ok(Box::new(Vdp::new(d, c)))));
        ctors.register("sms.vdp", Box::new(|d, c| Ok(Box::new(Vdp::new(d, c)))));
        ctors.register("gg.vdp", Box::new(|d, c| Ok(Box::new(Vdp::new(d, c)))));
        ctors.register(
            "test.lines",
            Box::new(|d, _| Ok(Box::new(LineRecorder::new(d)))),
        );
        let mut machine = Machine::assemble(config, &ctors).expect("assemble");
        machine.boot();
        machine
    }

    fn vdp_machine(kind: &str, record: bool) -> Machine {
        machine_for(demo_instance(kind, "vdp"), record)
    }

    fn sms_machine(record: bool) -> Machine {
        vdp_machine("sms.vdp", record)
    }

    fn vdp_of(machine: &mut Machine) -> &mut Vdp {
        let any = machine
            .device_by_name_mut("vdp")
            .expect("device")
            .as_any_mut();
        any.downcast_mut::<Vdp>().expect("the vdp")
    }

    fn rec_of(machine: &mut Machine) -> &mut LineRecorder {
        let any = machine
            .device_by_name_mut("lines")
            .expect("device")
            .as_any_mut();
        any.downcast_mut::<LineRecorder>().expect("the recorder")
    }

    /// A control-port write through the machine bus.
    fn ctl(machine: &mut Machine, v: u8) {
        assert!(machine.debug_write("bus", Address(0xBF), v));
    }

    /// A data-port write through the machine bus.
    fn dat(machine: &mut Machine, v: u8) {
        assert!(machine.debug_write("bus", Address(0xBE), v));
    }

    /// Set the machine VRAM pointer to a write address.
    fn wa(machine: &mut Machine, addr: u16) {
        ctl(machine, (addr & 0xFF) as u8);
        ctl(machine, 0x80 | ((addr >> 8) as u8 & 0x3F));
    }

    /// Write one machine VRAM byte through the protocol.
    fn poke(machine: &mut Machine, addr: u16, v: u8) {
        wa(machine, addr);
        dat(machine, v);
    }

    /// Write a machine VRAM span through the protocol.
    fn poke_span(machine: &mut Machine, addr: u16, bytes: &[u8]) {
        wa(machine, addr);
        for b in bytes {
            dat(machine, *b);
        }
    }

    /// Write one machine register through the protocol.
    fn reg(machine: &mut Machine, index: u8, v: u8) {
        ctl(machine, v);
        ctl(machine, index & 0x1F);
    }

    /// The status read through the machine device.
    fn status(machine: &mut Machine) -> u8 {
        let tick = machine.now().0;
        vdp_of(machine)
            .read(AccessWidth::Byte, Address(0xBF), Tick(tick))
            .try_into()
            .expect("byte")
    }

    /// The published frame pixels.
    fn frame_bytes(machine: &mut Machine) -> Vec<u8> {
        let video = machine
            .device_by_name("vdp")
            .expect("device")
            .video()
            .expect("video");
        video.frame().expect("frame").pixels.clone()
    }

    /// The pixel of a 256-wide frame.
    fn pixel_of(bytes: &[u8], row: usize, col: usize) -> [u8; 3] {
        let base = (row * WIDTH + col) * 3;
        [bytes[base], bytes[base + 1], bytes[base + 2]]
    }

    // ---
    // Port protocol tests
    // ---

    #[test]
    fn control_writes_reach_the_register_matrix() {
        let mut s = unit("tms9918.vdp");
        dctl(&mut s, 0x07);
        dctl(&mut s, 0x07); // R7 = 0x07
        dctl(&mut s, 0x24);
        dctl(&mut s, 0x07); // R7 = 0x24
        assert_eq!(s.regs[7], 0x24);
        dctl(&mut s, 0x55);
        dctl(&mut s, 0x0A); // R10 = 0x55
        assert_eq!(s.regs[10], 0x55);
        dctl(&mut s, 0x11);
        dctl(&mut s, 0x0F); // the index 15 stores no register
        assert_eq!(s.regs[10], 0x55);
    }

    #[test]
    fn sms_registers_store_the_r8_to_r10_band() {
        let mut s = unit("sms.vdp");
        dwr(&mut s, 8, 0x11);
        dwr(&mut s, 9, 0x22);
        dwr(&mut s, 10, 0x33);
        assert_eq!(
            (s.regs[8], s.regs[9], s.regs[10], s.line_counter),
            (0x11, 0x22, 0x33, 0x33)
        );
    }

    #[test]
    fn tms_r10_stores_without_a_counter_reload() {
        let mut s = unit("tms9918.vdp");
        dwr(&mut s, 10, 0x33);
        assert_eq!(s.regs[10], 0x33);
        assert_eq!(s.line_counter, 0, "the Tms store keeps the counter idle");
    }

    #[test]
    fn data_writes_step_the_pointer_on_every_form() {
        for kind in ["tms9918.vdp", "sms.vdp", "gg.vdp"] {
            let mut s = unit(kind);
            dwa(&mut s, 0x1234);
            ddat(&mut s, 0xAA);
            ddat(&mut s, 0xBB);
            assert_eq!((s.vram[0x1234], s.vram[0x1235]), (0xAA, 0xBB));
            assert_eq!(s.addr, 0x1236);
        }
    }

    #[test]
    fn tms_data_read_holds_the_pointer() {
        let mut s = unit("tms9918.vdp");
        s.vram[0x0200] = 0x5A;
        dra(&mut s, 0x0200);
        let a = dd_rd(&mut s);
        let b = dd_rd(&mut s);
        assert_eq!((a, b, s.addr), (0x5A, 0x5A, 0x0200));
    }

    #[test]
    fn mode4_data_read_reports_the_v_counter() {
        let mut machine = sms_machine(false);
        machine.run_until(Tick(5 * LINE + LINE / 2));
        assert_eq!(vdp_of(&mut machine).vcounter(), 6);
        // The published fold: past line 218 the counter reports 213-255.
        machine.run_until(Tick(219 * LINE + LINE / 2));
        assert_eq!(vdp_of(&mut machine).vcounter(), 214);
        machine.run_until(Tick(260 * LINE + LINE / 2));
        assert_eq!(vdp_of(&mut machine).vcounter(), 255);
    }

    #[test]
    fn tms_form_keeps_the_vram_data_read() {
        let mut s = unit("tms9918.vdp");
        s.vram[0x0200] = 0x5A;
        dra(&mut s, 0x0200);
        assert_eq!(dd_rd(&mut s), 0x5A);
    }

    #[test]
    fn sms_cram_hides_under_the_low_vram_addresses() {
        let mut s = unit("sms.vdp");
        dwa(&mut s, 0x0005);
        ddat(&mut s, 0x0C);
        dwa(&mut s, 0x0025);
        ddat(&mut s, 0x0F);
        // `$0025` mirrors onto the entry 5, the VRAM stays zero.
        assert_eq!((s.cram[5], s.vram[0x0005]), (0x0F, 0));
        // The address at `$0040` reaches the VRAM.
        dwa(&mut s, 0x0040);
        ddat(&mut s, 0x77);
        assert_eq!(s.vram[0x0040], 0x77);
    }

    #[test]
    fn gg_cram_holds_two_bytes_per_entry() {
        let mut s = unit("gg.vdp");
        dwa(&mut s, 0x0002);
        ddat(&mut s, 0x3C);
        ddat(&mut s, 0x0F);
        // Entry 1: the first byte blue 12 and green 3, the second red 15.
        assert_eq!(s.cram_rgb(1), [255, 51, 204]);
        assert_eq!(
            s.dump_layer(VideoLayer::Palette).expect("palette").len(),
            64
        );
    }

    #[test]
    fn the_kind_picks_the_form() {
        assert_eq!(VdpForm::from_kind("tms9918.vdp"), VdpForm::Tms9918);
        assert_eq!(VdpForm::from_kind("sms.vdp"), VdpForm::Sms);
        assert_eq!(VdpForm::from_kind("gg.vdp"), VdpForm::GameGear);
        // The mode-4 class holds the form default.
        assert_eq!(VdpForm::from_kind("unknown"), VdpForm::Sms);
    }

    // ---
    // Interrupt tests
    // ---

    #[test]
    fn frame_flag_sets_at_the_boundary_and_clears_on_the_read() {
        let mut machine = sms_machine(false);
        machine.run_until(Tick(BOUND + 2 * LINE));
        assert_eq!(vdp_of(&mut machine).frames, 1);
        assert_ne!(status(&mut machine) & 0x80, 0, "the frame flag held");
        assert_eq!(status(&mut machine) & 0x80, 0, "the read cleared the flag");
        machine.run_until(Tick(BOUND + FRAMES + 2 * LINE));
        assert_eq!(vdp_of(&mut machine).frames, 2);
        assert_ne!(status(&mut machine) & 0x80, 0);
    }

    #[test]
    fn the_vblank_trigger_fires_at_every_boundary() {
        let mut machine = sms_machine(false);
        let mut hits = 0u32;
        machine.run_hook(
            Tick(BOUND + FRAMES + 200),
            &mut |e: &TriggerEvent| {
                if *e == TriggerEvent::Vblank {
                    hits += 1;
                }
                false
            },
            true,
        );
        assert_eq!(hits, 2, "one vblank event per frame boundary");
    }

    #[test]
    fn irq_follows_ie0_at_the_boundary() {
        let mut machine = sms_machine(true);
        reg(&mut machine, 1, 0x20); // IE0 on, display off
        machine.run_until(Tick(BOUND + 200));
        let rec = rec_of(&mut machine).lines.clone();
        assert_eq!(rec, vec![("irq".to_string(), Level::High, BOUND)]);
        // The status read clears the flag; the next visit drops the line.
        assert_ne!(status(&mut machine) & 0x80, 0);
        machine.run_until(Tick(BOUND + LINE + 200));
        let rec = rec_of(&mut machine).lines.clone();
        assert_eq!(
            rec.last(),
            Some(&("irq".to_string(), Level::Low, BOUND + LINE))
        );
    }

    #[test]
    fn irq_needs_the_enable_bit() {
        let mut machine = sms_machine(true);
        machine.run_until(Tick(BOUND + FRAMES));
        assert!(rec_of(&mut machine).lines.is_empty(), "IE0 off, no edge");
    }

    #[test]
    fn r10_value_zero_requests_every_line() {
        let mut machine = sms_machine(true);
        reg(&mut machine, 0, 0x10); // IE1 on
        reg(&mut machine, 10, 0);
        machine.run_until(Tick(2 * LINE + 200));
        let rec = rec_of(&mut machine).lines.clone();
        assert_eq!(rec, vec![("irq".to_string(), Level::High, LINE)]);
    }

    #[test]
    fn r10_value_one_requests_every_second_line() {
        let mut machine = sms_machine(true);
        reg(&mut machine, 0, 0x10);
        reg(&mut machine, 10, 1);
        machine.run_until(Tick(2 * LINE + 200));
        let rec = rec_of(&mut machine).lines.clone();
        assert_eq!(rec, vec![("irq".to_string(), Level::High, 2 * LINE)]);
    }

    #[test]
    fn r10_disable_band_holds_the_request() {
        let mut machine = sms_machine(true);
        reg(&mut machine, 0, 0x10);
        reg(&mut machine, 10, 0xC0);
        machine.run_until(Tick(6 * LINE + 200));
        assert!(rec_of(&mut machine).lines.is_empty());
    }

    #[test]
    fn the_status_read_clears_the_line_request() {
        let mut machine = sms_machine(true);
        reg(&mut machine, 0, 0x10);
        reg(&mut machine, 10, 0x1F); // thirty-two lines per request
        machine.run_until(Tick(32 * LINE + 200));
        let rec = rec_of(&mut machine).lines.clone();
        assert_eq!(rec, vec![("irq".to_string(), Level::High, 32 * LINE)]);
        // The status read clears the pending request; the line drops on
        // the next visit and the next request fires at line 64.
        status(&mut machine);
        machine.run_until(Tick(34 * LINE + 200));
        let rec = rec_of(&mut machine).lines.clone();
        assert_eq!(
            rec.last(),
            Some(&("irq".to_string(), Level::Low, 33 * LINE))
        );
    }

    // ---
    // Tms display mode tests
    // ---

    #[test]
    fn tms_graphics1_renders_pattern_and_color_bytes() {
        let mut machine = vdp_machine("tms9918.vdp", false);
        reg(&mut machine, 1, 0x40); // display on
        reg(&mut machine, 2, 0x06); // name table at $1800
        reg(&mut machine, 3, 0xFF); // color table at $3FC0
        poke(&mut machine, 0x1800, 0x00);
        poke(&mut machine, 0x0000, 0xAA); // char 0, row 0
        poke(&mut machine, 0x3FC0, 0xF2); // chars 0-7: fg 15, bg 2
        machine.run_until(Tick(FRAMES + 200));
        let bytes = frame_bytes(&mut machine);
        assert_eq!(pixel_of(&bytes, 0, 0), TMS_PALETTE[15]);
        assert_eq!(pixel_of(&bytes, 0, 1), TMS_PALETTE[2]);
        assert_eq!(pixel_of(&bytes, 1, 0), TMS_PALETTE[2]);
    }

    #[test]
    fn tms_text_renders_40_columns_fixed_colors() {
        let mut machine = vdp_machine("tms9918.vdp", false);
        reg(&mut machine, 1, 0x50); // display on, text mode
        reg(&mut machine, 2, 0x06);
        reg(&mut machine, 7, 0x24); // fg 4, bg 2
        poke(&mut machine, 0x1800, 0x01); // char 1
        poke(&mut machine, 0x0008, 0x3C); // pattern row 0: pixels 2-5
        machine.run_until(Tick(FRAMES + 200));
        let bytes = frame_bytes(&mut machine);
        assert_eq!(pixel_of(&bytes, 0, 0), TMS_PALETTE[2]);
        assert_eq!(pixel_of(&bytes, 0, 3), TMS_PALETTE[4]);
        assert_eq!(pixel_of(&bytes, 0, 245), TMS_PALETTE[2]);
        // The last row shows the text backdrop.
        assert_eq!(pixel_of(&bytes, 190, 0), TMS_PALETTE[2]);
    }

    #[test]
    fn tms_graphics2_walks_the_third_area() {
        let mut machine = vdp_machine("tms9918.vdp", false);
        reg(&mut machine, 1, 0x40);
        reg(&mut machine, 0, 0x02); // M3
        reg(&mut machine, 2, 0x06);
        poke(&mut machine, 0x1800, 0x05); // area 0, char 5
        poke(&mut machine, 0x0028, 0x9A); // area 0, char 5, row 0
        poke(&mut machine, 0x1900, 0x07); // area 1, line 64, char 7
        poke(&mut machine, 0x0838, 0x9A); // area 1, char 7, row 0
        machine.run_until(Tick(FRAMES + 200));
        let bytes = frame_bytes(&mut machine);
        // Area 0: fg 9 and bg 10 through the byte 0x9A.
        assert_eq!(pixel_of(&bytes, 0, 0), TMS_PALETTE[9]);
        assert_eq!(pixel_of(&bytes, 0, 1), TMS_PALETTE[10]);
        // Area 1 walks the second table third.
        assert_eq!(pixel_of(&bytes, 64, 0), TMS_PALETTE[9]);
    }

    #[test]
    fn tms_multicolor_renders_four_blocks() {
        let mut machine = vdp_machine("tms9918.vdp", false);
        reg(&mut machine, 1, 0x48); // display on, multicolor
        reg(&mut machine, 2, 0x06);
        poke(&mut machine, 0x1800, 0b0001_1011); // four block colors
        poke(&mut machine, 0x00D8, 0b0100_0000); // char $1B block pattern
        machine.run_until(Tick(FRAMES + 200));
        let bytes = frame_bytes(&mut machine);
        // The left 4x4 block: the name 3 over the pattern 1 = index 13.
        assert_eq!(pixel_of(&bytes, 0, 0), TMS_PALETTE[13]);
        // The right 4x4 block: the name 2 over the pattern 0 = index 8.
        assert_eq!(pixel_of(&bytes, 0, 4), TMS_PALETTE[8]);
    }

    #[test]
    fn tms_fifth_sprite_sets_the_flag_and_index() {
        let mut machine = vdp_machine("tms9918.vdp", false);
        reg(&mut machine, 1, 0x40);
        reg(&mut machine, 5, 0x3E); // sprite list at $1F00
        for i in 0..5u8 {
            poke_span(
                &mut machine,
                0x1F00 + u16::from(i) * 4,
                &[0x00, 8 * i, 0x01, 0x06],
            );
        }
        poke(&mut machine, 0x0008, 0xFF); // tile 1, row 0
        machine.run_until(Tick(4 * LINE + 200));
        // The fifth flag with index 4, no collision at the side-by-side
        // spots, and no frame flag inside the display.
        assert_eq!(status(&mut machine), 0x44);
    }

    #[test]
    fn tms_collision_fires_on_the_sprite_overlap() {
        let mut machine = vdp_machine("tms9918.vdp", false);
        reg(&mut machine, 1, 0x40);
        reg(&mut machine, 5, 0x3E);
        // Two sprites on the same spot.
        poke_span(
            &mut machine,
            0x1F00,
            &[0x00, 0x00, 0x01, 0x06, 0x00, 0x00, 0x01, 0x06],
        );
        poke(&mut machine, 0x0008, 0xFF);
        machine.run_until(Tick(4 * LINE + 200));
        assert_ne!(status(&mut machine) & 0x20, 0);
    }

    #[test]
    fn tms_16x16_sprites_pair_the_even_tiles() {
        let mut machine = vdp_machine("tms9918.vdp", false);
        reg(&mut machine, 1, 0x42); // display on, 16x16 sprites
        reg(&mut machine, 5, 0x3E);
        poke_span(&mut machine, 0x1F00, &[0x00, 0x00, 0x10, 0x06]);
        poke(&mut machine, 0x0080, 0xFF); // tile $10, row 0
        poke(&mut machine, 0x0088, 0x0F); // tile $11, row 0
        machine.run_until(Tick(FRAMES + 200));
        let bytes = frame_bytes(&mut machine);
        // The left tile lights columns 0-7, the right tile 12-15.
        assert_eq!(pixel_of(&bytes, 1, 0), TMS_PALETTE[6]);
        assert_eq!(pixel_of(&bytes, 1, 11), TMS_PALETTE[0]);
        assert_eq!(pixel_of(&bytes, 1, 12), TMS_PALETTE[6]);
    }

    #[test]
    fn tms_early_clock_shifts_the_sprite() {
        let mut machine = vdp_machine("tms9918.vdp", false);
        reg(&mut machine, 1, 0x40);
        reg(&mut machine, 5, 0x3E);
        // The attribute control byte carries the early clock bit.
        poke_span(&mut machine, 0x1F00, &[0x00, 0x00, 0x01, 0x86]);
        poke(&mut machine, 0x0008, 0xFF);
        machine.run_until(Tick(FRAMES + 200));
        let bytes = frame_bytes(&mut machine);
        assert_eq!(pixel_of(&bytes, 1, 31), TMS_PALETTE[0]);
        assert_eq!(pixel_of(&bytes, 1, 32), TMS_PALETTE[6]);
    }

    // ---
    // Mode 4 raster tests
    // ---

    #[test]
    fn sms_background_renders_the_cram_colors() {
        let mut machine = sms_machine(false);
        reg(&mut machine, 1, 0x40); // display on
        reg(&mut machine, 2, 0x06); // name at $1800
        poke(&mut machine, 0x1800, 0x05); // tile 5, clear of the CRAM window
        poke(&mut machine, 0x00A2, 0xF0); // tile 5, row 0, plane 2
        poke(&mut machine, 0x0004, 0x30); // CRAM entry 4 green
        poke(&mut machine, 0x0000, 0x0C); // CRAM entry 0 blue
        machine.run_until(Tick(FRAMES + 200));
        let bytes = frame_bytes(&mut machine);
        assert_eq!(pixel_of(&bytes, 0, 0), [0, 255, 0]);
        assert_eq!(pixel_of(&bytes, 0, 7), [0, 0, 255]);
    }

    #[test]
    fn sms_hscroll_shifts_the_name_row() {
        let mut machine = sms_machine(false);
        reg(&mut machine, 1, 0x40);
        reg(&mut machine, 2, 0x06);
        reg(&mut machine, 8, 0x08); // h scroll of one tile
        poke(&mut machine, 0x1801, 0x05); // tile 5 at name column 1
        poke(&mut machine, 0x00A2, 0xFF);
        poke(&mut machine, 0x0004, 0x30);
        machine.run_until(Tick(FRAMES + 200));
        let bytes = frame_bytes(&mut machine);
        // Column 0 shows the tile of name column 1.
        assert_eq!(pixel_of(&bytes, 0, 0), [0, 255, 0]);
    }

    #[test]
    fn sms_vscroll_rolls_the_28_rows() {
        let mut machine = sms_machine(false);
        reg(&mut machine, 1, 0x40);
        reg(&mut machine, 2, 0x06);
        reg(&mut machine, 9, 0x08); // one name row down
        poke(&mut machine, 0x1820, 0x05);
        poke(&mut machine, 0x00A2, 0xFF);
        poke(&mut machine, 0x0004, 0x30);
        machine.run_until(Tick(FRAMES + 200));
        let bytes = frame_bytes(&mut machine);
        assert_eq!(pixel_of(&bytes, 0, 0), [0, 255, 0]);
    }

    #[test]
    fn sms_vlock_pins_the_top_16_rows() {
        let mut machine = sms_machine(false);
        reg(&mut machine, 1, 0x40);
        reg(&mut machine, 0, 0x80); // v scroll lock
        reg(&mut machine, 2, 0x06);
        reg(&mut machine, 8, 0x08); // one tile of h scroll
        poke(&mut machine, 0x1841, 0x05); // row 2, name column 1
        poke(&mut machine, 0x00A2, 0xFF);
        poke(&mut machine, 0x0004, 0x30);
        poke(&mut machine, 0x0000, 0x0C); // the backdrop blue
        machine.run_until(Tick(FRAMES + 200));
        let bytes = frame_bytes(&mut machine);
        // Row 0: locked, so the unscrolled name column 0 shows.
        assert_eq!(pixel_of(&bytes, 0, 0), [0, 0, 255]);
        // Row 16: unlocked, so the scroll walks the tile of column 1.
        assert_eq!(pixel_of(&bytes, 16, 0), [0, 255, 0]);
    }

    #[test]
    fn sms_hlock_pins_the_left_8_columns() {
        let mut machine = sms_machine(false);
        reg(&mut machine, 1, 0x40);
        reg(&mut machine, 0, 0x40); // h scroll lock
        reg(&mut machine, 2, 0x06);
        reg(&mut machine, 9, 0x10); // two name rows down
        poke(&mut machine, 0x1800 + 2 * 32, 0x05);
        poke(&mut machine, 0x00A2, 0xFF);
        poke(&mut machine, 0x0004, 0x30);
        poke(&mut machine, 0x0000, 0x0C);
        machine.run_until(Tick(FRAMES + 200));
        let bytes = frame_bytes(&mut machine);
        // Line 16: the left column holds the vertical scroll 0, so it
        // reads the name row 2.
        assert_eq!(pixel_of(&bytes, 16, 0), [0, 255, 0]);
        // Column 8 walks the vertical scroll, so the name row 4 of the
        // column shows the backdrop.
        assert_eq!(pixel_of(&bytes, 16, 8), [0, 0, 255]);
    }

    #[test]
    fn sms_sprite_renders_8x8_with_d0_end() {
        let mut machine = sms_machine(false);
        reg(&mut machine, 1, 0x40);
        reg(&mut machine, 5, 0x3E);
        poke_span(
            &mut machine,
            0x1F00,
            &[0x00, 0x00, 0x05, 0x00, 0xD0, 0x00, 0x06, 0x00],
        );
        poke(&mut machine, 0x00A2, 0xF0); // tile 5, plane 2, upper half
        poke(&mut machine, 0x0014, 0x0C); // CRAM entry $14 blue
        machine.run_until(Tick(FRAMES + 200));
        let bytes = frame_bytes(&mut machine);
        // Line 1 draws the sprite at column 0; plane 2 lights the upper
        // half of the row.
        assert_eq!(pixel_of(&bytes, 1, 0), [0, 0, 255]);
        assert_eq!(pixel_of(&bytes, 1, 3), [0, 0, 255]);
        assert_eq!(pixel_of(&bytes, 1, 4), [0, 0, 0]);
    }

    #[test]
    fn sms_sprite_stops_at_the_d0_entry() {
        let mut machine = sms_machine(false);
        reg(&mut machine, 1, 0x40);
        reg(&mut machine, 5, 0x3E);
        // Sprite 0 carries the stop row, sprite 1 would draw.
        poke_span(
            &mut machine,
            0x1F00,
            &[0xD0, 0x00, 0x05, 0x00, 0x00, 0x00, 0x05, 0x00],
        );
        poke(&mut machine, 0x00A2, 0xFF);
        poke(&mut machine, 0x0014, 0x0C);
        machine.run_until(Tick(FRAMES + 200));
        let bytes = frame_bytes(&mut machine);
        // The scan ended at sprite 0, so line 1 holds the backdrop.
        assert_eq!(pixel_of(&bytes, 1, 0), [0, 0, 0]);
    }

    #[test]
    fn sms_sprite_8x16_takes_the_second_pattern() {
        let mut machine = sms_machine(false);
        reg(&mut machine, 1, 0x42); // display on, 8x16 sprites
        reg(&mut machine, 5, 0x3E);
        poke_span(&mut machine, 0x1F00, &[0x00, 0x00, 0x05, 0x00]);
        poke(&mut machine, 0x00A2, 0xF0);
        // The lower half takes pattern 21, row 0.
        poke(&mut machine, 0x02A2, 0x0F);
        poke(&mut machine, 0x0014, 0x0C);
        machine.run_until(Tick(FRAMES + 200));
        let bytes = frame_bytes(&mut machine);
        // Line 1 from pattern 1, line 9 from pattern 17.
        assert_eq!(pixel_of(&bytes, 1, 0), [0, 0, 255]);
        assert_eq!(pixel_of(&bytes, 9, 7), [0, 0, 255]);
        assert_eq!(pixel_of(&bytes, 9, 0), [0, 0, 0]);
    }

    #[test]
    fn sms_sprite_magnify_doubles_both_axes() {
        let mut machine = sms_machine(false);
        reg(&mut machine, 1, 0x41); // display on, zoom
        reg(&mut machine, 5, 0x3E);
        poke_span(&mut machine, 0x1F00, &[0x00, 0x00, 0x05, 0x00]);
        poke(&mut machine, 0x00A2, 0xF0);
        poke(&mut machine, 0x0014, 0x0C);
        machine.run_until(Tick(FRAMES + 200));
        let bytes = frame_bytes(&mut machine);
        // The sprite spans 16 screen rows, columns 0-7 map the pattern
        // columns 0-3 twofold, and columns 8-15 map the pattern
        // columns 4-7.
        assert_eq!(pixel_of(&bytes, 1, 0), [0, 0, 255]);
        assert_eq!(pixel_of(&bytes, 1, 1), [0, 0, 255]);
        assert_eq!(pixel_of(&bytes, 2, 0), [0, 0, 255]);
        assert_eq!(pixel_of(&bytes, 1, 9), [0, 0, 0]);
        assert_eq!(pixel_of(&bytes, 16, 0), [0, 0, 0]);
    }

    #[test]
    fn sms_overflow_records_the_ninth_sprite() {
        let mut machine = sms_machine(false);
        reg(&mut machine, 1, 0x40);
        reg(&mut machine, 5, 0x3E);
        // Nine sprites on line 1, each on its own column band.
        for i in 0..9u8 {
            poke_span(
                &mut machine,
                0x1F00 + u16::from(i) * 4,
                &[0x00, 8 * i, 0x05, 0x00],
            );
        }
        poke(&mut machine, 0x0022, 0xFF);
        poke(&mut machine, 0x0014, 0x0C);
        machine.run_until(Tick(4 * LINE + 200));
        // The overflow flag lands with the index 8, and the flag bits
        // clear on the read while the recorded index holds.
        assert_eq!(status(&mut machine), 0x48);
        let again = status(&mut machine);
        assert_eq!(again & 0xF0, 0, "the flags cleared on the read");
        assert_eq!(again & 0x1F, 8, "the recorded index is not a flag");
    }

    #[test]
    fn sms_collision_fires_on_the_overlap() {
        let mut machine = sms_machine(false);
        reg(&mut machine, 1, 0x40);
        reg(&mut machine, 5, 0x3E);
        // Two sprites on the same spot.
        poke_span(
            &mut machine,
            0x1F00,
            &[0x00, 0x00, 0x05, 0x00, 0x00, 0x00, 0x05, 0x00],
        );
        poke(&mut machine, 0x00A2, 0xFF);
        poke(&mut machine, 0x0014, 0x0C);
        machine.run_until(Tick(4 * LINE + 200));
        assert_ne!(
            status(&mut machine) & 0x20,
            0,
            "two opaque pixels overlapped"
        );
    }

    #[test]
    fn sms_left_column_mask_blanks_and_drops_sprites() {
        let mut machine = sms_machine(false);
        reg(&mut machine, 1, 0x40);
        reg(&mut machine, 0, 0x20); // left column mask
        reg(&mut machine, 5, 0x3E);
        poke_span(&mut machine, 0x1F00, &[0x00, 0x04, 0x05, 0x00]);
        poke(&mut machine, 0x00A2, 0xFF);
        poke(&mut machine, 0x0014, 0x0C);
        machine.run_until(Tick(FRAMES + 200));
        let bytes = frame_bytes(&mut machine);
        // Columns 0-7 hold the backdrop; the sprite pixels at 8-11 show.
        assert_eq!(pixel_of(&bytes, 1, 0), [0, 0, 0]);
        assert_eq!(pixel_of(&bytes, 1, 8), [0, 0, 255]);
    }

    // ---
    // Frame and form tests
    // ---

    #[test]
    fn sms_display_off_fills_the_backdrop() {
        let mut machine = sms_machine(false);
        reg(&mut machine, 1, 0x00); // display off
        reg(&mut machine, 7, 0x01); // backdrop = CRAM entry 1
        poke(&mut machine, 0x0001, 0x30); // green
        machine.run_until(Tick(FRAMES + 200));
        let bytes = frame_bytes(&mut machine);
        assert_eq!(pixel_of(&bytes, 0, 0), [0, 255, 0]);
        assert_eq!(pixel_of(&bytes, 191, 255), [0, 255, 0]);
    }

    #[test]
    fn gg_frame_crops_the_center_window() {
        let mut machine = vdp_machine("gg.vdp", false);
        reg(&mut machine, 1, 0x40);
        reg(&mut machine, 2, 0x06);
        poke(&mut machine, 0x1800, 0x05);
        poke(&mut machine, 0x00A2, 0xFF);
        poke(&mut machine, 0x0004, 0x30);
        machine.run_until(Tick(FRAMES + 200));
        let bytes = frame_bytes(&mut machine);
        assert_eq!(bytes.len(), 160 * 144 * 3);
        // The window pixel (0, 0) reads the raster row 24, column 48,
        // and the window pixel (1, 1) reads the raster row 25, column
        // 49 through the pre-window framebuffer dump.
        let dump = machine
            .device_by_name("vdp")
            .expect("device")
            .video()
            .expect("video")
            .dump_layer(VideoLayer::Framebuffer)
            .expect("framebuffer");
        for (frow, fcol, rrow, rcol) in [(0usize, 0, 24, 48), (1, 1, 25, 49)] {
            let fbase = (frow * 160 + fcol) * 3;
            let dbase = (rrow * WIDTH + rcol) * 3;
            assert_eq!(
                (bytes[fbase], bytes[fbase + 1], bytes[fbase + 2]),
                (dump[dbase], dump[dbase + 1], dump[dbase + 2])
            );
        }
    }

    #[test]
    fn layer_dumps_carry_the_named_spans() {
        let mut machine = vdp_machine("tms9918.vdp", false);
        reg(&mut machine, 1, 0x50); // text mode
        reg(&mut machine, 2, 0x06);
        let video = machine
            .device_by_name("vdp")
            .expect("device")
            .video()
            .expect("video");
        assert_eq!(
            video.dump_layer(VideoLayer::Vram).expect("vram").len(),
            VRAM_SIZE
        );
        assert_eq!(
            video
                .dump_layer(VideoLayer::NameTable)
                .expect("names")
                .len(),
            960
        );
        assert_eq!(video.dump_layer(VideoLayer::Oam).expect("oam").len(), 256);
        assert_eq!(
            video
                .dump_layer(VideoLayer::Palette)
                .expect("palette")
                .len(),
            48
        );
        assert_eq!(
            video.dump_layer(VideoLayer::Framebuffer).expect("fb").len(),
            256 * 192 * 3
        );
    }

    #[test]
    fn extended_height_forms_the_224_line_mode() {
        let mut vdp = demo_instance("sms.vdp", "vdp");
        vdp.params
            .insert("extended_height".to_string(), toml::Value::Boolean(true));
        let mut machine = machine_for(vdp, false);
        reg(&mut machine, 1, 0x50); // display on, 224-line form
        machine.run_until(Tick(226 * LINE + 200));
        assert_eq!(vdp_of(&mut machine).frames, 1);
        let f = machine
            .device_by_name("vdp")
            .expect("device")
            .video()
            .expect("video")
            .frame()
            .expect("frame")
            .clone();
        assert_eq!((f.width, f.height), (256, 224));
    }

    #[test]
    fn extended_height_gate_keeps_192_lines() {
        let mut machine = sms_machine(false);
        reg(&mut machine, 1, 0x50); // R1 bit 4 without the parameter
        machine.run_until(Tick(BOUND + 2 * LINE + 200));
        assert_eq!(vdp_of(&mut machine).frames, 1);
        let f = machine
            .device_by_name("vdp")
            .expect("device")
            .video()
            .expect("video")
            .frame()
            .expect("frame")
            .clone();
        assert_eq!((f.width, f.height), (256, 192));
    }

    // ---
    // Snapshot tests
    // ---

    #[test]
    fn snapshot_round_trips_every_form() {
        for kind in ["tms9918.vdp", "sms.vdp", "gg.vdp"] {
            let mut s = unit(kind);
            dwa(&mut s, 0x1000);
            ddat(&mut s, 0x42);
            dwr(&mut s, 8, 0x11);
            if kind != "tms9918.vdp" {
                dwa(&mut s, 0x0003);
                ddat(&mut s, 0x0C);
            }
            let snap = s.snapshot();
            assert_eq!(snap.len(), snapshot_len(s.form));
            let mut twin = unit(kind);
            twin.restore(&snap).expect("restore");
            assert_eq!(twin.snapshot(), snap);
            assert_eq!(twin.regs[8], 0x11);
            assert_eq!(twin.vram[0x1000], 0x42);
            match s.form {
                VdpForm::GameGear => {
                    assert_eq!(twin.frame().expect("frame").pixels.len(), 160 * 144 * 3)
                }
                _ => assert_eq!(twin.frame().expect("frame").pixels.len(), 256 * 192 * 3),
            }
        }
    }

    #[test]
    fn snapshot_rejects_a_short_chunk() {
        let s = unit("sms.vdp");
        let snap = s.snapshot();
        let mut twin = unit("sms.vdp");
        let err = twin
            .restore(&snap[..snap.len() - 1])
            .expect_err("a short chunk rejects");
        assert!(matches!(err, SnapshotError::Mismatch(_)));
        // A crossed tag rejects too.
        let mut crossed = snap.clone();
        crossed[0] = b'G';
        let err = twin.restore(&crossed).expect_err("a foreign tag rejects");
        assert!(matches!(err, SnapshotError::Mismatch(_)));
    }

    #[test]
    fn restore_recomposes_the_frame_from_state() {
        let mut s = unit("sms.vdp");
        dwr(&mut s, 1, 0x40);
        dwr(&mut s, 2, 0x06);
        dpoke(&mut s, 0x1800, 0x05);
        dpoke(&mut s, 0x00A2, 0xFF);
        dpoke(&mut s, 0x0004, 0x30);
        let snap = s.snapshot();
        let mut twin = unit("sms.vdp");
        twin.restore(&snap).expect("restore");
        // The re-compose pass paints the tile without a tick visit.
        let px = twin.frame().expect("frame");
        assert_eq!([px.pixels[0], px.pixels[1], px.pixels[2]], [0, 255, 0]);
    }
}
