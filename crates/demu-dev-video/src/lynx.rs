//! The Atari Lynx Suzy die: the sprite engine, the math unit, and the
//! four pulse-width audio channels as the `lynx.suzy` device kind.
//!
//! The device owns the register pages of the die plus the Mikey page
//! ranges a display build needs: the pulse-width audio channels, the
//! master mute byte, and the color palette. The configuration maps its
//! bus window over `$FC00-$FDFF` so every range below resolves; the
//! Mikey timer block and the other Mikey bytes belong to a Mikey device
//! in a full machine and read back `$FF` here. The joypad byte at
//! `$FCB0` and the switch byte at `$FCB1` answer their idle forms
//! here; the Mikey die of the full machine owns them through its own
//! window over the page.
//!
//! # Bus coupling
//!
//! The sprite control blocks and the sprite data live in machine RAM.
//! The walk reads them through [`MachineCtx::read`] on the bus named by
//! the `bus` parameter, the same way the ANTIC display list walks
//! machine RAM. The `bus` parameter accepts a `bus` string and falls
//! back to the first bus of the configuration, and falls back further
//! to the constant `cpu`, which mirrors the MOS65SC02 constructor.
//!
//! The `suzy_ram` inbound line is the register-view gate of the real
//! die: Level High activates the machine RAM view and the register
//! pages drop out of the bus decode ([`BusAccess::decodes`] returns
//! false), so the RAM underneath answers; Level Low restores the
//! register answer. The overlay priority that picks between the two
//! stays with the configuration. The `sleep` line is accepted and holds
//! nothing in this build; the CPU sleep handshake of the real die pair
//! is recorded out of build.
//!
//! # Pinned Suzy register page
//!
//! The table pins the implemented register surface in page offsets from
//! `$FC00`. Every register pair is little-endian, the low byte at the
//! even address. A CPU write to a low byte clears the high byte, and a
//! write to a high byte keeps the low byte.
//!
//! | Offset | Name | Meaning |
//! |--------|------|---------|
//! | 0x00/01 | TMPADR | temporary address (engine) |
//! | 0x02/03 | TILTACUM | tilt accumulator (engine) |
//! | 0x04/05 | HOFF | window horizontal offset in the world |
//! | 0x06/07 | VOFF | window vertical offset in the world |
//! | 0x08/09 | VIDBAS | video build buffer base |
//! | 0x0A/0B | COLLBAS | collision build buffer base |
//! | 0x0C/0D | VIDADR | current video build address (engine) |
//! | 0x0E/0F | COLLADR | current collision build address (engine) |
//! | 0x10/11 | SCBNEXT | address of the next sprite control block |
//! | 0x12/13 | SPRDLINE | address of the current sprite data line |
//! | 0x14/15 | HPOSSTRT | starting horizontal position |
//! | 0x16/17 | VPOSSTRT | starting vertical position |
//! | 0x18/19 | SPRHSIZ | horizontal size register |
//! | 0x1A/1B | SPRVSIZ | vertical size register |
//! | 0x1C/1D | STRETCH | horizontal size adder |
//! | 0x1E/1F | TILT | horizontal position adder |
//! | 0x20/21 | SPRDOFF | offset to the next sprite data line (engine) |
//! | 0x22/23 | SPRVPOS | current vertical position (engine) |
//! | 0x24/25 | COLLOFF | offset to the collision depository |
//! | 0x26/27 | VSIZACUM | vertical size accumulator (engine) |
//! | 0x28/29 | HSIZOFF | horizontal size offset |
//! | 0x2A/2B | VSIZOFF | vertical size offset |
//! | 0x2C/2D | SCBADR | address of the current control block (engine) |
//! | 0x2E/2F | PROCADR | current sprite data process address (engine) |
//! | 0x52 | MATHD | multiply operand CD low, quotient byte 0 |
//! | 0x53 | MATHC | multiply operand CD high |
//! | 0x54 | MATHB | multiply operand AB low |
//! | 0x55 | MATHA | multiply operand AB high, the multiply trigger |
//! | 0x56 | MATHP | divisor low |
//! | 0x57 | MATHN | divisor high |
//! | 0x60 | MATHH | dividend byte 0 and product byte 0 |
//! | 0x61 | MATHG | dividend byte 1 and product byte 1 |
//! | 0x62 | MATHF | dividend byte 2 and product byte 2 |
//! | 0x63 | MATHE | dividend byte 3 and the divide trigger |
//! | 0x6C | MATHM | remainder byte 0 |
//! | 0x6D | MATHL | remainder byte 1 |
//! | 0x6E | MATHK | remainder byte 2 |
//! | 0x6F | MATHJ | remainder byte 3 |
//! | 0x80 | SPRCTL0 | sprite control bits 0 |
//! | 0x81 | SPRCTL1 | sprite control bits 1 |
//! | 0x82 | SPRCOLL | sprite collision number |
//! | 0x83 | SPRINIT | sprite initialization bits |
//! | 0x88 | SUZYHREV | hardware revision, reads 0x01 |
//! | 0x89 | SUZYSREV | no register, reads 0x00 |
//! | 0x90 | SUZYBUSEN | bus enable, bit 0 |
//! | 0x91 | SPRGO | sprite process start bit 0, everon bit 2 |
//! | 0x92 | SPRSYS | system control and status |
//! | 0xB0 | JOYSTICK | joystick and buttons |
//! | 0xB1 | SWITCHES | pause and cart strobes, reads 0x04 |
//! | 0xB2/B3 | RCART | cart strobe bytes |
//! | 0xC0 | LEDS | led byte |
//! | 0xC2..0xC4 | parallel port and Howie | stored bytes |
//!
//! `SPRCTL0` bits 7-6 carry the pixel depth minus one: 00 is one bit
//! per pixel through 11 is four. Bit 5 is the horizontal flip, bit 4
//! the vertical flip, and bits 2-0 the sprite type: 7 shadow, 6
//! exclusive-or, 5 non-collidable, 4 normal, 3 boundary, 2
//! boundary-shadow, 1 background-no-collision, 0 background. `SPRCTL1`
//! bit 7 selects the literal data form, bit 6 the sizing algorithm
//! select, bits 5-4 the reload depth (0 none, 1 hsize and vsize, 2 plus
//! stretch, 3 plus tilt), bit 3 the palette reuse (1 drops the palette
//! bytes from the block), bit 2 the skip flag, bit 1 the draw-up
//! direction, bit 0 the draw-left direction. `SPRCOLL` bit 5 is the
//! no-collide flag and bits 3-0 the four-bit collision number.
//! `SPRGO` bit 0 starts the sprite walk and self-clears when the chain
//! ends, bit 2 enables the everon detector. `SPRSYS` write bits carry
//! sign math (7), multiply accumulate (6), no-collide (5), vertical
//! stretch (4), left handed (3), the unsafe clear (2), and the stop at
//! the current sprite (1); the read form carries math in process (7,
//! never set here), the math warning (6), the last carry (5, stays
//! clear), vertical stretch (4), left handed (3), the unsafe access
//! flag (2), the stop request (1), and the sprite working bit (0). The
//! write side keeps the armed controls in the register byte and the
//! read assembles them with the live status bits.
//!
//! # Pinned sprite control block
//!
//! The block is the documented RAM form; every multi-byte field is
//! little-endian, as the 65C02 pointer convention stores them.
//!
//! | Offset | Field | Size |
//! |--------|-------|------|
//! | 0 | SPRCTL0 | 1 |
//! | 1 | SPRCTL1 | 1 |
//! | 2 | SPRCOLL | 1 |
//! | 3 | next control block pointer | 2, 0 ends the chain |
//! | 5 | sprite data pointer | 2 |
//! | 7 | starting horizontal position | 2 |
//! | 9 | starting vertical position | 2 |
//! | 11 | horizontal size (reload depth 1+) | 2 |
//! | 13 | vertical size (reload depth 1+) | 2 |
//! | 15 | stretch (reload depth 2+) | 2 |
//! | 17 | tilt (reload depth 3+) | 2 |
//! | next | pen palette | 1, 2, 4 or 8 bytes |
//!
//! The pen palette holds one byte per pair of pen indices, size
//! `2^(depth-1)`: eight bytes cover all sixteen pen indices, four cover
//! the eight pens of a three-bit sprite, two the four pens of two bits,
//! and one the two pens of one bit. The pen number referenced by pen
//! index 0 sits in the upper nibble of the first byte, index 1 in the
//! low nibble, and so on. The palette-reuse bit keeps the palette of
//! the previous sprite and drops the palette bytes; this build loads
//! the full eight-byte table whenever a block reloads, since the pen
//! indices beyond the sprite depth stay unused.
//!
//! # Pinned sprite data format (packed form)
//!
//! Sprite data is a chain of lines. Each line begins with one header
//! byte: 0 ends the data, 1 ends the quadrant and the engine advances
//! counter-clockwise to the next quadrant, and any other value is the
//! byte distance from that header to the next line's header. Packed
//! lines carry packets: each packet is one literal-mode bit (1 literal,
//! 0 packed), a four-bit pixel count, and then for the packed run a
//! single pen index that repeats for count plus one pixels, or for the
//! literal run one pen index per pixel. A packed packet with a zero
//! count ends the line. Pixels fill each byte from the most significant
//! bit down, and a line's bits end at the byte distance the header
//! names. In the literal form the packets are absent: each pixel is one
//! depth-sized field until the line's bits end, and a zero bit pattern
//! as the last pixel of a line ends it.
//!
//! # Pinned engine behavior
//!
//! The world is 512 pixels square; the display window is 160 pixels by
//! 102 rows and sits in the world at the HOFF and VOFF offsets. Sprites
//! are positioned in the world and clipped at the window. The quadrant
//! order is south-east, north-east, north-west, south-west from the two
//! SPRCTL1 direction bits, with the vertical and horizontal flip bits
//! mirroring the drawn directions. Sized sprites scale through the
//! horizontal and vertical size accumulators seeded by the HSIZOFF and
//! VSIZOFF offsets; the stretch value adds to the horizontal size and
//! the tilt value adds to the horizontal position once per destination
//! row, and the vertical stretch bit extends the vertical size with the
//! same adder per source line. Pen numbers 1 through 13 are opaque and
//! collidable for normal sprites, pen 0 is transparent, pen 15 turns
//! transparent for the boundary types, pen 14 stops colliding for the
//! shadow types, and the background types overwrite the build and
//! collision buffers without a read. During painting the engine reads
//! each collision cell, keeps the highest collision number seen, and
//! writes the number back; at the end of a collideable sprite the
//! number lands in the depository byte, which lives in machine RAM at
//! `SCBADR + COLLOFF` through the bus, and the everon detector flips
//! bit 7 of the depository on whether the sprite ever reached the
//! window. The chain ends when the top byte of SCBNEXT is zero.
//!
//! # Palette and pulse-width audio
//!
//! The palette holds sixteen entries of twelve-bit color at `$FDA0` and
//! `$FDB0`: one green byte per entry with the four low bits used, then
//! one byte per entry with blue in the high nibble and red in the low
//! nibble. A pen number picks one entry, and each four-bit channel
//! scales to eight bits by 17. The composed frame paints palette entry
//! 0 as the background, then the sprite walk paints over it; the RGB
//! conversion runs once at the end of the walk. The composed frame is
//! the 160-pixel display of the 80-byte four-bit line, so the frame is
//! 160 by 102.
//!
//! The four audio channels sit at `$FD20-$FD3F`, eight bytes per
//! channel: 0 the two's-complement volume byte that serves as the DAC
//! value, 1 the feedback tap select for shift bits 11, 10, and 5 down
//! to 0, 2 the current output value, 3 the low eight bits of the shift
//! register, 4 the timer backup, 5 the control byte (bit 7 the shift
//! bit 7 tap, bit 6 the timer-done reset, bit 5 the integrate mode,
//! bit 4 the reload enable, bit 3 the count enable, bits 2-0 the clock
//! select), 6 the counter readback, 7 the other bits (bits 7-4 the
//! shift bits 8-11, bit 0 the borrow-out flag). The prescaler selects 1
//! to 64 microseconds of the 16 MHz master, 16 to 1024 master ticks, or
//! the link clock, which takes the period of the previous channel;
//! channel 0 links from the timer 7 die outside this device, so its
//! link period takes the period of channel 3. A reload-disabled counter
//! spans a full 256-step wrap. Each reload edge advances the twelve-bit
//! polynomial shift register: the selected taps exclusive-or, the
//! inverted result is the emitted bit and feeds the shift input, and
//! the register shifts right with the new bit entering at bit 11. The
//! emitted bit drives the DAC to the volume register or its two's
//! complement; integrate mode keeps a running total in the output byte
//! instead and clips at the byte bounds. A write of the backup byte
//! re-arms the countdown, and a write of the counter byte loads the
//! countdown from the visible value. The mute byte `$FD50` disables one
//! ear per channel; the mixed mono sample keeps a channel while either
//! ear stays connected.
//!
//! # Schedule
//!
//! Each scheduler visit equals one display line of 159 microseconds,
//! the 60 Hz line of the hardware specification: 2544 master ticks
//! divided by the `dot` divider (default 1). The frame holds 105 lines,
//! 102 display rows plus three blank lines, for 59.9 frames per second.
//! A set `SPRGO` walk arm paints at the next visit: the background
//! paints palette entry 0, the whole control block chain walks through
//! the machine bus, the composed pen surface converts to RGB888, the
//! frame counter increments, and `SPRGO` self-clears. Audio steps
//! inside the line visit on a fixed 1024-master-tick cadence, a
//! 15.625 kHz sample stream inside the published 15-16 kHz pulse-width
//! band; each sample sums the DAC bytes of the connected channels
//! scaled by 64.
//!
//! # Recorded approximations
//!
//! - The walk runs synchronously inside one visit; the real die
//!   interleaves its bus use with the sleeping CPU line by line.
//! - The build and collision buffers are internal mirrors; the real die
//!   paints machine RAM at VIDBAS and COLLBAS and leaves the RAM there
//!   unchanged. The collision depository still lands in machine RAM at
//!   the published `SCBADR + COLLOFF` address through the bus.
//! - The collision read-back is the depository byte plus the collision
//!   buffer cells of the internal mirror; sprites read the collision
//!   numbers of earlier sprites only.
//! - The everon depository bit 7 reads 1 when the sprite never reached
//!   the window and 0 when it did, the Handy emulator's polarity.
//! - The everon flag stores only on the sprite end read; the real die
//!   updates the bit when the detector arms.
//! - The math unit computes at the trigger write; `SPRSYS` read bit 7
//!   never sets, bit 6 carries the multiply overflow or the
//!   divide-by-zero warning, and bit 5 stays clear. The divide latency
//!   of 176 plus 14 ticks per leading zero of the divisor is recorded,
//!   not simulated. The divide runs unsigned, and the signed multiply
//!   reproduces the die corner where 0x8000 counts positive and 0x0000
//!   negative, the Handy sign handling.
//! - Super-clipping renders only the quadrants whose directions point
//!   back toward the window; the exact halfway-line quadrant tests of
//!   the die are simplified.
//! - The link clock of channel 0 takes the period of channel 3 because
//!   its real source, timer 7, lives outside this device, and a chain
//!   of linked clocks resolves one hop.
//! - The sixteen-bit positions read as signed values everywhere; Handy
//!   mixes signed and unsigned views in the quadrant tests.
//! - The unsafe-access status bit stays clear in this build: the flag
//!   starts false, its read reports clear, and the CPU write under an
//!   active walk that would set it is out of the model; the snapshot
//!   carries the stored form for the round trip.
//! - The Mikey timer block, the serial port, and the CPU sleep
//!   handshake stay outside the device.
//!
//! Sources: the Epyx hardware specification through its published
//! reference copies (the memory map, sprite, collision, audio, and
//! display chapters), the Handy emulator `susie.cpp` sprite walk, and
//! the `cc65` `_suzy.h` register structures.

