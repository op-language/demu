//! The Sega Genesis Video Display Processor (315-5313), the `genesis.vdp`
//! device kind.
//!
//! The VDP is the video die of the Mega Drive: it answers the data, control,
//! and HV-counter ports at `$C00000-$C0001F` (the PSG and debug register
//! offsets behind address bit 4 stay outside this device), holds 64 KiB of
//! video RAM, 128 bytes of color RAM (64 entries of 9-bit color), and 80
//! bytes of vertical-scroll RAM (40 entries of 10-bit scroll), and composes
//! two scrolling planes plus the window plane over the backdrop color with
//! sprites of four palettes, priority, and shadow/highlight shading.
//!
//! # Pinned line and frame timing
//!
//! | Fact | Value | Source |
//! |------|-------|--------|
//! | Master clock NTSC | 53.693175 MHz | emmd Genesis VDP technical reference |
//! | Master clock PAL | 53.203424 MHz | emmd |
//! | Scanline length | 3420 master clocks in both H32 and H40 | emmd; Kabuto hardware notes |
//! | H32 line | 342 dots at MCLK/10 | emmd; Kabuto |
//! | H40 line | 390 dots at MCLK/8 + 30 at MCLK/10 = 420 dots | emmd; Kabuto |
//! | Lines per frame | 262 NTSC, 313 PAL | emmd; Kabuto |
//! | Active lines | 224 (V28), 240 (V30) | emmd; Kabuto `Picture size` |
//! | Border rows | NTSC: 11 top + 8 bottom; PAL: 38 top + 32 bottom; V30 takes 8 from each | Kabuto `Picture size`; MacDonald's measured V-counter tables |
//! | Blanking rows | 19 sync/blanking rows per frame in both regions | Kabuto `Picture size` |
//!
//! The frame composes bottom-first and then the blanking cluster: the
//! active lines start at internal line 0, and the bottom border, the bottom
//! blanking, the vertical sync, the top blanking, and the top border run to
//! the end of the internal numbering (the picture starts at raster line 0
//! per Kabuto). The V-counter readback maps through the published jump
//! table (Nemesis' hardware measurements): NTSC `$00-$EA` then `$E5-$FF`;
//! PAL V28 `$00-$FF`, `$00-$02`, then `$CA-$FF`; PAL V30 `$00-$FF`,
//! `$00-$0A`, then `$D2-$FF`. For NTSC V30, whose picture rolls without a
//! stable frame on real hardware, this build keeps the NTSC 262-line
//! schedule and renders 240 active rows; the border shrink of V30 takes 8
//! lines from each border (11 becomes 3 at the top and 8 becomes 0 at the
//! bottom on NTSC), per the Kabuto rule that V30 removes 16 lines from the
//! borders of both regions.
//!
//! # Pinned H/V counter readback
//!
//! Source: Nemesis, "VDP HV counter/Status register progression and
//! timings" (SpritesMind thread 768), mirrored in the Kabuto hardware notes
//! under "H/V counter values".
//!
//! | Property | H32 | H40 |
//! |----------|-----|-----|
//! | H counter progression | `$00-$93`, `$E9-$FF` | `$00-$B5` two slots each, `$B6` one slot, `$E4` one slot, then `$E5-$FF` |
//! | V counter increments | H counter `$84` to `$85` | H counter `$A4` to `$A5` |
//! | H-blank flag set | H counter `$92` to `$93` | H counter `$B2` to `$B3` |
//! | H-blank flag clear | H counter `$04` to `$05` | H counter `$05` to `$06` |
//!
//! H40's `$B6` and `$E4` occupy one slot each (Eke's nine-bit internal
//! form, confirmed by Nemesis). The device schedules one visit per scanline
//! at the H-counter `$00` slot, so port accesses between visits answer from
//! the slot form computed out of the stored visit tick. The V-counter flip
//! of the readback rides the line boundary slot (330 in H40, 266 in H32),
//! 90 slots before `$00`; edges the device can only deliver at a visit
//! (the interrupts and the blanking state changes) deliver at that visit,
//! and the module records the delay.
//!
//! # Pinned interrupt positions
//!
//! Sources: MacDonald section 4 (the interrupt list and the interval
//! register), Nemesis' blanking and F-flag measurements, and Kabuto's note
//! that vblank switches roughly 32 pixels before the H-blank starts. All
//! instants sit inside the 3420-clock line; the device delivers at its line
//! visits.
//!
//! | Line | Instant | Gating |
//! |------|---------|--------|
//! | `int6` (68k level 6) | the start of the line whose V counter turns `$E0` (V28) or `$F0` (V30), the H-counter `$00`-to-`$01` change per Nemesis; delivered at the visit of that line | the v-interrupt pending flag and register `$01` bit 5 (IE0) |
//! | `int4` (68k level 4) | the line boundary (the V-counter increment slot, Eke's form and BlastEm's placement) when the interval counter expires: loaded at line 0, at expiry, and on the blanking lines after the first vblank line, so it fires only inside the active display plus that first vblank line | the expiry and register `$00` bit 4 (IE1) |
//! | `zint` (the Z80's own feed) | high from the vblank start line to the last line where the V counter reaches `$FF` (Nemesis' vblank-clear point) | ungated |
//!
//! MacDonald's priority note holds: a line interrupt due on the same line
//! as the vertical interrupt delivers first, so a HINT due on the `$E0`
//! line precedes the level-6 edge. The pending flags clear on a control
//! port read (the emmd status table; the repo's VDP siblings clear their
//! pending flags on the same read since the machine model carries no
//! device-visible acknowledge edge). The external level-2 interrupt of
//! register `$0B` bit 3 stays out of build; its source lives in the I/O
//! controller.
//!
//! # The ports and the control protocol
//!
//! Per MacDonald sections 3 and 7 and the emmd bus table: the data port
//! sits at offsets `$00-$03`, the control at `$04-$07`, and the HV counter
//! at `$08-$0F`; address bit 4 switches to the PSG and the undecoded debug
//! range, which this device leaves to their own device entries. A byte
//! write enters the byte in both lanes of the 16-bit form (an `$87` byte
//! write to the control port reads back `$8787`). A byte read answers the
//! even byte as the high byte of the port word and the odd byte as the low
//! byte. Mirrors of any window stride fold onto these low bits.
//!
//! The control write decodes per MacDonald:
//!
//! - Bits 15-14 = `10`: a register write of the index in bits 12-8 (the
//!   values past `$17` store without effect) and the data in bits 7-0. It
//!   cancels a pending command and clears the code register, per
//!   MacDonald's note verified by Golden Axe II.
//! - Bits 15-14 = `11`: the register read arm of this build. The index
//!   loads from bits 12-8 and the next control read returns the stored
//!   register byte in the high byte. (Kabuto records the real chip's upper
//!   status bits as open bus; this arm is the recorded readback form
//!   standing in for it.)
//! - Any other word starts or completes the command form: the first word
//!   enters CD1-CD0 from bits 15-14 and address bits 13-0 and sets the
//!   write-pending flag; the second word enters CD5-CD2 from bits 7-4 and
//!   A15-A14 from bits 1-0 and clears the flag.
//!
//! A data port write, a data port read, or a control read clears the
//! pending flag. Code bits CD3-CD0 decode the RAM: 0 the VRAM read, 1 the
//! VRAM write, 3 the CRAM write, 4 the VSRAM read, 5 the VSRAM write, 8
//! the CRAM read, and `$C` the undocumented 8-bit VRAM read. Reading under
//! a write code or writing under a read code still advances the address
//! and answers nothing (GPGX's form of MacDonald's rule). CD4 only matters
//! for the copy DMA. CD5 starts a DMA when register `$01` bit 4 enables
//! it. A read command set up by a completed two-word write pre-fetches the
//! addressed VRAM value and advances the address once (Kabuto's pre-fetch
//! form); the data port answers the buffer and re-fetches per access. The
//! CRAM and VSRAM reads answer directly at the access, the GPGX form. CRAM
//! reads unpack the 9-bit entry into `----BBB0GGG0RRR0`, VSRAM reads the
//! 10-bit value, and the 8-bit VRAM read answers `vram[address ^ 1]` in
//! the low byte. VRAM word writes swap the value's bytes when the address
//! bit 0 is set; a byte write to VRAM enters the byte in both lanes. CRAM
//! addresses fold on 128 bytes (64 word entries), VSRAM addresses fold on
//! 128 with writes past entry 39 ignored and reads wrapping to entry 0
//! (MacDonald's VSRAM notes), and every data access advances by the
//! auto-increment of register `$0F`.
//!
//! # The status register
//!
//! Reading the control port returns, per MacDonald and the emmd table:
//! bit 9 the FIFO empty, bit 8 the FIFO full, bit 7 the v-interrupt
//! pending, bit 6 the sprite overflow, bit 5 the sprite collision, bit 4
//! the odd field, bit 3 the vertical blanking, bit 2 the horizontal
//! blanking, bit 1 the DMA busy, and bit 0 the PAL flag. Bits 15-10 read
//! zero (the open-bus form is out of build). The read clears the pending
//! command, the v-interrupt pending flag, and the sprite overflow and
//! collision flags. The vertical blanking flag covers the lines from the
//! vblank start through the line before the V counter reaches `$FF`
//! (Nemesis), the horizontal blanking flag tracks the H-counter span
//! exactly per the table above, and both read position-accurately from the
//! access tick between visits.
//!
//! # The registers
//!
//! | Reg | Form in this build |
//! |-----|--------------------|
//! | `$00` | bit 4 the H-interrupt enable, bit 5 the leftmost-8-pixel backdrop blank, bit 1 the HV latch (set latches, a write with the bit clear releases) |
//! | `$01` | bit 6 the display enable, bit 5 the v-interrupt enable, bit 4 the DMA enable, bit 3 V30, bit 2 selects mode 5 and always stays 1; bit 7 (128 KiB VRAM) stores and behaves as the 64 KiB form |
//! | `$02` / `$04` | plane A / B name base: `(reg << 10) & 0x3800` / `(reg << 13) & 0x3800` |
//! | `$03` | window name base: `$800` aligned in H32 (`& 0xF800`), `$1000` in H40 (`& 0xF000`) |
//! | `$05` | sprite table base: `$200` aligned in H32 (`& 0xFE00`), `$400` in H40 (`& 0xFC00`) |
//! | `$07` | backdrop color: the palette in bits 5-4, the color in bits 3-0 |
//! | `$0A` | the H-interrupt interval; not reloaded by a write, per MacDonald |
//! | `$0B` | bit 3 the external interrupt enable (stored), bit 2 the column v-scroll, bits 1-0 the h-scroll mode (`00` full, `01` first-eight, `10` per 8 lines, `11` per line) |
//! | `$0C` | bit 0 the H40 enable (immediate), bits 2-1 the interlace (latched at the vblank start per MacDonald), bit 3 the shadow/highlight |
//! | `$0D` | h-scroll table base: `(reg & 0x3F) << 10` |
//! | `$0F` | the auto-increment |
//! | `$10` | bits 5-4 the vertical and bits 1-0 the horizontal plane size: 32, 64, invalid (treated as 32), 128 cells |
//! | `$11` / `$12` | the window boundaries: `$11` in 2-cell units with the right-side bit 7, `$12` in 8-line units with the down bit 7; both constraints union per MacDonald |
//! | `$13-$14` | the DMA length; zero means `$10000` (the emmd form) |
//! | `$15-$17` | the DMA source: the transfer source byte address is `(reg23 & 0x7F) << 17 | reg22 << 9 | reg21 << 1`, the copy source is registers `$21-$22` as a VRAM byte address; register `$17` bits 7-6 select the form |
//!
//! Reset stores `$FF` in register `$0A` (the verified power-on form: the
//! H-interrupt counter rests) and zeroes the rest.
//!
//! # DMA
//!
//! Register `$17` bits 7-6 select the form: `0x` the 68K-to-VDP transfer,
//! `10` the VRAM fill, `11` the VRAM copy; CD5 of a completed command with
//! register `$01` bit 4 set starts the form. Per MacDonald section 11:
//!
//! - The transfer moves words from the 68k bus: each word reads through
//!   the `bus`-named bus, writes per the code form, and steps the source
//!   by two bytes over the 24-bit space. The 68000 is frozen for the
//!   duration on the real machine; this build seizes it through the
//!   declared `hold` output line from the first transfer visit until the
//!   last word moves. Per-line bytes: 16 active / 167 blanking in H32,
//!   18 / 205 in H40, and these counts are words for a CRAM or VSRAM
//!   destination per MacDonald's rate note. The transfer runs linearly;
//!   the bank-wrap forms of the later 128 KiB research stay out of build.
//! - The fill waits for the next data-port write, which stores the written
//!   word normally; the fill then writes the written value's high byte to
//!   `vram[address ^ 1]` once per remaining length byte while the address
//!   steps by the auto-increment (GPGX's implementation of MacDonald's
//!   corrected text). The fill budget drops one slot from the transfer
//!   rate: 15 active / 166 blanking in H32, 17 / 204 in H40. In this
//!   build the fill also drives the `hold` line, standing in for the
//!   hardware's port-access restriction while it runs; the restriction
//!   itself (the 68000 may only poll during fills and copies) stays out
//!   of build, and the hold never deadlocks because the fill completes on
//!   its own visits.
//! - The copy reads `vram[source ^ 1]` and writes `vram[dest ^ 1]`, one
//!   byte plus one address step per length byte: the flipped-lane form of
//!   the emmd and GPGX. Its budget is 8 active / 83 blanking in H32 and
//!   9 / 102 in H40. CD4 must be set in the copy's command or the copy
//!   moves nothing.
//!
//! The length and source registers update at completion (the form GPGX
//! uses; MacDonald's doc marks the mid-transfer update as unverified and
//! this build keeps the completion form). The DMA-busy status bit covers
//! every running form. The FIFO models as an elastic queue: queued writes
//! record instantly and the depth only raises the full status bit past
//! four entries and drains at the next line visit, so the real DTACK stall
//! forms stay out of build. Mid-DMA control writes apply immediately; the
//! hardware's DTACK-refused and corrupt-register forms stay out of build.
//!
//! # Plane composition and the window
//!
//! Both scroll planes walk their name tables per MacDonald section 13: the
//! entry word packs priority, palette, vertical and horizontal flip, and
//! an 11-bit tile index; tiles are 4 bpp, 32 bytes per 8x8 tile with the
//! left pixel in the high nibble of each byte pair. The h scroll applies
//! as `planeX = (screenX - hscroll) & (planeWidth - 1)` with the value read
//! from the 4-byte-per-entry table at the mode's line offset over the low
//! eight bits of the raster line (Kabuto). The v scroll adds the VSRAM
//! value to the line before the row mask; the column form takes plane A at
//! VSRAM entry `2 * column` and plane B at `2 * column + 1`, columns of 16
//! pixels. The window replaces plane A in its region: the horizontal rule
//! from MacDonald section 17 (WHP in 2-cell units, RIGT flips the range,
//! WHP zero with RIGT set makes the whole line the window) applied on the
//! lines outside the vertical range, and the vertical range covering whole
//! lines inside, which composes to MacDonald's union form; GPGX's
//! `window_clip` guided the column split. MacDonald section 14 orders the
//! layers back to front: backdrop, low-priority plane B, low plane A, low
//! sprites, high plane B, high plane A, high sprites; a plane pixel with
//! its priority bit set renders normal-intensity in shadow/highlight mode
//! even when transparent, so a high-priority transparent column lifts
//! everything under it, per his own Ranger-X note. Map entries address
//! through the row-and-column masks of the plane size form with the
//! 8-KiB-name-table entry wrap; the invalid `10b` size fields behave as
//! the 32-cell size. The interlace doubled mode walks tiles of 64 bytes
//! (8x16) with name bit zero ignored, MacDonald's section 13 note, and his
//! own to-do list records that the doubled map-row and VSRAM details were
//! never measured; this build walks the same masks.
//!
//! # Sprites
//!
//! The attribute table holds 8-byte entries per MacDonald section 15: the
//! raw Y in word 0 (the screen line plus 128), the size byte in bits 3-2
//! and 1-0 of word 1 (cells minus one each way), the link in word 1 byte 1
//! (zero stops the walk), the attribute word of priority, palette, flips,
//! and the tile index, and the raw X in word 3 (the screen column plus
//! 128). The render walk starts at sprite 0 and follows the links with the
//! per-line limits of 20 sprites and 320 pixels in H40 (16 and 256 in
//! H32): a visible sprite past either cap is skipped and raises the
//! overflow flag. Sprites parse by the Y-window test of BLASTEm: the line
//! plus 128 wrapped in a 512-wide frame, against the raw Y in the same
//! window, with an 8-per-cell height (16 in the double-res interlace
//! mode, where the sprite screen line doubles per BlastEm). Tiles of a
//! multi-cell sprite walk column-major (`base + cellColumn * vCells +
//! cellRow`) and the flip flags mirror the whole sprite image at the pixel
//! and cell level. A raw X of zero hides every sprite parsed after it on
//! the lines it spans once a sprite with a nonzero X parsed first on that
//! line (Kabuto's form of MacDonald's masking mode 1; MacDonald's separate
//! X-of-one mode 2 arm reduces to the same first-nonzero-parse rule in
//! this build). Opaque sprite pixels over another sprite's opaque pixel
//! raise the collision flag; sprites draw in link order and only fill
//! transparent slots, so earlier sprites win (Kabuto's buffer rule).
//!
//! Interlace modes: mode 1 (LSM `01`) keeps the 224/240-row frame and
//! alternates the odd-field flag at each frame's vblank start (Nemesis);
//! mode 3 (LSM `11`) doubles the raster into 448/480 rows (`line * 2` plus
//! the field offset), makes tiles 64 bytes tall (8x16, exactly the 64 KiB
//! over the 1024 name pairs MacDonald's bit-zero note gives), and the HV
//! readback substitutes the published bit forms. Out-of-build list for
//! sprites and interlace: the one-line-ahead fetch pipeline (each line
//! renders from the register state at its visit, the established
//! one-visit-per-line form of the repo's video devices), Kabuto's
//! cross-line masking refinement and the beyond-80 junk forms, the
//! write-through sprite cache, MacDonald's open sprite table questions,
//! the interlace PAL odd-frame line-count shifts, and the NTSC V30 roll.
//!
//! # Shadow/highlight
//!
//! Register `$0C` bit 3 enables the three-intensity form. Composition per
//! MacDonald section 16 and GPGX/BLASTEm's pixel resolution: the backdrop
//! and low-priority content render at half intensity, a set priority (the
//! plane priority bits lift even through transparent pixels) raises the
//! visible pixel one step, low-priority sprites inherit the lifted or
//! shadow state of the background beneath them, and the palette-3 sprite
//! colors `$E` and `$F` act as operators on the winning pixel: `$3E`
//! raises the value one step (highlight, the emmd table's `14` operator)
//! and `$3F` forces half (shadow, the `15` operator). MacDonald's v1.5f
//! text swaps the two operators; the GPGX, BLASTEm, emmd, and Rasterscroll
//! forms agree with this encoding, and the divergence records here. Sprite
//! colors `0E`/`1E`/`2E` render normal regardless of priority (MacDonald's
//! bug note). Channels convert from 3 bits by `(v << 5) | (v << 2) |
//! (v >> 1)`; shadow halves the channel and highlight adds 128 capped at
//! 255, the emmd intensity forms.
//!
//! # Sources
//!
//! The emmd Genesis VDP (315-5313) technical reference; Charles MacDonald,
//! Sega Genesis VDP documentation v1.5f (the port protocol, the register
//! list, the DMA rates and pseudocode, the sprite table, the priority
//! ladder, shadow/highlight); Nemesis' HV counter and status register
//! measurements (SpritesMind thread 768) with Eke's H40 nine-bit
//! refinement; Kabuto's Mega Drive hardware notes (picture size, H/V
//! counter table, sprite buffer rules, the bus notes). Genesis Plus GX and
//! BLASTEm sources guided the port forms, the DMA byte-lane details, the
//! window clip, and the compositor.

use demu_config::model::{BootState, DeviceInstance};
use demu_core::device::{AccessWidth, BusAccess, Frame, MachineCtx, VideoLayer, VideoSource};
use demu_core::frequency::{Address, Tick};
use demu_core::signal::Level;
use demu_core::triggers::TriggerEvent;
use demu_core::{Device, Snapshot, SnapshotError};

/// The VRAM bytes.
const VRAM_SIZE: usize = 0x1_0000;
/// The VRAM address mask.
const VRAM_MASK: usize = VRAM_SIZE - 1;
/// The CRAM bytes (64 entries of 9-bit color).
const CRAM_BYTES: usize = 0x80;
/// The live VSRAM entries (the writes past 40 are ignored).
const VSRAM_ENTRIES: usize = 40;
/// The stored registers `$00-$17`.
const REGS: usize = 24;
/// The master clocks of one scanline in both horizontal modes.
const LINE_MASTER: u64 = 3420;
/// The NTSC lines per frame.
const LINES_NTSC: u64 = 262;
/// The PAL lines per frame.
const LINES_PAL: u64 = 313;
/// The snapshot tag and version.
const TAG: &[u8; 4] = b"MDVD";
const VERSION: u8 = 1;

/// The H40 boundary slots of the pinned events (the H counter's two-slot steps).
const H40_SLOT_VINCREMENT: usize = 330;
const H40_SLOT_HBLANK_SET: usize = 358;
const H40_SLOT_HBLANK_CLEAR: usize = 12;
const H32_SLOT_VINCREMENT: usize = 266;
const H32_SLOT_HBLANK_SET: usize = 294;
const H32_SLOT_HBLANK_CLEAR: usize = 10;

/// The DMA byte rates per line. Index 0 is H32 and index 1 is H40.
const RATE_TRANSFER_ACTIVE: [u32; 2] = [16, 18];
const RATE_TRANSFER_BLANK: [u32; 2] = [167, 205];
const RATE_FILL_ACTIVE: [u32; 2] = [15, 17];
const RATE_FILL_BLANK: [u32; 2] = [166, 204];
const RATE_COPY_ACTIVE: [u32; 2] = [8, 9];
const RATE_COPY_BLANK: [u32; 2] = [83, 102];

/// The plane sizes in cells for the HSZ and VSZ encodings of register `$10`.
const PLANE_CELLS: [u32; 4] = [32, 64, 32, 128];

/// The DMA forms behind register `$17` bits 7-6.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DmaForm {
    /// No transfer running.
    Off,
    /// The 68K-to-VDP transfer (the words read through the bus).
    Transfer,
    /// The fill armed, waiting for the data-port write.
    FillArmed,
    /// The fill moving bytes.
    Fill,
    /// The copy moving bytes.
    Copy,
}

impl DmaForm {
    /// True while the form moves data (and holds the 68k in this build).
    fn running(self) -> bool {
        matches!(self, DmaForm::Transfer | DmaForm::Fill | DmaForm::Copy)
    }
}

/// One parsed sprite of the render walk.
#[derive(Debug, Clone, Copy)]
struct LineSprite {
    /// The leftmost screen column (the raw X minus 128).
    left: i32,
    /// The first screen line of the sprite (the raw Y minus 128).
    top: i64,
    /// The base tile index.
    tiles: u16,
    /// The horizontal cells (1-4).
    wc: u8,
    /// The vertical cells (1-4).
    hc: u8,
    /// The palette 0-3.
    pal: u8,
    /// The priority bit.
    pri: bool,
    /// The horizontal flip.
    hf: bool,
    /// The vertical flip.
    vf: bool,
}

/// The decoded port behind the low address bits of the VDP window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Port {
    /// The data port at offsets `$00-$03`.
    Data,
    /// The control port at `$04-$07`.
    Control,
    /// The HV counter at `$08-$0F`.
    Hv,
    /// The PSG and debug zone behind address bit 4 (not this device).
    Psg,
}

/// The Sega Genesis Video Display Processor.
#[derive(Debug)]
pub struct GenesisVdp {
    name: String,
    /// The bus the DMA transfer reads source words through.
    bus: String,
    /// The region latch: false NTSC, true PAL (the `region` parameter).
    pal: bool,
    /// The domain ticks per master clock (the `dot` divider, default 1).
    tpm: u64,
    /// The 64 KiB video RAM.
    vram: Box<[u8]>,
    /// The 64 entries of 9-bit color RAM.
    cram: [u16; 64],
    /// The vertical-scroll RAM: 64 addressable words, 40 live entries.
    vsram: [u16; 64],
    /// The stored registers `$00-$17`.
    regs: [u8; REGS],
    /// The address register (bits 15-0).
    addr: u16,
    /// The latched A15-A14 of the command's second word.
    addr_latch: u16,
    /// The code register CD5-CD0.
    code: u8,
    /// The two-word command's write-pending flag.
    pending: bool,
    /// The prefetched read value.
    read_buf: u16,
    /// Whether `read_buf` holds the prefetched word of the current command.
    read_fetched: bool,
    /// The register read arm of the control port.
    reg_read_reg: Option<u8>,
    /// The number of queued data-port writes (the elastic FIFO form).
    fifo_count: u8,
    /// The DMA form.
    dma_form: DmaForm,
    /// The remaining DMA bytes, counting the transfer as two bytes per word.
    dma_remaining: u32,
    /// The DMA source byte address (the transfer) or VRAM byte address (the copy).
    dma_src: u32,
    /// The fill's data word latched from the trigger write.
    dma_fill_data: u16,
    /// The internal scanline of the current span (0 = the first active line).
    line: u64,
    /// The visit tick of the current line span.
    visit_tick: u64,
    last_tick: Option<Tick>,
    next: Option<Tick>,
    /// The H-interval countdown.
    hint_counter: u8,
    /// The line-interrupt pending latch (the control read clears it).
    hint_pending: bool,
    /// The v-interrupt pending flag (the status bit 7).
    vip: bool,
    /// The sprite overflow flag (the status bit 6).
    sov: bool,
    /// The sprite collision flag (the status bit 5).
    scl: bool,
    /// The odd-field flag (the status bit 4).
    odd: bool,
    /// The latched interlace mode (the LSM bits at the last vblank start).
    lsm_latched: u8,
    /// The latched HV word (register `$00` bit 1).
    hv_latch: Option<u16>,
    /// The driven interrupt and hold lines.
    int6_driven: bool,
    int4_driven: bool,
    zint_driven: bool,
    hold_driven: bool,
    /// The composed frames since boot.
    frames: u64,
    /// The composed canvas in RGB888.
    screen: Vec<u8>,
    /// The cached frame for the shell.
    frame: Frame,
    /// The hot per-line layer buffers: the plane A, B, and sprite rows.
    row_a: Vec<u8>,
    row_b: Vec<u8>,
    row_s: Vec<u8>,
    /// The parsed sprites of the current line.
    sprites: Vec<LineSprite>,
}

impl GenesisVdp {
    /// The widest frame this build composes (the H40 active width).
    const FRAME_WIDTH_MAX: usize = 320;
    /// The tallest progressive frame.
    const FRAME_HEIGHT_MAX: usize = 240;
    /// The line buffer span: the H40 active width plus both borders.
    const LINE_BUFFER: usize = 347;
    /// The maximal sprites the table walk touches per line.
    const SPRITE_WALK: usize = 80;

    /// A VDP from its configuration entry.
    ///
    /// Parameters: `region` (`ntsc` default or `pal`) and `bus` (the bus
    /// the DMA transfer reads through, default `cpu`). The `dot` divider
    /// in the named dividers counts domain ticks per master clock; the
    /// default 1 matches the machine running the VDP on the master
    /// crystal domain, where one visit per scanline spans 3420 ticks.
    pub fn new(device: &DeviceInstance) -> Result<Self, String> {
        let param = |key: &str| device.params.get(key).and_then(|v| v.as_str());
        let region = param("region").unwrap_or("ntsc").to_ascii_lowercase();
        let pal = match region.as_str() {
            "ntsc" => false,
            "pal" => true,
            other => {
                return Err(format!(
                    "the genesis.vdp region must be ntsc or pal, got '{other}'"
                ));
            }
        };
        let bus = param("bus").unwrap_or("cpu").to_string();
        let tpm = u64::from(device.dividers.get("dot").copied().unwrap_or(1)).max(1);
        let width = Self::FRAME_WIDTH_MAX;
        let height = Self::FRAME_HEIGHT_MAX;
        let mut regs = [0u8; REGS];
        regs[0x0A] = 0xFF;
        Ok(Self {
            name: device.name.to_string(),
            bus,
            pal,
            tpm,
            vram: vec![0; VRAM_SIZE].into_boxed_slice(),
            cram: [0; 64],
            vsram: [0; 64],
            regs,
            addr: 0,
            addr_latch: 0,
            code: 0,
            pending: false,
            read_buf: 0,
            read_fetched: false,
            reg_read_reg: None,
            fifo_count: 0,
            dma_form: DmaForm::Off,
            dma_remaining: 0,
            dma_src: 0,
            dma_fill_data: 0,
            line: 0,
            visit_tick: 0,
            last_tick: None,
            next: None,
            hint_counter: 0xFF,
            hint_pending: false,
            vip: false,
            sov: false,
            scl: false,
            odd: false,
            lsm_latched: 0,
            hv_latch: None,
            int6_driven: false,
            int4_driven: false,
            zint_driven: false,
            hold_driven: false,
            frames: 0,
            screen: vec![0; width * height * 3],
            frame: Frame {
                width: width as u32,
                height: height as u32,
                pixels: vec![0; width * height * 3],
            },
            row_a: vec![0; Self::LINE_BUFFER],
            row_b: vec![0; Self::LINE_BUFFER],
            row_s: vec![0; Self::LINE_BUFFER],
            sprites: Vec::with_capacity(24),
        })
    }

    // -- Region, mode, and schedule helpers --------------------------------

    /// The lines per frame for the region.
    fn total_lines(&self) -> u64 {
        if self.pal { LINES_PAL } else { LINES_NTSC }
    }

    /// The first line of the bottom-border and vblank cluster: the V
    /// counter turns `$E0` (V28) or `$F0` (V30) here, and the active
    /// display ends before it. (The border-row layout for each region
    /// sits in the module timing table.)
    fn vblank_line(&self) -> u64 {
        if self.m2() { 240 } else { 224 }
    }

    /// The H40 flag of the current register `$0C` bit 0.
    fn h40(&self) -> bool {
        self.regs[0x0C] & 0x01 != 0
    }

    /// The V30 flag of the current register `$01` bit 3.
    fn m2(&self) -> bool {
        self.regs[0x01] & 0x08 != 0
    }

    /// The display-enabled flag of register `$01` bit 6.
    fn display_on(&self) -> bool {
        self.regs[0x01] & 0x40 != 0
    }

    /// The shadow/highlight flag of register `$0C` bit 3.
    fn shadow_mode(&self) -> bool {
        self.regs[0x0C] & 0x08 != 0
    }

    /// The latched interlace form: 0 none, 1 mode 1, 3 double.
    fn lsm(&self) -> u8 {
        self.lsm_latched & 3
    }

    /// The domain ticks of one scanline.
    fn line_ticks(&self) -> u64 {
        LINE_MASTER * self.tpm
    }

    /// The H counter's line boundary slot: the V counter increments here.
    fn vincrement_slot(h40: bool) -> usize {
        if h40 {
            H40_SLOT_VINCREMENT
        } else {
            H32_SLOT_VINCREMENT
        }
    }

    /// The H-blank flag set slot (the H counter `$92-$93` / `$B2-$B3` change).
    fn hblank_set_slot(h40: bool) -> usize {
        if h40 {
            H40_SLOT_HBLANK_SET
        } else {
            H32_SLOT_HBLANK_SET
        }
    }

    /// The H-blank flag clear slot (the `$04-$05` / `$05-$06` change).
    fn hblank_clear_slot(h40: bool) -> usize {
        if h40 {
            H40_SLOT_HBLANK_CLEAR
        } else {
            H32_SLOT_HBLANK_CLEAR
        }
    }

    /// The slot that owns master-clock offset `m` inside the line.
    fn slot_at_mclk(h40: bool, m: u64) -> usize {
        if h40 {
            if m < 3120 {
                (m / 8) as usize
            } else {
                390 + ((m - 3120) / 10) as usize
            }
        } else {
            (m / 10) as usize
        }
    }