use demu_config::model::{BootState, DeviceInstance, SystemConfig};
use demu_core::device::{AccessWidth, BusAccess, Frame, MachineCtx, VideoLayer, VideoSource};
use demu_core::frequency::{Address, Tick};
use demu_core::signal::Level;
use demu_core::{Device, Snapshot, SnapshotError};

/// The display rows.
const ROWS: usize = 102;
/// The display columns.
const COLS: usize = 160;
/// The bytes of one four-bit display line.
const LINE_BYTES: usize = COLS / 2;
/// The pen nibbles of a surface byte.
const PEN_MASK: u8 = 0x0F;
/// The master ticks of one 159-microsecond line at 16 MHz.
const LINE_MASTER: u64 = 2544;
/// The lines of one frame: 102 display rows plus three blank lines.
const FRAME_LINES: u64 = 105;
/// The master ticks of one audio sample: 16 MHz / 1024 = 15.625 kHz.
const SAMPLE_MASTER: u64 = 1024;
/// The audio sample buffer cap; the media pipeline drains earlier.
const SAMPLE_CAP: usize = 0x4000;
/// The snapshot length of the Suzy chunk: tag and version, both
/// register pages, the pen palette, the engine state, the four audio
/// channels, and both four-bit surfaces.
const SUZY_SNAPSHOT_LEN: usize = 5
    + 0x80
    + 0x80
    + 16
    + 2
    + 1
    + 8
    + 6
    + 5
    + 44
    + 8
    + 16
    + 16
    + 8
    + ROWS * LINE_BYTES
    + ROWS * LINE_BYTES;
/// The register page base on the bus.
const PAGE: u32 = 0xFC00;
/// The audio register block base on the bus.
const AUDIO_PAGE: u32 = 0xFD20;
/// The palette block base on the bus.
const PALETTE_PAGE: u32 = 0xFDA0;
/// The chain walk stops after this many control blocks.
const CHAIN_LIMIT: usize = 4096;

/// The upper register page offsets.
const R_TMPADR: usize = 0x00;
const R_TILTACUM: usize = 0x02;
const R_HOFF: usize = 0x04;
const R_VOFF: usize = 0x06;
const R_SCBNEXT: usize = 0x10;
const R_SPRDLINE: usize = 0x12;
const R_HPOSSTRT: usize = 0x14;
const R_VPOSSTRT: usize = 0x16;
const R_SPRHSIZ: usize = 0x18;
const R_SPRVSIZ: usize = 0x1A;
const R_STRETCH: usize = 0x1C;
const R_TILT: usize = 0x1E;
const R_SPRDOFF: usize = 0x20;
const R_COLLOFF: usize = 0x24;
const R_VSIZACUM: usize = 0x26;
const R_HSIZOFF: usize = 0x28;
const R_VSIZOFF: usize = 0x2A;
const R_SCBADR: usize = 0x2C;
/// The math register offsets, with their operand half views.
const MATH_ABCD: usize = 0x52;
/// The MATHB/MATHA operand pair inside the page.
const MATH_ABCD_LOW: usize = 0x54;
const MATH_NP: usize = 0x56;
/// The MATHE/MATHF/MATHG/MATHH thirty-two-bit block.
const MATH_EFGH: usize = 0x60;
/// The MATHM/MATHL/MATHK/MATHJ thirty-two-bit block.
const MATH_JKLM: usize = 0x6C;
/// The control page offsets, relative to `$FC80`.
const C_SPRCTL0: usize = 0x00;
const C_SPRCTL1: usize = 0x01;
const C_SPRCOLL: usize = 0x02;
const C_SUZYBUSEN: usize = 0x10;
const C_SPRGO: usize = 0x11;
const C_SPRSYS: usize = 0x12;
const C_JOYSTICK: usize = 0x30;
const C_SWITCHES: usize = 0x31;
const C_MSTEREO: usize = 0x50;

/// One pulse-width audio channel.
#[derive(Debug, Clone, Copy)]
struct PwmChannel {
    /// The two's-complement volume byte, also the DAC value register.
    volume: u8,
    /// The feedback tap select: shift bits 11, 10, and 5 down to 0.
    feedback: u8,
    /// The current output byte: the DAC value or the integrate total.
    output: u8,
    /// The low eight bits of the twelve-bit shift register.
    shift_lo: u8,
    /// The timer backup value.
    backup: u8,
    /// The control byte: tap 7, done reset, integrate, reload, count,
    /// clock select.
    control: u8,
    /// The other bits: shift bits 8-11 and the borrow-out flag.
    other: u8,
    /// The remaining master ticks of the current period.
    remain: u32,
}

impl PwmChannel {
    fn new() -> Self {
        Self {
            volume: 0,
            feedback: 0,
            output: 0,
            shift_lo: 0,
            backup: 0,
            control: 0,
            other: 0,
            remain: 0,
        }
    }

    /// The twelve-bit shift register from its stored halves.
    fn shift(&self) -> u16 {
        u16::from(self.shift_lo) | ((u16::from(self.other) & 0xF0) << 4)
    }

    fn set_shift(&mut self, value: u16) {
        self.shift_lo = value as u8;
        self.other = (self.other & 0x0F) | (((value >> 4) & 0x0F) as u8) << 4;
    }

    /// The count-enable bit of the control byte.
    fn counting(&self) -> bool {
        self.control & 0x08 != 0
    }

    /// The reload-enable bit of the control byte.
    fn reloading(&self) -> bool {
        self.control & 0x10 != 0
    }

    /// The integrate-mode bit of the control byte.
    fn integrating(&self) -> bool {
        self.control & 0x20 != 0
    }

    /// The base period in master ticks for one channel's prescaler
    /// select, backup, and reload flag.
    fn base_period(select: u8, backup: u8, reload: bool) -> u32 {
        let span = if reload { u32::from(backup) + 1 } else { 256 };
        (16u32 << (u32::from(select) & 0x07).min(6)) * span
    }
}

/// The end-of-line marker in the pixel value space.
const LINE_END: u32 = 0xFFFF;

/// One line decode of the sprite data stream, transcribed from the
/// documented engine: a twenty-four-bit byte stream register, a packet
/// bit budget against the line length, and the run state.
#[derive(Debug)]
struct LineEngine {
    shreg: u32,
    shcount: u32,
    bits_left: u32,
    repeat: u32,
    /// The current pen number, or `LINE_END`.
    pixel: u32,
    /// The current run is the literal form.
    literal: bool,
    /// The whole line is the absolute literal form.
    abs_literal: bool,
    /// The pixel depth in bits.
    bpp: u32,
    /// The pen palette copy of this sprite.
    penindex: [u8; 16],
}

impl LineEngine {
    /// A fresh stream for one sprite depth and palette.
    fn new(bpp: u32, penindex: [u8; 16]) -> Self {
        Self {
            shreg: 0,
            shcount: 0,
            bits_left: 0,
            repeat: 0,
            pixel: 0,
            literal: false,
            abs_literal: false,
            bpp,
            penindex,
        }
    }

    /// Reset the stream at the sprite data line pointer and read the
    /// line's header byte. Returns the header value.
    fn init(&mut self, reader: &mut RamReader<'_, '_, '_>, abs_literal: bool, bpp: u32) -> u16 {
        self.shreg = 0;
        self.shcount = 0;
        self.repeat = 0;
        self.pixel = 0;
        self.literal = false;
        self.abs_literal = abs_literal;
        self.bpp = bpp;
        self.bits_left = 0xFFFF;
        let offset = self.bits(8, reader);
        // The line body runs offset-1 bytes behind the header; the
        // header values 0 and 1 leave the budget unused, and the
        // walker breaks on them before any pixel.
        self.bits_left = (offset.wrapping_sub(1)).wrapping_mul(8);
        if self.abs_literal {
            self.repeat = self.bits_left / self.bpp;
        }
        offset as u16
    }

    /// Consume `count` bits from the stream, most significant bit of
    /// each source byte first. Runs past the line's byte budget yield
    /// zeros, which the packet decoder reads as the line end.
    fn bits(&mut self, count: u32, reader: &mut RamReader<'_, '_, '_>) -> u32 {
        if self.bits_left <= count {
            return 0;
        }
        if self.shcount < count {
            let b0 = u32::from(reader.read());
            let b1 = u32::from(reader.read());
            let b2 = u32::from(reader.read());
            self.shreg = self.shreg.wrapping_mul(0x0100_0000) | (b0 << 16) | (b1 << 8) | b2;
            self.shcount += 24;
        }
        let value = (self.shreg >> (self.shcount - count)) & ((1u32 << count) - 1);
        self.shcount -= count;
        self.bits_left -= count;
        value
    }

    /// One pixel of the line, or `LINE_END` at the line's end.
    fn next_pixel(&mut self, reader: &mut RamReader<'_, '_, '_>) -> u32 {
        if self.repeat == 0 {
            if !self.abs_literal {
                self.literal = self.bits(1, reader) != 0;
            }
            if self.abs_literal {
                return LINE_END;
            }
            if self.literal {
                self.repeat = self.bits(4, reader) + 1;
            } else {
                let count = self.bits(4, reader);
                self.pixel = if count == 0 {
                    LINE_END
                } else {
                    let index = self.bits(self.bpp, reader) & 0x0F;
                    let slot = usize::try_from(index).unwrap_or(0);
                    u32::from(self.penindex[slot])
                };
                self.repeat = count + 1;
            }
        }
        if self.pixel == LINE_END {
            return self.pixel;
        }
        self.repeat = self.repeat.saturating_sub(1);
        if self.abs_literal {
            let raw = self.bits(self.bpp, reader);
            let slot = usize::try_from(raw & 0x0F).unwrap_or(0);
            self.pixel = if self.repeat == 0 && raw == 0 {
                LINE_END
            } else {
                u32::from(self.penindex[slot])
            };
        } else if self.literal {
            let index = self.bits(self.bpp, reader) & 0x0F;
            let slot = usize::try_from(index).unwrap_or(0);
            self.pixel = u32::from(self.penindex[slot]);
        }
        self.pixel
    }
}

/// The machine RAM read side of one sprite walk.
struct RamReader<'m, 'b, 'q> {
    bus: &'b str,
    tick: Tick,
    ctx: &'m mut MachineCtx<'q>,
    position: u16,
}

impl RamReader<'_, '_, '_> {
    fn read(&mut self) -> u8 {
        let value = self.ctx.read(
            self.bus,
            AccessWidth::Byte,
            Address(u32::from(self.position)),
            self.tick,
        ) as u8;
        self.position = self.position.wrapping_add(1);
        value
    }
}