    /// The H counter readback of one slot, per the pinned progression.
    fn hcount_readback(h40: bool, slot: usize) -> u8 {
        if h40 {
            match slot {
                0..=363 => (slot / 2) as u8,
                364 => 0xB6,
                365 => 0xE4,
                other => (0xE5 + (other - 366) / 2) as u8,
            }
        } else {
            match slot {
                0..=295 => (slot / 2) as u8,
                other => (0xE9 + (other - 296) / 2) as u8,
            }
        }
    }

    /// The line where the internal V counter jumps (the frame's last pass
    /// of the counter runs from here to the wrap).
    fn v_jump_start(&self) -> u64 {
        if self.pal {
            if self.m2() { 267 } else { 259 }
        } else {
            235
        }
    }

    /// The internal nine-bit V counter of one line.
    fn v_internal(&self, line: u64) -> u16 {
        if line < self.v_jump_start() {
            line as u16
        } else {
            (line.wrapping_sub(self.total_lines()) & 0x1FF) as u16
        }
    }

    /// The displayed V counter of one line (the low byte of the internal).
    fn v_display(&self, line: u64) -> u8 {
        (self.v_internal(line) & 0xFF) as u8
    }

    /// The slot inside the current line span at `tick`.
    fn slot_in_line(&self, tick: Tick) -> usize {
        let rel_ticks = tick.0.saturating_sub(self.visit_tick) % self.line_ticks();
        let rel_mclk = rel_ticks / self.tpm;
        Self::slot_at_mclk(self.h40(), rel_mclk)
    }

    /// The effective V line at `tick`: the current internal line, or the
    /// next one once the slot pass crosses the V-increment boundary.
    fn v_line_at(&self, tick: Tick) -> u64 {
        let total = self.total_lines();
        if self.slot_in_line(tick) >= Self::vincrement_slot(self.h40()) {
            (self.line + 1) % total
        } else {
            self.line
        }
    }

    /// The readback V counter at `tick`.
    fn v_counter_at(&self, tick: Tick) -> u8 {
        self.v_display(self.v_line_at(tick))
    }

    /// The readback H counter at `tick`.
    fn h_counter_at(&self, tick: Tick) -> u8 {
        let slot = self.slot_in_line(tick);
        Self::hcount_readback(self.h40(), slot)
    }

    /// The packed HV word at `tick` (the V in the high byte, the H low).
    fn hv_word_at(&self, tick: Tick) -> u16 {
        (u16::from(self.v_counter_at(tick)) << 8) | u16::from(self.h_counter_at(tick))
    }

    /// The HV readback at `tick` with the interlace substitution forms.
    fn hv_read(&self, tick: Tick) -> u16 {
        if let Some(latch) = self.hv_latch {
            return latch;
        }
        let internal = self.v_internal(self.v_line_at(tick));
        let h = self.h_counter_at(tick);
        let v = match self.lsm() {
            3 => {
                // The doubled form: the low seven bits at 1-7, and the
                // bit 7 of the internal counter in bit 0 (Nemesis).
                let v8 = (internal & 0xFF) as u8;
                u16::from(((v8 & 0x7F) << 1) | ((v8 >> 7) & 1))
            }
            1 => {
                // The mode-1 form: bits 7-1 of the counter, and the ninth
                // (overflow) bit of the nine-bit internal in bit 0.
                ((internal & 0xFE) | ((internal >> 8) & 1)) & 0xFF
            }
            _ => internal & 0xFF,
        };
        (v << 8) | u16::from(h)
    }

    /// The horizontal blanking flag at `tick`: on from the H-counter set
    /// slot, held through the line wrap, cleared at the clear slot.
    fn hblank_flag_at(&self, tick: Tick) -> bool {
        let h40 = self.h40();
        let slot = self.slot_in_line(tick);
        slot >= Self::hblank_set_slot(h40) || slot < Self::hblank_clear_slot(h40)
    }

    /// The vertical blanking flag state of one line: on from the vblank
    /// start line through the line before the V counter reaches `$FF`.
    fn vblank_flag_line(&self, line: u64) -> bool {
        line >= self.vblank_line() && line + 1 < self.total_lines()
    }

    /// The frame dimensions for the current mode.
    fn frame_dims(&self) -> (usize, usize) {
        let width = if self.h40() { 320 } else { 256 };
        let base = if self.m2() { 240 } else { 224 };
        let height = match self.lsm() {
            3 => base * 2,
            _ => base,
        };
        (width, height)
    }

    /// Resize the composed canvas when the mode changed it.
    fn ensure_canvas(&mut self) {
        let (w, h) = self.frame_dims();
        if self.frame.width != w as u32
            || self.frame.height != h as u32
            || self.screen.len() != w * h * 3
        {
            self.frame.width = w as u32;
            self.frame.height = h as u32;
            self.screen = vec![0; w * h * 3];
            self.frame.pixels = vec![0; w * h * 3];
        }
    }

    // -- The control port ---------------------------------------------------

    /// The control port write: the register form, the register read arm,
    /// or the two-word command form.
    fn control_write(&mut self, word: u16) {
        match word & 0xC000 {
            0x8000 => {
                // The register write: cancels a pending command and clears
                // the code register (MacDonald's Golden Axe II note).
                self.pending = false;
                self.code = 0;
                self.read_fetched = false;
                self.reg_read_reg = None;
                self.set_register(((word >> 8) & 0x1F) as usize, (word & 0xFF) as u8);
            }
            _ => {
                if self.pending {
                    // The second word: CD5-CD2 from bits 7-4, A15-A14 from
                    // bits 1-0; a pending register read arm aborts with it.
                    self.pending = false;
                    self.reg_read_reg = None;
                    self.addr_latch = (word & 3) << 14;
                    self.addr = self.addr_latch | (self.addr & 0x3FFF);
                    self.code = (self.code & 0x03) | (((word >> 2) & 0x3C) as u8);
                    self.arm_dma();
                    if self.code & 0x20 == 0 {
                        self.prefetch_read();
                    }
                } else {
                    // The first word: CD1-CD0 from bits 15-14, bits 13-0.
                    // CD1-CD0 = 11 (a CRAM-write-style prefix) also arms the
                    // register read form of the module docs; the arm rides
                    // along and aborts on the second word or a port access.
                    if word & 0xC000 == 0xC000 {
                        self.reg_read_reg = Some(((word >> 8) & 0x1F) as u8);
                    } else {
                        self.reg_read_reg = None;
                    }
                    self.addr = (word & 0x3FFF) | (self.addr & 0xC000);
                    self.code = (self.code & 0x3C) | ((word >> 14) & 3) as u8;
                    self.pending = true;
                }
            }
        }
    }

    /// The pre-fetch of a completed read command: the buffer fills from
    /// the address and the address steps once (Kabuto's pre-fetch form).
    fn prefetch_read(&mut self) {
        let code = self.code & 0x0F;
        if code == 0x00 || code == 0x0C {
            if code == 0x0C {
                self.read_buf = u16::from(self.vram[usize::from(self.addr ^ 1) & VRAM_MASK]);
            } else {
                let index = usize::from(self.addr & 0xFFFE);
                self.read_buf = u16::from(self.vram[index & VRAM_MASK]) << 8
                    | u16::from(self.vram[(index + 1) & VRAM_MASK]);
            }
            self.read_fetched = true;
            self.step_addr();
        }
    }

    /// The control port read at `tick`: the armed register byte, else the
    /// status word with its side effects.
    fn control_read(&mut self, tick: Tick) -> u16 {
        self.clear_pending();
        if let Some(reg) = self.reg_read_reg.take() {
            return u16::from(self.regs[usize::from(reg & 0x1F)]) << 8;
        }
        let status = self.status_word(tick);
        self.vip = false;
        self.sov = false;
        self.scl = false;
        self.hint_pending = false;
        status
    }

    /// The status word at `tick`.
    fn status_word(&self, tick: Tick) -> u16 {
        let mut status = u16::from(self.pal);
        if self.dma_form != DmaForm::Off {
            status |= 0x02;
        }
        if self.hblank_flag_at(tick) {
            status |= 0x04;
        }
        if self.vblank_flag_line(self.line) {
            status |= 0x08;
        }
        if self.odd {
            status |= 0x10;
        }
        if self.scl {
            status |= 0x20;
        }
        if self.sov {
            status |= 0x40;
        }
        if self.vip {
            status |= 0x80;
        }
        if self.fifo_count == 0 {
            status |= 0x200;
        }
        if self.fifo_count >= 4 {
            status |= 0x100;
        }
        status
    }

    /// A data port or control read clears the pending flag.
    fn clear_pending(&mut self) {
        self.pending = false;
    }

    // -- The register write forms ------------------------------------------

    /// The register write of index `reg` with `data`.
    fn set_register(&mut self, reg: usize, data: u8) {
        if reg >= REGS {
            return;
        }
        let was_m3 = self.regs[0x00] & 0x02 != 0;
        self.regs[reg] = data;
        if reg == 0x00 {
            if data & 0x02 != 0 && !was_m3 {
                // The latch bit set from clear: the HV word latches now.
                self.hv_latch = Some(self.hv_word_at(Tick(self.visit_tick)));
            }
            if data & 0x02 == 0 {
                self.hv_latch = None;
            }
        }
    }

    // -- The data port ------------------------------------------------------

    /// The data port write with the 16-bit value.
    fn data_write(&mut self, word: u16) {
        self.clear_pending();
        self.reg_read_reg = None;
        if self.dma_form == DmaForm::FillArmed {
            // The trigger write stores the word normally; the fill then
            // moves the length's remaining bytes at the next visit.
            self.dma_form = DmaForm::Fill;
            self.dma_remaining = self.dma_length_fill_bytes();
            self.dma_fill_data = word;
        }
        self.push_fifo_write();
        self.bus_write(word);
    }

    /// The data port read with the 16-bit word value.
    fn data_read(&mut self) -> u16 {
        self.clear_pending();
        self.reg_read_reg = None;
        let value = self.read_value();
        self.advance_read();
        value
    }

    /// The address step of one VRAM or RAM access.
    fn step_addr(&mut self) {
        let inc = u16::from(self.regs[0x0F]);
        self.addr = self.addr.wrapping_add(inc);
    }

    /// The write path of one 16-bit data word per the code form.
    fn bus_write(&mut self, word: u16) {
        match self.code & 0x0F {
            0x01 => {
                // VRAM write: the aligned word, byteswapped on odd.
                let data = if self.addr & 1 != 0 {
                    word.rotate_right(8)
                } else {
                    word
                };
                let index = usize::from(self.addr & 0xFFFE);
                self.vram[index & VRAM_MASK] = (data >> 8) as u8;
                self.vram[(index + 1) & VRAM_MASK] = (data & 0xFF) as u8;
            }
            0x03 => {
                // CRAM write: pack to the 9-bit form at the folded index.
                let packed =
                    ((word & 0x0E00) >> 3) | ((word & 0x00E0) >> 2) | ((word & 0x000E) >> 1);
                let index = (usize::from(self.addr) >> 1) & 0x3F;
                self.cram[index] = packed;
            }
            0x05 => {
                // VSRAM write: the 10-bit value, live entries only.
                let index = (usize::from(self.addr) >> 1) & 0x3F;
                if index < VSRAM_ENTRIES {
                    self.vsram[index] = word & 0x03FF;
                }
            }
            _ => {
                // The read codes and the invalid codes take no data; the
                // address still steps, per GPGX's form of MacDonald's rule.
            }
        }
        self.step_addr();
    }

    /// The read value per the code form, before the address step.
    fn read_value(&mut self) -> u16 {
        match self.code & 0x0F {
            0x00 => {
                if self.read_fetched {
                    self.read_buf
                } else {
                    let index = usize::from(self.addr & 0xFFFE);
                    u16::from(self.vram[index & VRAM_MASK]) << 8
                        | u16::from(self.vram[(index + 1) & VRAM_MASK])
                }
            }
            0x0C => {
                // The undocumented 8-bit VRAM read of the flipped lane.
                if self.read_fetched {
                    self.read_buf & 0xFF
                } else {
                    u16::from(self.vram[usize::from(self.addr ^ 1) & VRAM_MASK])
                }
            }
            0x08 => {
                // The CRAM read: unpack to ----BBB0GGG0RRR0.
                let packed = self.cram[(usize::from(self.addr) >> 1) & 0x3F];
                ((packed & 0x1C0) << 3) | ((packed & 0x038) << 2) | ((packed & 0x007) << 1)
            }
            0x04 => {
                // The VSRAM read: the 10-bit value; reads past entry 40 wrap.
                let mut index = (usize::from(self.addr) >> 1) & 0x3F;
                if index >= VSRAM_ENTRIES {
                    index = 0;
                }
                self.vsram[index]
            }
            _ => 0,
        }
    }

    /// The pipeline advance after a read: the pre-fetch of the next VRAM
    /// buffer where the code form runs one.
    fn advance_read(&mut self) {
        match self.code & 0x0F {
            0x00 | 0x0C => {
                if self.code & 0x0F == 0x0C {
                    self.read_buf = u16::from(self.vram[usize::from(self.addr ^ 1) & VRAM_MASK]);
                } else {
                    let index = usize::from(self.addr & 0xFFFE);
                    self.read_buf = u16::from(self.vram[index & VRAM_MASK]) << 8
                        | u16::from(self.vram[(index + 1) & VRAM_MASK]);
                }
                self.read_fetched = true;
            }
            _ => {
                self.read_fetched = false;
            }
        }
        self.step_addr();
    }

    /// The elastic FIFO bookkeeping of one data write.
    fn push_fifo_write(&mut self) {
        self.fifo_count = self.fifo_count.saturating_add(1);
    }

    // -- DMA ----------------------------------------------------------------

    /// The DMA length in bytes for the transfer (the count is words, hence
    /// doubled).
    fn dma_length_bytes(&self) -> u32 {
        let raw = u32::from((u16::from(self.regs[0x14]) << 8) | u16::from(self.regs[0x13]));
        let words = if raw == 0 { 0x1_0000 } else { raw };
        words * 2
    }

    /// The DMA length in raw bytes for the fill and copy (zero `$10000`).
    fn dma_length_fill_bytes(&self) -> u32 {
        let raw = u32::from((u16::from(self.regs[0x14]) << 8) | u16::from(self.regs[0x13]));
        if raw == 0 { 0x1_0000 } else { raw }
    }

    /// The arm of the DMA form behind register `$17` bits 7-6 at the
    /// completed command with CD5.
    fn arm_dma(&mut self) {
        if self.code & 0x20 == 0 || self.regs[0x01] & 0x10 == 0 {
            return;
        }
        match (self.regs[0x17] >> 6) & 3 {
            0 | 1 => {
                // The 68K-to-VDP transfer, the length in words.
                self.dma_form = DmaForm::Transfer;
                self.dma_remaining = self.dma_length_bytes();
                self.dma_src = (u32::from(self.regs[0x17] & 0x7F) << 17)
                    | (u32::from(self.regs[0x16]) << 9)
                    | (u32::from(self.regs[0x15]) << 1);
            }
            2 => {
                self.dma_form = DmaForm::FillArmed;
                self.dma_remaining = self.dma_length_fill_bytes();
            }
            _ => {
                // The copy of a VRAM byte source in registers `$21-$22`.
                self.dma_form = DmaForm::Copy;
                self.dma_remaining = self.dma_length_fill_bytes();
                self.dma_src = (u32::from(self.regs[0x16]) << 8) | u32::from(self.regs[0x15]);
            }
        }
    }

    /// The per-line byte budget of the running form.
    fn dma_budget(&self) -> u32 {
        let blank = !self.display_on() || self.line >= self.vblank_line();
        let mode = usize::from(self.h40());
        let (active, blank_rate) = match self.dma_form {
            DmaForm::Transfer => (RATE_TRANSFER_ACTIVE, RATE_TRANSFER_BLANK),
            DmaForm::Fill | DmaForm::FillArmed => (RATE_FILL_ACTIVE, RATE_FILL_BLANK),
            DmaForm::Copy => (RATE_COPY_ACTIVE, RATE_COPY_BLANK),
            DmaForm::Off => ([0, 0], [0, 0]),
        };
        let rate = if blank { blank_rate } else { active };
        if self.dma_form == DmaForm::Transfer && self.code & 0x0F != 0x01 {
            // The CRAM and VSRAM destinations count words, so the byte
            // budget doubles, per MacDonald's rate note.
            rate[mode] * 2
        } else {
            rate[mode]
        }
    }

    /// The per-line DMA step at the visit: move the budget's bytes or words.
    fn step_dma(&mut self, tick: Tick, ctx: &mut MachineCtx) {
        if !self.dma_form.running() {
            return;
        }
        let budget = self.dma_budget();
        let mut budget = budget;
        match self.dma_form {
            DmaForm::Transfer => {
                // Whole words per step, two bytes of budget each.
                while self.dma_remaining >= 2 && budget >= 2 {
                    let word = self.dma_read_word(tick, ctx);
                    self.bus_write(word);
                    self.dma_remaining -= 2;
                    budget -= 2;
                }
            }
            DmaForm::Fill => {
                let data = self.dma_fill_data;
                while self.dma_remaining > 0 && budget > 0 {
                    match self.code & 0x0F {
                        0x01 => {
                            // The high byte of the fill value at the
                            // opposite lane, then the auto-increment step.
                            let index = usize::from(self.addr ^ 1) & VRAM_MASK;
                            self.vram[index] = (data >> 8) as u8;
                        }
                        0x03 | 0x05 => {
                            // The CRAM and VSRAM fills write the latched
                            // word per step (GPGX's form).
                            self.bus_write_word_only(data);
                        }
                        _ => {
                            // The invalid destination moves nothing but
                            // still steps, per GPGX's form.
                        }
                    }
                    self.step_addr();
                    self.dma_remaining -= 1;
                    budget -= 1;
                }
            }
            DmaForm::Copy => {
                while self.dma_remaining > 0 && budget > 0 {
                    let byte = self.vram[(self.dma_src ^ 1) as usize & 0xFFFF];
                    let index = usize::from(self.addr ^ 1) & VRAM_MASK;
                    self.vram[index] = byte;
                    self.dma_src = (self.dma_src + 1) & 0xFFFF;
                    self.step_addr();
                    self.dma_remaining -= 1;
                    budget -= 1;
                }
            }
            DmaForm::Off | DmaForm::FillArmed => {}
        }
        if self.dma_remaining == 0 {
            self.finish_dma();
        }
    }

    /// The word-only RAM write of the fill's CRAM and VSRAM forms.
    fn bus_write_word_only(&mut self, word: u16) {
        match self.code & 0x0F {
            0x03 => {
                let packed =
                    ((word & 0x0E00) >> 3) | ((word & 0x00E0) >> 2) | ((word & 0x000E) >> 1);
                let index = (usize::from(self.addr) >> 1) & 0x3F;
                self.cram[index] = packed;
            }
            _ => {
                let index = (usize::from(self.addr) >> 1) & 0x3F;
                if index < VSRAM_ENTRIES {
                    self.vsram[index] = word & 0x03FF;
                }
            }
        }
    }

    /// One bus word read of the transfer: the address runs linearly over
    /// the 24-bit space and steps by two bytes per word.
    fn dma_read_word(&mut self, tick: Tick, ctx: &mut MachineCtx) -> u16 {
        let address = self.dma_src & 0x00FF_FFFF;
        let raw = ctx.read(&self.bus, AccessWidth::Word, Address(address), tick);
        self.dma_src = (self.dma_src + 2) & 0x00FF_FFFF;
        raw as u16
    }

    /// The DMA completion: the length registers zero and the source
    /// registers update, per the completion form of GPGX.
    fn finish_dma(&mut self) {
        self.regs[0x13] = 0;
        self.regs[0x14] = 0;
        match self.dma_form {
            DmaForm::Transfer => {
                let src = self.dma_src >> 1;
                self.regs[0x15] = (src & 0xFF) as u8;
                self.regs[0x16] = ((src >> 8) & 0xFF) as u8;
            }
            DmaForm::Copy => {
                self.regs[0x15] = (self.dma_src & 0xFF) as u8;
                self.regs[0x16] = ((self.dma_src >> 8) & 0xFF) as u8;
            }
            _ => {}
        }
        self.dma_form = DmaForm::Off;
    }

    // -- The line engine ----------------------------------------------------

    /// The H-interval counter step of one line, per MacDonald: loaded at
    /// line zero, at expiry, and on the blanking lines after the first
    /// vblank line; the first vblank line itself still counts.
    fn step_hint(&mut self) {
        if self.line == 0
            || !self.display_on()
            || self.line + 1 >= self.total_lines()
            || self.line > self.vblank_line()
        {
            self.hint_counter = self.regs[0x0A];
            return;
        }
        if self.hint_counter == 0 {
            self.hint_pending = true;
            self.hint_counter = self.regs[0x0A];
        } else {
            self.hint_counter -= 1;
        }
    }

    /// The per-line events of the visit: the DMA budget, the H-interval
    /// step, and the frame-boundary state of the vblank start.
    fn step_line_events(&mut self, tick: Tick, ctx: &mut MachineCtx) {
        self.step_dma(tick, ctx);
        self.step_hint();
        if self.line == self.vblank_line() {
            // The vblank start line: the VIP flag sets regardless of the
            // enables (Nemesis), the interlace register latches, the odd
            // field toggles in the interlace forms, and the frame shows.
            self.vip = true;
            self.lsm_latched = (self.regs[0x0C] >> 1) & 3;
            if self.lsm() != 0 {
                self.odd = !self.odd;
            }
            self.publish_frame();
            ctx.emit(TriggerEvent::Vblank);
        }
        // The elastic FIFO drains at the line visit.
        self.fifo_count = 0;
    }

    /// The interrupt and hold line edges at the end of one visit: a change
    /// in the wanted level notifies one edge per line.
    fn deliver_lines(&mut self, tick: Tick, ctx: &mut MachineCtx) {
        let ie0 = self.regs[0x01] & 0x20 != 0;
        let int6 = self.vip && ie0;
        let int4 = self.hint_pending && self.regs[0x00] & 0x10 != 0;
        let zint = self.vblank_flag_line(self.line);
        let hold = self.dma_form.running();
        if int6 != self.int6_driven {
            self.int6_driven = int6;
            ctx.notify("int6", if int6 { Level::High } else { Level::Low }, tick);
        }
        if int4 != self.int4_driven {
            self.int4_driven = int4;
            ctx.notify("int4", if int4 { Level::High } else { Level::Low }, tick);
        }
        if zint != self.zint_driven {
            self.zint_driven = zint;
            ctx.notify("zint", if zint { Level::High } else { Level::Low }, tick);
        }
        if hold != self.hold_driven {
            self.hold_driven = hold;
            ctx.notify("hold", if hold { Level::High } else { Level::Low }, tick);
        }
    }

    /// The frame publish at the vblank start: the canvas into the cache.
    fn publish_frame(&mut self) {
        self.frame.pixels = self.screen.clone();
        self.frames += 1;
    }

    /// The visit of one scanline: the DMA budget, the line events, the
    /// render, and the line advance.
    fn visit_line(&mut self, tick: Tick, ctx: &mut MachineCtx) {
        self.visit_tick = tick.0;
        self.ensure_canvas();
        self.step_line_events(tick, ctx);
        self.render_line();
        self.line += 1;
        if self.line >= self.total_lines() {
            self.line = 0;
        }
        self.deliver_lines(tick, ctx);
    }

    // -- The render engine --------------------------------------------------

    /// A VRAM word at a byte address, big-endian per the 68k view.
    fn vword(&self, byte: usize) -> u16 {
        let i = byte & VRAM_MASK;
        u16::from(self.vram[i]) << 8 | u16::from(self.vram[(i + 1) & VRAM_MASK])
    }

    /// The 4bpp pixel nibble of one normal-mode tile: 32 bytes per tile,
    /// the left pixel in the high nibble of each byte pair.
    fn tile_nibble(&self, index: u16, row: u32, col: u32) -> u8 {
        let base = (usize::from(index & 0x7FF) * 32) & VRAM_MASK;
        let byte = self.vram[(base + (row as usize) * 4 + (col as usize >> 1)) & VRAM_MASK];
        if col & 1 == 0 { byte >> 4 } else { byte & 0x0F }
    }

    /// The 4bpp pixel nibble of one double-res tile: 64 bytes over the
    /// 1024 name pairs (the name bit zero ignored, per MacDonald).
    fn tile_nibble_double(&self, index: u16, row: u32, col: u32) -> u8 {
        let base = ((usize::from(index & 0x7FF) >> 1) * 64) & VRAM_MASK;
        let byte = self.vram[(base + (row as usize) * 4 + (col as usize >> 1)) & VRAM_MASK];
        if col & 1 == 0 { byte >> 4 } else { byte & 0x0F }
    }

    /// The plane name table base of the plane form in the registers
    /// (the emmd register formulas).
    fn plane_base(&self, plane: u8) -> usize {
        match plane {
            0 => (usize::from(self.regs[0x02] & 0x38)) << 10,
            1 => (usize::from(self.regs[0x04] & 0x07)) << 13,
            _ => {
                // The window: `$800` aligned in H32, `$1000` in H40.
                if self.h40() {
                    (usize::from(self.regs[0x03] & 0x3C)) << 10
                } else {
                    (usize::from(self.regs[0x03] & 0x3E)) << 10
                }
            }
        }
    }

    /// The h-scroll values of plane A and B for one line.
    fn hscroll_values(&self, line: u64) -> (u32, u32) {
        let base = usize::from(self.regs[0x0D] & 0x3F) << 10;
        let l = line & 0xFF;
        let entry = match self.regs[0x0B] & 3 {
            0 => 0,
            1 => l & 7,
            2 => l & !7,
            _ => l,
        };
        let byte = base + (entry as usize) * 4;
        (
            u32::from(self.vword(byte) & 0x03FF),
            u32::from(self.vword(byte + 2) & 0x03FF),
        )
    }

    /// The vertical scroll value of one plane for column `col`.
    fn vscroll_value(&self, col: usize, plane: u8) -> u32 {
        if self.regs[0x0B] & 0x04 != 0 {
            let index = (col * 2 + usize::from(plane)) & 0x3F;
            u32::from(self.vsram[index])
        } else {
            u32::from(self.vsram[usize::from(plane)])
        }
    }

    /// The map entry word of one cell, over the row and column masks of
    /// the plane size form (both planes share the form; entries wrap on
    /// the 8-KiB name table).
    fn map_word(&self, plane: u8, v_line: u64, plane_x: u32, cells: u32) -> u16 {
        let vsz = PLANE_CELLS[((self.regs[0x10] >> 4) & 3) as usize];
        let row = ((v_line >> 3) as u32) & (vsz - 1);
        let col = (plane_x >> 3) & (cells - 1);
        let base = self.plane_base(plane);
        self.vword(base + ((row * cells + col) as usize & 0x0FFF) * 2)
    }

    /// The window's horizontal range per the union rule, or `None` when
    /// plane A runs the whole line.
    fn window_range(&self, line: u64) -> Option<(usize, usize)> {
        let width = if self.h40() { 320 } else { 256 };
        let hp = self.regs[0x11] & 0x1F;
        let rigt = self.regs[0x11] & 0x80 != 0;
        let a = (u64::from(self.regs[0x12] & 0x1F)) << 3;
        let down = self.regs[0x12] & 0x80 != 0;
        // The vertical range covers whole lines inside the boundary.
        let whole_line = if down { line >= a } else { line < a };
        if hp == 0 && !rigt && !whole_line {
            return None;
        }
        if hp == 0 {
            // WHP zero with the vertical rule, or RIGT set: the whole
            // line is the window (MacDonald's boundary rules).
            return Some((0, width));
        }
        let hp = usize::from(hp) * 16;
        if whole_line {
            Some((0, width))
        } else if rigt {
            Some((hp, width))
        } else {
            Some((0, hp))
        }
    }

    /// The packed layer pixel of one plane pixel: color, palette, priority.
    fn plane_pixel(&self, entry: u16, v_line: u64, plane_x: u32) -> u8 {
        let pri = entry & 0x8000 != 0;
        let pal = ((entry >> 13) & 3) as u8;
        let hflip = entry & 0x0800 != 0;
        let vflip = entry & 0x1000 != 0;
        let index = entry & 0x7FF;
        let row = v_line & 7;
        let row = if vflip { 7 ^ row } else { row };
        let col = plane_x & 7;
        let col = if hflip { 7 ^ col } else { col };
        let color = self.tile_nibble(index, row as u32, col);
        color | (pal << 4) | (u8::from(pri) << 6)
    }

    /// Render one active line into the canvas.
    fn render_line(&mut self) {
        let line = self.line;
        if line >= self.vblank_line() {
            return;
        }
        let (width, height) = self.frame_dims();
        let row = match self.lsm() {
            3 => line as usize * 2 + usize::from(self.odd),
            _ => line as usize,
        };
        if row >= height {
            return;
        }
        if !self.display_on() {
            // The blanked lines fill with the backdrop color.
            let backdrop = usize::from(self.regs[0x07] & 0x3F);
            let rgb = self.cram_rgb(backdrop, 0);
            let base = row * width * 3;
            for x in 0..width {
                self.screen[base + x * 3..base + x * 3 + 3].copy_from_slice(&rgb);
            }
            return;
        }
        self.render_planes(line);
        self.render_sprites(line);
        self.compose_line(row, width);
    }

    /// Fill the plane A and B layer rows for one line (packed color,
    /// palette in bits 4-5, priority in bit 6).
    fn render_planes(&mut self, line: u64) {
        let width = if self.h40() { 320 } else { 256 };
        let cells = PLANE_CELLS[(self.regs[0x10] & 3) as usize];
        let rows = PLANE_CELLS[((self.regs[0x10] >> 4) & 3) as usize];
        let row_mask = rows * 8 - 1;
        let plane_mask = cells * 8 - 1;
        let (ha, hb) = self.hscroll_values(line);
        let win = self.window_range(line);
        let win_base = self.plane_base(2);
        let win_cols = if self.h40() { 64 } else { 32 } as usize;
        for x in 0..width {
            let plane_x = (x as u32).wrapping_sub(ha) & plane_mask;
            let plane_x_b = (x as u32).wrapping_sub(hb) & plane_mask;
            // Plane A (or the window on its range; the planes never
            // overlap, MacDonald's rule). The window map walks straight
            // over the fixed 32-or-64 cell columns.
            let in_win = win.is_some_and(|(a, b)| x >= a && x < b);
            let v_line_a = (line + u64::from(self.vscroll_value(x / 16, 0))) & u64::from(row_mask);
            self.row_a[x] = if in_win {
                // The window map: no scroll, the cell column counts straight.
                let col = x >> 3;
                let row = (line >> 3) & 0x1F;
                let entry = self.vword(win_base + row as usize * win_cols * 2 + col * 2);
                self.plane_pixel(entry, line, x as u32)
            } else {
                let word = self.map_word(0, v_line_a, plane_x, cells);
                self.plane_pixel(word, v_line_a, plane_x)
            };
            // Plane B.
            let v_line_b = (line + u64::from(self.vscroll_value(x / 16, 1))) & u64::from(row_mask);
            let word_b = self.map_word(1, v_line_b, plane_x_b, cells);
            self.row_b[x] = self.plane_pixel(word_b, v_line_b, plane_x_b);
        }
    }