/// The Atari Lynx Suzy die.
#[derive(Debug)]
pub struct LynxSuzy {
    name: String,
    /// The name of the bus the sprite list reads walk.
    bus: String,
    /// The domain ticks per master tick, from the `dot` divider.
    div: u64,
    /// The upper register page `$FC00-$FC7F`.
    page: [u8; 0x80],
    /// The control page `$FC80-$FCFF`.
    ctl: [u8; 0x80],
    /// The pen index palette in engine order.
    penindex: [u8; 16],
    /// The internal horizontal size accumulator.
    hsizacum: u16,
    /// The collision maximum of the sprite being painted.
    collision: u8,
    /// The display lines since boot.
    line: u64,
    /// True while the machine RAM view gates the register pages.
    ram_view: bool,
    /// The walk arm set by SPRGO, painted at the next visit.
    walk_arm: bool,
    /// The everon detector enable.
    everon: bool,
    /// The unsafe-access status bit.
    unsafe_access: bool,
    /// The stop request bit of SPRSYS.
    stop_request: bool,
    /// The math warning flag: multiply overflow or divide by zero.
    math_warning: bool,
    /// The multiply sign flags: 1 positive, minus 1 negative.
    math_ab_sign: i32,
    math_cd_sign: i32,
    /// The signed math bit of SPRSYS.
    signmath: bool,
    /// The multiply accumulate bit of SPRSYS.
    accumulate: bool,
    /// The system no-collide bit of SPRSYS.
    sys_nocollide: bool,
    /// The four audio channels.
    channels: [PwmChannel; 4],
    /// The audio sample accumulator in master ticks.
    sample_acc: u64,
    /// The four-pen palette: green bytes, then blue and red bytes.
    palette_green: [u8; 16],
    palette_bluered: [u8; 16],
    /// The four-bit build surface, 80 bytes per display row.
    surface: Vec<u8>,
    /// The four-bit collision buffer, same shape as the surface.
    collbuf: Vec<u8>,
    /// The mixed audio samples.
    samples: Vec<i16>,
    /// The composed screen in RGB888.
    screen: Vec<u8>,
    /// The cached frame for the shell.
    frame: Frame,
    frames: u64,
    next: Option<Tick>,
    last_tick: Option<Tick>,
}

impl LynxSuzy {
    /// A device from its configuration entry.
    ///
    /// The sprite bus takes the `bus` parameter, the first bus of the
    /// configuration, or the `cpu` fallback, in that order. The `dot`
    /// divider scales the line cadence; the default of 1 puts one
    /// 159-microsecond line on 2544 domain ticks of the 16 MHz master.
    pub fn new(device: &DeviceInstance, config: &SystemConfig) -> Self {
        let bus = device
            .params
            .get("bus")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| {
                config
                    .buses
                    .first()
                    .map(|b| b.name.clone())
                    .unwrap_or_else(|| "cpu".to_string())
            });
        let div = u64::from(device.dividers.get("dot").copied().unwrap_or(1)).max(1);
        let mut suzy = Self {
            name: device.name.to_string(),
            bus,
            div,
            page: [0; 0x80],
            ctl: [0; 0x80],
            penindex: [0; 16],
            hsizacum: 0x007F,
            collision: 0,
            line: 0,
            ram_view: false,
            walk_arm: false,
            everon: false,
            unsafe_access: false,
            stop_request: false,
            math_warning: false,
            math_ab_sign: 1,
            math_cd_sign: 1,
            signmath: false,
            accumulate: false,
            sys_nocollide: false,
            channels: [
                PwmChannel::new(),
                PwmChannel::new(),
                PwmChannel::new(),
                PwmChannel::new(),
            ],
            sample_acc: 0,
            palette_green: [0; 16],
            palette_bluered: [0; 16],
            surface: vec![0; ROWS * LINE_BYTES],
            collbuf: vec![0; ROWS * LINE_BYTES],
            samples: Vec::new(),
            screen: vec![0; COLS * ROWS * 3],
            frame: Frame {
                width: COLS as u32,
                height: ROWS as u32,
                pixels: vec![0; COLS * ROWS * 3],
            },
            frames: 0,
            next: None,
            last_tick: None,
        };
        // The pen index palette powers up as the identity map, and the
        // cart strobe bits idle inactive.
        for (i, pen) in suzy.penindex.iter_mut().enumerate() {
            *pen = i as u8;
        }
        suzy.ctl[C_SWITCHES] = 0x04;
        suzy
    }

    /// The line cadence in domain ticks: the fixed 2544-master-tick
    /// line divided by the divisor, never zero.
    fn line_ticks(&self) -> u64 {
        (LINE_MASTER / self.div).max(1)
    }

    /// A register pair from the upper page.
    fn pair(&self, offset: usize) -> u16 {
        u16::from(self.page[offset]) | (u16::from(self.page[offset + 1]) << 8)
    }

    /// The signed view of a register pair.
    fn signed(&self, offset: usize) -> i32 {
        i32::from(self.pair(offset) as i16)
    }

    fn set_pair(&mut self, offset: usize, value: u16) {
        self.page[offset] = value as u8;
        self.page[offset + 1] = (value >> 8) as u8;
    }

    /// Store one register pair byte: a low-byte write clears the high
    /// byte, a high-byte write keeps the low byte.
    fn set_pair_byte(&mut self, offset: usize, value: u8) {
        if offset & 1 == 0 {
            self.page[offset] = value;
            self.page[offset + 1] = 0;
        } else {
            self.page[offset] = value;
        }
    }

    /// A thirty-two-bit register from four little-endian page bytes.
    fn word(&self, offset: usize) -> u32 {
        u32::from(self.page[offset])
            | (u32::from(self.page[offset + 1]) << 8)
            | (u32::from(self.page[offset + 2]) << 16)
            | (u32::from(self.page[offset + 3]) << 24)
    }

    fn set_word(&mut self, offset: usize, value: u32) {
        self.page[offset] = value as u8;
        self.page[offset + 1] = (value >> 8) as u8;
        self.page[offset + 2] = (value >> 16) as u8;
        self.page[offset + 3] = (value >> 24) as u8;
    }

    /// The collision value at one column of a collision row.
    fn collision_at(&self, row: usize, col: usize) -> u8 {
        let b = self.collbuf[row * LINE_BYTES + col / 2];
        if col & 1 == 0 {
            (b & 0xF0) >> 4
        } else {
            b & PEN_MASK
        }
    }

    /// Store the collision value at one column of a collision row.
    fn set_collision_at(&mut self, row: usize, col: usize, value: u8) {
        let b = &mut self.collbuf[row * LINE_BYTES + col / 2];
        if col & 1 == 0 {
            *b = (*b & PEN_MASK) | (value << 4);
        } else {
            *b = (*b & 0xF0) | (value & PEN_MASK);
        }
    }

    /// Set the display pen at one column of a display row.
    fn set_pen_at(&mut self, row: usize, col: usize, pen: u8) {
        let b = &mut self.surface[row * LINE_BYTES + col / 2];
        if col & 1 == 0 {
            *b = (*b & PEN_MASK) | (pen << 4);
        } else {
            *b = (*b & 0xF0) | (pen & PEN_MASK);
        }
    }

    /// Read the display pen at one column of a display row.
    fn pen_at(&self, row: usize, col: usize) -> u8 {
        let b = self.surface[row * LINE_BYTES + col / 2];
        if col & 1 == 0 {
            (b & 0xF0) >> 4
        } else {
            b & PEN_MASK
        }
    }

    /// The RGB888 bytes of one palette entry.
    fn pen_rgb(&self, pen: u8) -> [u8; 3] {
        let i = usize::from(pen & PEN_MASK);
        let green = self.palette_green[i] & PEN_MASK;
        let red = self.palette_bluered[i] & PEN_MASK;
        let blue = (self.palette_bluered[i] >> 4) & PEN_MASK;
        [Self::scale4(red), Self::scale4(green), Self::scale4(blue)]
    }

    /// The four-bit replication scale to eight bits.
    fn scale4(value: u8) -> u8 {
        (value << 4) | (value & PEN_MASK)
    }

    /// Convert the composed pen surface to the RGB888 screen.
    fn refresh_screen(&mut self) {
        for row in 0..ROWS {
            for col in 0..COLS {
                let [r, g, b] = self.pen_rgb(self.pen_at(row, col));
                let base = (row * COLS + col) * 3;
                self.screen[base] = r;
                self.screen[base + 1] = g;
                self.screen[base + 2] = b;
            }
        }
    }

    /// One machine RAM byte through the bus.
    fn ram_read(&mut self, address: u16, tick: Tick, ctx: &mut MachineCtx) -> u8 {
        ctx.read(
            &self.bus,
            AccessWidth::Byte,
            Address(u32::from(address)),
            tick,
        ) as u8
    }

    /// One machine RAM byte write through the bus.
    fn ram_write(&mut self, address: u16, value: u8, tick: Tick, ctx: &mut MachineCtx) {
        ctx.write(
            &self.bus,
            AccessWidth::Byte,
            Address(u32::from(address)),
            u32::from(value),
            tick,
        );
    }

    /// One machine RAM word (two bytes) through the bus, advancing the
    /// pointer.
    fn ram_word(&mut self, ptr: &mut u16, tick: Tick, ctx: &mut MachineCtx) -> u16 {
        let lo = self.ram_read(*ptr, tick, ctx);
        *ptr = ptr.wrapping_add(1);
        let hi = self.ram_read(*ptr, tick, ctx);
        *ptr = ptr.wrapping_add(1);
        u16::from(lo) | (u16::from(hi) << 8)
    }
}

impl LynxSuzy {
    // ----------------------------------------------------------------
    // The math unit.
    // ----------------------------------------------------------------

    /// Convert the CD operand to magnitude form when the signed math
    /// is armed, recording the sign flag. The die corner counts values
    /// from 0x8000 up as positive and 0x0000 as negative, so the
    /// conversion subtracts one first.
    fn math_convert_cd(&mut self) {
        let raw = self.pair(MATH_ABCD);
        if self.signmath && (raw.wrapping_sub(1) & 0x8000) != 0 {
            self.math_cd_sign = -1;
            let conv = (raw ^ 0xFFFF).wrapping_add(1);
            self.set_pair(MATH_ABCD, conv);
        } else {
            self.math_cd_sign = 1;
        }
    }

    /// Convert the AB operand to magnitude form when the signed math
    /// is armed, recording the sign flag.
    fn math_convert_ab(&mut self) {
        let raw = self.pair(MATH_ABCD_LOW);
        if self.signmath && (raw.wrapping_sub(1) & 0x8000) != 0 {
            self.math_ab_sign = -1;
            let conv = (raw ^ 0xFFFF).wrapping_add(1);
            self.set_pair(MATH_ABCD_LOW, conv);
        } else {
            self.math_ab_sign = 1;
        }
    }

    /// The multiply trigger: the unsigned product of the two operand
    /// words, then the sign correction and the accumulate step.
    fn math_multiply(&mut self) {
        let ab = self.pair(MATH_ABCD_LOW);
        let cd = self.pair(MATH_ABCD);
        let mut result = u32::from(ab) * u32::from(cd);
        if (self.math_ab_sign + self.math_cd_sign) == 0 {
            result = result.wrapping_neg();
        }
        self.set_word(MATH_EFGH, result);
        if self.accumulate {
            self.set_word(MATH_JKLM, self.word(MATH_JKLM).wrapping_add(result));
        }
        // The overflow warning arms when the top bit of the product is
        // set.
        self.math_warning = result >= 0x8000_0000;
    }

    /// The divide trigger: the unsigned quotient and remainder of the
    /// dividend by the divisor. The divide runs unsigned.
    fn math_divide(&mut self) {
        let divisor = self.pair(MATH_NP);
        let dividend = self.word(MATH_EFGH);
        if divisor == 0 {
            self.set_word(MATH_ABCD, 0xFFFF_FFFF);
            self.set_word(MATH_JKLM, 0);
            self.math_warning = true;
        } else {
            self.set_word(MATH_ABCD, dividend / u32::from(divisor));
            self.set_word(MATH_JKLM, dividend % u32::from(divisor));
            self.math_warning = false;
        }
    }

    /// The SPRSYS read form: the armed controls with the status bits.
    fn sprsys_read(&self) -> u8 {
        let mut value = 0u8;
        if self.ctl[C_SPRSYS] & 0x10 != 0 {
            value |= 0x10;
        }
        if self.ctl[C_SPRSYS] & 0x08 != 0 {
            value |= 0x08;
        }
        if self.math_warning {
            value |= 0x40;
        }
        if self.unsafe_access {
            value |= 0x04;
        }
        if self.stop_request {
            value |= 0x02;
        }
        if self.walk_arm {
            value |= 0x01;
        }
        value
    }

    /// The upper page write, with the multiply and divide triggers.
    fn page_write(&mut self, offset: usize, value: u8) {
        match offset {
            0x00..=0x2F => self.set_pair_byte(offset, value),
            // The reserved and do-not-use bytes of the register page.
            0x30..=0x51 => {}
            0x52 => {
                self.page[MATH_ABCD] = value;
                self.page[MATH_ABCD + 1] = 0;
                // The die converts the CD operand sign at the low byte
                // write of the pair, per the Handy implementation.
                self.math_convert_cd();
            }
            0x53 => {
                self.page[MATH_ABCD + 1] = value;
                self.math_convert_cd();
            }
            0x54 => {
                self.page[MATH_ABCD_LOW] = value;
                self.page[MATH_ABCD_LOW + 1] = 0;
            }
            0x55 => {
                self.page[MATH_ABCD_LOW + 1] = value;
                self.math_convert_ab();
                self.math_multiply();
            }
            0x56 => {
                self.page[MATH_NP] = value;
                self.page[MATH_NP + 1] = 0;
            }
            0x57 => self.page[MATH_NP + 1] = value,
            0x58..=0x5F => {}
            0x60 => {
                self.page[MATH_EFGH] = value;
                self.page[MATH_EFGH + 1] = 0;
            }
            0x61 => self.page[MATH_EFGH + 1] = value,
            0x62 => self.page[MATH_EFGH + 2] = value,
            0x63 => {
                self.page[MATH_EFGH + 3] = value;
                self.math_divide();
            }
            0x64..=0x6B => {}
            0x6C..=0x6F => self.page[offset] = value,
            _ => {}
        }
    }