    /// Parse the sprite walk of one line into the sprite list.
    fn parse_sprites(&mut self, line: u64) {
        self.sprites.clear();
        let base = self.sat_base();
        let h40 = self.h40();
        let max_entries = if h40 { Self::SPRITE_WALK } else { 64 };
        let max_line: u32 = if h40 { 20 } else { 16 };
        let max_px: u32 = if h40 { 320 } else { 256 };
        let double = self.lsm() == 3;
        let height_mult: u32 = if double { 16 } else { 8 };
        let ymask: u32 = if double { 0x3FF } else { 0x1FF };
        let yoff: usize = if double { 256 } else { 128 };
        let scan = if double { line * 2 } else { line };
        let test_line = (((scan + u64::from(self.odd)) + yoff as u64) & u64::from(ymask)) as u32;
        let mut count: u32 = 0;
        let mut pixels: u32 = 0;
        let mut seen_nonzero_x = false;
        let mut index = 0usize;
        for _ in 0..max_entries {
            let at = (base + index * 8) & VRAM_MASK;
            let raw_y = u32::from(self.vword(at) & 0x03FF);
            let size = self.vram[(at + 2) & VRAM_MASK];
            let link = usize::from(self.vram[(at + 3) & VRAM_MASK]) & 0x7F;
            let hc = u32::from(size & 3) + 1;
            let wc = u32::from((size >> 2) & 3) + 1;
            let attr = self.vword((at + 4) & VRAM_MASK);
            let raw_x = self.vword((at + 6) & VRAM_MASK) & 0x01FF;
            let y = raw_y & ymask;
            let visible = y <= test_line && test_line < y + hc * height_mult;
            if visible {
                if seen_nonzero_x && raw_x == 0 {
                    // The raw-X-zero mask: the rest of the line hides
                    // without a flag (Kabuto's rule).
                    return;
                }
                let width = wc * height_mult;
                if count >= max_line || pixels + width > max_px {
                    // The per-line sprite and pixel caps raise the
                    // overflow flag and stop parsing (MacDonald).
                    self.sov = true;
                    return;
                }
                count += 1;
                pixels += width;
                self.sprites.push(LineSprite {
                    left: i32::from(raw_x) - 128,
                    top: i64::from(raw_y) - 128,
                    tiles: attr & 0x7FF,
                    wc: wc as u8,
                    hc: hc as u8,
                    pal: ((attr >> 13) & 3) as u8,
                    pri: attr & 0x8000 != 0,
                    hf: attr & 0x0800 != 0,
                    vf: attr & 0x1000 != 0,
                });
            }
            if visible && raw_x != 0 {
                seen_nonzero_x = true;
            }
            if link == 0 {
                return;
            }
            index = link;
            if index >= max_entries {
                return;
            }
        }
    }

    /// The sprite table base for the current width mode.
    fn sat_base(&self) -> usize {
        if self.h40() {
            usize::from(self.regs[0x05]) << 9 & 0xFC00
        } else {
            usize::from(self.regs[0x05]) << 9 & 0xFE00
        }
    }

    /// Draw the parsed sprites into the sprite layer row.
    fn render_sprites(&mut self, line: u64) {
        self.row_s.clear();
        self.row_s.resize(self.row_a.len(), 0);
        self.parse_sprites(line);
        let width = if self.h40() { 320 } else { 256 };
        let double = self.lsm() == 3;
        let sprite_list = &self.sprites;
        for sprite in sprite_list {
            let height_mult: u32 = if double { 16 } else { 8 };
            let height = u32::from(sprite.hc) * height_mult;
            let rel_line = (line as i64 - sprite.top) & 0x1F;
            let rel = if sprite.vf {
                (height - 1) - (rel_line as u32 & 0x1F) % height
            } else {
                (rel_line as u32) & (height - 1)
            };
            let row_cell = rel / height_mult;
            let row_px = rel % height_mult;
            for col in 0..(u32::from(sprite.wc) * 8) {
                let sx = sprite.left + col as i32;
                if sx < 0 || sx >= width {
                    continue;
                }
                let rel_col = if sprite.hf {
                    u32::from(sprite.wc) * 8 - 1 - col
                } else {
                    col
                };
                let col_cell = rel_col / 8;
                let col_px = rel_col % 8;
                let tile = sprite.tiles + (col_cell * u32::from(sprite.hc) + row_cell) as u16;
                let color = if double {
                    self.tile_nibble_double(tile, row_px, col_px)
                } else {
                    self.tile_nibble(tile, row_px, col_px)
                };
                if color == 0 {
                    continue;
                }
                let x = sx as usize;
                let px = color | (sprite.pal << 4) | (if sprite.pri { 0x40 } else { 0 });
                if self.row_s[x] & 0x0F != 0 {
                    // Overlapping opaque pixels raise the collision flag;
                    // the earlier sprite keeps the slot (Kabuto's rule).
                    self.scl = true;
                    continue;
                }
                self.row_s[x] = px;
            }
        }
    }

    /// Compose the layered row into the canvas with the priority ladder
    /// and the shadow/highlight intensity.
    fn compose_line(&mut self, row: usize, width: usize) {
        let sh = self.shadow_mode();
        let backdrop = usize::from(self.regs[0x07] & 0x3F);
        let base = row * width * 3;
        for x in 0..width {
            let a = self.row_a[x];
            let b = self.row_b[x];
            let s = self.row_s[x];
            let a_opaque = a & 0x0F != 0;
            let a_pri = a & 0x40 != 0;
            let b_opaque = b & 0x0F != 0;
            let b_pri = b & 0x40 != 0;
            // The plane ladder: plane A fronts when its priority is set or
            // plane B's is clear; a transparent winner shows the other
            // plane, and transparency falls to the backdrop.
            let (mut px, pri, opaque) = if a_opaque && (a_pri || !b_pri) {
                (a & 0x3F, a_pri, true)
            } else if b_opaque {
                (b & 0x3F, b_pri, true)
            } else if a_opaque {
                (a & 0x3F, a_pri, true)
            } else {
                (backdrop as u8, false, false)
            };
            // The intensity: any plane priority lifts to normal.
            let lifted = a_pri || b_pri;
            let mut level: u8 = if sh { u8::from(lifted) } else { 1 };
            if s & 0x0F != 0 {
                let s_pri = s & 0x40 != 0;
                if s_pri || !(pri && opaque) {
                    let sw = s & 0x3F;
                    if sh && sw == 0x3E {
                        // The highlight operator: one step up.
                        level = (level + 1).min(2);
                    } else if sh && sw == 0x3F {
                        // The shadow operator: force half.
                        level = 0;
                    } else {
                        px = s & 0x3F;
                        if sh && s & 0x0F == 0x0E {
                            // Colors 0E/1E/2E render normal regardless.
                            level = 1;
                        } else if sh {
                            level = level.max(u8::from(s_pri));
                        }
                    }
                }
            }
            let rgb = self.cram_rgb(usize::from(px & 0x3F), level);
            let at = base + x * 3;
            self.screen[at] = rgb[0];
            self.screen[at + 1] = rgb[1];
            self.screen[at + 2] = rgb[2];
        }
        // The leftmost-8-pixel blank of register `$00` bit 5.
        if self.regs[0x00] & 0x20 != 0 {
            let rgb = self.cram_rgb(backdrop, if sh { 0 } else { 1 });
            for x in 0..width.min(8) {
                let at = base + x * 3;
                self.screen[at] = rgb[0];
                self.screen[at + 1] = rgb[1];
                self.screen[at + 2] = rgb[2];
            }
        }
    }

    /// The RGB888 bytes of one CRAM entry at the intensity level (0
    /// shadow, 1 normal, 2 highlight).
    fn cram_rgb(&self, entry: usize, level: u8) -> [u8; 3] {
        let v = self.cram[entry & 63];
        let channels = [v & 7, (v >> 3) & 7, (v >> 6) & 7];
        channels.map(|c| {
            let base = (c << 5) | (c << 2) | (c >> 1);
            match level {
                0 => (base >> 1) as u8,
                1 => base as u8,
                _ => (base + 128).min(255) as u8,
            }
        })
    }

    // -- Snapshots ----------------------------------------------------------

    /// The snapshot byte layout, shared by the write and the restore.
    fn snapshot_len(&self) -> usize {
        4 + 1
            + VRAM_SIZE
            + CRAM_BYTES
            + 64 * 2
            + REGS
            + 2
            + 2
            + 1
            + 1
            + 2
            + 1
            + 1
            + 1
            + 1
            + 4
            + 4
            + 2
            + 8
            + 8
            + 1
            + 1
            + 1
            + 1
            + 3
            + 8
            + self.screen.len()
    }

    /// The little-endian snapshot helpers.
    fn le_bytes(data: &[u8], at: usize, len: usize) -> Result<[u8; 8], SnapshotError> {
        let tail = data.get(at..at + len).ok_or_else(|| {
            SnapshotError::Mismatch("the vdp chunk ends inside a field".to_string())
        })?;
        let mut out = [0u8; 8];
        out[..len].copy_from_slice(tail);
        Ok(out)
    }

    fn le_u16(data: &[u8], at: usize) -> Result<u16, SnapshotError> {
        let raw = Self::le_bytes(data, at, 2)?;
        Ok(u16::from_le_bytes([raw[0], raw[1]]))
    }

    fn le_u32(data: &[u8], at: usize) -> Result<u32, SnapshotError> {
        let raw = Self::le_bytes(data, at, 4)?;
        Ok(u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]))
    }

    fn le_u64(data: &[u8], at: usize) -> Result<u64, SnapshotError> {
        let raw = Self::le_bytes(data, at, 8)?;
        Ok(u64::from_le_bytes(raw))
    }
}

impl Device for GenesisVdp {
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
        self.cram = [0; 64];
        self.vsram = [0; 64];
        self.regs = [0; REGS];
        self.regs[0x0A] = 0xFF;
        self.addr = 0;
        self.addr_latch = 0;
        self.code = 0;
        self.pending = false;
        self.read_buf = 0;
        self.read_fetched = false;
        self.reg_read_reg = None;
        self.fifo_count = 0;
        self.dma_form = DmaForm::Off;
        self.dma_remaining = 0;
        self.dma_src = 0;
        self.dma_fill_data = 0;
        self.line = 0;
        self.visit_tick = 0;
        self.hint_counter = 0xFF;
        self.hint_pending = false;
        self.vip = false;
        self.sov = false;
        self.scl = false;
        self.odd = false;
        self.lsm_latched = 0;
        self.hv_latch = None;
        self.int6_driven = false;
        self.int4_driven = false;
        self.zint_driven = false;
        self.hold_driven = false;
        self.frames = 0;
        self.next = None;
        self.last_tick = None;
    }

    fn boot(&mut self, state: Option<&BootState>) {
        let _ = state;
        self.reset();
        self.next = Some(Tick(1));
    }

    /// The snapshot chunk: `MDVD`, the version, and every mutated field.
    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(self.snapshot_len());
        out.extend_from_slice(TAG);
        out.push(VERSION);
        out.extend_from_slice(&self.vram);
        for word in &self.cram {
            out.extend_from_slice(&word.to_le_bytes());
        }
        for word in &self.vsram {
            out.extend_from_slice(&word.to_le_bytes());
        }
        out.extend_from_slice(&self.regs);
        out.extend_from_slice(&self.addr.to_le_bytes());
        out.extend_from_slice(&self.addr_latch.to_le_bytes());
        out.push(self.code);
        out.push(u8::from(self.pending));
        out.extend_from_slice(&self.read_buf.to_le_bytes());
        out.push(u8::from(self.read_fetched));
        out.push(self.reg_read_reg.unwrap_or(0xFF));
        out.push(self.fifo_count);
        out.push(match self.dma_form {
            DmaForm::Off => 0,
            DmaForm::Transfer => 1,
            DmaForm::FillArmed => 2,
            DmaForm::Fill => 3,
            DmaForm::Copy => 4,
        });
        out.extend_from_slice(&self.dma_remaining.to_le_bytes());
        out.extend_from_slice(&self.dma_src.to_le_bytes());
        out.extend_from_slice(&self.dma_fill_data.to_le_bytes());
        out.extend_from_slice(&self.line.to_le_bytes());
        out.extend_from_slice(&self.visit_tick.to_le_bytes());
        out.push(self.hint_counter);
        out.push(u8::from(self.hint_pending));
        let mut flags = 0u8;
        if self.vip {
            flags |= 0x01;
        }
        if self.sov {
            flags |= 0x02;
        }
        if self.scl {
            flags |= 0x04;
        }
        if self.odd {
            flags |= 0x08;
        }
        if self.int6_driven {
            flags |= 0x10;
        }
        if self.int4_driven {
            flags |= 0x20;
        }
        if self.zint_driven {
            flags |= 0x40;
        }
        if self.hold_driven {
            flags |= 0x80;
        }
        out.push(flags);
        out.push(self.lsm_latched);
        out.push(u8::from(self.hv_latch.is_some()));
        out.extend_from_slice(&self.hv_latch.unwrap_or(0).to_le_bytes());
        out.extend_from_slice(&self.frames.to_le_bytes());
        out.extend_from_slice(&self.screen);
        out
    }

    /// Read a snapshot written by the same device version.
    ///
    /// # Errors
    ///
    /// Returns an error when the chunk does not match this device's
    /// expectations.
    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        let fixed = self.snapshot_len() - self.screen.len();
        if data.len() < fixed + 3 || data[..4] != *TAG || data[4] != VERSION {
            return Err(SnapshotError::Mismatch(format!(
                "the vdp chunk must carry {:?} with a screen of the fixed prefix {fixed} bytes",
                *TAG
            )));
        }
        let screen_len = data.len() - fixed;
        if !screen_len.is_multiple_of(3) {
            return Err(SnapshotError::Mismatch(
                "the vdp screen tail is not a multiple of the RGB888 form".to_string(),
            ));
        }
        let mut pos = 4usize;
        pos += 1;
        self.vram[..VRAM_SIZE].copy_from_slice(&data[pos..pos + VRAM_SIZE]);
        pos += VRAM_SIZE;
        for word in self.cram.iter_mut() {
            *word = Self::le_u16(data, pos)?;
            pos += 2;
        }
        for word in self.vsram.iter_mut() {
            *word = Self::le_u16(data, pos)?;
            pos += 2;
        }
        self.regs.copy_from_slice(&data[pos..pos + REGS]);
        pos += REGS;
        self.addr = Self::le_u16(data, pos)?;
        pos += 2;
        self.addr_latch = Self::le_u16(data, pos)?;
        pos += 2;
        self.code = data[pos];
        pos += 1;
        self.pending = data[pos] != 0;
        pos += 1;
        self.read_buf = Self::le_u16(data, pos)?;
        pos += 2;
        self.read_fetched = data[pos] != 0;
        pos += 1;
        self.reg_read_reg = (data[pos] != 0xFF).then_some(data[pos]);
        pos += 1;
        self.fifo_count = data[pos];
        pos += 1;
        self.dma_form = match data[pos] {
            1 => DmaForm::Transfer,
            2 => DmaForm::FillArmed,
            3 => DmaForm::Fill,
            4 => DmaForm::Copy,
            _ => DmaForm::Off,
        };
        pos += 1;
        self.dma_remaining = Self::le_u32(data, pos)?;
        pos += 4;
        self.dma_src = Self::le_u32(data, pos)?;
        pos += 4;
        self.dma_fill_data = Self::le_u16(data, pos)?;
        pos += 2;
        self.line = Self::le_u64(data, pos)?;
        pos += 8;
        self.visit_tick = Self::le_u64(data, pos)?;
        pos += 8;
        self.hint_counter = data[pos];
        pos += 1;
        self.hint_pending = data[pos] != 0;
        pos += 1;
        let flags = data[pos];
        self.vip = flags & 0x01 != 0;
        self.sov = flags & 0x02 != 0;
        self.scl = flags & 0x04 != 0;
        self.odd = flags & 0x08 != 0;
        self.int6_driven = flags & 0x10 != 0;
        self.int4_driven = flags & 0x20 != 0;
        self.zint_driven = flags & 0x40 != 0;
        self.hold_driven = flags & 0x80 != 0;
        pos += 1;
        self.lsm_latched = data[pos];
        pos += 1;
        self.hv_latch = (data[pos] == 1)
            .then(|| Self::le_u16(data, pos + 1))
            .transpose()?;
        pos += 3;
        self.frames = Self::le_u64(data, pos)?;
        pos += 8;
        self.screen = data[pos..pos + screen_len].to_vec();
        self.frame.pixels = self.screen.clone();
        if screen_len / 3 >= self.frame.width as usize {
            self.frame.height = (screen_len / 3 / self.frame.width as usize) as u32;
        }
        // The schedule re-arms from the restored visit tick so the next
        // line base lands one scanline later.
        self.last_tick = Some(Tick(self.visit_tick));
        self.next = Some(Tick(self.visit_tick + self.line_ticks()));
        Ok(())
    }

    fn notify_line(&mut self, _line: &str, _level: Level, _tick: Tick) {}

    fn next_event(&self) -> Option<Tick> {
        self.next
    }

    /// One scheduler visit: the whole scanline (the line engine of the
    /// module docs).
    fn tick(&mut self, tick: Tick, ctx: &mut MachineCtx) {
        if self.last_tick.is_some_and(|t| t.0 >= tick.0) {
            return;
        }
        self.last_tick = Some(tick);
        self.visit_line(tick, ctx);
        self.next = Some(Tick(tick.0 + self.line_ticks()));
    }
}