    /// The upper page read.
    fn page_read(&mut self, offset: usize) -> u32 {
        match offset {
            0x00..=0x2F => u32::from(self.page[offset]),
            0x30..=0x51 => 0x00,
            0x52..=0x57 => u32::from(self.page[offset]),
            0x58..=0x5F => 0x00,
            0x60..=0x63 => u32::from(self.page[offset]),
            0x64..=0x6B => 0x00,
            0x6C..=0x6F => u32::from(self.page[offset]),
            _ => 0x00,
        }
    }

    /// The control page write.
    fn ctl_write(&mut self, offset: usize, value: u8) {
        match offset {
            0x00..=0x03 => self.ctl[offset] = value,
            C_SUZYBUSEN => self.ctl[C_SUZYBUSEN] = value,
            C_SPRGO => {
                // A write of either level clears the stop request; the
                // low bit arms or forces off the walk.
                self.walk_arm = value & 0x01 != 0;
                self.stop_request = false;
                self.everon = value & 0x04 != 0;
                self.ctl[C_SPRGO] = value;
            }
            C_SPRSYS => {
                self.ctl[C_SPRSYS] = value;
                self.signmath = value & 0x80 != 0;
                self.accumulate = value & 0x40 != 0;
                self.sys_nocollide = value & 0x20 != 0;
                self.stop_request = value & 0x02 != 0;
                if value & 0x04 != 0 {
                    self.unsafe_access = false;
                }
            }
            // The joystick and switches bytes read side only.
            C_JOYSTICK..=C_SWITCHES => {}
            C_MSTEREO => self.ctl[C_MSTEREO] = value,
            // The cart strobes, the led byte, and the parallel port.
            0x32..=0x33 | 0x40 | 0x42..=0x44 => self.ctl[offset] = value,
            _ => {}
        }
    }

    /// The control page read.
    fn ctl_read(&self, offset: usize) -> u8 {
        match offset {
            0x00..=0x03 => self.ctl[offset],
            0x08 => 0x01, // SUZYHREV
            0x09 => 0x00, // SUZYSREV: no register
            C_SUZYBUSEN => self.ctl[C_SUZYBUSEN],
            C_SPRGO => u8::from(self.walk_arm) | (u8::from(self.everon) << 2),
            C_SPRSYS => self.sprsys_read(),
            C_JOYSTICK => 0x00,
            C_SWITCHES => self.ctl[C_SWITCHES],
            0x32..=0x33 | 0x40 | 0x42..=0x44 => self.ctl[offset],
            _ => 0x00,
        }
    }
}

impl LynxSuzy {
    // ----------------------------------------------------------------
    // The sprite walk.
    // ----------------------------------------------------------------

    /// Walk the whole control block chain, painting each sprite.
    fn walk_sprites(&mut self, tick: Tick, ctx: &mut MachineCtx) {
        // The background paint: the palette entry 0 covers the build
        // surface before any sprite paints.
        self.surface.fill(0);
        let mut count = 0usize;
        while count < CHAIN_LIMIT {
            if self.pair(R_SCBNEXT) & 0xFF00 == 0 {
                // The top byte of the chain pointer ends the list.
                break;
            }
            count += 1;
            self.paint_sprite(tick, ctx);
            if self.stop_request {
                self.stop_request = false;
                break;
            }
        }
        self.walk_arm = false;
        self.refresh_screen();
        self.frames += 1;
        self.frame.pixels = self.screen.clone();
    }

    /// Load the next control block through the bus and paint its
    /// sprite. The walk leaves the loaded values in the engine
    /// registers, which is why an unsized sprite keeps the size
    /// register of its neighbor in the list.
    fn paint_sprite(&mut self, tick: Tick, ctx: &mut MachineCtx) {
        let scbadr = self.pair(R_SCBNEXT);
        let mut ptr = scbadr;
        let sprctl0 = self.ram_read(ptr, tick, ctx);
        ptr = ptr.wrapping_add(1);
        let sprctl1 = self.ram_read(ptr, tick, ctx);
        ptr = ptr.wrapping_add(1);
        let sprcoll = self.ram_read(ptr, tick, ctx);
        ptr = ptr.wrapping_add(1);
        let next_lo = self.ram_read(ptr, tick, ctx);
        let next_hi = self.ram_read(ptr.wrapping_add(1), tick, ctx);
        let next = u16::from(next_lo) | (u16::from(next_hi) << 8);
        ptr = ptr.wrapping_add(2);

        let bpp = u32::from(sprctl0 >> 6) + 1;
        let hflip = sprctl0 & 0x20 != 0;
        let vflip = sprctl0 & 0x10 != 0;
        let sprtype = sprctl0 & 0x07;
        let literal = sprctl1 & 0x80 != 0;
        let reload_depth = u32::from((sprctl1 >> 4) & 0x03);
        let reload_palette = sprctl1 & 0x08 != 0;
        let skip = sprctl1 & 0x04 != 0;
        let draw_up = sprctl1 & 0x02 != 0;
        let draw_left = sprctl1 & 0x01 != 0;
        let coll_number = sprcoll & 0x0F;
        let dont_collide = sprcoll & 0x20 != 0;
        let sys_nocollide = self.sys_nocollide;

        self.ctl[C_SPRCTL0] = sprctl0;
        self.ctl[C_SPRCTL1] = sprctl1;
        self.ctl[C_SPRCOLL] = sprcoll & 0x2F;
        self.set_pair(R_SCBNEXT, next);
        self.set_pair(R_SCBADR, scbadr);
        self.collision = 0;

        if skip {
            // A skipped sprite still walks to the next control block
            // and loads the first control bytes; nothing paints.
            return;
        }

        let sprdline = self.ram_word(&mut ptr, tick, ctx);
        self.set_pair(R_SPRDLINE, sprdline);
        let hpos = self.ram_word(&mut ptr, tick, ctx);
        self.set_pair(R_HPOSSTRT, hpos);
        let vpos = self.ram_word(&mut ptr, tick, ctx);
        self.set_pair(R_VPOSSTRT, vpos);

        // The optional register pairs behind the reload depth: the
        // sizes, then the stretch adder, then the tilt adder.
        let pairs = match reload_depth {
            0 => 0,
            1 => 2,
            2 => 3,
            _ => 4,
        };
        for address in [R_SPRHSIZ, R_SPRVSIZ, R_STRETCH, R_TILT]
            .into_iter()
            .take(pairs)
        {
            let value = self.ram_word(&mut ptr, tick, ctx);
            self.set_pair(address, value);
        }

        // The pen palette loads sixteen indices from eight bytes.
        if !reload_palette {
            for index in 0..8usize {
                let byte = self.ram_read(ptr, tick, ctx);
                ptr = ptr.wrapping_add(1);
                self.penindex[index * 2] = (byte >> 4) & 0x0F;
                self.penindex[index * 2 + 1] = byte & 0x0F;
            }
        }
        self.set_pair(R_TMPADR, ptr);

        let everon_screen = self.paint_quadrants(
            tick,
            ctx,
            bpp,
            hflip,
            vflip,
            sprtype,
            literal,
            draw_up,
            draw_left,
            coll_number,
            dont_collide,
            sys_nocollide,
            reload_depth >= 2,
            reload_depth >= 3,
        );

        // The depository write of the collideable types and the everon
        // bookkeeping byte in machine RAM.
        let depository = scbadr.wrapping_add(self.pair(R_COLLOFF));
        if collide_allowed(sprtype, dont_collide, sys_nocollide) {
            self.ram_write(depository, self.collision, tick, ctx);
        }
        if self.everon {
            let byte = self.ram_read(depository, tick, ctx);
            let byte = if everon_screen {
                byte & 0x7F
            } else {
                byte | 0x80
            };
            self.ram_write(depository, byte, tick, ctx);
        }
    }

    /// Paint the four quadrants of one sprite; returns whether any
    /// pixel of the sprite reached the display window.
    #[allow(clippy::too_many_arguments)]
    fn paint_quadrants(
        &mut self,
        tick: Tick,
        ctx: &mut MachineCtx,
        bpp: u32,
        hflip: bool,
        vflip: bool,
        sprtype: u8,
        literal: bool,
        draw_up: bool,
        draw_left: bool,
        coll_number: u8,
        dont_collide: bool,
        sys_nocollide: bool,
        stretch_on: bool,
        tilt_on: bool,
    ) -> bool {
        let screen_h_start = self.signed(R_HOFF);
        let screen_h_end = screen_h_start + COLS as i32;
        let screen_v_start = self.signed(R_VOFF);
        let screen_v_end = screen_v_start + ROWS as i32;
        let hpos_start = self.signed(R_HPOSSTRT);
        let vpos_start = self.signed(R_VPOSSTRT);
        let superclip = !(hpos_start >= screen_h_start
            && hpos_start < screen_h_end
            && vpos_start >= screen_v_start
            && vpos_start < screen_v_end);

        // The quadrant mapping: SE 0, NE 1, NW 2, SW 3, entered
        // counter-clockwise from the two direction bits.
        let mut quadrant = match (draw_left, draw_up) {
            (true, true) => 2,
            (true, false) => 3,
            (false, true) => 1,
            (false, false) => 0,
        };

        let sprite_form = SpritePixel {
            coll_number,
            dont_collide,
            sys_nocollide,
            sprtype,
        };

        let mut everon_screen = false;
        let mut data_end = false;
        let mut vquadoff = 0i32;
        let mut hquadoff = 0i32;
        let bus = self.bus.clone();

        for quad in 0..4u32 {
            // The quadrant end flag resets per quadrant: each
            // quadrant's own line stream ends on the header byte 1.
            let mut quadrant_done = false;
            let raw_hsign = if quadrant == 0 || quadrant == 1 {
                1
            } else {
                -1
            };
            let mut vsign = if quadrant == 0 || quadrant == 3 {
                1
            } else {
                -1
            };
            if vflip {
                vsign = -vsign;
            }
            let hsign = if hflip { -raw_hsign } else { raw_hsign };

            // Super-clip: only the quadrants whose directions point
            // back toward the window render their data.
            let render = if superclip {
                ((hsign == 1 && hpos_start < screen_h_end)
                    || (hsign == -1 && hpos_start >= screen_h_start))
                    && ((vsign == 1 && vpos_start < screen_v_end)
                        || (vsign == -1 && vpos_start >= screen_v_start))
            } else {
                true
            };

            if render {
                let mut voff = vpos_start - screen_v_start;
                // The stretch and tilt accumulators reset per sprite,
                // and the size accumulator seeds from VSIZOFF.
                self.set_pair(R_TILTACUM, 0);
                let vsiz_seed = if vsign == 1 { self.pair(R_VSIZOFF) } else { 0 };
                self.set_pair(R_VSIZACUM, vsiz_seed);
                if quad == 0 {
                    vquadoff = vsign;
                }
                if vsign != vquadoff {
                    voff += vsign;
                }
                while !data_end {
                    // The vertical scaling: one adder step per source
                    // line, the accumulator high byte is the repeat
                    // count and the fraction stays behind.
                    let vsiz = self.pair(R_SPRVSIZ);
                    let acc = self.pair(R_VSIZACUM).wrapping_add(vsiz);
                    let pixel_height = u32::from((acc >> 8) & 0x00FF);
                    self.set_pair(R_VSIZACUM, acc & 0x00FF);

                    // One line of data: the header byte names the byte
                    // distance to the next line's header.
                    let sprdline = self.pair(R_SPRDLINE);
                    let sprdoff = {
                        let mut reader = RamReader {
                            bus: bus.as_str(),
                            tick,
                            ctx,
                            position: sprdline,
                        };
                        let mut engine = LineEngine::new(bpp, self.penindex);
                        let sprdoff = engine.init(&mut reader, literal, bpp);
                        self.set_pair(R_SPRDOFF, sprdoff);
                        if sprdoff == 1 {
                            // The end of the quadrant: one byte steps
                            // the data line into the next quadrant.
                            self.set_pair(R_SPRDLINE, self.pair(R_SPRDLINE).wrapping_add(1));
                            quadrant_done = true;
                            1
                        } else if sprdoff == 0 {
                            data_end = true;
                            0
                        } else {
                            // The destination rows of this source line.
                            for _row in 0..pixel_height {
                                if vsign == 1 && voff >= ROWS as i32 {
                                    break;
                                }
                                if vsign == -1 && voff < 0 {
                                    break;
                                }
                                if (0..ROWS as i32).contains(&voff) {
                                    // The tilt integer joins the start
                                    // position once per destination row.
                                    let tilt = self.pair(R_TILTACUM) as i16;
                                    self.set_pair(
                                        R_HPOSSTRT,
                                        self.pair(R_HPOSSTRT).wrapping_add((tilt >> 8) as u16),
                                    );
                                    self.set_pair(R_TILTACUM, self.pair(R_TILTACUM) & 0x00FF);

                                    let mut hoff = self.signed(R_HPOSSTRT) - screen_h_start;
                                    self.hsizacum = self.pair(R_HSIZOFF);
                                    if quad == 0 {
                                        hquadoff = hsign;
                                    }
                                    if hsign != hquadoff {
                                        hoff += hsign;
                                    }

                                    // The row paints through its own
                                    // fresh decode from the line start.
                                    let mut reader = RamReader {
                                        bus: bus.as_str(),
                                        tick,
                                        ctx,
                                        position: sprdline,
                                    };
                                    let mut engine = LineEngine::new(bpp, self.penindex);
                                    engine.init(&mut reader, literal, bpp);
                                    let mut onscreen = false;
                                    loop {
                                        let pixel = engine.next_pixel(&mut reader);
                                        if pixel == LINE_END {
                                            break;
                                        }
                                        let hsiz = self.pair(R_SPRHSIZ);
                                        self.hsizacum = self.hsizacum.wrapping_add(hsiz);
                                        let pixel_width = u32::from((self.hsizacum >> 8) & 0x00FF);
                                        self.hsizacum &= 0x00FF;
                                        for _h in 0..pixel_width {
                                            if hoff >= 0 && hoff < COLS as i32 {
                                                self.process_pixel(
                                                    voff as usize,
                                                    hoff as usize,
                                                    pixel as u8,
                                                    &sprite_form,
                                                );
                                                onscreen = true;
                                                everon_screen = true;
                                            } else if onscreen {
                                                break;
                                            }
                                            hoff += hsign;
                                        }
                                    }
                                }
                                voff += vsign;
                                if stretch_on {
                                    let stretch = self.pair(R_STRETCH);
                                    self.set_pair(
                                        R_SPRHSIZ,
                                        self.pair(R_SPRHSIZ).wrapping_add(stretch),
                                    );
                                }
                                if tilt_on {
                                    let tilt = self.pair(R_TILT);
                                    self.set_pair(
                                        R_TILTACUM,
                                        self.pair(R_TILTACUM).wrapping_add(tilt),
                                    );
                                }
                            }
                            if self.ctl[C_SPRSYS] & 0x10 != 0 {
                                let stretch = self.pair(R_STRETCH);
                                let repeat = (pixel_height as u16).wrapping_mul(stretch);
                                self.set_pair(R_SPRVSIZ, self.pair(R_SPRVSIZ).wrapping_add(repeat));
                            }
                            self.set_pair(R_SPRDLINE, self.pair(R_SPRDLINE).wrapping_add(sprdoff));
                            sprdoff
                        }
                    };
                    let _ = sprdoff;
                    if quadrant_done {
                        // The quadrant's data ended at the header: the
                        // data line points at the next quadrant, and
                        // the walk advances counter-clockwise.
                        break;
                    }
                }
            } else {
                // The super-clipped quadrant skips its data, walking
                // line headers only.
                while !data_end {
                    let mut reader = RamReader {
                        bus: bus.as_str(),
                        tick,
                        ctx,
                        position: self.pair(R_SPRDLINE),
                    };
                    let mut engine = LineEngine::new(bpp, self.penindex);
                    let sprdoff = engine.init(&mut reader, literal, bpp);
                    self.set_pair(R_SPRDLINE, self.pair(R_SPRDLINE).wrapping_add(sprdoff));
                    if sprdoff == 1 {
                        break;
                    }
                    if sprdoff == 0 {
                        data_end = true;
                    }
                }
            }
            if data_end {
                break;
            }
            quadrant = (quadrant + 1) & 0x03;
        }
        everon_screen
    }

    /// Apply one pixel of one sprite to the build surface and the
    /// collision buffer, carrying the sprite type table.
    fn process_pixel(&mut self, row: usize, col: usize, pen: u8, sprite: &SpritePixel) {
        let colliding = !sprite.dont_collide && !sprite.sys_nocollide;
        let coll_number = sprite.coll_number;
        let sprtype = sprite.sprtype;
        match sprtype {
            // Background shadow: opaque pens, buffer writes, no detect.
            0 => {
                self.set_pen_at(row, col, pen);
                if colliding && pen != 0x0E {
                    self.set_collision_at(row, col, coll_number);
                }
            }
            // Background no-collision: opaque pens, no buffer access.
            1 => self.set_pen_at(row, col, pen),
            // Non-collidable: pen 0 transparent, no buffer access.
            5 => {
                if pen != 0x00 {
                    self.set_pen_at(row, col, pen);
                }
            }
            // Boundary: pens 0 and 15 transparent, collideable.
            3 => {
                if pen != 0x00 && pen != 0x0F {
                    self.set_pen_at(row, col, pen);
                }
                if pen != 0x00 && colliding {
                    self.collide_pixel(row, col, coll_number);
                }
            }
            // Normal: pen 0 transparent.
            4 => {
                if pen != 0x00 {
                    self.set_pen_at(row, col, pen);
                    if colliding {
                        self.collide_pixel(row, col, coll_number);
                    }
                }
            }
            // Boundary shadow: pens 0, 14 and 15 transparent.
            2 => {
                if pen != 0x00 && pen != 0x0E && pen != 0x0F {
                    self.set_pen_at(row, col, pen);
                }
                if pen != 0x00 && pen != 0x0E && colliding {
                    self.collide_pixel(row, col, coll_number);
                }
            }
            // Shadow: pen 0 transparent, pen 14 non-collideable.
            7 => {
                if pen != 0x00 {
                    self.set_pen_at(row, col, pen);
                }
                if pen != 0x00 && pen != 0x0E && colliding {
                    self.collide_pixel(row, col, coll_number);
                }
            }
            // Exclusive-or shadow: pen 0 transparent.
            6 if pen != 0x00 => {
                let target = self.pen_at(row, col) ^ (pen & PEN_MASK);
                self.set_pen_at(row, col, target);
                if pen != 0x0E && colliding {
                    self.collide_pixel(row, col, coll_number);
                }
            }
            _ => {}
        }
    }

    /// Read a collision cell, keep the highest number seen, and store
    /// this sprite's number.
    fn collide_pixel(&mut self, row: usize, col: usize, coll_number: u8) {
        let cell = self.collision_at(row, col);
        if cell > self.collision {
            self.collision = cell;
        }
        self.set_collision_at(row, col, coll_number);
    }
}

/// The per-sprite parameters the pixel step needs.
struct SpritePixel {
    coll_number: u8,
    dont_collide: bool,
    sys_nocollide: bool,
    sprtype: u8,
}

/// The sprite types the collision detect and deposit path covers.
fn collide_allowed(sprtype: u8, dont_collide: bool, sys_nocollide: bool) -> bool {
    matches!(sprtype, 2 | 3 | 4 | 6 | 7) && !dont_collide && !sys_nocollide
}

impl LynxSuzy {
    // ----------------------------------------------------------------
    // The audio engine.
    // ----------------------------------------------------------------

    /// The reload period of one channel in master ticks, resolving the
    /// link clock against the previous channel. Channel 0's link
    /// source is timer 7 outside this device, so it takes channel 3's
    /// period, and a chain of linked clocks stops after four hops.
    fn channel_period(&self, index: usize) -> u32 {
        self.channel_period_hops(index, 0)
    }

    fn channel_period_hops(&self, index: usize, hops: u32) -> u32 {
        let ch = &self.channels[index];
        if ch.control & 0x07 == 0x07 && hops < 4 {
            let prev = if index == 0 { 3 } else { index - 1 };
            self.channel_period_hops(prev, hops + 1)
        } else {
            PwmChannel::base_period(ch.control & 0x07, ch.backup, ch.reloading())
        }
    }

    /// Advance one channel's countdown and shift its polynomial
    /// register at every expiry.
    fn advance_channel(&mut self, index: usize, ticks: u32) {
        if !self.channels[index].counting() {
            return;
        }
        let period = self.channel_period(index);
        let ch = &mut self.channels[index];
        let mut remain = if ch.remain == 0 { period } else { ch.remain };
        let mut left = ticks;
        while left >= remain {
            left -= remain;
            // The polynomial shift: the selected taps exclusive-or and
            // the inverted result feeds the shift input at bit 11 and
            // drives the DAC.
            let reg = ch.shift();
            let mut taps = 0u16;
            for shift in 0..6usize {
                if ch.feedback & (1 << shift) != 0 {
                    taps |= 1 << shift;
                }
            }
            if ch.feedback & 0x40 != 0 {
                taps |= 1 << 10;
            }
            if ch.feedback & 0x80 != 0 {
                taps |= 1 << 11;
            }
            if ch.control & 0x80 != 0 {
                taps |= 1 << 7;
            }
            let mut parity = 0u16;
            for b in 0..12u16 {
                parity ^= (reg >> b) & (taps >> b) & 1;
            }
            let emit = parity == 0;
            if ch.integrating() {
                // The integrate mode accumulates the seven-bit volume
                // magnitude and clips at the byte bounds.
                let volume = i32::from(ch.volume & 0x7F);
                let delta = if emit { volume } else { -volume };
                let total = i32::from(ch.output as i8) + delta;
                ch.output = total.clamp(-128, 127) as u8;
            } else {
                ch.output = if emit {
                    ch.volume
                } else {
                    (!ch.volume).wrapping_add(1)
                };
            }
            ch.set_shift((reg >> 1) | (u16::from(emit) << 11));
            ch.other |= 0x01;
            remain = period;
        }
        ch.remain = remain - left;
    }

    /// The mixed mono sample of one pulse-width tick.
    fn audio_sample(&mut self) {
        let mute = self.ctl[C_MSTEREO];
        let mut mix = 0i32;
        for (i, ch) in self.channels.iter().enumerate() {
            // One bit disables one ear; the mono output keeps the
            // channel while either ear stays connected.
            let left_off = (mute >> (4 + i)) & 1 == 1;
            let right_off = (mute >> i) & 1 == 1;
            if left_off && right_off {
                continue;
            }
            mix += i32::from(ch.output as i8);
        }
        self.samples.push((mix * 64) as i16);
    }
}

/// The prescaler of one clock select in master ticks; the link clock
/// selects raw master ticks.
fn prescale_of(select: u8) -> u32 {
    if select == 0x07 {
        1
    } else {
        16u32 << u32::from(select).min(6)
    }
}

impl LynxSuzy {
    /// One audio channel field write.
    fn audio_write(&mut self, index: usize, field: usize, value: u8) {
        match field {
            0 => self.channels[index].volume = value,
            1 => self.channels[index].feedback = value,
            2 => self.channels[index].output = value,
            3 => self.channels[index].shift_lo = value,
            4 => {
                // The backup write re-arms the countdown.
                self.channels[index].backup = value;
                self.channels[index].remain = self.channel_period(index);
            }
            5 => self.channels[index].control = value,
            6 => {
                // The counter write loads the visible count at the
                // channel's resolved period, the link form included.
                let period = self.channel_period(index);
                self.channels[index].remain = u32::from(value) * period;
            }
            _ => self.channels[index].other = value,
        }
    }

    /// One audio channel field read.
    fn audio_read(&self, index: usize, field: usize) -> u8 {
        let ch = &self.channels[index];
        match field {
            0 => ch.volume,
            1 => ch.feedback,
            2 => ch.output,
            3 => ch.shift_lo,
            4 => ch.backup,
            5 => ch.control,
            6 => {
                // The counter readback: the remaining master ticks
                // divided by the prescaler, clamped to the byte.
                let p = prescale_of(ch.control & 0x07);
                let count = ch.remain.div_ceil(p);
                count.min(0xFF) as u8
            }
            _ => ch.other,
        }
    }
}

impl BusAccess for LynxSuzy {
    fn read(&mut self, _width: AccessWidth, address: Address, _tick: Tick) -> u32 {
        let raw = address.0;
        match raw {
            PAGE..=0xFCFF => {
                let offset = (raw - PAGE) as usize;
                if offset < 0x80 {
                    self.page_read(offset)
                } else {
                    u32::from(self.ctl_read(offset - 0x80))
                }
            }
            0xFD20..=0xFD3F => {
                let index = ((raw - AUDIO_PAGE) / 8) as usize;
                let field = ((raw - AUDIO_PAGE) % 8) as usize;
                if index < self.channels.len() {
                    u32::from(self.audio_read(index, field))
                } else {
                    0xFF
                }
            }
            0xFD50 => u32::from(self.ctl[C_MSTEREO]),
            0xFDA0..=0xFDBF => {
                let offset = (raw - PALETTE_PAGE) as usize;
                if offset < 16 {
                    u32::from(self.palette_green[offset])
                } else {
                    u32::from(self.palette_bluered[offset - 16])
                }
            }
            _ => 0xFF,
        }
    }

    fn write(&mut self, _width: AccessWidth, address: Address, value: u32, _tick: Tick) {
        let raw = address.0;
        let value = value as u8;
        match raw {
            PAGE..=0xFCFF => {
                let offset = (raw - PAGE) as usize;
                if offset < 0x80 {
                    self.page_write(offset, value);
                } else {
                    self.ctl_write(offset - 0x80, value);
                }
            }
            0xFD20..=0xFD3F => {
                let index = ((raw - AUDIO_PAGE) / 8) as usize;
                let field = ((raw - AUDIO_PAGE) % 8) as usize;
                if index < self.channels.len() {
                    self.audio_write(index, field, value);
                }
            }
            0xFD50 => self.ctl[C_MSTEREO] = value,
            0xFDA0..=0xFDBF => {
                let offset = (raw - PALETTE_PAGE) as usize;
                if offset < 16 {
                    self.palette_green[offset] = value;
                } else {
                    self.palette_bluered[offset - 16] = value;
                }
            }
            _ => {}
        }
    }