impl BusAccess for GenesisVdp {
    /// The data, control, and HV ports, decoded by the low address bits
    /// (any window mirror stride folds onto them). Byte forms follow the
    /// published protocol: byte writes enter the byte in both lanes and
    /// byte reads answer the even byte as the high byte of the port word.
    fn read(&mut self, width: AccessWidth, address: Address, tick: Tick) -> u32 {
        let word = match port_of(address) {
            Port::Psg => 0xFF,
            Port::Data => self.data_read(),
            Port::Control => self.control_read(tick),
            Port::Hv => self.hv_read(tick),
        };
        match width {
            AccessWidth::Byte => {
                if address.0 & 1 == 0 {
                    u32::from((word >> 8) & 0xFF)
                } else {
                    u32::from(word & 0xFF)
                }
            }
            _ => u32::from(word),
        }
    }

    fn write(&mut self, width: AccessWidth, address: Address, value: u32, _tick: Tick) {
        let lane = u16::from((value & 0xFF) as u8);
        let word = match width {
            // The byte write enters the byte in both lanes.
            AccessWidth::Byte => (lane << 8) | lane,
            AccessWidth::Word => (value & 0xFFFF) as u16,
            AccessWidth::Dword => ((value >> 16) & 0xFFFF) as u16,
        };
        match port_of(address) {
            Port::Psg => {}
            Port::Data => self.data_write(word),
            Port::Control => self.control_write(word),
            Port::Hv => {}
        }
    }

    /// The side-effect-free read for debug commands: the even port byte.
    fn peek(&self, address: Address) -> u8 {
        match port_of(address) {
            Port::Psg => 0xFF,
            Port::Data => (self.read_buf >> 8) as u8,
            Port::Control => (self.status_word(Tick(self.visit_tick)) >> 8) as u8,
            Port::Hv => self.v_display(self.line),
        }
    }

    fn decodes(&self, _address: Address) -> bool {
        true
    }
}

impl VideoSource for GenesisVdp {
    /// The composed frame: RGB888, row-major, the active area (256 or 320
    /// wide, 224 or 240 tall, doubled in the double-res interlace mode).
    fn frame(&self) -> Option<&Frame> {
        Some(&self.frame)
    }

    fn dump_layer(&self, layer: VideoLayer) -> Option<Vec<u8>> {
        match layer {
            VideoLayer::Vram => Some(self.vram[..VRAM_SIZE].to_vec()),
            VideoLayer::NameTable => {
                // The plane A name table: 8 KiB behind the `$02` register
                // base.
                let base = usize::from(self.regs[0x02]) << 10 & 0x3800;
                Some(self.vram[base..base + 0x2000].to_vec())
            }
            VideoLayer::Oam => {
                // The sprite attribute table: 80 entries of 8 bytes behind
                // the `$05` register base.
                let base = self.sat_base();
                let mut out = Vec::with_capacity(640);
                for i in 0..640 {
                    out.push(self.vram[(base + i) & VRAM_MASK]);
                }
                Some(out)
            }
            VideoLayer::Palette => {
                // The CRAM as 64 little-endian 16-bit words.
                let mut out = Vec::with_capacity(CRAM_BYTES);
                for word in &self.cram {
                    out.extend_from_slice(&word.to_le_bytes());
                }
                Some(out)
            }
            VideoLayer::Framebuffer => Some(self.screen.clone()),
        }
    }

    fn frames(&self) -> u64 {
        self.frames
    }
}