    fn peek(&self, address: Address) -> u8 {
        let raw = address.0;
        match raw {
            PAGE..=0xFCFF => {
                let offset = (raw - PAGE) as usize;
                if offset < 0x80 {
                    match offset {
                        0x00..=0x2F | 0x52..=0x57 | 0x60..=0x63 | 0x6C..=0x6F => self.page[offset],
                        _ => 0x00,
                    }
                } else {
                    self.ctl_read(offset - 0x80)
                }
            }
            0xFD20..=0xFD3F => self.audio_read(
                ((raw - AUDIO_PAGE) / 8) as usize,
                ((raw - AUDIO_PAGE) % 8) as usize,
            ),
            0xFD50 => self.ctl[C_MSTEREO],
            0xFDA0..=0xFDBF => {
                let offset = (raw - PALETTE_PAGE) as usize;
                if offset < 16 {
                    self.palette_green[offset]
                } else {
                    self.palette_bluered[offset - 16]
                }
            }
            _ => 0x00,
        }
    }

    fn decodes(&self, _address: Address) -> bool {
        // The machine RAM view gate: while the suzy_ram line drives
        // high, the RAM underneath answers instead.
        !self.ram_view
    }
}

impl VideoSource for LynxSuzy {
    fn frame(&self) -> Option<&Frame> {
        Some(&self.frame)
    }

    fn dump_layer(&self, layer: VideoLayer) -> Option<Vec<u8>> {
        match layer {
            VideoLayer::Palette => {
                let mut out = Vec::with_capacity(32);
                out.extend_from_slice(&self.palette_green);
                out.extend_from_slice(&self.palette_bluered);
                Some(out)
            }
            VideoLayer::Framebuffer => Some(self.screen.clone()),
            _ => None,
        }
    }

    fn frames(&self) -> u64 {
        self.frames
    }
}

impl Device for LynxSuzy {
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

    fn audio(&self) -> Option<&dyn demu_core::AudioSource> {
        Some(self)
    }

    fn reset(&mut self) {
        self.page = [0; 0x80];
        self.ctl = [0; 0x80];
        self.ctl[C_SWITCHES] = 0x04;
        for (i, pen) in self.penindex.iter_mut().enumerate() {
            *pen = i as u8;
        }
        self.hsizacum = 0x007F;
        self.collision = 0;
        self.line = 0;
        self.walk_arm = false;
        self.everon = false;
        self.unsafe_access = false;
        self.stop_request = false;
        self.math_warning = false;
        self.math_ab_sign = 1;
        self.math_cd_sign = 1;
        self.signmath = false;
        self.accumulate = false;
        self.sys_nocollide = false;
        self.channels = [
            PwmChannel::new(),
            PwmChannel::new(),
            PwmChannel::new(),
            PwmChannel::new(),
        ];
        self.sample_acc = 0;
        self.palette_green = [0; 16];
        self.palette_bluered = [0; 16];
        self.surface.fill(0);
        self.collbuf.fill(0);
        self.samples.clear();
        self.frames = 0;
        self.refresh_screen();
        self.frame.pixels = self.screen.clone();
        self.next = None;
        self.last_tick = None;
    }

    fn boot(&mut self, _state: Option<&BootState>) {
        self.reset();
        self.next = Some(Tick(0));
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(SUZY_SNAPSHOT_LEN);
        out.extend_from_slice(b"LSUZ");
        out.push(1);
        out.extend_from_slice(&self.page);
        out.extend_from_slice(&self.ctl);
        out.extend_from_slice(&self.penindex);
        out.extend_from_slice(&self.hsizacum.to_le_bytes());
        out.push(self.collision);
        out.extend_from_slice(&self.line.to_le_bytes());
        out.push(u8::from(self.ram_view));
        out.push(u8::from(self.walk_arm));
        out.push(u8::from(self.everon));
        out.push(u8::from(self.unsafe_access));
        out.push(u8::from(self.stop_request));
        out.push(u8::from(self.math_warning));
        out.push(u8::from(self.math_ab_sign < 0));
        out.push(u8::from(self.math_cd_sign < 0));
        out.push(u8::from(self.signmath));
        out.push(u8::from(self.accumulate));
        out.push(u8::from(self.sys_nocollide));
        for ch in &self.channels {
            out.push(ch.volume);
            out.push(ch.feedback);
            out.push(ch.output);
            out.push(ch.shift_lo);
            out.push(ch.backup);
            out.push(ch.control);
            out.push(ch.other);
            out.extend_from_slice(&ch.remain.to_le_bytes());
        }
        out.extend_from_slice(&self.sample_acc.to_le_bytes());
        out.extend_from_slice(&self.palette_green);
        out.extend_from_slice(&self.palette_bluered);
        out.extend_from_slice(&self.frames.to_le_bytes());
        out.extend_from_slice(&self.surface);
        out.extend_from_slice(&self.collbuf);
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        if data.len() != SUZY_SNAPSHOT_LEN || &data[..4] != b"LSUZ" || data[4] != 1 {
            return Err(SnapshotError::Mismatch(format!(
                "lynx Suzy chunk must be {SUZY_SNAPSHOT_LEN} bytes"
            )));
        }
        let mut pos = 5usize;
        self.page.copy_from_slice(&data[pos..pos + 0x80]);
        pos += 0x80;
        self.ctl.copy_from_slice(&data[pos..pos + 0x80]);
        pos += 0x80;
        self.penindex.copy_from_slice(&data[pos..pos + 16]);
        pos += 16;
        self.hsizacum = u16::from_le_bytes(data[pos..pos + 2].try_into().expect("2 bytes"));
        pos += 2;
        self.collision = data[pos];
        pos += 1;
        self.line = u64::from_le_bytes(data[pos..pos + 8].try_into().expect("8 bytes"));
        pos += 8;
        self.ram_view = data[pos] != 0;
        pos += 1;
        self.walk_arm = data[pos] != 0;
        pos += 1;
        self.everon = data[pos] != 0;
        pos += 1;
        self.unsafe_access = data[pos] != 0;
        pos += 1;
        self.stop_request = data[pos] != 0;
        pos += 1;
        self.math_warning = data[pos] != 0;
        pos += 1;
        self.math_ab_sign = if data[pos] != 0 { -1 } else { 1 };
        pos += 1;
        self.math_cd_sign = if data[pos] != 0 { -1 } else { 1 };
        pos += 1;
        self.signmath = data[pos] != 0;
        pos += 1;
        self.accumulate = data[pos] != 0;
        pos += 1;
        self.sys_nocollide = data[pos] != 0;
        pos += 1;
        for ch in self.channels.iter_mut() {
            ch.volume = data[pos];
            pos += 1;
            ch.feedback = data[pos];
            pos += 1;
            ch.output = data[pos];
            pos += 1;
            ch.shift_lo = data[pos];
            pos += 1;
            ch.backup = data[pos];
            pos += 1;
            ch.control = data[pos];
            pos += 1;
            ch.other = data[pos];
            pos += 1;
            ch.remain = u32::from_le_bytes(data[pos..pos + 4].try_into().expect("4 bytes"));
            pos += 4;
        }
        self.sample_acc = u64::from_le_bytes(data[pos..pos + 8].try_into().expect("8 bytes"));
        pos += 8;
        self.palette_green.copy_from_slice(&data[pos..pos + 16]);
        pos += 16;
        self.palette_bluered.copy_from_slice(&data[pos..pos + 16]);
        pos += 16;
        self.frames = u64::from_le_bytes(data[pos..pos + 8].try_into().expect("8 bytes"));
        pos += 8;
        self.surface
            .copy_from_slice(&data[pos..pos + ROWS * LINE_BYTES]);
        pos += ROWS * LINE_BYTES;
        self.collbuf
            .copy_from_slice(&data[pos..pos + ROWS * LINE_BYTES]);
        // The composed screen rebuilds from the restored pen surface
        // and the restored palette.
        self.refresh_screen();
        self.frame.pixels = self.screen.clone();
        self.samples.clear();
        self.next = None;
        self.last_tick = None;
        Ok(())
    }

    fn notify_line(&mut self, line: &str, level: Level, _tick: Tick) {
        match line {
            // The register view gate of the real die: high drives the
            // machine RAM view.
            "suzy_ram" => self.ram_view = level == Level::High,
            // The CPU sleep line holds nothing in this build.
            "sleep" => {}
            _ => {}
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
        if self.walk_arm {
            self.walk_sprites(tick, ctx);
        }
        // The audio engine: the 159 microsecond line carries fixed
        // 1024-master-tick pulse-width steps.
        let mut acc = self.sample_acc + LINE_MASTER;
        while acc >= SAMPLE_MASTER {
            acc -= SAMPLE_MASTER;
            for i in 0..4usize {
                self.advance_channel(i, SAMPLE_MASTER as u32);
            }
            if self.samples.len() >= SAMPLE_CAP {
                self.samples.clear();
            }
            self.audio_sample();
        }
        self.sample_acc = acc;
        self.line = (self.line + 1) % FRAME_LINES;
        self.next = Some(Tick(tick.0 + self.line_ticks()));
    }
}

impl demu_core::AudioSource for LynxSuzy {
    fn sample_rate(&self) -> u32 {
        15_625
    }