/// The port the address's low bits select, mirroring any window stride.
fn port_of(address: Address) -> Port {
    let a = address.0;
    if a & 0x10 != 0 {
        Port::Psg
    } else if a & 0x08 != 0 {
        Port::Hv
    } else if a & 0x04 != 0 {
        Port::Control
    } else {
        Port::Data
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use demu_config::model::{
        Bus, ClockDomain, Connection, ConnectionDirection, ConnectionSemantics, SystemConfig,
        Window,
    };
    use demu_config::newtypes::{DeviceName, Frequency, KindName, LinePath};
    use demu_core::machine::DeviceCtors;

    /// A configured device instance of the VDP kind.
    fn instance(name: &str, params: &[(&str, &str)]) -> DeviceInstance {
        let mut entries = std::collections::BTreeMap::new();
        for (key, value) in params {
            entries.insert(
                (*key).to_string(),
                toml::Value::String((*value).to_string()),
            );
        }
        DeviceInstance {
            name: DeviceName::new(name).unwrap(),
            kind: KindName::new("genesis.vdp").unwrap(),
            domain: "main".to_string(),
            params: entries,
            dividers: std::collections::BTreeMap::new(),
        }
    }

    /// The VDP of the default parameters.
    fn vdp() -> GenesisVdp {
        let device = instance("vdp", &[]);
        GenesisVdp::new(&device).unwrap()
    }

    /// The data address of the port form.
    fn dport(offset: u32) -> Address {
        Address(0xC00000 + offset)
    }

    /// Register write through the control port.
    fn set_reg(v: &mut GenesisVdp, reg: u8, data: u8) {
        let word = u16::from(0x80 | reg) << 8 | u16::from(data);
        v.write(AccessWidth::Word, dport(4), u32::from(word), Tick(0));
    }

    /// The data VRAM write form helper: the two-word control command.
    fn vram_write(v: &mut GenesisVdp, addr: u16, word: u16) {
        let first = 0x4000 | u32::from(addr & 0x3FFF);
        let second = 0x0004 | (u32::from(addr & 0xC000) >> 14);
        v.write(AccessWidth::Word, dport(4), first, Tick(0));
        v.write(AccessWidth::Word, dport(4), second, Tick(0));
        v.write(AccessWidth::Word, dport(0), u32::from(word), Tick(0));
    }

    /// The control-port data command form: code and address in one pair.
    fn setup_ram(v: &mut GenesisVdp, code: u8, addr: u16) {
        let first = u32::from(code & 3) << 14 | u32::from(addr & 0x3FFF);
        let second = u32::from(code >> 2) << 4 | u32::from(addr >> 14);
        v.write(AccessWidth::Word, dport(4), first, Tick(0));
        v.write(AccessWidth::Word, dport(4), second, Tick(0));
    }

    #[test]
    fn vram_write_read_round_trip() {
        let mut v = vdp();
        set_reg(&mut v, 0x0F, 2);
        vram_write(&mut v, 0x0000, 0x1234);
        vram_write(&mut v, 0x0002, 0xABCD);
        assert_eq!(&v.vram[0..4], &[0x12, 0x34, 0xAB, 0xCD]);
        // The word-sized read of the same address.
        setup_ram(&mut v, 0x00, 0x0000);
        assert_eq!(v.read(AccessWidth::Word, dport(0), Tick(0)), 0x1234);
        assert_eq!(v.read(AccessWidth::Word, dport(0), Tick(0)), 0xABCD);
    }

    #[test]
    fn byte_lanes_and_odd_swap_forms() {
        let mut v = vdp();
        set_reg(&mut v, 0x0F, 2);
        setup_ram(&mut v, 0x01, 0x0010);
        v.write(AccessWidth::Word, dport(0), 0x0102, Tick(0));
        assert_eq!(&v.vram[0x10..0x12], &[0x01, 0x02]);
        // The odd address swaps the value's bytes.
        setup_ram(&mut v, 0x01, 0x0011);
        v.write(AccessWidth::Word, dport(0), 0x0304, Tick(0));
        assert_eq!(&v.vram[0x10..0x12], &[0x04, 0x03]);
        // The byte write enters the byte in both lanes.
        setup_ram(&mut v, 0x01, 0x0014);
        v.write(AccessWidth::Byte, dport(0), 0x7E, Tick(0));
        assert_eq!(&v.vram[0x14..0x16], &[0x7E, 0x7E]);
    }

    #[test]
    fn cram_write_read_round_trip() {
        let mut v = vdp();
        setup_ram(&mut v, 0x03, 0x0000);
        v.write(AccessWidth::Word, dport(0), 0x0888, Tick(0));
        // 0x0888 packs the 9-bit form (b=1, g=1, r=1).
        assert_eq!(v.cram[0], 0x124);
        setup_ram(&mut v, 0x08, 0x0000);
        assert_eq!(v.read(AccessWidth::Word, dport(0), Tick(0)), 0x0888);
        // The address folds on the 64-entry space: $80 lands on entry 0.
        // 0x0022 carries g=1, r=1 and packs to the 9-bit 0x009.
        setup_ram(&mut v, 0x03, 0x0080);
        v.write(AccessWidth::Word, dport(0), 0x0022, Tick(0));
        assert_eq!(v.cram[0], 0x009);
    }

    #[test]
    fn vsram_write_read_and_wrap() {
        let mut v = vdp();
        set_reg(&mut v, 0x0F, 2);
        setup_ram(&mut v, 0x05, 0x0002);
        v.write(AccessWidth::Word, dport(0), 0x03FF, Tick(0));
        assert_eq!(v.vsram[1], 0x03FF);
        setup_ram(&mut v, 0x04, 0x0002);
        assert_eq!(v.read(AccessWidth::Word, dport(0), Tick(0)), 0x03FF);
        // Writes past the live entries are ignored (MacDonald).
        setup_ram(&mut v, 0x05, 0x0062);
        v.write(AccessWidth::Word, dport(0), 0x0123, Tick(0));
        assert_eq!(v.vsram[49], 0);
        // Reads past the live entries wrap to entry 0 (MacDonald/GPGX).
        setup_ram(&mut v, 0x04, 0x0062);
        assert_eq!(v.read(AccessWidth::Word, dport(0), Tick(0)), 0x0000);
    }

    #[test]
    fn read_prefetch_and_auto_increment() {
        let mut v = vdp();
        set_reg(&mut v, 0x0F, 4);
        vram_write(&mut v, 0x0100, 0xAAAA);
        vram_write(&mut v, 0x0104, 0xBBBB);
        // The command pre-fetches; the read answers the prefetched word.
        setup_ram(&mut v, 0x00, 0x0100);
        assert_eq!(v.read_buf, 0xAAAA);
        assert_eq!(v.read(AccessWidth::Word, dport(0), Tick(0)), 0xAAAA);
        assert_eq!(v.read(AccessWidth::Word, dport(0), Tick(0)), 0xBBBB);
        // The byte reads split the word by the port byte lane, and each
        // access walks the read pipeline once.
        setup_ram(&mut v, 0x00, 0x0100);
        assert_eq!(v.read(AccessWidth::Byte, dport(0), Tick(0)), 0xAA);
        assert_eq!(v.read(AccessWidth::Byte, dport(1), Tick(0)), 0xBB);
    }

    #[test]
    fn register_store_and_readback() {
        let mut v = vdp();
        for reg in 0..24u8 {
            set_reg(&mut v, reg, 0x5A | (reg & 3));
        }
        // The register write cancels a pending command and clears the code.
        let first = 0x4000 | 0x0300;
        v.write(AccessWidth::Word, dport(4), first, Tick(0));
        assert!(v.pending);
        set_reg(&mut v, 0x07, 0x3F);
        assert!(!v.pending);
        assert_eq!(v.code, 0);
        // The register readback arm answers the stored byte.
        let word = 0xC000u16;
        v.write(AccessWidth::Word, dport(4), u32::from(word), Tick(0));
        let answer = v.read(AccessWidth::Word, dport(4), Tick(0));
        assert_eq!(answer >> 8, u32::from(v.regs[0]));
        let word = (0xC000u16) | (0x17 << 8);
        v.write(AccessWidth::Word, dport(4), u32::from(word), Tick(0));
        let answer = v.read(AccessWidth::Word, dport(4), Tick(0));
        assert_eq!(answer >> 8, u32::from(v.regs[0x17]));
    }

    #[test]
    fn registers_beyond_17_store_without_effect() {
        let mut v = vdp();
        // The index $18-$1F has no register; the write stores nothing
        // and leaves register 0 untouched.
        let before = v.regs;
        set_reg(&mut v, 0x18, 0x40);
        set_reg(&mut v, 0x1F, 0x40);
        assert_eq!(v.regs, before);
    }

    // -- The mini machine ---------------------------------------------------

    /// A RAM shim for the DMA source tests.
    #[derive(Debug, Default)]
    struct TestRam {
        name: String,
        bytes: Box<[u8]>,
    }

    impl TestRam {
        fn new(device: &DeviceInstance, len: usize) -> Self {
            Self {
                name: device.name.to_string(),
                bytes: vec![0; len].into_boxed_slice(),
            }
        }
    }

    impl BusAccess for TestRam {
        fn read(&mut self, width: AccessWidth, address: Address, _tick: Tick) -> u32 {
            let i = address.0 as usize % self.bytes.len();
            match width {
                AccessWidth::Word | AccessWidth::Dword => {
                    let hi = self.bytes[i];
                    let lo = self.bytes[(i + 1) % self.bytes.len()];
                    u32::from(u16::from(hi) << 8 | u16::from(lo))
                }
                AccessWidth::Byte => u32::from(self.bytes[i]),
            }
        }

        fn write(&mut self, width: AccessWidth, address: Address, value: u32, _tick: Tick) {
            let i = address.0 as usize % self.bytes.len();
            match width {
                AccessWidth::Word | AccessWidth::Dword => {
                    self.bytes[i] = (value >> 8) as u8;
                    self.bytes[(i + 1) % self.bytes.len()] = value as u8;
                }
                AccessWidth::Byte => self.bytes[i] = value as u8,
            }
        }

        fn peek(&self, _address: Address) -> u8 {
            0
        }
    }

    impl Device for TestRam {
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

        fn reset(&mut self) {}
        fn boot(&mut self, _state: Option<&BootState>) {}
        fn snapshot(&self) -> Snapshot {
            Vec::new()
        }
        fn restore(&mut self, _data: &[u8]) -> Result<(), SnapshotError> {
            Ok(())
        }
        fn notify_line(&mut self, _line: &str, _level: Level, _tick: Tick) {}
        fn tick(&mut self, _tick: Tick, _ctx: &mut MachineCtx) {}
    }

    /// A probe that records the line edges it receives.
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

    fn mini_config(params: &[(&str, &str)]) -> SystemConfig {
        let mut clocks = std::collections::BTreeMap::new();
        clocks.insert(
            "main".to_string(),
            ClockDomain {
                name: "main".to_string(),
                frequency: Frequency::new(53_693_175, 1).unwrap(),
                origin: "test".to_string(),
            },
        );
        let vdp = instance("vdp", params);
        let ram = DeviceInstance {
            name: DeviceName::new("ram").unwrap(),
            kind: KindName::new("test.ram").unwrap(),
            domain: "main".to_string(),
            params: std::collections::BTreeMap::new(),
            dividers: std::collections::BTreeMap::new(),
        };
        let probe = DeviceInstance {
            name: DeviceName::new("probe").unwrap(),
            kind: KindName::new("test.probe").unwrap(),
            domain: "main".to_string(),
            params: std::collections::BTreeMap::new(),
            dividers: std::collections::BTreeMap::new(),
        };
        let devices = vec![vdp, ram, probe];
        let mut devices_by_name = std::collections::BTreeMap::new();
        for (i, device) in devices.iter().enumerate() {
            devices_by_name.insert(device.name.clone(), i);
        }
        let buses = vec![Bus {
            name: "cpu".to_string(),
            address_bits: 24,
            owner: None,
            parent: None,
        }];
        let windows = vec![
            Window {
                name: "vdp".to_string(),
                bus: "cpu".to_string(),
                device: DeviceName::new("vdp").unwrap(),
                base: 0xC00000,
                size: 0x20,
                kind: None,
                mirror: None,
                overlay: None,
                overlay_read_priority: None,
                overlay_write_priority: None,
                conflict: None,
            },
            Window {
                name: "ram".to_string(),
                bus: "cpu".to_string(),
                device: DeviceName::new("ram").unwrap(),
                base: 0x000000,
                size: 0x100000,
                kind: None,
                mirror: None,
                overlay: None,
                overlay_read_priority: None,
                overlay_write_priority: None,
                conflict: None,
            },
        ];
        let conns = [
            ("int6", "int6"),
            ("int4", "int4"),
            ("zint", "zint"),
            ("hold", "hold"),
        ]
        .map(|(from_line, to_line)| Connection {
            from: LinePath::new(
                DeviceName::new("vdp").unwrap(),
                demu_config::newtypes::LineName::new(from_line).unwrap(),
            ),
            to: LinePath::new(
                DeviceName::new("probe").unwrap(),
                demu_config::newtypes::LineName::new(to_line).unwrap(),
            ),
            semantics: ConnectionSemantics::Irq,
            direction: ConnectionDirection::Forward,
        });
        SystemConfig {
            name: "genesis-mini".to_string(),
            triple: "test".to_string(),
            description: "The Genesis VDP mini test machine.".to_string(),
            clocks,
            devices,
            devices_by_name,
            buses,
            windows,
            connections: conns.to_vec(),
            signals: Vec::new(),
            boot: std::collections::BTreeMap::new(),
            quirks: std::collections::BTreeMap::new(),
        }
    }

    fn ctors() -> DeviceCtors {
        let mut ctors = DeviceCtors::new();
        ctors.register(
            "genesis.vdp",
            Box::new(|d, _| {
                Ok(
                    Box::new(
                        GenesisVdp::new(d).map_err(demu_core::machine::MachineError::Assembly)?,
                    ) as Box<dyn Device>,
                )
            }),
        );
        ctors.register(
            "test.ram",
            Box::new(|d, _| Ok(Box::new(TestRam::new(d, 0x20_0000)) as Box<dyn Device>)),
        );
        ctors.register(
            "test.probe",
            Box::new(|_, _| Ok(Box::new(IrqProbe::default()) as Box<dyn Device>)),
        );
        ctors
    }

    fn machine_with(params: &[(&str, &str)]) -> demu_core::Machine {
        let config = mini_config(params);
        demu_core::Machine::assemble(config, &ctors()).expect("assemble")
    }

    /// The VDP's vslot down in a machine.
    fn write_port(machine: &mut demu_core::Machine, offset: u32, value: u32, tick: Tick) {
        let slot = machine.device_slot("vdp").expect("vdp slot");
        let vdp = machine.devices[slot]
            .as_any_mut()
            .downcast_mut::<GenesisVdp>()
            .expect("the vdp");
        vdp.write(AccessWidth::Word, dport(offset), value, tick);
    }

    fn read_port(machine: &mut demu_core::Machine, offset: u32, tick: Tick) -> u32 {
        let slot = machine.device_slot("vdp").expect("vdp slot");
        let vdp = machine.devices[slot]
            .as_any_mut()
            .downcast_mut::<GenesisVdp>()
            .expect("the vdp");
        vdp.read(AccessWidth::Word, dport(offset), tick)
    }

    #[test]
    fn transfer_dma_moves_planted_words() {
        let mut machine = machine_with(&[]);
        machine.boot();
        // The planted source words at $10000.
        {
            let slot = machine.device_slot("ram").unwrap();
            let ram = machine.devices[slot]
                .as_any_mut()
                .downcast_mut::<TestRam>()
                .unwrap();
            let data: [u8; 8] = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88];
            ram.bytes[0x1_0000..0x1_0008].copy_from_slice(&data);
        }
        let vdp = machine_as_vdp(&mut machine);
        set_reg(vdp, 0x0F, 2);
        set_reg(vdp, 0x01, 0x10); // the DMA enable (M1)
        write_port(&mut machine, 4, 0x9304, Tick(0)); // the length = 4 words
        write_port(&mut machine, 4, 0x9500, Tick(0)); // the source low = $10000 >> 1 low byte
        write_port(&mut machine, 4, 0x9680, Tick(0)); // the source mid = $10000 >> 9
        write_port(&mut machine, 4, 0x9700, Tick(0)); // the form transfer, bank 0
        // The command: VRAM write at $0200, CD5 set.
        write_port(&mut machine, 4, 0x4000 | 0x0200, Tick(0));
        write_port(&mut machine, 4, 0x0080, Tick(0));
        // The transfer completes within the first display-off line.
        machine.run_until(Tick(3421));
        let slot = machine.device_slot("vdp").unwrap();
        let vdp = machine.devices[slot]
            .as_any()
            .downcast_ref::<GenesisVdp>()
            .unwrap();
        assert_eq!(
            &vdp.vram[0x0200..0x0208],
            &[0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88]
        );
        assert_eq!(vdp.dma_form, DmaForm::Off);
        // The length registers zeroed and the source walked by four words.
        assert_eq!(vdp.regs[0x15], 4);
    }

    #[test]
    fn fill_dma_writes_high_byte_form() {
        let mut machine = machine_with(&[]);
        machine.boot();
        let vdp = machine_as_vdp(&mut machine);
        set_reg(vdp, 0x0F, 2);
        set_reg(vdp, 0x01, 0x10); // the DMA enable
        write_port(&mut machine, 4, 0x9308, Tick(0)); // the length = 8 bytes
        write_port(&mut machine, 4, 0x9780, Tick(0)); // the fill form
        // The command: VRAM write at $0400 (the address's high bits clear).
        write_port(&mut machine, 4, 0x4000 | 0x0400, Tick(0));
        write_port(&mut machine, 4, 0x0080, Tick(0)); // CD5 set
        // The trigger write (both lanes carry the byte form) stores the
        // word normally.
        write_port(&mut machine, 0, 0xA5A5, Tick(0));
        machine.run_until(Tick(3421));
        let slot = machine.device_slot("vdp").unwrap();
        let vdp = machine.devices[slot]
            .as_any()
            .downcast_ref::<GenesisVdp>()
            .unwrap();
        // The trigger word at $0400/$0401 and the fill's high byte walks
        // the opposite lane of each stepped address.
        assert_eq!(vdp.vram[0x0400], 0xA5);
        assert_eq!(vdp.vram[0x0401], 0xA5);
        let mut at = 0x0402usize;
        for _ in 0..7 {
            assert_eq!(vdp.vram[at ^ 1], 0xA5, "fill byte at {}", at ^ 1);
            at += 2;
        }
        assert_eq!(vdp.dma_form, DmaForm::Off);
    }

    fn machine_as_vdp(machine: &mut demu_core::Machine) -> &mut GenesisVdp {
        let slot = machine.device_slot("vdp").unwrap();
        machine.devices[slot]
            .as_any_mut()
            .downcast_mut::<GenesisVdp>()
            .unwrap()
    }

    #[test]
    fn copy_dma_moves_bytes() {
        let mut machine = machine_with(&[]);
        machine.boot();
        let vdp = machine_as_vdp(&mut machine);
        set_reg(vdp, 0x0F, 2);
        set_reg(vdp, 0x01, 0x10); // the DMA enable
        // The planted VRAM source bytes (the copy reads the flipped lane).
        vdp.vram[0x31] = 0xCA;
        vdp.vram[0x32] = 0xFE;
        vdp.vram[0x33] = 0x42;
        vdp.vram[0x34] = 0x33;
        write_port(&mut machine, 4, 0x9304, Tick(0)); // 4 bytes
        write_port(&mut machine, 4, 0x9530, Tick(0)); // src low = $30
        write_port(&mut machine, 4, 0x9600, Tick(0)); // src high = $00
        write_port(&mut machine, 4, 0x97C0, Tick(0)); // copy form
        // The command: CD1-CD0 = 00 with CD4+CD5 = the copy's form.
        write_port(&mut machine, 4, 0x0500, Tick(0));
        write_port(&mut machine, 4, 0x00C0, Tick(0));
        machine.run_until(Tick(3421));
        let vdp = machine_as_vdp(&mut machine);
        // Per the flipped-lane source-and-dest form with one address step
        // per byte: the reads alternate lanes ($31, $30, $33, $32).
        assert_eq!(vdp.vram[0x0501], 0xCA);
        assert_eq!(vdp.vram[0x0503], 0x00);
        assert_eq!(vdp.vram[0x0505], 0x42);
        assert_eq!(vdp.vram[0x0507], 0xFE);
        assert_eq!(vdp.dma_form, DmaForm::Off);
        assert_eq!(vdp.regs[0x15], 0x34);
    }

    #[test]
    fn transfer_holds_the_bus_and_releases() {
        let mut machine = machine_with(&[]);
        machine.boot();
        let vdp = machine_as_vdp(&mut machine);
        set_reg(vdp, 0x0F, 2);
        set_reg(vdp, 0x01, 0x10); // the DMA enable (M1)
        write_port(&mut machine, 4, 0x9300, Tick(0)); // length 0 = $10000 words
        write_port(&mut machine, 4, 0x9700, Tick(0));
        write_port(&mut machine, 4, 0x4000 | 0x0600, Tick(0));
        write_port(&mut machine, 4, 0x0080, Tick(0));
        // The first visits move 83 words (166 of the 167-byte budget:
        // whole words) per display-off H32 line.
        machine.run_until(Tick(3421));
        let vdp = machine_as_vdp(&mut machine);
        assert_eq!(vdp.dma_remaining, 0x2_0000 - 332);
        assert!(vdp.dma_form.running(), "the transfer runs after the visit");
        assert!(vdp.hold_driven, "the hold asserted");
        // The whole $10000-word transfer spans 789 more visits at 166
        // bytes per display-off H32 line; run with margin.
        let lines = 0x2_0000u64 / 166 + 4;
        machine.run_until(Tick(1 + 3420 * lines));
        let vdp = machine_as_vdp(&mut machine);
        assert!(!vdp.dma_form.running());
        assert!(!vdp.hold_driven, "the hold released");
        assert_eq!(vdp.regs[0x13], 0);
        assert_eq!(vdp.regs[0x14], 0);
    }

    #[test]
    fn fill_budget_per_line() {
        let mut machine = machine_with(&[]);
        machine.boot();
        let vdp = machine_as_vdp(&mut machine);
        set_reg(vdp, 0x0F, 2);
        set_reg(vdp, 0x01, 0x50); // the display on + the DMA enable
        // A fill of 300 bytes moves 15 bytes per active H32 line.
        write_port(&mut machine, 4, 0x9300 + (300 & 0xFF), Tick(0));
        write_port(&mut machine, 4, 0x9400 + (300 >> 8), Tick(0));
        write_port(&mut machine, 4, 0x9780, Tick(0));
        write_port(&mut machine, 4, 0x4000 | 0x0700, Tick(0));
        write_port(&mut machine, 4, 0x0080, Tick(0));
        write_port(&mut machine, 0, 0x5555, Tick(0));
        machine.run_until(Tick(2_000)); // one visit only (the visit at tick 1)
        let vdp = machine_as_vdp(&mut machine);
        assert_eq!(vdp.dma_remaining, 300 - 15);
        assert_eq!(vdp.dma_form, DmaForm::Fill);
    }

    // -- The plane composition -----------------------------------------------

    fn plant_tile(v: &mut GenesisVdp, tile: u16, pixel: u8) {
        // One pixel of color at the tile's row 0 col 0.
        v.vram[usize::from(tile) * 32] = pixel << 4;
    }

    fn plant_word(v: &mut GenesisVdp, addr: usize, word: u16) {
        v.vram[addr] = (word >> 8) as u8;
        v.vram[addr + 1] = (word & 0xFF) as u8;
    }

    /// The gray value of a packed CRAM entry with equal channels.
    fn gray(c: u16) -> [u8; 3] {
        let base = (c << 5) | (c << 2) | (c >> 1);
        let v = base as u8;
        [v, v, v]
    }

    #[test]
    fn plane_tile_composes_at_known_position() {
        let mut v = vdp();
        set_reg(&mut v, 0x01, 0x40); // display on
        set_reg(&mut v, 0x0F, 2);
        set_reg(&mut v, 0x02, 0x08); // plane A at $2000
        plant_tile(&mut v, 1, 0x01);
        v.cram[1] = 0x049; // gray channel 1
        // The map entry 0 = tile 1, palette 0.
        plant_word(&mut v, 0x2000, 0x0001);
        v.render_line();
        assert_eq!(&v.screen[0..3], &[36, 36, 36]);
        // Col 1 stays the backdrop (transparent tile pixel).
        assert_eq!(&v.screen[3..6], &[0, 0, 0]);
    }

    #[test]
    fn plane_priority_layers_over_each_other() {
        let mut v = vdp();
        set_reg(&mut v, 0x01, 0x40);
        set_reg(&mut v, 0x02, 0x08); // plane A at $2000
        set_reg(&mut v, 0x04, 0x02); // plane B at $4000
        plant_tile(&mut v, 1, 0x01); // plane A color 1
        plant_tile(&mut v, 2, 0x02); // plane B color 2
        v.cram[1] = 0x049;
        v.cram[2] = 0x092;
        plant_word(&mut v, 0x2000, 0x0001);
        plant_word(&mut v, 0x4000, 0x0002);
        v.render_line();
        let read_px = {
            let screen = v.screen.clone();
            move |x: usize| -> [u8; 3] {
                let at = x * 3;
                [screen[at], screen[at + 1], screen[at + 2]]
            }
        };
        // Plane A low and plane B low: plane A fronts.
        assert_eq!(read_px(0), gray(1));
        // High B over low A shows B's pixel.
        plant_word(&mut v, 0x4000, 0x8002);
        v.render_line();
        let read_px = {
            let screen = v.screen.clone();
            move |x: usize| -> [u8; 3] {
                let at = x * 3;
                [screen[at], screen[at + 1], screen[at + 2]]
            }
        };
        assert_eq!(read_px(0), gray(2));
        // A high-priority transparent plane A still fronts B (the ladder:
        // A fronts when its priority is set) but shows B's pixel.
        plant_word(&mut v, 0x2000, 0x8000);
        v.render_line();
        let read_px = {
            let screen = v.screen.clone();
            move |x: usize| -> [u8; 3] {
                let at = x * 3;
                [screen[at], screen[at + 1], screen[at + 2]]
            }
        };
        assert_eq!(read_px(0), gray(2));
    }

    #[test]
    fn backdrop_shows_through_transparent_planes() {
        let mut v = vdp();
        set_reg(&mut v, 0x01, 0x40);
        set_reg(&mut v, 0x07, 0x33); // palette 3 color 3
        v.cram[0x33] = 0x92; // gray channel 2
        v.render_line();
        assert_eq!(&v.screen[5 * 3..5 * 3 + 3], &[73, 73, 73]);
    }

    #[test]
    fn hscroll_wraps_and_shifts() {
        let mut v = vdp();
        set_reg(&mut v, 0x01, 0x40);
        set_reg(&mut v, 0x02, 0x08); // plane A at $2000
        set_reg(&mut v, 0x0D, 0x00); // the h-scroll table at $0000
        set_reg(&mut v, 0x0F, 2);
        plant_tile(&mut v, 1, 0x01);
        v.cram[1] = 0x049;
        // The h-scroll table: plane A scroll 8 at entry 0.
        plant_word(&mut v, 0x0000, 0x0008);
        plant_word(&mut v, 0x0002, 0x0000);
        // The h scroll of 8 puts screen 0 on plane cell 31 of row 0.
        plant_word(&mut v, 0x2000 + 31 * 2, 0x0001);
        v.render_line();
        assert_eq!(&v.screen[0..3], &[36, 36, 36]);
    }

    #[test]
    fn column_vscroll_uses_vsram_pairs() {
        let mut v = vdp();
        set_reg(&mut v, 0x01, 0x40);
        set_reg(&mut v, 0x02, 0x08); // plane A at $2000
        set_reg(&mut v, 0x0F, 2);
        set_reg(&mut v, 0x0B, 0x04); // the column v-scroll
        plant_tile(&mut v, 1, 0x01);
        v.cram[1] = 0x049;
        // The column form: the entries alternate A, B per 16-pixel column.
        v.vsram[0] = 0; // plane A of column 0
        v.vsram[3] = 8; // plane B of column 1 (entry 2*1+1)
        plant_word(&mut v, 0x2000, 0x0001);
        // Plane B of column 1 scrolls by 8: the map entry = the row 1,
        // cell 2 = word 34 of plane B's map (the base is $0000 here).
        plant_word(&mut v, 68, 0x0001);
        v.render_line();
        assert_eq!(v.row_a[0] & 0x0F, 1);
        assert_eq!(v.row_b[16] & 0x0F, 1, "plane B of column 1 scrolled by 8");
    }

    #[test]
    fn window_replaces_plane_a_in_region() {
        let mut v = vdp();
        set_reg(&mut v, 0x01, 0x40);
        set_reg(&mut v, 0x02, 0x08); // plane A at $2000
        set_reg(&mut v, 0x03, 0x02); // window at $0800 (the $800-aligned form)
        set_reg(&mut v, 0x11, 0x02); // the window's 16px columns 0-1 (cells 0-3)
        set_reg(&mut v, 0x04, 0x00);
        plant_tile(&mut v, 1, 0x01);
        plant_tile(&mut v, 2, 0x02);
        v.cram[1] = 0x049;
        v.cram[2] = 0x092;
        // The plane A map word at cell (4, 0) = tile 1.
        plant_word(&mut v, 0x2000 + 4 * 2, 0x0001);
        // The window map word at cell 0 = tile 2.
        plant_word(&mut v, 0x0800, 0x0002);
        v.render_line();
        assert_eq!(v.row_a[0] & 0x0F, 2, "the window covers column 0");
        assert_eq!(v.row_a[32] & 0x0F, 1, "plane A runs past the window");
    }

    // -- The sprite tests ----------------------------------------------------

    fn sat_entry(v: &mut GenesisVdp, index: usize, y: u16, size: u8, link: u8, attr: u16, x: u16) {
        let base = v.sat_base() + index * 8;
        let bytes = [
            (y >> 8) as u8,
            (y & 0xFF) as u8,
            size,
            link,
            (attr >> 8) as u8,
            (attr & 0xFF) as u8,
            (x >> 8) as u8,
            (x & 0xFF) as u8,
        ];
        for (i, b) in bytes.iter().enumerate() {
            v.vram[base + i] = *b;
        }
    }

    #[test]
    fn sprite_link_chain_walks_in_order() {
        let mut v = vdp();
        set_reg(&mut v, 0x01, 0x40);
        set_reg(&mut v, 0x05, 0x01); // SAT at $0200
        set_reg(&mut v, 0x0F, 2);
        plant_tile(&mut v, 1, 0x01);
        v.cram[1] = 0x049;
        // The walk: sprite 0 (screen x 0) links to sprite 2 (screen x 8),
        // whose link 0 stops the chain; sprite 1 never parses.
        sat_entry(&mut v, 0, 128, 0, 2, 0x0001, 128);
        sat_entry(&mut v, 1, 999, 0, 0, 0x0001, 0x0100);
        sat_entry(&mut v, 2, 128, 0, 0, 0x0001, 136);
        v.render_line();
        assert_eq!(v.sprites.len(), 2, "the walk took sprites 0 and 2");
        assert_eq!(v.row_s[0] & 0x0F, 1);
        assert_eq!(v.row_s[8] & 0x0F, 1);
    }

    #[test]
    fn sprite_size_walks_column_major() {
        let mut v = vdp();
        set_reg(&mut v, 0x01, 0x40);
        set_reg(&mut v, 0x05, 0x01);
        set_reg(&mut v, 0x0F, 2);
        // A 2-wide 1-tall sprite of tiles 1 and 2 (the size byte puts the
        // horizontal cells in bits 3-2, the vertical in bits 1-0).
        plant_tile(&mut v, 1, 0x01);
        plant_tile(&mut v, 2, 0x02);
        v.cram[1] = 0x049;
        v.cram[2] = 0x092;
        sat_entry(&mut v, 0, 128, 0x04, 0, 0x0001, 128); // 2-wide, 1-tall
        v.render_line();
        assert_eq!(v.row_s[0] & 0x0F, 1, "the first column shows tile 1");
        assert_eq!(v.row_s[8] & 0x0F, 2, "the second column shows tile 2");
    }

    #[test]
    fn sprite_raw_x_zero_masks_rest_of_line() {
        let mut v = vdp();
        set_reg(&mut v, 0x01, 0x40);
        set_reg(&mut v, 0x05, 0x01);
        set_reg(&mut v, 0x0F, 2);
        plant_tile(&mut v, 1, 0x01);
        v.cram[1] = 0x049;
        // Sprite 0 parses at screen 20, then a raw-X-zero sprite masks;
        // sprite 2 (screen 40) never draws.
        sat_entry(&mut v, 0, 128, 0, 1, 0x0001, 20 + 128);
        sat_entry(&mut v, 1, 128, 0, 2, 0x0001, 0);
        sat_entry(&mut v, 2, 128, 0, 0, 0x0001, 40 + 128);
        v.render_line();
        assert_eq!(v.row_s[20] & 0x0F, 1);
        assert_eq!(v.row_s[40], 0, "masked by the raw X of zero");
        assert!(v.sprites.len() == 1, "the walk stopped at the mask");
    }

    #[test]
    fn sprite_overflow_flag_on_line_limit() {
        let mut v = vdp();
        set_reg(&mut v, 0x01, 0x40);
        set_reg(&mut v, 0x05, 0x02); // SAT at $0400 (the H40-aligned form)
        set_reg(&mut v, 0x0C, 0x01); // H40: 20 sprites and 320 px per line
        set_reg(&mut v, 0x0F, 2);
        for i in 0..21usize {
            let link = match i {
                20 => 0,
                _ => (i + 1) as u8,
            };
            sat_entry(&mut v, i, 128, 0, link, 0x0001, 128);
        }
        v.render_line();
        assert!(v.sov);
        assert_eq!(v.sprites.len(), 20);
    }

    #[test]
    fn sprite_collision_flag_on_overlap() {
        let mut v = vdp();
        set_reg(&mut v, 0x01, 0x40);
        set_reg(&mut v, 0x05, 0x01);
        set_reg(&mut v, 0x0F, 2);
        plant_tile(&mut v, 1, 0x01);
        v.cram[1] = 0x049;
        sat_entry(&mut v, 0, 128, 0, 1, 0x0001, 128);
        sat_entry(&mut v, 1, 128, 0, 0, 0x0001, 128);
        v.render_line();
        assert!(v.scl);
        assert_eq!(v.row_s[0] & 0x0F, 1, "the earlier sprite keeps the slot");
    }

    #[test]
    fn shadow_highlight_operators() {
        let mut v = vdp();
        set_reg(&mut v, 0x01, 0x40);
        set_reg(&mut v, 0x05, 0x01);
        set_reg(&mut v, 0x0F, 2);
        set_reg(&mut v, 0x0C, 0x08); // S/H on
        set_reg(&mut v, 0x07, 0x0F);
        v.cram[0x0F] = 0x1FF; // the backdrop full bright
        plant_tile(&mut v, 1, 0x0E);
        // The backdrop alone: the shadow level halves the channel.
        v.render_line();
        assert_eq!(v.screen[5 * 3 + 2], 127);
        // A palette-3 color-14 sprite pixel at screen 5 highlights the
        // backdrop one step (to normal).
        sat_entry(&mut v, 0, 128, 0, 0, 0x6001, 5 + 128);
        v.render_line();
        assert_eq!(v.screen[5 * 3 + 2], 255);
        // A palette-3 color-15 pixel forces the shadow level instead.
        plant_tile(&mut v, 2, 0x0F);
        sat_entry(&mut v, 0, 128, 0, 0, 0x6002, 6 + 128);
        v.render_line();
        assert_eq!(v.screen[6 * 3 + 2], 127);
    }

    // -- The interrupt tests -------------------------------------------------

    #[test]
    fn vblank_int6_asserts_at_vblank_line() {
        let mut machine = machine_with(&[]);
        machine.boot();
        let vdp = machine_as_vdp(&mut machine);
        set_reg(vdp, 0x01, 0x60); // display on, IE0
        // Run to the vblank start line's visit (line 224 at visit 225).
        machine.run_until(Tick(1 + 3420 * 224));
        let vdp = machine_as_vdp(&mut machine);
        assert!(vdp.vip);
        assert!(vdp.int6_driven);
        let probe_slot = machine.device_slot("probe").unwrap();
        let probe = machine.devices[probe_slot]
            .as_any()
            .downcast_ref::<IrqProbe>()
            .unwrap();
        assert!(
            probe
                .seen
                .borrow()
                .iter()
                .any(|(line, level)| line == "int6" && *level == Level::High)
        );
    }

    #[test]
    fn zint_covers_vblank_window() {
        let mut machine = machine_with(&[]);
        machine.boot();
        let vdp = machine_as_vdp(&mut machine);
        set_reg(vdp, 0x01, 0x40);
        // The zint asserts on the vblank start line's visit (line 224)
        // and holds through the blanking lines.
        machine.run_until(Tick(1 + 3420 * 224));
        {
            let vdp = machine_as_vdp(&mut machine);
            assert!(vdp.zint_driven, "zint off at line {}", vdp.line);
        }
        // The zint holds through the blanking lines up to 259's visit.
        machine.run_until(Tick(1 + 3420 * 259));
        {
            let vdp = machine_as_vdp(&mut machine);
            assert!(vdp.zint_driven, "zint dropped early at line {}", vdp.line);
        }
        // The last lines: the V counter reaches $FF; the vblank ends there.
        machine.run_until(Tick(1 + 3420 * 260));
        let vdp = machine_as_vdp(&mut machine);
        assert!(!vdp.zint_driven, "the zint drops on the final lines");
        machine.run_until(Tick(1 + 3420 * 262 + 1));
        let vdp = machine_as_vdp(&mut machine);
        assert!(!vdp.zint_driven);
    }

    #[test]
    fn hint_fires_at_interval_with_enable() {
        let mut machine = machine_with(&[]);
        machine.boot();
        let vdp = machine_as_vdp(&mut machine);
        set_reg(vdp, 0x01, 0x40);
        set_reg(vdp, 0x00, 0x10); // IE1
        set_reg(vdp, 0x0A, 0x02); // the interval of 3 lines
        machine.run_until(Tick(1 + 3420 * 6));
        let vdp = machine_as_vdp(&mut machine);
        assert!(vdp.hint_pending);
        assert!(vdp.int4_driven);
        // The control read clears the pending flag; the line drops at the
        // next visit.
        let _ = read_port(&mut machine, 4, Tick(1 + 3420 * 8));
        {
            let vdp = machine_as_vdp(&mut machine);
            assert!(!vdp.hint_pending);
        }
        // Stop between visits: the visit at line 9 would refire.
        machine.run_until(Tick(1 + 3420 * 8 + 1_000));
        let vdp = machine_as_vdp(&mut machine);
        assert!(!vdp.int4_driven);
    }

    // -- The HV counter tests ------------------------------------------------

    #[test]
    fn hv_counter_reads_positions() {
        let mut v = vdp();
        // Before any visit, line 0 at slot 0.
        assert_eq!(v.h_counter_at(Tick(0)), 0);
        assert_eq!(v.v_counter_at(Tick(0)), 0);
        v.visit_tick = 100;
        v.line = 10;
        v.regs[0x0C] = 0x01; // H40
        // slot 330 => 2640 mclk => tick 2740.
        assert_eq!(v.h_counter_at(Tick(100 + 2640)), 0xA5);
        assert_eq!(v.v_counter_at(Tick(100 + 2640)), 11);
        assert_eq!(v.h_counter_at(Tick(100 + 2639)), 0xA4);
        // The H32 form of the hcounter jump.
        v.regs[0x0C] = 0x00;
        assert_eq!(v.h_counter_at(Tick(100 + 2640)), 0x84);
        // The line boundary sits at the slot 266 (2660 master clocks):
        // the H counter turns $85 and the V counter flips.
        assert_eq!(v.h_counter_at(Tick(100 + 2660)), 0x85);
        assert_eq!(v.v_counter_at(Tick(100 + 2660)), 11);
    }

    #[test]
    fn hblank_flag_tracks_pinned_span() {
        let mut v = vdp();
        v.regs[0x0C] = 0x00; // H32
        v.visit_tick = 0;
        assert!(!v.hblank_flag_at(Tick(2930)));
        assert!(v.hblank_flag_at(Tick(2950)));
        assert!(v.hblank_flag_at(Tick(3410)));
        assert!(v.hblank_flag_at(Tick(3420 + 10)));
        assert!(!v.hblank_flag_at(Tick(3420 + 110)));
    }

    // -- The snapshot tests --------------------------------------------------

    #[test]
    fn snapshot_round_trips_every_form() {
        let mut v = vdp();
        set_reg(&mut v, 0x01, 0x74);
        set_reg(&mut v, 0x0F, 3);
        vram_write(&mut v, 0x1234, 0xBEEF);
        setup_ram(&mut v, 0x03, 0x0040);
        v.write(AccessWidth::Word, dport(0), 0x0EAE, Tick(0));
        setup_ram(&mut v, 0x05, 0x0040);
        v.write(AccessWidth::Word, dport(0), 0x0200, Tick(0));
        set_reg(&mut v, 0x00, 0x02); // the HV latch set
        v.dma_form = DmaForm::Copy;
        v.dma_remaining = 99;
        v.sov = true;
        v.scl = true;
        v.odd = true;
        let snap = v.snapshot();
        assert_eq!(&snap[..4], TAG);
        let mut twin = vdp();
        twin.restore(&snap).expect("restore");
        let snap2 = twin.snapshot();
        assert_eq!(snap, snap2, "save after restore is byte-identical");
        assert!(twin.sov && twin.scl && twin.odd);
    }

    #[test]
    fn restore_rejects_bad_chunks() {
        let mut v = vdp();
        assert!(v.restore(&[]).is_err());
        assert!(v.restore(b"RONG1").is_err());
        let snap = v.snapshot();
        assert!(v.restore(&snap[..snap.len() - 1]).is_err());
    }

    #[test]
    fn two_restored_machines_run_identically() {
        let mut machine = machine_with(&[]);
        machine.boot();
        let vdp = machine_as_vdp(&mut machine);
        set_reg(vdp, 0x01, 0x40);
        set_reg(vdp, 0x0F, 2);
        // Two frames of state, then the save.
        machine.run_until(Tick(3421 * 20));
        let mut buf = Vec::new();
        machine.save(&mut buf).expect("save");
        let mut twin = machine_with(&[]);
        twin.boot();
        twin.load(&buf).expect("load");
        machine.run_until(Tick(3421 * 36));
        twin.run_until(Tick(3421 * 36));
        {
            let a = machine_as_vdp(&mut machine);
            let b = machine_as_vdp(&mut twin);
            eprintln!(
                "DBG tw: a(line={} visit={} frames={} next={:?} vip={}) b(line={} visit={} frames={} next={:?} vip={})",
                a.line,
                a.visit_tick,
                a.frames,
                a.next,
                a.vip,
                b.line,
                b.visit_tick,
                b.frames,
                b.next,
                b.vip
            );
        }
        let snap_a = machine.devices[machine.device_slot("vdp").unwrap()].snapshot();
        let snap_b = twin.devices[twin.device_slot("vdp").unwrap()].snapshot();
        assert_eq!(snap_a, snap_b, "the restored twin continues byte-identical");
        let mut buf2 = Vec::new();
        twin.save(&mut buf2).expect("resave");
        let mut buf3 = Vec::new();
        machine.save(&mut buf3).expect("resave a");
        assert_eq!(buf3, buf2, "the whole machine save matches after restore");
    }

    #[test]
    fn two_plain_runs_identical() {
        let run = || {
            let mut machine = machine_with(&[]);
            machine.boot();
            let vdp = machine_as_vdp(&mut machine);
            set_reg(vdp, 0x01, 0x40);
            machine.run_until(Tick(3421 * 5));
            machine.save(&mut Vec::new()).expect("save");
            machine.devices[machine.device_slot("vdp").unwrap()].snapshot()
        };
        assert_eq!(run(), run(), "identical runs produce identical state");
    }

    // -- The PAL mode and the region -----------------------------------------

    #[test]
    fn pal_mode_lines_and_v_jump() {
        let device = instance("vdp", &[("region", "pal")]);
        let v = GenesisVdp::new(&device).unwrap();
        assert!(v.pal);
        assert_eq!(v.total_lines(), 313);
        assert_eq!(v.v_internal(258), 0x102);
        assert_eq!(v.v_internal(259), 0x1CA);
        assert_eq!(v.v_display(258), 0x02);
        assert_eq!(v.v_display(259), 0xCA);
        let device = instance("vdp", &[("region", "bogus")]);
        assert!(GenesisVdp::new(&device).is_err());
    }
}