    fn take_samples(&mut self) -> Vec<i16> {
        std::mem::take(&mut self.samples)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use demu_config::model::{Bus as CBus, ClockDomain, Window as CWindow};
    use demu_config::newtypes::{DeviceName, KindName};
    use demu_core::machine::Machine;
    use std::collections::BTreeMap;

    /// The device entry of the sprite test machine.
    fn suzy_instance() -> DeviceInstance {
        let mut params = BTreeMap::new();
        params.insert("bus".to_string(), toml::Value::String("cpu".to_string()));
        DeviceInstance {
            name: DeviceName::new("suzy").expect("name"),
            kind: KindName::new("lynx.suzy").expect("kind"),
            domain: "main".to_string(),
            params,
            dividers: BTreeMap::new(),
        }
    }

    fn ram_instance() -> DeviceInstance {
        let mut params = BTreeMap::new();
        params.insert("size".to_string(), toml::Value::Integer(0x10000));
        DeviceInstance {
            name: DeviceName::new("ram").expect("name"),
            kind: KindName::new("smoke.ram").expect("kind"),
            domain: "main".to_string(),
            params,
            dividers: BTreeMap::new(),
        }
    }

    /// One bus, one RAM, one Suzy with the register pages overlayed on
    /// the machine RAM through an overlay group.
    fn suzy_config() -> SystemConfig {
        let ram = ram_instance();
        let suzy = suzy_instance();
        let mut clocks = BTreeMap::new();
        clocks.insert(
            "main".to_string(),
            ClockDomain {
                name: "main".to_string(),
                frequency: demu_config::newtypes::Frequency::new(16_000_000, 1).expect("freq"),
                origin: "the 16 MHz sprite clock".to_string(),
            },
        );
        let mut by_name = BTreeMap::new();
        by_name.insert(ram.name.clone(), 0usize);
        by_name.insert(suzy.name.clone(), 1usize);
        SystemConfig {
            name: "lynx-test".to_string(),
            triple: "atari/lynx".to_string(),
            description: "the Suzy test machine".to_string(),
            clocks,
            devices: vec![ram, suzy],
            devices_by_name: by_name,
            buses: vec![CBus {
                name: "cpu".to_string(),
                address_bits: 16,
                owner: None,
                parent: None,
            }],
            windows: vec![
                CWindow {
                    name: "ram".to_string(),
                    bus: "cpu".to_string(),
                    device: DeviceName::new("ram").expect("name"),
                    base: 0x0000,
                    size: 0x10000,
                    kind: None,
                    mirror: None,
                    overlay: None,
                    overlay_read_priority: None,
                    overlay_write_priority: None,
                    conflict: None,
                },
                CWindow {
                    name: "suzy".to_string(),
                    bus: "cpu".to_string(),
                    device: DeviceName::new("suzy").expect("name"),
                    base: 0xFC00,
                    size: 0x0200,
                    kind: None,
                    mirror: None,
                    overlay: Some("suzy".to_string()),
                    overlay_read_priority: Some(1),
                    overlay_write_priority: Some(1),
                    conflict: None,
                },
            ],
            connections: Vec::new(),
            signals: Vec::new(),
            boot: BTreeMap::new(),
            quirks: BTreeMap::new(),
        }
    }

    fn suzy_machine() -> Machine {
        let mut ctors = demu_core::smoke_ctors();
        ctors.register(
            "lynx.suzy",
            Box::new(|d, c| Ok(Box::new(LynxSuzy::new(d, c)))),
        );
        let mut machine = Machine::assemble(suzy_config(), &ctors).expect("assemble");
        machine.boot();
        machine
    }

    fn frame_bytes_of(machine: &mut Machine) -> Vec<u8> {
        let video = machine
            .device_by_name("suzy")
            .expect("device")
            .video()
            .expect("video");
        video.frame().expect("frame").pixels.clone()
    }

    fn pixel_of(bytes: &[u8], row: usize, col: usize) -> [u8; 3] {
        let base = (row * COLS + col) * 3;
        [bytes[base], bytes[base + 1], bytes[base + 2]]
    }

    /// A four-bit REHV sprite with the literal run of pens 1, 2 and 3
    /// at the position and the palette of the test.
    fn write_sprite_a(machine: &mut Machine, scb: u32, data: u32, hpos: u32, vpos: u32) {
        let w = |m: &mut Machine, a: u32, v: u8| {
            assert!(m.debug_write("cpu", Address(a), v));
        };
        w(machine, scb, 0xC0); // SPRCTL0: 4bpp, the background type
        w(machine, scb + 1, 0x10); // SPRCTL1: hsize/vsize reload
        w(machine, scb + 2, 0x04); // collision number 4
        w(machine, scb + 3, 0x00); // the next control block = 0
        w(machine, scb + 4, 0x00);
        w(machine, scb + 5, (data & 0xFF) as u8);
        w(machine, scb + 6, (data >> 8) as u8);
        w(machine, scb + 7, (hpos & 0xFF) as u8);
        w(machine, scb + 8, (hpos >> 8) as u8);
        w(machine, scb + 9, (vpos & 0xFF) as u8);
        w(machine, scb + 10, (vpos >> 8) as u8);
        w(machine, scb + 11, 0x00); // hsize = 0x0100
        w(machine, scb + 12, 0x01);
        w(machine, scb + 13, 0x00); // vsize = 0x0100
        w(machine, scb + 14, 0x01);
        w(machine, scb + 15, 0x01); // pen indices 0,1 -> 0,1
        w(machine, scb + 16, 0x23); // pen indices 2,3 -> 2,3
        for i in 17..=22u32 {
            w(machine, scb + i, 0x00);
        }
        // The data line: the offset 4, then the literal run of pens
        // 1, 2, 3 and the end packet; the bits (22):
        // 1 0011 0001 0010 0011 0 0000.
        w(machine, data, 0x04);
        w(machine, data + 1, 0x98);
        w(machine, data + 2, 0x91);
        w(machine, data + 3, 0x80);
        w(machine, data + 4, 0x00); // the next line ends the data
    }

    #[test]
    fn register_pairs_carry_the_lsb_clear_rule() {
        let mut s = LynxSuzy::new(&suzy_instance(), &suzy_config());
        s.write(AccessWidth::Byte, Address(0xFC12), 0x34, Tick(0));
        // A low-byte write clears the high byte.
        assert_eq!(s.pair(R_SPRDLINE), 0x0034);
        s.write(AccessWidth::Byte, Address(0xFC13), 0x56, Tick(0));
        assert_eq!(s.pair(R_SPRDLINE), 0x5634);
        s.write(AccessWidth::Byte, Address(0xFC14), 0x11, Tick(0));
        s.write(AccessWidth::Byte, Address(0xFC15), 0x22, Tick(0));
        assert_eq!(s.pair(R_HPOSSTRT), 0x2211);
        // The revision register reads 0x01 per the die.
        assert_eq!(s.read(AccessWidth::Byte, Address(0xFC88), Tick(0)), 0x01);
        assert_eq!(s.read(AccessWidth::Byte, Address(0xFC89), Tick(0)), 0x00);
    }

    #[test]
    fn math_unit_multiplies_and_divides() {
        let mut s = LynxSuzy::new(&suzy_instance(), &suzy_config());
        // 0x1234 times 0x5678 = 0x06260060 as the 32-bit product; the
        // product lands at $FC60-$FC63.
        s.write(AccessWidth::Byte, Address(0xFC52), 0x34, Tick(0));
        s.write(AccessWidth::Byte, Address(0xFC53), 0x12, Tick(0));
        s.write(AccessWidth::Byte, Address(0xFC54), 0x78, Tick(0));
        s.write(AccessWidth::Byte, Address(0xFC55), 0x56, Tick(0));
        assert_eq!(s.word(MATH_EFGH), 0x0626_0060);

        // The divide: the dividend at $FC60, the divisor at $FC56; the
        // H write clears G, and the E write runs the divide.
        s.write(AccessWidth::Byte, Address(0xFC60), 0xA0, Tick(0));
        s.write(AccessWidth::Byte, Address(0xFC61), 0x0A, Tick(0));
        s.write(AccessWidth::Byte, Address(0xFC62), 0x00, Tick(0));
        s.write(AccessWidth::Byte, Address(0xFC63), 0x00, Tick(0));
        s.write(AccessWidth::Byte, Address(0xFC56), 100, Tick(0));
        s.write(AccessWidth::Byte, Address(0xFC63), 0x00, Tick(0));
        // 0x0AA0 = 2720: the quotient 27 and the remainder 20.
        assert_eq!(s.word(MATH_ABCD), 27);
        assert_eq!(s.word(MATH_JKLM), 20);

        // The divide by zero arms the warning bit of the SPRSYS read.
        s.write(AccessWidth::Byte, Address(0xFC56), 0x00, Tick(0));
        s.write(AccessWidth::Byte, Address(0xFC63), 0x00, Tick(0));
        assert_ne!(s.sprsys_read() & 0x40, 0);
    }

    #[test]
    fn signed_multiply_records_the_sign_flags() {
        let mut s = LynxSuzy::new(&suzy_instance(), &suzy_config());
        s.write(AccessWidth::Byte, Address(0xFC92), 0x80, Tick(0)); // SIGNMATH
        // The operands -3 and -5: the positive product 15.
        s.write(AccessWidth::Byte, Address(0xFC52), 0xFD, Tick(0));
        s.write(AccessWidth::Byte, Address(0xFC53), 0xFF, Tick(0));
        s.write(AccessWidth::Byte, Address(0xFC54), 0xFB, Tick(0));
        s.write(AccessWidth::Byte, Address(0xFC55), 0xFF, Tick(0));
        assert_eq!(s.word(MATH_EFGH), 15);

        // One negative operand gives the negative product.
        s.write(AccessWidth::Byte, Address(0xFC92), 0x80, Tick(0));
        s.write(AccessWidth::Byte, Address(0xFC52), 0xFB, Tick(0));
        s.write(AccessWidth::Byte, Address(0xFC53), 0xFF, Tick(0));
        s.write(AccessWidth::Byte, Address(0xFC54), 0x03, Tick(0));
        s.write(AccessWidth::Byte, Address(0xFC55), 0x00, Tick(0));
        assert_eq!(s.word(MATH_EFGH), 0xFFFF_FFF1);
    }

    #[test]
    fn sprite_walk_paints_through_machine_ram() {
        let mut machine = suzy_machine();
        write_sprite_a(&mut machine, 0x2000, 0x2100, 8, 2);
        let w = |m: &mut Machine, a: u32, v: u8| {
            assert!(m.debug_write("cpu", Address(a), v));
        };
        // The palette: entry 0 green, 1 red, 2 blue, 3 dark green.
        w(&mut machine, 0xFDA0, 0x0F);
        w(&mut machine, 0xFDB0, 0x00);
        w(&mut machine, 0xFDA1, 0x00);
        w(&mut machine, 0xFDB1, 0x0F);
        w(&mut machine, 0xFDA2, 0x00);
        w(&mut machine, 0xFDB2, 0xF0);
        w(&mut machine, 0xFDA3, 0x09);
        w(&mut machine, 0xFDB3, 0x00);
        // The window offsets and the chain pointer.
        w(&mut machine, 0xFC04, 0x00);
        w(&mut machine, 0xFC05, 0x00);
        w(&mut machine, 0xFC06, 0x00);
        w(&mut machine, 0xFC07, 0x00);
        w(&mut machine, 0xFC10, 0x00);
        w(&mut machine, 0xFC11, 0x20);
        w(&mut machine, 0xFC91, 0x01); // SPRGO
        machine.run_until(Tick(LINE_MASTER * 3));

        let bytes = frame_bytes_of(&mut machine);
        // The background paints palette entry 0.
        assert_eq!(pixel_of(&bytes, 5, 50), [0, 255, 0]);
        // The sprite paints pens 1, 2, 3 at row 2, columns 8 to 10.
        assert_eq!(pixel_of(&bytes, 2, 8), [255, 0, 0]);
        assert_eq!(pixel_of(&bytes, 2, 9), [0, 0, 255]);
        assert_eq!(pixel_of(&bytes, 2, 10), [0, 153, 0]);
        // Beyond the run the background shows again.
        assert_eq!(pixel_of(&bytes, 2, 11), [0, 255, 0]);
        let video = machine
            .device_by_name("suzy")
            .expect("device")
            .video()
            .expect("video");
        assert_eq!(video.frames(), 1);
    }

    #[test]
    fn one_bit_packets_replicate_the_run_pen() {
        let mut machine = suzy_machine();
        let w = |m: &mut Machine, a: u32, v: u8| {
            assert!(m.debug_write("cpu", Address(a), v));
        };
        // A one-bit REHV sprite: one packed run of six pen-1 pixels
        // (the run paints count plus one pixels per the pinned form).
        // The pen palette maps pen index 1 to pen number 1.
        w(&mut machine, 0x2000, 0x00); // SPRCTL0: 1bpp normal
        w(&mut machine, 0x2001, 0x10); // SPRCTL1: hsize/vsize reload
        w(&mut machine, 0x2002, 0x00);
        w(&mut machine, 0x2003, 0x00); // next = 0
        w(&mut machine, 0x2004, 0x00);
        w(&mut machine, 0x2005, 0x00); // data = 0x2100
        w(&mut machine, 0x2006, 0x21);
        w(&mut machine, 0x2007, 0x04); // hpos = 4
        w(&mut machine, 0x2008, 0x00);
        w(&mut machine, 0x2009, 0x06); // vpos = 6
        w(&mut machine, 0x200A, 0x00);
        w(&mut machine, 0x200B, 0x00);
        w(&mut machine, 0x200C, 0x01);
        w(&mut machine, 0x200D, 0x00);
        w(&mut machine, 0x200E, 0x01);
        w(&mut machine, 0x200F, 0x01); // pen palette byte 0
        for a in 0x2010..=0x2016 {
            w(&mut machine, a, 0x00);
        }
        // The line: the offset 3, then the packet L=0 count=5 pen=1
        // and the end packet, the bits 0010 1100 0 0000.
        w(&mut machine, 0x2100, 0x03);
        w(&mut machine, 0x2101, 0x2C);
        w(&mut machine, 0x2102, 0x00);
        w(&mut machine, 0x2103, 0x00); // the next line ends the data
        // The palette: entry 0 dark green 15, entry 1 pale green 5.
        w(&mut machine, 0xFDA0, 0x0F);
        w(&mut machine, 0xFDB0, 0x00);
        w(&mut machine, 0xFDA1, 0x05);
        w(&mut machine, 0xFDB1, 0x00);
        w(&mut machine, 0xFC04, 0x00);
        w(&mut machine, 0xFC05, 0x00);
        w(&mut machine, 0xFC06, 0x00);
        w(&mut machine, 0xFC07, 0x00);
        w(&mut machine, 0xFC10, 0x00);
        w(&mut machine, 0xFC11, 0x20);
        w(&mut machine, 0xFC91, 0x01); // SPRGO
        machine.run_until(Tick(LINE_MASTER * 3));

        let bytes = frame_bytes_of(&mut machine);
        // The six pixels of the run paint the pen 1 color at columns
        // 4 to 9 of row 6; the background stays entry 0.
        for col in 4..10 {
            assert_eq!(pixel_of(&bytes, 6, col), [0, 85, 0]);
        }
        assert_eq!(pixel_of(&bytes, 6, 10), [0, 255, 0]);
        assert_eq!(pixel_of(&bytes, 6, 3), [0, 255, 0]);
    }

    #[test]
    fn skip_sprite_keeps_the_chain_running() {
        let mut machine = suzy_machine();
        let w = |m: &mut Machine, a: u32, v: u8| {
            assert!(m.debug_write("cpu", Address(a), v));
        };
        // The first control block carries the skip flag and chains to
        // the painted second block. The skipped block reads its first
        // control bytes and its next pointer, and nothing paints.
        w(&mut machine, 0x2000, 0xC0);
        w(&mut machine, 0x2001, 0x04); // the skip flag
        w(&mut machine, 0x2002, 0x00);
        w(&mut machine, 0x2003, 0x10); // next = 0x2010
        w(&mut machine, 0x2004, 0x20);
        write_sprite_a(&mut machine, 0x2010, 0x2100, 8, 2);
        // The palette of the four entries.
        w(&mut machine, 0xFDA0, 0x0F);
        w(&mut machine, 0xFDB0, 0x00);
        w(&mut machine, 0xFDA1, 0x00);
        w(&mut machine, 0xFDB1, 0x0F);
        w(&mut machine, 0xFDA2, 0x00);
        w(&mut machine, 0xFDB2, 0xF0);
        w(&mut machine, 0xFDA3, 0x09);
        w(&mut machine, 0xFDB3, 0x00);
        w(&mut machine, 0xFC04, 0x00);
        w(&mut machine, 0xFC05, 0x00);
        w(&mut machine, 0xFC06, 0x00);
        w(&mut machine, 0xFC07, 0x00);
        w(&mut machine, 0xFC10, 0x00);
        w(&mut machine, 0xFC11, 0x20);
        w(&mut machine, 0xFC24, 0x10); // COLLOFF = 0x10
        w(&mut machine, 0xFC91, 0x01);
        machine.run_until(Tick(LINE_MASTER * 3));

        let bytes = frame_bytes_of(&mut machine);
        // The second sprite painted exactly as the direct sprite walk.
        assert_eq!(pixel_of(&bytes, 2, 8), [255, 0, 0]);
        assert_eq!(pixel_of(&bytes, 2, 9), [0, 0, 255]);
        assert_eq!(pixel_of(&bytes, 2, 10), [0, 153, 0]);
        let video = machine
            .device_by_name("suzy")
            .expect("device")
            .video()
            .expect("video");
        assert_eq!(video.frames(), 1);
    }

    #[test]
    fn collision_deposit_records_earlier_sprite_numbers() {
        let mut machine = suzy_machine();
        let w = |m: &mut Machine, a: u32, v: u8| {
            assert!(m.debug_write("cpu", Address(a), v));
        };
        // Sprite A paints three pens over a row; sprite B chains after
        // and draws over the same cells, so B deposits A's number 4
        // while A deposits 0. The normal sprite type arms the deposit.
        // The blocks sit far enough apart that the depository writes
        // stay inside the consumed pen tables.
        w(&mut machine, 0x2000, 0xC4);
        w(&mut machine, 0x2001, 0x10);
        w(&mut machine, 0x2002, 0x04); // coll 4
        w(&mut machine, 0x2003, 0x00); // next = 0x2300
        w(&mut machine, 0x2004, 0x23);
        w(&mut machine, 0x2005, 0x00); // data = 0x2400
        w(&mut machine, 0x2006, 0x24);
        w(&mut machine, 0x2007, 0x08); // hpos 8
        w(&mut machine, 0x2008, 0x00);
        w(&mut machine, 0x2009, 0x02); // vpos 2
        w(&mut machine, 0x200A, 0x00);
        w(&mut machine, 0x200B, 0x00);
        w(&mut machine, 0x200C, 0x01);
        w(&mut machine, 0x200D, 0x00);
        w(&mut machine, 0x200E, 0x01);
        w(&mut machine, 0x200F, 0x01); // pens 1..3 -> 1,2,3
        w(&mut machine, 0x2010, 0x23);
        for a in 0x2011..=0x2016 {
            w(&mut machine, a, 0x00);
        }
        w(&mut machine, 0x2300, 0xC4);
        w(&mut machine, 0x2301, 0x10);
        w(&mut machine, 0x2302, 0x09); // coll 9
        w(&mut machine, 0x2303, 0x00); // next = 0
        w(&mut machine, 0x2304, 0x00);
        w(&mut machine, 0x2305, 0x00);
        w(&mut machine, 0x2306, 0x24); // the same data
        w(&mut machine, 0x2307, 0x08);
        w(&mut machine, 0x2308, 0x00);
        w(&mut machine, 0x2309, 0x02);
        w(&mut machine, 0x230A, 0x00);
        w(&mut machine, 0x230B, 0x00);
        w(&mut machine, 0x230C, 0x01);
        w(&mut machine, 0x230D, 0x00);
        w(&mut machine, 0x230E, 0x01);
        w(&mut machine, 0x230F, 0x01);
        w(&mut machine, 0x2310, 0x23);
        for a in 0x2311..=0x2316 {
            w(&mut machine, a, 0x00);
        }
        w(&mut machine, 0x2400, 0x04);
        w(&mut machine, 0x2401, 0x98);
        w(&mut machine, 0x2402, 0x91);
        w(&mut machine, 0x2403, 0x80);
        w(&mut machine, 0x2404, 0x00);
        w(&mut machine, 0xFC04, 0x00);
        w(&mut machine, 0xFC05, 0x00);
        w(&mut machine, 0xFC06, 0x00);
        w(&mut machine, 0xFC07, 0x00);
        w(&mut machine, 0xFC10, 0x00);
        w(&mut machine, 0xFC11, 0x20);
        w(&mut machine, 0xFC24, 0x10); // COLLOFF = 0x10
        w(&mut machine, 0xFC91, 0x01);
        machine.run_until(Tick(LINE_MASTER * 3));

        // Sprite A deposited 0 (the buffer was empty) and sprite B
        // deposited A's number 4.
        assert_eq!(machine.peek("cpu", Address(0x2010)), 0x00);
        assert_eq!(machine.peek("cpu", Address(0x2310)), 0x04);
    }

    #[test]
    fn pwm_channel_makes_deterministic_samples() {
        let run = |machine: &mut Machine| -> Vec<i16> {
            machine.run_until(Tick(LINE_MASTER * 60));
            let any = machine
                .device_by_name_mut("suzy")
                .expect("device")
                .as_any_mut();
            let suzy = any.downcast_mut::<LynxSuzy>().expect("the Suzy die");
            let audio: &mut dyn demu_core::AudioSource = suzy;
            audio.sample_rate();
            audio.take_samples()
        };
        let mut machine = suzy_machine();
        let w = |m: &mut Machine, a: u32, v: u8| {
            assert!(m.debug_write("cpu", Address(a), v));
        };
        // Channel 0: the volume 0x40, the taps bits 0 and 5, the clock
        // 1 microsecond, count and reload armed, backup 0.
        w(&mut machine, 0xFD20, 0x40);
        w(&mut machine, 0xFD21, 0x21);
        w(&mut machine, 0xFD23, 0x00);
        w(&mut machine, 0xFD24, 0x00);
        w(&mut machine, 0xFD25, 0x18);
        w(&mut machine, 0xFD27, 0x00); // the shift high nibble
        let samples = run(&mut machine);
        assert!(!samples.is_empty());
        assert!(samples.iter().any(|s| *s != 0));

        // A fresh machine with the same setup produces the same bytes.
        let mut machine = suzy_machine();
        w(&mut machine, 0xFD20, 0x40);
        w(&mut machine, 0xFD21, 0x21);
        w(&mut machine, 0xFD23, 0x00);
        w(&mut machine, 0xFD24, 0x00);
        w(&mut machine, 0xFD25, 0x18);
        w(&mut machine, 0xFD27, 0x00);
        let twin_samples = run(&mut machine);
        assert_eq!(samples.len(), twin_samples.len());
        assert_eq!(samples, twin_samples);
    }

    #[test]
    fn integrate_mode_clips_the_running_total() {
        let mut s = LynxSuzy::new(&suzy_instance(), &suzy_config());
        // With no feedback taps the polynomial emits one on every
        // reload; the integrate total climbs and clips at the byte
        // bound.
        s.channels[0].volume = 10;
        s.channels[0].control = 0x38; // integrate, reload, count, 1us
        s.channels[0].backup = 0;
        s.channels[0].feedback = 0x00;
        s.channels[0].remain = 0;
        for _ in 0..300 {
            s.advance_channel(0, 16);
        }
        assert_eq!(s.channels[0].output, 127);
        // The plain form holds the volume byte itself on every fire.
        s.channels[0].control = 0x18; // plain, reload, count, 1us
        s.channels[0].output = 0;
        for _ in 0..300 {
            s.advance_channel(0, 16);
        }
        assert_eq!(s.channels[0].output, 10);
    }

    #[test]
    fn suzy_ram_line_gates_the_register_view() {
        let mut s = LynxSuzy::new(&suzy_instance(), &suzy_config());
        assert!(s.bus().expect("bus").decodes(Address(0xFC91)));
        s.notify_line("suzy_ram", Level::High, Tick(0));
        // The RAM view active: the register page drops from the decode.
        assert!(!s.bus().expect("bus").decodes(Address(0xFC91)));
        s.notify_line("suzy_ram", Level::Low, Tick(0));
        assert!(s.bus().expect("bus").decodes(Address(0xFC91)));
        // The sleep line holds nothing but is accepted.
        s.notify_line("sleep", Level::High, Tick(0));
        assert!(s.bus().expect("bus").decodes(Address(0xFC91)));
    }

    #[test]
    fn suzy_frame_is_deterministic() {
        fn run() -> Vec<u8> {
            let mut machine = suzy_machine();
            write_sprite_a(&mut machine, 0x2000, 0x2100, 32, 48);
            let w = |m: &mut Machine, a: u32, v: u8| {
                assert!(m.debug_write("cpu", Address(a), v));
            };
            w(&mut machine, 0xFDA0, 0x05);
            w(&mut machine, 0xFC04, 0x00);
            w(&mut machine, 0xFC05, 0x00);
            w(&mut machine, 0xFC06, 0x00);
            w(&mut machine, 0xFC07, 0x00);
            w(&mut machine, 0xFC10, 0x00);
            w(&mut machine, 0xFC11, 0x20);
            w(&mut machine, 0xFC91, 0x01);
            machine.run_until(Tick(LINE_MASTER * 3));
            frame_bytes_of(&mut machine)
        }
        assert_eq!(run(), run());
    }

    #[test]
    fn the_quadrant_end_header_advances_to_the_next_quadrant() {
        let mut machine = suzy_machine();
        let w = |m: &mut Machine, a: u32, v: u8| {
            assert!(m.debug_write("cpu", Address(a), v));
        };
        // A four-bit REHV sprite of two quadrants: the first line ends
        // its quadrant on the header byte 1, and the second quadrant
        // then paints upward from its own first line.
        w(&mut machine, 0x2000, 0xC0); // SPRCTL0: 4bpp, background type
        w(&mut machine, 0x2001, 0x10); // SPRCTL1: reload depth 1, SE start
        w(&mut machine, 0x2002, 0x04); // collision number 4
        w(&mut machine, 0x2003, 0x00); // the next control block = 0
        w(&mut machine, 0x2004, 0x00);
        w(&mut machine, 0x2005, 0x00); // data = 0x2100
        w(&mut machine, 0x2006, 0x21);
        w(&mut machine, 0x2007, 0x08); // hpos = 8
        w(&mut machine, 0x2008, 0x00);
        w(&mut machine, 0x2009, 0x04); // vpos = 4
        w(&mut machine, 0x200A, 0x00);
        w(&mut machine, 0x200B, 0x00); // hsize = 0x0100
        w(&mut machine, 0x200C, 0x01);
        w(&mut machine, 0x200D, 0x00);
        w(&mut machine, 0x200E, 0x01); // vsize = 0x0100
        w(&mut machine, 0x200F, 0x01); // pen indices 0,1 -> 0,1
        w(&mut machine, 0x2010, 0x23); // pen indices 2,3 -> 2,3
        for a in 0x2011..=0x2016 {
            w(&mut machine, a, 0x00);
        }
        // The data stream: the first quadrant one line, the quadrant
        // end header, the second quadrant two lines, and the
        // end-of-data header.
        for (i, b) in [
            0x04u8, 0x98, 0x91, 0x80, 0x01, 0x04, 0x98, 0x91, 0x80, 0x04, 0x98, 0x91, 0x80, 0x00,
        ]
        .iter()
        .enumerate()
        {
            w(&mut machine, 0x2100 + i as u32, *b);
        }
        // The palette: entry 0 green, 1 red, 2 blue, 3 dark green.
        w(&mut machine, 0xFDA0, 0x0F);
        w(&mut machine, 0xFDB0, 0x00);
        w(&mut machine, 0xFDA1, 0x00);
        w(&mut machine, 0xFDB1, 0x0F);
        w(&mut machine, 0xFDA2, 0x00);
        w(&mut machine, 0xFDB2, 0xF0);
        w(&mut machine, 0xFDA3, 0x09);
        w(&mut machine, 0xFDB3, 0x00);
        // The window offsets and the walk arm.
        w(&mut machine, 0xFC04, 0x00);
        w(&mut machine, 0xFC05, 0x00);
        w(&mut machine, 0xFC06, 0x00);
        w(&mut machine, 0xFC07, 0x00);
        w(&mut machine, 0xFC10, 0x00);
        w(&mut machine, 0xFC11, 0x20);
        w(&mut machine, 0xFC91, 0x01); // SPRGO
        machine.run_until(Tick(LINE_MASTER * 3));

        let bytes = frame_bytes_of(&mut machine);
        // The second quadrant painted upward: row 3 carries the pens
        // of its first line and row 2 the pens of its second line.
        assert_eq!(pixel_of(&bytes, 3, 8), [255, 0, 0]);
        assert_eq!(pixel_of(&bytes, 3, 9), [0, 0, 255]);
        assert_eq!(pixel_of(&bytes, 3, 10), [0, 153, 0]);
        assert_eq!(pixel_of(&bytes, 2, 8), [255, 0, 0]);
        assert_eq!(pixel_of(&bytes, 2, 10), [0, 153, 0]);
        // The first quadrant painted its own row downward.
        assert_eq!(pixel_of(&bytes, 4, 8), [255, 0, 0]);
        // Row 5 stays background: the walk ended at the data end, and
        // the third quadrant never painted on the remaining stream.
        assert_eq!(pixel_of(&bytes, 5, 8), [0, 255, 0]);
        assert_eq!(pixel_of(&bytes, 1, 8), [0, 255, 0]);
    }

    #[test]
    fn snapshot_round_trips() {
        let mut s = LynxSuzy::new(&suzy_instance(), &suzy_config());
        s.write(AccessWidth::Byte, Address(0xFC12), 0xAA, Tick(0));
        s.write(AccessWidth::Byte, Address(0xFC13), 0xBB, Tick(0));
        s.write(AccessWidth::Byte, Address(0xFD20 + 5), 0x18, Tick(0));
        s.write(AccessWidth::Byte, Address(0xFDA5), 0x07, Tick(0));
        let snap = s.snapshot();
        assert_eq!(snap.len(), SUZY_SNAPSHOT_LEN);
        let mut twin = LynxSuzy::new(&suzy_instance(), &suzy_config());
        twin.restore(&snap).expect("restore");
        assert_eq!(twin.snapshot(), snap);
        assert_eq!(twin.pair(R_SPRDLINE), 0xBBAA);
        // The fourth entry green byte 0x07 scales by 17.
        assert_eq!(twin.pen_rgb(5), [0, 119, 0]);
    }

    #[test]
    fn restore_rejects_wrong_lengths() {
        let mut s = LynxSuzy::new(&suzy_instance(), &suzy_config());
        let snap = s.snapshot();
        assert!(matches!(
            s.restore(&snap[..snap.len() - 1]),
            Err(SnapshotError::Mismatch(_))
        ));
    }
}
