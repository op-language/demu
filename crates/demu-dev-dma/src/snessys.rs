//! The SNES 5A22 system controller.
//!
//! One device carries the support die of the 5A22: the control
//! registers `$4200-$420D`, the status registers `$4210-$421F`, the
//! eight general DMA channel files `$4300-$437F`, the general-purpose
//! DMA engine, the HDMA per-scanline engine, the H/V timers, and the
//! auto-joypad read sequencer. The config maps the two register
//! windows here raw; the device decodes them from the low ten address
//! bits: `0x200..0x21F` are the system registers and `0x300..0x37F`
//! are the channels at a 16-byte stride.
//!
//! Wiring:
//!
//! - `ppu.nmi` to `sys.vblank` raises and closes the VBlank region and
//!   latches the VBlank NMI on the raising edge: the shipped config
//!   wires the PPU's own NMI form there. The device holds its `nmi`
//!   line to the CPU while the latch stands and the `$4200` bit 7
//!   enables it; the latch clears on the `$4210` read. A `sys.nmi_pend`
//!   wire is the second accepted form.
//! - `sys.irq` and `sys.nmi` feed the CPU. The device holds `irq`
//!   while the TimeUp flag stands, and the `$4211` read drops it.
//! - Without a VBlank wire the device infers the region from its
//!   own line count and the PAL or NTSC parameters.
//! - `ppu.scanline` ticks the line counter: one line per high edge.
//! - `sys.halt` takes the CPU bus for each DMA burst, the ready-line
//!   form the OAM DMA and ANTIC devices use.
//! - `sys.ext_latch` pulses on the `$4201` bit 7 low edge.
//!
//! Deliberate approximations:
//!
//! - The enabling write schedules each engine action one CPU cycle
//!   later on the divider, and the action then completes inside one
//!   visit. The general DMA burst moves every byte at once between a
//!   single halt pulse; no per-byte cycle walk is modeled.
//! - The H position is tracked at line granularity only. The H timer
//!   compares against the fixed dot 144 at the per-line check, so a
//!   program whose target misses that dot gets no H timer event. The
//!   HBlank flag of `$4212` reads zero.
//! - The auto-joypad read runs as one instantaneous sequence at the
//!   `$4200` bit 0 rising edge: the latch strobe through a port
//!   write, then sixteen `$4016` reads and sixteen `$4017` reads in
//!   sequence. The hardware reads both ports per shift and repeats
//!   the whole sequence every VBlank; neither form is modeled, and the
//!   `$4212` busy flag reads zero after the visit.
//! - The general DMA moves full units only: a byte count that is not
//!   a multiple of the unit size leaves the last bytes undone and the
//!   `$43n5/6` register keeps them. The A address steps per byte and
//!   wraps within its 16-bit pointer without carrying into the bank.
//! - HDMA tables freeze over VBlank and resume on the first active
//!   line. A fetched table header in `01`-`80` moves one unit and
//!   pauses `N-1` lines; a header in `81`-`FF` moves `N-80` units on
//!   consecutive lines, the fetch line included. A header of `00`
//!   ends the table and auto-clears the enable bit. The tables walk
//!   channels 0 through 7 in order; no finer channel priority is
//!   modeled. A channel re-arms on the `$420C` bit rising edge and
//!   reloads its table pointer from A1T.
//! - The `$4210` read-back version nibble is 0x02. The `$4200-$420D`
//!   write-only registers read back the bytes the machine stored
//!   rather than open bus, which keeps the debug interface
//!   deterministic.

use demu_config::model::{BootState, DeviceInstance};
use demu_core::device::{AccessWidth, BusAccess, MachineCtx};
use demu_core::frequency::{Address, Tick};
use demu_core::signal::Level;
use demu_core::triggers::{DmaPhase, TriggerEvent};
use demu_core::{Device, Snapshot, SnapshotError};

/// The DMA channels the engine carries.
const CHANNELS: usize = 8;
/// The fixed dot the line-granular H timer check compares against.
const H_APPROX: u16 = 144;
/// The controller port 1 data register.
const JOY1_PORT: u32 = 0x004016;
/// The controller port 2 data register.
const JOY2_PORT: u32 = 0x004017;
/// The start of the B-bus page.
const B_BASE: u32 = 0x2100;
/// The snapshot chunk length.
const SNAP_LENGTH: usize =
    4 + 1 + 4 + 2 + 1 + 2 + 2 + 3 + 2 + 2 + 4 + 7 + 4 + 4 + 4 + 8 + 4 + CHANNELS * 14;

/// The B-port offsets of one DMA unit, by transfer mode. A mode of 6
/// or 7 behaves as the mode 2 or 3 form.
const B_PATTERN: [[u8; 4]; 8] = [
    [0, 0, 0, 0],
    [0, 1, 0, 0],
    [0, 0, 0, 0],
    [0, 0, 1, 1],
    [0, 1, 2, 3],
    [0, 1, 0, 1],
    [0, 0, 0, 0],
    [0, 0, 1, 1],
];
/// The unit size in bytes, by transfer mode.
const UNIT_SIZE: [u8; 8] = [1, 2, 2, 4, 4, 4, 2, 4];

/// The byte-granular bus view the engines drive.
///
/// The machine path routes a [`CtxIo`] through the bus decoder; the
/// device tests hand their own plain implementations. One type covers
/// the read and write halves because a transfer interleaves them per
/// byte and the two cannot hold separate mutable borrows.
trait BusIo {
    /// One byte read at the absolute address.
    fn read(&mut self, address: u32) -> u8;
    /// One byte write at the absolute address.
    fn write(&mut self, address: u32, value: u8);
}

/// The machine-context bus view: every access lands on the named bus
/// at the visit tick.
struct CtxIo<'ctx, 'bus> {
    ctx: &'ctx mut MachineCtx<'bus>,
    bus: String,
    tick: Tick,
}

impl BusIo for CtxIo<'_, '_> {
    fn read(&mut self, address: u32) -> u8 {
        self.ctx
            .read(&self.bus, AccessWidth::Byte, Address(address), self.tick) as u8
    }

    fn write(&mut self, address: u32, value: u8) {
        self.ctx.write(
            &self.bus,
            AccessWidth::Byte,
            Address(address),
            u32::from(value),
            self.tick,
        );
    }
}

/// One DMA channel's register file and HDMA walk state.
#[derive(Debug, Clone, Copy, Default)]
struct Channel {
    dmap: u8,
    bbad: u8,
    a1t: u16,
    a1tb: u8,
    das: u16,
    dasb: u8,
    a2: u16,
    /// The line counter the HDMA walks: bits 6 to 0 count the lines
    /// left before the next header fetch and bit 7 marks the repeat
    /// form.
    ntrl: u8,
    /// The current HDMA table pointer inside the A1T bank.
    table: u16,
    /// The table reached its 00 terminator.
    done: bool,
    /// The next scanline fetches a table header.
    fetch: bool,
}

/// The 5A22 system controller.
#[derive(Debug)]
pub struct SnesSys {
    name: String,
    /// The bus the DMA transfers and the auto-joy reads walk.
    bus_name: String,
    /// Domain ticks per CPU cycle.
    cpu_div: u64,
    /// Scanlines of the frame: 262 NTSC, 312 PAL.
    lines: u32,
    /// The first VBlank line: 224 NTSC, 240 PAL.
    vblank_start: u32,

    // The $4200-$420D control file.
    nmitimen: u8,
    wrio: u8,
    mult_a: u8,
    mult_b: u8,
    dividend: u16,
    divisor: u8,
    htime: u16,
    vtime: u16,
    mdmaen: u8,
    hdmaen: u8,
    memsel: u8,

    // The results and status forms.
    quotient: u16,
    /// The multiplication product, or the division remainder.
    result: u16,
    /// The latched VBlank NMI: the `$4210` bit 7 and the NMI service
    /// latch in one flag.
    nmi_latch: bool,
    /// The TimeUp flag of `$4211` bit 7.
    timeup_flag: bool,
    /// The levels currently driven on the CPU's lines.
    nmi_out: bool,
    irq_out: bool,
    /// The VBlank region of `$4212` bit 7.
    in_vblank: bool,
    /// True once a `vblank` line notify arrived; the region then
    /// follows the PPU's edges instead of the line count.
    vblank_wired: bool,
    /// The auto-joypad read stands busy.
    joy_busy: bool,
    joy: [u16; 2],

    /// The line counter, one V per frame.
    v: u32,

    ch: [Channel; CHANNELS],

    // The work the next visit drains.
    dma_burst: bool,
    joy_read: bool,
    line_work: bool,
    ext_strobe: bool,

    next: Option<Tick>,
    last_tick: Option<Tick>,
}

impl SnesSys {
    /// A system controller from its configuration entry.
    pub fn new(device: &DeviceInstance) -> Self {
        let pal = device
            .params
            .get("pal")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        Self {
            name: device.name.to_string(),
            bus_name: device
                .params
                .get("bus")
                .and_then(|v| v.as_str())
                .unwrap_or("cpu")
                .to_string(),
            cpu_div: u64::from(device.dividers.get("cpu").copied().unwrap_or(6)).max(1),
            lines: if pal { 312 } else { 262 },
            vblank_start: if pal { 240 } else { 224 },
            nmitimen: 0,
            wrio: 0,
            mult_a: 0,
            mult_b: 0,
            dividend: 0,
            divisor: 0,
            htime: 0,
            vtime: 0,
            mdmaen: 0,
            hdmaen: 0,
            memsel: 0,
            quotient: 0,
            result: 0,
            nmi_latch: false,
            timeup_flag: false,
            nmi_out: false,
            irq_out: false,
            in_vblank: false,
            vblank_wired: false,
            joy_busy: false,
            joy: [0; 2],
            v: 0,
            ch: [Channel::default(); CHANNELS],
            dma_burst: false,
            joy_read: false,
            line_work: false,
            ext_strobe: false,
            next: None,
            last_tick: None,
        }
    }

    /// The system-register index of an absolute address: the low ten
    /// bits, in the `0x200..0x21F` copy of the `$4200-$421F` file.
    fn reg_index(&self, raw: u32) -> Option<usize> {
        match raw & 0x3FF {
            0x200..=0x21F => Some((raw & 0x3FF) as usize - 0x200),
            _ => None,
        }
    }

    /// The channel number and register index of an absolute address,
    /// from the `0x300..0x37F` copy of the `$4300-$437F` files.
    fn chan_index(&self, raw: u32) -> Option<(usize, usize)> {
        match raw & 0x3FF {
            0x300..=0x37F => {
                let off = (raw & 0x3FF) - 0x300;
                Some(((off / 16) as usize, (off % 16) as usize))
            }
            _ => None,
        }
    }

    /// The B-port address of one unit's byte: the port page plus the
    /// mode's offset, carried within the page.
    fn b_addr(mode: usize, bbad: u8, sub: usize) -> Address {
        Address(B_BASE | u32::from(bbad.wrapping_add(B_PATTERN[mode][sub])))
    }

    /// The A-bus address from a bank with a 16-bit pointer.
    fn a_addr(bank: u8, ptr: u16) -> Address {
        Address((u32::from(bank) << 16) | u32::from(ptr))
    }

    /// The A-address step per byte, per the hardware bit positions:
    /// bit 5 fixes the address and bit 4 decrements it.
    fn a_step(dmap: u8) -> u16 {
        if dmap & 0x20 != 0 {
            0
        } else if dmap & 0x10 != 0 {
            u16::MAX
        } else {
            1
        }
    }

    /// Re-visit at `at` unless an earlier visit stands scheduled.
    fn wake_at(&mut self, at: Tick) {
        if self.next.is_none_or(|n| n.0 > at.0) {
            self.next = Some(at);
        }
    }

    /// Read a system register, with the acknowledge side effects of
    /// the status registers.
    fn sys_read(&mut self, idx: usize, tick: Tick) -> u8 {
        match idx {
            0x10 => {
                let out = 0x02 | (u8::from(self.nmi_latch) << 7);
                self.nmi_latch = false;
                self.wake_at(Tick(tick.0 + 1));
                out
            }
            0x11 => {
                let out = u8::from(self.timeup_flag) << 7;
                self.timeup_flag = false;
                self.wake_at(Tick(tick.0 + 1));
                out
            }
            _ => self.sys_peek(idx),
        }
    }

    /// A system register without read side effects.
    fn sys_peek(&self, idx: usize) -> u8 {
        match idx {
            0x00 => self.nmitimen,
            0x01 => self.wrio,
            0x02 => self.mult_a,
            0x03 => self.mult_b,
            0x04 => self.dividend as u8,
            0x05 => (self.dividend >> 8) as u8,
            0x06 => self.divisor,
            0x07 => self.htime as u8,
            0x08 => (self.htime >> 8) as u8,
            0x09 => self.vtime as u8,
            0x0A => (self.vtime >> 8) as u8,
            0x0B => self.mdmaen,
            0x0C => self.hdmaen,
            0x0D => self.memsel,
            0x0E | 0x0F => 0xFF,
            0x10 => 0x02 | (u8::from(self.nmi_latch) << 7),
            0x11 => u8::from(self.timeup_flag) << 7,
            0x12 => {
                let mut out = 0u8;
                if self.in_vblank {
                    out |= 0x80;
                }
                if self.joy_busy {
                    out |= 0x01;
                }
                out
            }
            0x13 => 0,
            0x14 => self.quotient as u8,
            0x15 => (self.quotient >> 8) as u8,
            0x16 => self.result as u8,
            0x17 => (self.result >> 8) as u8,
            0x18 => self.joy[0] as u8,
            0x19 => (self.joy[0] >> 8) as u8,
            0x1A => self.joy[1] as u8,
            0x1B => (self.joy[1] >> 8) as u8,
            _ => 0,
        }
    }

    /// Write a system register. The status side of the file is
    /// write-ignored.
    fn sys_write(&mut self, idx: usize, v: u8, tick: Tick) {
        match idx {
            0x00 => {
                let was_joy = self.nmitimen & 0x01 != 0;
                self.nmitimen = v;
                if !was_joy && v & 0x01 != 0 {
                    self.joy_read = true;
                    self.joy_busy = true;
                    self.wake_at(Tick(tick.0 + self.cpu_div));
                }
                // The NMI gate re-evaluates at the next visit.
                self.wake_at(Tick(tick.0 + 1));
            }
            0x01 => {
                let was_ext = self.wrio & 0x80 != 0;
                self.wrio = v;
                if was_ext && v & 0x80 == 0 {
                    self.ext_strobe = true;
                    self.wake_at(Tick(tick.0 + 1));
                }
            }
            0x02 => self.mult_a = v,
            0x03 => {
                self.mult_b = v;
                self.result = u16::from(self.mult_a) * u16::from(self.mult_b);
            }
            0x04 => self.dividend = (self.dividend & 0xFF00) | u16::from(v),
            0x05 => self.dividend = (self.dividend & 0x00FF) | (u16::from(v) << 8),
            0x06 => {
                self.divisor = v;
                if v == 0 {
                    // The documented form: the quotient saturates and
                    // the remainder answers with the dividend.
                    self.quotient = 0xFFFF;
                    self.result = self.dividend;
                } else {
                    self.quotient = self.dividend / u16::from(v);
                    self.result = self.dividend % u16::from(v);
                }
            }
            0x07 => self.htime = (self.htime & 0xFF00) | u16::from(v),
            0x08 => self.htime = (self.htime & 0x00FF) | (u16::from(v & 0x03) << 8),
            0x09 => self.vtime = (self.vtime & 0xFF00) | u16::from(v),
            0x0A => self.vtime = (self.vtime & 0x00FF) | (u16::from(v & 0x03) << 8),
            0x0B => {
                self.mdmaen = v;
                if v != 0 {
                    self.dma_burst = true;
                    self.wake_at(Tick(tick.0 + self.cpu_div));
                }
            }
            0x0C => {
                for c in 0..CHANNELS {
                    let was = self.hdmaen >> c & 1 != 0;
                    if !was && v >> c & 1 != 0 {
                        // The channel re-arms: the table pointer
                        // reloads from A1T and the first fetch waits
                        // for the next scanline.
                        let ch = &mut self.ch[c];
                        ch.table = ch.a1t;
                        ch.done = false;
                        ch.fetch = true;
                        ch.ntrl = 0;
                    }
                }
                self.hdmaen = v;
            }
            0x0D => self.memsel = v,
            _ => {}
        }
    }

    /// Read a DMA channel register.
    fn chan_read(&self, c: usize, r: usize) -> u8 {
        let ch = &self.ch[c];
        match r {
            0 => ch.dmap,
            1 => ch.bbad,
            2 => ch.a1t as u8,
            3 => (ch.a1t >> 8) as u8,
            4 => ch.a1tb,
            5 => ch.das as u8,
            6 => (ch.das >> 8) as u8,
            7 => ch.dasb,
            8 => ch.a2 as u8,
            9 => (ch.a2 >> 8) as u8,
            0xA => ch.ntrl,
            _ => 0xFF,
        }
    }

    /// Write a DMA channel register.
    fn chan_write(&mut self, c: usize, r: usize, v: u8) {
        let ch = &mut self.ch[c];
        match r {
            0 => ch.dmap = v,
            1 => ch.bbad = v,
            2 => ch.a1t = (ch.a1t & 0xFF00) | u16::from(v),
            3 => ch.a1t = (ch.a1t & 0x00FF) | (u16::from(v) << 8),
            4 => ch.a1tb = v,
            5 => ch.das = (ch.das & 0xFF00) | u16::from(v),
            6 => ch.das = (ch.das & 0x00FF) | (u16::from(v) << 8),
            7 => ch.dasb = v,
            8 => ch.a2 = (ch.a2 & 0xFF00) | u16::from(v),
            9 => ch.a2 = (ch.a2 & 0x00FF) | (u16::from(v) << 8),
            0xA => ch.ntrl = v,
            _ => {}
        }
    }

    /// Run one enabled channel's general-purpose transfer: the mode's
    /// unit pattern moves `total` bytes' worth of units, and the
    /// channel's registers end at their run position.
    fn dma_channel(&mut self, c: usize, io: &mut dyn BusIo) {
        let mode = usize::from(self.ch[c].dmap & 0x07);
        let unit = usize::from(UNIT_SIZE[mode]);
        let to_b = self.ch[c].dmap & 0x80 == 0;
        let bank = self.ch[c].a1tb;
        let bbad = self.ch[c].bbad;
        let step = Self::a_step(self.ch[c].dmap);
        let total = if self.ch[c].das == 0 {
            0x1_0000
        } else {
            u32::from(self.ch[c].das)
        };
        let units = total / unit as u32;
        let mut ptr = self.ch[c].a1t;
        for _ in 0..units {
            for sub in 0..unit {
                let target = Self::b_addr(mode, bbad, sub);
                if to_b {
                    // The A to B direction: the CPU memory feeds the
                    // register port.
                    let value = io.read(Self::a_addr(bank, ptr).0);
                    io.write(target.0, value);
                } else {
                    // The B to A direction: the port feeds the CPU
                    // memory.
                    let value = io.read(target.0);
                    io.write(Self::a_addr(bank, ptr).0, value);
                }
                ptr = ptr.wrapping_add(step);
            }
        }
        // Full units only: the byte count keeps any partial tail.
        let ch = &mut self.ch[c];
        ch.a1t = ptr;
        ch.das = (total % unit as u32) as u16;
    }

    /// Run the general-purpose burst. The channels walk 0 through 7
    /// in order and each enabled channel auto-clears its enable bit.
    fn run_dma_with(&mut self, io: &mut dyn BusIo) {
        for c in 0..CHANNELS {
            if self.mdmaen >> c & 1 != 0 {
                self.dma_channel(c, io);
                self.mdmaen &= !(1 << c);
            }
        }
    }

    /// The machine path of the burst: one halt pulse around the whole
    /// transfer, with the start and complete events.
    fn do_burst(&mut self, ctx: &mut MachineCtx<'_>, tick: Tick) {
        ctx.emit(TriggerEvent::Dma(DmaPhase::Start));
        ctx.notify("halt", Level::High, tick);
        {
            let mut io = CtxIo {
                ctx: &mut *ctx,
                bus: self.bus_name.clone(),
                tick,
            };
            self.run_dma_with(&mut io);
        }
        ctx.emit(TriggerEvent::Dma(DmaPhase::Complete));
        ctx.notify("halt", Level::Low, tick);
    }

    /// One table byte through `io`, advancing the table pointer within
    /// the A1T bank.
    fn table_byte(&mut self, c: usize, io: &mut dyn BusIo) -> u8 {
        let value = io.read(Self::a_addr(self.ch[c].a1tb, self.ch[c].table).0);
        self.ch[c].table = self.ch[c].table.wrapping_add(1);
        value
    }

    /// Move one transfer unit of a channel's mode. The direct form
    /// reads the unit bytes from the table stream; the indirect form
    /// takes the unit address from the table and reads it inside the
    /// DASB bank.
    fn hdma_move_unit(&mut self, c: usize, io: &mut dyn BusIo) {
        let mode = usize::from(self.ch[c].dmap & 0x07);
        let unit = usize::from(UNIT_SIZE[mode]);
        if self.ch[c].dmap & 0x40 != 0 {
            let low = self.table_byte(c, io);
            let high = self.table_byte(c, io);
            self.ch[c].a2 = (u16::from(high) << 8) | u16::from(low);
            for sub in 0..unit {
                let value = io.read(Self::a_addr(self.ch[c].dasb, self.ch[c].a2).0);
                io.write(Self::b_addr(mode, self.ch[c].bbad, sub).0, value);
                self.ch[c].a2 = self.ch[c].a2.wrapping_add(1);
            }
        } else {
            for sub in 0..unit {
                let value = self.table_byte(c, io);
                io.write(Self::b_addr(mode, self.ch[c].bbad, sub).0, value);
            }
        }
    }

    /// One channel's scanline step: fetch a table header when due,
    /// else run the pause or repeat line the counter describes.
    fn hdma_channel_line(&mut self, c: usize, io: &mut dyn BusIo) {
        if self.ch[c].fetch {
            self.ch[c].fetch = false;
            let header = self.table_byte(c, io);
            if header == 0 {
                // The table ended: the channel stops and its enable
                // bit auto-clears.
                self.ch[c].done = true;
                self.hdmaen &= !(1 << c);
            } else if header < 0x81 {
                // One unit now, then a pause of (header - 1) lines.
                self.ch[c].ntrl = (header - 1) & 0x7F;
                self.hdma_move_unit(c, io);
            } else {
                // (header - 80) units on consecutive lines, the fetch
                // line included.
                self.ch[c].ntrl = 0x80 | (header - 0x81);
                self.hdma_move_unit(c, io);
            }
        } else {
            let left = self.ch[c].ntrl & 0x7F;
            if left > 0 {
                self.ch[c].ntrl = (self.ch[c].ntrl & 0x80) | (left - 1);
                if self.ch[c].ntrl & 0x80 != 0 {
                    // The repeat form moves on every counted line.
                    self.hdma_move_unit(c, io);
                }
            }
            if left <= 1 {
                // The counter emptied: the next header fetch waits for
                // the next scanline.
                self.ch[c].fetch = true;
            }
        }
    }

    /// Run all channels' work for one scanline. The channels walk 0
    /// through 7 in order; the tables freeze over VBlank.
    fn hdma_line_with(&mut self, io: &mut dyn BusIo) {
        if self.in_vblank {
            return;
        }
        for c in 0..CHANNELS {
            if self.hdmaen >> c & 1 != 0 && !self.ch[c].done {
                self.hdma_channel_line(c, io);
            }
        }
    }

    /// The machine path of the per-scanline HDMA work.
    fn do_hdma_line(&mut self, ctx: &mut MachineCtx<'_>, tick: Tick) {
        let mut io = CtxIo {
            ctx: &mut *ctx,
            bus: self.bus_name.clone(),
            tick,
        };
        self.hdma_line_with(&mut io);
    }

    /// The auto-joypad read: the latch strobe through the port write,
    /// then sixteen shift reads per controller port. The first read's
    /// bit lands in bit 0 of the latched word.
    fn run_auto_joy(&mut self, io: &mut dyn BusIo) {
        io.write(JOY1_PORT, 0x01);
        io.write(JOY1_PORT, 0x00);
        let mut words = [0u16; 2];
        for (port, word) in words.iter_mut().enumerate() {
            let address = if port == 0 { JOY1_PORT } else { JOY2_PORT };
            for _ in 0..16 {
                let bit = u16::from(io.read(address) & 0x01);
                *word = (*word >> 1) | (bit << 15);
            }
        }
        self.joy = words;
        self.joy_busy = false;
    }

    /// The machine path of the auto-joypad read.
    fn do_auto_joy(&mut self, ctx: &mut MachineCtx<'_>, tick: Tick) {
        let mut io = CtxIo {
            ctx: &mut *ctx,
            bus: self.bus_name.clone(),
            tick,
        };
        self.run_auto_joy(&mut io);
    }

    /// The per-scanline status work: the VBlank region, the NMI latch
    /// of an unwired machine, and the H and V timer compares.
    fn line_check(&mut self) {
        if !self.vblank_wired {
            // The PPU's vblank line is not wired: the region and the
            // NMI latch follow this device's own line count.
            let inside = self.v >= self.vblank_start;
            if inside && !self.in_vblank {
                self.in_vblank = true;
                self.nmi_latch = true;
            }
            if self.v == 0 && self.in_vblank {
                // The frame start closes the region; a latch the
                // machine never acknowledged clears with it.
                self.in_vblank = false;
                self.nmi_latch = false;
            }
        }
        // The timer mode of `$4200` bits 5-4: bit 0 of the field picks
        // the H timer and bit 1 the V timer.
        let mode = (self.nmitimen >> 4) & 0x03;
        if mode & 0x02 != 0 && self.v as u16 == self.vtime {
            self.timeup_flag = true;
        }
        if mode & 0x01 != 0 && self.htime == H_APPROX {
            self.timeup_flag = true;
        }
    }

    /// The interrupt lines the current state asks for.
    fn line_wants(&self) -> (bool, bool) {
        (
            self.nmi_latch && self.nmitimen & 0x80 != 0,
            self.timeup_flag,
        )
    }

    /// Drive the CPU's interrupt lines to the asked levels. The
    /// source holds each line while the source flag stands.
    fn apply_lines(&mut self, ctx: &mut MachineCtx<'_>, tick: Tick) {
        let (nmi_wanted, irq_wanted) = self.line_wants();
        if nmi_wanted != self.nmi_out {
            self.nmi_out = nmi_wanted;
            ctx.notify(
                "nmi",
                if nmi_wanted { Level::High } else { Level::Low },
                tick,
            );
        }
        if irq_wanted != self.irq_out {
            self.irq_out = irq_wanted;
            ctx.notify(
                "irq",
                if irq_wanted { Level::High } else { Level::Low },
                tick,
            );
        }
    }
}

impl Device for SnesSys {
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

    fn reset(&mut self) {
        self.nmitimen = 0;
        self.wrio = 0;
        self.mult_a = 0;
        self.mult_b = 0;
        self.dividend = 0;
        self.divisor = 0;
        self.htime = 0;
        self.vtime = 0;
        self.mdmaen = 0;
        self.hdmaen = 0;
        self.memsel = 0;
        self.quotient = 0;
        self.result = 0;
        self.nmi_latch = false;
        self.timeup_flag = false;
        self.nmi_out = false;
        self.irq_out = false;
        self.in_vblank = false;
        self.vblank_wired = false;
        self.joy_busy = false;
        self.joy = [0; 2];
        self.v = 0;
        self.ch = [Channel::default(); CHANNELS];
        self.dma_burst = false;
        self.joy_read = false;
        self.line_work = false;
        self.ext_strobe = false;
        self.next = None;
        self.last_tick = None;
    }

    fn boot(&mut self, _state: Option<&BootState>) {
        self.reset();
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(SNAP_LENGTH);
        out.extend_from_slice(b"SNSS");
        out.push(1);
        out.push(self.nmitimen);
        out.push(self.wrio);
        out.push(self.mult_a);
        out.push(self.mult_b);
        out.extend_from_slice(&self.dividend.to_le_bytes());
        out.push(self.divisor);
        out.extend_from_slice(&self.htime.to_le_bytes());
        out.extend_from_slice(&self.vtime.to_le_bytes());
        out.push(self.mdmaen);
        out.push(self.hdmaen);
        out.push(self.memsel);
        out.extend_from_slice(&self.quotient.to_le_bytes());
        out.extend_from_slice(&self.result.to_le_bytes());
        out.extend_from_slice(&self.joy[0].to_le_bytes());
        out.extend_from_slice(&self.joy[1].to_le_bytes());
        out.push(u8::from(self.in_vblank));
        out.push(u8::from(self.vblank_wired));
        out.push(u8::from(self.nmi_latch));
        out.push(u8::from(self.nmi_out));
        out.push(u8::from(self.irq_out));
        out.push(u8::from(self.timeup_flag));
        out.push(u8::from(self.joy_busy));
        out.extend_from_slice(&self.v.to_le_bytes());
        out.extend_from_slice(&self.vblank_start.to_le_bytes());
        out.extend_from_slice(&self.lines.to_le_bytes());
        out.extend_from_slice(&self.cpu_div.to_le_bytes());
        out.push(u8::from(self.dma_burst));
        out.push(u8::from(self.joy_read));
        out.push(u8::from(self.line_work));
        out.push(u8::from(self.ext_strobe));
        for ch in &self.ch {
            out.push(ch.dmap);
            out.push(ch.bbad);
            out.extend_from_slice(&ch.a1t.to_le_bytes());
            out.push(ch.a1tb);
            out.extend_from_slice(&ch.das.to_le_bytes());
            out.push(ch.dasb);
            out.extend_from_slice(&ch.a2.to_le_bytes());
            out.push(ch.ntrl);
            out.extend_from_slice(&ch.table.to_le_bytes());
            out.push(u8::from(ch.done) | (u8::from(ch.fetch) << 1));
        }
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        if data.len() != SNAP_LENGTH || &data[..4] != b"SNSS" || data[4] != 1 {
            return Err(SnapshotError::Mismatch(format!(
                "snes sys chunk must be {SNAP_LENGTH} bytes"
            )));
        }
        let mut pos = 5usize;
        self.nmitimen = data[pos];
        pos += 1;
        self.wrio = data[pos];
        pos += 1;
        self.mult_a = data[pos];
        pos += 1;
        self.mult_b = data[pos];
        pos += 1;
        self.dividend = u16::from_le_bytes(data[pos..pos + 2].try_into().expect("2 bytes"));
        pos += 2;
        self.divisor = data[pos];
        pos += 1;
        self.htime = u16::from_le_bytes(data[pos..pos + 2].try_into().expect("2 bytes"));
        pos += 2;
        self.vtime = u16::from_le_bytes(data[pos..pos + 2].try_into().expect("2 bytes"));
        pos += 2;
        self.mdmaen = data[pos];
        pos += 1;
        self.hdmaen = data[pos];
        pos += 1;
        self.memsel = data[pos];
        pos += 1;
        self.quotient = u16::from_le_bytes(data[pos..pos + 2].try_into().expect("2 bytes"));
        pos += 2;
        self.result = u16::from_le_bytes(data[pos..pos + 2].try_into().expect("2 bytes"));
        pos += 2;
        self.joy[0] = u16::from_le_bytes(data[pos..pos + 2].try_into().expect("2 bytes"));
        pos += 2;
        self.joy[1] = u16::from_le_bytes(data[pos..pos + 2].try_into().expect("2 bytes"));
        pos += 2;
        self.in_vblank = data[pos] != 0;
        pos += 1;
        self.vblank_wired = data[pos] != 0;
        pos += 1;
        self.nmi_latch = data[pos] != 0;
        pos += 1;
        self.nmi_out = data[pos] != 0;
        pos += 1;
        self.irq_out = data[pos] != 0;
        pos += 1;
        self.timeup_flag = data[pos] != 0;
        pos += 1;
        self.joy_busy = data[pos] != 0;
        pos += 1;
        self.v = u32::from_le_bytes(data[pos..pos + 4].try_into().expect("4 bytes"));
        pos += 4;
        self.vblank_start = u32::from_le_bytes(data[pos..pos + 4].try_into().expect("4 bytes"));
        pos += 4;
        self.lines = u32::from_le_bytes(data[pos..pos + 4].try_into().expect("4 bytes"));
        pos += 4;
        self.cpu_div =
            u64::from_le_bytes(data[pos..pos + 8].try_into().expect("8 bytes")).clamp(1, 64);
        pos += 8;
        self.dma_burst = data[pos] != 0;
        pos += 1;
        self.joy_read = data[pos] != 0;
        pos += 1;
        self.line_work = data[pos] != 0;
        pos += 1;
        self.ext_strobe = data[pos] != 0;
        pos += 1;
        for ch in &mut self.ch {
            ch.dmap = data[pos];
            pos += 1;
            ch.bbad = data[pos];
            pos += 1;
            ch.a1t = u16::from_le_bytes(data[pos..pos + 2].try_into().expect("2 bytes"));
            pos += 2;
            ch.a1tb = data[pos];
            pos += 1;
            ch.das = u16::from_le_bytes(data[pos..pos + 2].try_into().expect("2 bytes"));
            pos += 2;
            ch.dasb = data[pos];
            pos += 1;
            ch.a2 = u16::from_le_bytes(data[pos..pos + 2].try_into().expect("2 bytes"));
            pos += 2;
            ch.ntrl = data[pos];
            pos += 1;
            ch.table = u16::from_le_bytes(data[pos..pos + 2].try_into().expect("2 bytes"));
            pos += 2;
            let flags = data[pos];
            pos += 1;
            ch.done = flags & 0x01 != 0;
            ch.fetch = flags & 0x02 != 0;
        }
        self.next = None;
        self.last_tick = None;
        Ok(())
    }

    fn notify_line(&mut self, line: &str, level: Level, tick: Tick) {
        match line {
            "scanline" if level == Level::High => {
                // One line per high edge of the scanline wire.
                self.v += 1;
                if self.v >= self.lines {
                    self.v = 0;
                }
                self.line_work = true;
                self.wake_at(Tick(tick.0 + self.cpu_div));
            }
            "vblank" => {
                // The PPU drives the region; it is authoritative from
                // here on.
                self.vblank_wired = true;
                if level == Level::High {
                    self.in_vblank = true;
                    self.nmi_latch = true;
                } else {
                    self.in_vblank = false;
                    // The region closed: a latch the machine never
                    // acknowledged clears with it.
                    self.nmi_latch = false;
                }
                self.wake_at(Tick(tick.0 + 1));
            }
            "nmi_pend" if level == Level::High => {
                // The PPU's VBlank NMI edge; the CPU gate re-evaluates
                // at the next visit.
                self.nmi_latch = true;
                self.wake_at(Tick(tick.0 + 1));
            }
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
        if self.ext_strobe {
            self.ext_strobe = false;
            // The $4201 bit 7 low edge pulses the external latch.
            ctx.notify("ext_latch", Level::High, tick);
            ctx.notify("ext_latch", Level::Low, tick);
        }
        if self.line_work {
            self.line_work = false;
            self.line_check();
            self.do_hdma_line(ctx, tick);
        }
        if self.dma_burst {
            self.dma_burst = false;
            self.do_burst(ctx, tick);
        }
        if self.joy_read {
            self.joy_read = false;
            self.do_auto_joy(ctx, tick);
        }
        self.apply_lines(ctx, tick);
        if !(self.ext_strobe || self.line_work || self.dma_burst || self.joy_read) {
            self.next = None;
        }
    }
}

impl BusAccess for SnesSys {
    fn read(&mut self, _width: AccessWidth, address: Address, tick: Tick) -> u32 {
        if let Some(idx) = self.reg_index(address.0) {
            return u32::from(self.sys_read(idx, tick));
        }
        if let Some((c, r)) = self.chan_index(address.0) {
            return u32::from(self.chan_read(c, r));
        }
        0xFF
    }

    fn write(&mut self, _width: AccessWidth, address: Address, value: u32, tick: Tick) {
        let v = value as u8;
        if let Some(idx) = self.reg_index(address.0) {
            self.sys_write(idx, v, tick);
            return;
        }
        if let Some((c, r)) = self.chan_index(address.0) {
            self.chan_write(c, r, v);
        }
    }

    fn peek(&self, address: Address) -> u8 {
        if let Some(idx) = self.reg_index(address.0) {
            return self.sys_peek(idx);
        }
        if let Some((c, r)) = self.chan_index(address.0) {
            return self.chan_read(c, r);
        }
        0xFF
    }

    fn decodes(&self, address: Address) -> bool {
        self.reg_index(address.0).is_some() || self.chan_index(address.0).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, VecDeque};

    fn instance() -> demu_config::model::DeviceInstance {
        demu_config::model::DeviceInstance {
            name: demu_config::newtypes::DeviceName::new("snessys").expect("name"),
            kind: demu_config::newtypes::KindName::new("snes.sys").expect("kind"),
            domain: "main".to_string(),
            params: BTreeMap::new(),
            dividers: [("cpu".to_string(), 4)].into_iter().collect(),
        }
    }

    fn sys() -> SnesSys {
        SnesSys::new(&instance())
    }

    fn wr(s: &mut SnesSys, addr: u32, v: u8) {
        s.write(AccessWidth::Byte, Address(addr), u32::from(v), Tick(0));
    }

    fn rd(s: &mut SnesSys, addr: u32) -> u8 {
        s.read(AccessWidth::Byte, Address(addr), Tick(0)) as u8
    }

    /// A plain bus view: the reads pop a byte queue in order and
    /// every access records itself.
    struct TestIo {
        queue: VecDeque<u8>,
        reads: Vec<u32>,
        writes: Vec<(u32, u8)>,
    }

    impl TestIo {
        fn fed(bytes: &[u8]) -> Self {
            Self {
                queue: bytes.iter().copied().collect(),
                reads: Vec::new(),
                writes: Vec::new(),
            }
        }
    }

    impl BusIo for TestIo {
        fn read(&mut self, address: u32) -> u8 {
            self.reads.push(address);
            self.queue.pop_front().unwrap_or(0xFF)
        }

        fn write(&mut self, address: u32, value: u8) {
            self.writes.push((address, value));
        }
    }

    #[test]
    fn window_decode_covers_both_mapped_regions() {
        let s = sys();
        for a in [0x4200, 0x421F, 0x420B, 0x4210, 0x4300, 0x4310, 0x437F] {
            assert!(s.decodes(Address(a)), "0x{a:X} decodes");
        }
        for a in [0x41FF, 0x4220, 0x4380, 0x4400, 0x1000] {
            assert!(!s.decodes(Address(a)), "0x{a:X} decodes nothing here");
        }
    }

    #[test]
    fn multiply_and_divide_results() {
        let mut s = sys();
        wr(&mut s, 0x4202, 0x0C);
        wr(&mut s, 0x4203, 0x0D);
        assert_eq!(rd(&mut s, 0x4216), 0x9C, "12 * 13 = 156");
        assert_eq!(rd(&mut s, 0x4217), 0x00);
        wr(&mut s, 0x4204, 0xD2);
        wr(&mut s, 0x4205, 0x04);
        wr(&mut s, 0x4206, 0x0C);
        assert_eq!(rd(&mut s, 0x4214), 0x66, "1234 / 12 = 102");
        assert_eq!(rd(&mut s, 0x4215), 0x00);
        assert_eq!(rd(&mut s, 0x4216), 0x0A, "the remainder is 10");
        assert_eq!(rd(&mut s, 0x4217), 0x00);
    }

    #[test]
    fn divide_by_zero_documented_form() {
        let mut s = sys();
        wr(&mut s, 0x4204, 0x21);
        wr(&mut s, 0x4205, 0x43);
        wr(&mut s, 0x4206, 0x00);
        assert_eq!(rd(&mut s, 0x4214), 0xFF, "the quotient saturates");
        assert_eq!(rd(&mut s, 0x4215), 0xFF);
        assert_eq!(
            rd(&mut s, 0x4216),
            0x21,
            "the remainder answers the dividend"
        );
        assert_eq!(rd(&mut s, 0x4217), 0x43);
    }

    #[test]
    fn htime_and_vtime_store_ten_bits() {
        let mut s = sys();
        wr(&mut s, 0x4207, 0xFF);
        wr(&mut s, 0x4208, 0x01);
        assert_eq!(rd(&mut s, 0x4207), 0xFF);
        assert_eq!(rd(&mut s, 0x4208), 0x01);
        wr(&mut s, 0x420A, 0xFF);
        assert_eq!(rd(&mut s, 0x420A), 0x03, "the high byte masks to two bits");
    }

    #[test]
    fn stored_and_open_bus_reads() {
        let mut s = sys();
        wr(&mut s, 0x4202, 0x05);
        assert_eq!(
            rd(&mut s, 0x4202),
            0x05,
            "the control file reads back stored"
        );
        wr(&mut s, 0x420D, 0x01);
        assert_eq!(rd(&mut s, 0x420D), 0x01, "MEMSEL stores");
        assert_eq!(rd(&mut s, 0x420E), 0xFF, "the unused pair is open bus");
        assert_eq!(rd(&mut s, 0x420F), 0xFF);
        assert_eq!(rd(&mut s, 0x4213), 0x00, "RDJOY2 stays zero");
        assert_eq!(rd(&mut s, 0x421C), 0x00, "JOY3 stays zero");
        assert_eq!(rd(&mut s, 0x421F), 0x00);
    }

    #[test]
    fn rdnmi_latches_clears_and_peeks() {
        let mut s = sys();
        s.notify_line("nmi_pend", Level::High, Tick(0));
        assert!(s.nmi_latch);
        assert_eq!(rd(&mut s, 0x4210), 0x82, "the flag plus version 0x02");
        assert_eq!(rd(&mut s, 0x4210), 0x02, "the read acknowledged the flag");
        assert!(!s.nmi_latch);
        // The peek carries no side effect.
        s.nmi_latch = true;
        assert_eq!(s.peek(Address(0x4210)), 0x82);
        assert!(s.nmi_latch, "the peek keeps the flag");
    }

    #[test]
    fn nmi_gate_follows_nmitimen_bit_7() {
        let mut s = sys();
        s.notify_line("nmi_pend", Level::High, Tick(0));
        // The bit 7 clear: the mask withholds the NMI line.
        assert_eq!(s.line_wants(), (false, false));
        wr(&mut s, 0x4200, 0x80);
        assert_eq!(s.line_wants(), (true, false));
        // The acknowledge drops the latch with the gate still armed.
        let _ = rd(&mut s, 0x4210);
        assert_eq!(s.line_wants(), (false, false));
    }

    #[test]
    fn timeup_fires_on_v_target_and_acknowledges() {
        let mut s = sys();
        wr(&mut s, 0x4200, 0x20); // the vertical timer mode
        wr(&mut s, 0x4209, 0x05); // the target V = 5
        for k in 0..4u32 {
            s.notify_line("scanline", Level::High, Tick(u64::from(k)));
        }
        s.line_check();
        assert_eq!(s.v, 4);
        assert!(!s.timeup_flag, "the flag waits for the target line");
        s.notify_line("scanline", Level::High, Tick(100));
        s.line_check();
        assert!(s.timeup_flag);
        assert_eq!(s.line_wants(), (false, true));
        assert_eq!(rd(&mut s, 0x4211), 0x80);
        assert_eq!(s.line_wants(), (false, false), "the read acknowledged");
    }

    #[test]
    fn timeup_wrap_fires_at_frame_start() {
        let mut s = sys();
        wr(&mut s, 0x4200, 0x20);
        // The target V = 0: every frame start latches the flag again.
        for k in 0..262u32 {
            s.notify_line("scanline", Level::High, Tick(u64::from(k)));
        }
        s.line_check();
        assert!(s.timeup_flag, "the wrapped V = 0 hit the target");
        // The next line leaves the target behind.
        s.notify_line("scanline", Level::High, Tick(1000));
        s.line_check();
        assert!(s.timeup_flag, "the flag holds until the acknowledge");
    }

    #[test]
    fn h_timer_fires_at_documented_dot_only() {
        let mut s = sys();
        wr(&mut s, 0x4200, 0x10); // the horizontal timer mode
        wr(&mut s, 0x4207, 0x90); // dot 144
        s.notify_line("scanline", Level::High, Tick(0));
        s.line_check();
        assert!(s.timeup_flag);
        // A target off the check dot stays silent.
        let mut t = sys();
        wr(&mut t, 0x4200, 0x10);
        wr(&mut t, 0x4207, 0xEB);
        t.notify_line("scanline", Level::High, Tick(0));
        t.line_check();
        assert!(
            !t.timeup_flag,
            "the line-granular H check holds dot 144 only"
        );
    }

    #[test]
    fn timer_mode_none_sets_nothing() {
        let mut s = sys();
        wr(&mut s, 0x4200, 0x80); // only the NMI enable
        wr(&mut s, 0x4209, 0x05);
        s.notify_line("scanline", Level::High, Tick(0));
        s.line_check();
        assert!(!s.timeup_flag, "the timer mode 00 disables the compares");
    }

    #[test]
    fn vblank_region_and_nmi_wrap_follow_line_count() {
        let mut s = sys();
        wr(&mut s, 0x4200, 0x20);
        // The NTSC vblank starts at line 224.
        for k in 0..223u32 {
            s.notify_line("scanline", Level::High, Tick(u64::from(k)));
        }
        s.line_check();
        assert!(!s.in_vblank);
        s.notify_line("scanline", Level::High, Tick(1));
        s.line_check();
        assert!(s.in_vblank, "line 224 opens the region");
        assert!(s.nmi_latch, "the unwired machine latches the NMI itself");
        // HVBJOY mirrors the region from the line count alone.
        assert_eq!(rd(&mut s, 0x4212), 0x80);
        // The wrap to line 0 closes the region again.
        for k in 0..38u32 {
            s.notify_line("scanline", Level::High, Tick(10_000 + u64::from(k)));
        }
        s.line_check();
        assert_eq!(s.v, 0);
        assert!(!s.in_vblank);
        assert!(
            !s.nmi_latch,
            "the frame start clears an unacknowledged latch"
        );
        assert_eq!(rd(&mut s, 0x4212), 0x00);
    }

    #[test]
    fn explicit_vblank_line_becomes_authoritative() {
        let mut s = sys();
        s.notify_line("vblank", Level::High, Tick(0));
        assert!(s.in_vblank);
        assert!(s.nmi_latch);
        assert!(s.vblank_wired, "the PPU now owns the region");
        s.notify_line("vblank", Level::Low, Tick(10));
        assert!(!s.in_vblank);
        assert!(!s.nmi_latch, "the region close clears the latch");
        // The region stays put while the line count says otherwise.
        s.notify_line("scanline", Level::High, Tick(20));
        s.line_check();
        assert!(!s.in_vblank);
    }

    #[test]
    fn auto_joy_latches_controller_words() {
        let mut s = sys();
        let (w1, w2) = (0x1234u16, 0xC3D0u16);
        let feeds: Vec<u8> = (0..16)
            .map(|k| ((w1 >> k) & 1) as u8)
            .chain((0..16).map(|k| ((w2 >> k) & 1) as u8))
            .collect();
        let mut io = TestIo::fed(&feeds);
        s.run_auto_joy(&mut io);
        assert_eq!(s.joy[0], w1, "the first read's bit lands in bit 0");
        assert_eq!(s.joy[1], w2);
        // The latch strobe walked through the port write.
        assert_eq!(io.writes, vec![(JOY1_PORT, 0x01), (JOY1_PORT, 0x00)]);
        assert_eq!(io.reads.len(), 32);
        assert_eq!(&io.reads[..16], &[JOY1_PORT; 16]);
        assert_eq!(&io.reads[16..], &[JOY2_PORT; 16]);
        // The latched words answer through the registers.
        assert_eq!(rd(&mut s, 0x4218), 0x34);
        assert_eq!(rd(&mut s, 0x4219), 0x12);
        assert_eq!(rd(&mut s, 0x421A), 0xD0);
        assert_eq!(rd(&mut s, 0x421B), 0xC3);
        assert_eq!(rd(&mut s, 0x4212) & 0x01, 0, "the busy flag rests");
    }

    #[test]
    fn auto_joy_arms_on_nmitimen_bit_0() {
        let mut s = sys();
        wr(&mut s, 0x4200, 0x01);
        assert!(s.joy_read);
        assert!(s.joy_busy);
        assert_eq!(
            s.next,
            Some(Tick(1)),
            "the gate re-check visit wins the race"
        );
        // A second write with the bit held sets no second run.
        wr(&mut s, 0x4200, 0x81);
        assert!(s.joy_read);
    }

    #[test]
    fn dma_unit_patterns() {
        // The mode 1: two consecutive bytes to the two ports.
        let mut s = sys();
        wr(&mut s, 0x4300, 0x01);
        wr(&mut s, 0x4301, 0x02);
        wr(&mut s, 0x4302, 0x00);
        wr(&mut s, 0x4303, 0x10);
        wr(&mut s, 0x4305, 0x04);
        let mut io = TestIo::fed(&[1, 2, 3, 4]);
        wr(&mut s, 0x420B, 0x01);
        s.run_dma_with(&mut io);
        assert_eq!(io.reads, vec![0x1000, 0x1001, 0x1002, 0x1003]);
        assert_eq!(
            io.writes,
            vec![(0x2102, 1), (0x2103, 2), (0x2102, 3), (0x2103, 4)]
        );
        // The mode 2: the same port twice per unit.
        let mut t = sys();
        wr(&mut t, 0x4300, 0x02);
        wr(&mut t, 0x4301, 0x02);
        wr(&mut t, 0x4302, 0x00);
        wr(&mut t, 0x4303, 0x10);
        wr(&mut t, 0x4305, 0x04);
        let mut io = TestIo::fed(&[1, 2, 3, 4]);
        wr(&mut t, 0x420B, 0x01);
        t.run_dma_with(&mut io);
        assert_eq!(io.reads, vec![0x1000, 0x1001, 0x1002, 0x1003]);
        assert_eq!(
            io.writes,
            vec![(0x2102, 1), (0x2102, 2), (0x2102, 3), (0x2102, 4)]
        );
        // The mode 4: four sequential ports.
        let mut u = sys();
        wr(&mut u, 0x4300, 0x04);
        wr(&mut u, 0x4301, 0x02);
        wr(&mut u, 0x4302, 0x00);
        wr(&mut u, 0x4303, 0x10);
        wr(&mut u, 0x4305, 0x04);
        let mut io = TestIo::fed(&[1, 2, 3, 4]);
        wr(&mut u, 0x420B, 0x01);
        u.run_dma_with(&mut io);
        assert_eq!(
            io.writes,
            vec![(0x2102, 1), (0x2103, 2), (0x2104, 3), (0x2105, 4)]
        );
    }

    #[test]
    fn dma_counts_wraps_and_clears_enable() {
        let mut s = sys();
        wr(&mut s, 0x4300, 0x00);
        wr(&mut s, 0x4301, 0x04);
        wr(&mut s, 0x4302, 0xF0);
        wr(&mut s, 0x4303, 0xFF);
        wr(&mut s, 0x4304, 0x01);
        wr(&mut s, 0x4305, 0x20);
        let feed = vec![7u8; 0x20];
        let mut io = TestIo::fed(&feed);
        wr(&mut s, 0x420B, 0x01);
        s.run_dma_with(&mut io);
        // The pointer walks the bank boundary without carrying.
        assert_eq!(io.reads[0], 0x1FFF0);
        assert_eq!(io.reads[15], 0x1FFFF);
        assert_eq!(io.reads[16], 0x10000);
        assert_eq!(io.reads[0x1F], 0x1000F);
        assert_eq!(io.writes.len(), 0x20);
        assert_eq!(io.writes[0].0, 0x2104);
        // The state: the pointer wrapped, the count emptied, the
        // enable cleared.
        assert_eq!(rd(&mut s, 0x4302), 0x10);
        assert_eq!(rd(&mut s, 0x4303), 0x00);
        assert_eq!(rd(&mut s, 0x4305), 0x00);
        assert_eq!(rd(&mut s, 0x4306), 0x00);
        assert_eq!(rd(&mut s, 0x420B), 0x00, "the active bit auto-cleared");
    }

    #[test]
    fn dma_zero_count_means_full_page() {
        let mut s = sys();
        wr(&mut s, 0x4300, 0x00);
        wr(&mut s, 0x4301, 0x04);
        wr(&mut s, 0x4302, 0x34);
        wr(&mut s, 0x4303, 0x12);
        // DAS of 0 means 65536 bytes.
        let feed = vec![0u8; 0x10000];
        let mut io = TestIo::fed(&feed);
        wr(&mut s, 0x420B, 0x01);
        s.run_dma_with(&mut io);
        assert_eq!(io.reads.len(), 0x10000);
        assert_eq!(
            io.reads.last().copied(),
            Some(0x1233),
            "the last byte wrapped"
        );
        // The pointer wrapped back to its start and the count rests.
        assert_eq!(rd(&mut s, 0x4302), 0x34);
        assert_eq!(rd(&mut s, 0x4303), 0x12);
    }

    #[test]
    fn dma_partial_tail_stays_uncounted() {
        let mut s = sys();
        wr(&mut s, 0x4300, 0x02); // unit 2
        wr(&mut s, 0x4301, 0x02);
        wr(&mut s, 0x4302, 0x00);
        wr(&mut s, 0x4303, 0x10);
        wr(&mut s, 0x4305, 0x03);
        let mut io = TestIo::fed(&[1, 2]);
        wr(&mut s, 0x420B, 0x01);
        s.run_dma_with(&mut io);
        assert_eq!(io.writes.len(), 2, "only whole units move");
        assert_eq!(rd(&mut s, 0x4302), 0x02, "the pointer ran one unit");
        assert_eq!(rd(&mut s, 0x4305), 0x01, "the DAS keeps the partial tail");
    }

    #[test]
    fn dma_reverse_direction_reads_b_writes_a() {
        let mut s = sys();
        wr(&mut s, 0x4300, 0x80);
        wr(&mut s, 0x4301, 0x3A);
        wr(&mut s, 0x4302, 0x00);
        wr(&mut s, 0x4303, 0x60);
        wr(&mut s, 0x4305, 0x02);
        let mut io = TestIo::fed(&[0xAB, 0xCD]);
        wr(&mut s, 0x420B, 0x01);
        s.run_dma_with(&mut io);
        assert_eq!(io.reads, vec![0x213A, 0x213A]);
        assert_eq!(io.writes, vec![(0x6000, 0xAB), (0x6001, 0xCD)]);
    }

    #[test]
    fn dma_fixed_and_decrement_a_steps() {
        // The fixed form: every byte reads the same A address.
        let mut s = sys();
        wr(&mut s, 0x4300, 0x20);
        wr(&mut s, 0x4301, 0x02);
        wr(&mut s, 0x4302, 0x00);
        wr(&mut s, 0x4303, 0x10);
        wr(&mut s, 0x4305, 0x04);
        let mut io = TestIo::fed(&[1, 2, 3, 4]);
        wr(&mut s, 0x420B, 0x01);
        s.run_dma_with(&mut io);
        assert_eq!(io.reads, vec![0x1000; 4]);
        // The decrement form walks backwards.
        let mut t = sys();
        wr(&mut t, 0x4300, 0x10);
        wr(&mut t, 0x4301, 0x02);
        wr(&mut t, 0x4302, 0x00);
        wr(&mut t, 0x4303, 0x10);
        wr(&mut t, 0x4305, 0x02);
        let mut io = TestIo::fed(&[1, 2]);
        wr(&mut t, 0x420B, 0x01);
        t.run_dma_with(&mut io);
        assert_eq!(io.reads, vec![0x1000, 0x0FFF]);
        assert_eq!(rd(&mut t, 0x4302), 0xFE, "the pointer wrapped backwards");
        assert_eq!(rd(&mut t, 0x4303), 0x0F);
    }

    #[test]
    fn dma_channels_walk_low_first() {
        let mut s = sys();
        for c in [5u32, 7] {
            let base = 0x4300 + c * 16;
            wr(&mut s, base, 0x00);
            wr(&mut s, base + 1, 0x02);
            wr(&mut s, base + 2, 0x00);
            wr(&mut s, base + 3, 0x10);
            wr(&mut s, base + 5, 0x01);
        }
        let mut io = TestIo::fed(&[1, 2]);
        wr(&mut s, 0x420B, 0xA0); // channels 5 and 7 enabled
        s.run_dma_with(&mut io);
        assert_eq!(
            io.writes,
            vec![(0x2102, 1), (0x2102, 2)],
            "channel 5 ran first"
        );
        assert_eq!(io.reads, vec![0x1000, 0x1000]);
    }

    #[test]
    fn dma_b_page_wraps_within_page() {
        let mut s = sys();
        wr(&mut s, 0x4300, 0x01);
        wr(&mut s, 0x4301, 0xFF);
        wr(&mut s, 0x4302, 0x00);
        wr(&mut s, 0x4303, 0x20);
        wr(&mut s, 0x4305, 0x04);
        let mut io = TestIo::fed(&[1, 2, 3, 4]);
        wr(&mut s, 0x420B, 0x01);
        s.run_dma_with(&mut io);
        assert_eq!(
            io.writes,
            vec![(0x21FF, 1), (0x2100, 2), (0x21FF, 3), (0x2100, 4)]
        );
    }

    #[test]
    fn burst_write_pokes_the_visit() {
        let mut s = sys();
        wr(&mut s, 0x420B, 0x01);
        assert!(s.dma_burst);
        assert_eq!(s.next, Some(Tick(4)), "the burst waits one CPU cycle");
        // A write with no channels set starts nothing.
        let mut t = sys();
        wr(&mut t, 0x420B, 0x00);
        assert!(!t.dma_burst);
        assert_eq!(t.next, None);
    }

    #[test]
    fn hdma_stop_ends_table_and_clears_enable() {
        let mut s = sys();
        wr(&mut s, 0x4301, 0x04);
        wr(&mut s, 0x4302, 0x00);
        wr(&mut s, 0x4303, 0x40);
        wr(&mut s, 0x4304, 0x01);
        wr(&mut s, 0x420C, 0x01);
        assert!(s.ch[0].fetch, "the arm arms the first fetch");
        // The table = a single 00 byte: the walk ends at once.
        let mut io = TestIo::fed(&[0x00]);
        s.hdma_line_with(&mut io);
        assert!(s.ch[0].done);
        assert_eq!(rd(&mut s, 0x420C), 0x00, "the enable auto-cleared");
        // Further lines walk nothing.
        let mut io = TestIo::fed(&[0x99]);
        s.hdma_line_with(&mut io);
        assert!(io.reads.is_empty() && io.writes.is_empty());
    }

    #[test]
    fn hdma_pause_and_repeat_forms() {
        let mut s = sys();
        wr(&mut s, 0x4300, 0x00); // one byte per unit
        wr(&mut s, 0x4301, 0x04);
        wr(&mut s, 0x4302, 0x00);
        wr(&mut s, 0x4303, 0x40);
        wr(&mut s, 0x420C, 0x01);
        // The table: 03 AA 82 BB CC 00. The 03 pauses two lines after
        // its unit; the 82 moves two units on consecutive lines.
        let mut io = TestIo::fed(&[0x03, 0xAA, 0x82, 0xBB, 0xCC, 0x00]);
        // Line 1: the header 03 fetches and moves AA.
        s.hdma_line_with(&mut io);
        assert_eq!(io.writes, vec![(0x2104, 0xAA)]);
        // Lines 2 and 3: the two pause lines of the header.
        s.hdma_line_with(&mut io);
        s.hdma_line_with(&mut io);
        assert_eq!(io.writes.len(), 1);
        // Line 4: the header 82 fetches and moves BB.
        s.hdma_line_with(&mut io);
        assert_eq!(io.writes, vec![(0x2104, 0xAA), (0x2104, 0xBB)]);
        // Line 5: the repeated unit moves CC.
        s.hdma_line_with(&mut io);
        assert_eq!(io.writes[2], (0x2104, 0xCC));
        // Line 6: the next header 00 ends the walk.
        s.hdma_line_with(&mut io);
        assert!(s.ch[0].done);
        assert_eq!(rd(&mut s, 0x420C), 0x00);
        // NTRL reads back the repeat bit with the run counter.
        assert_eq!(rd(&mut s, 0x430A) & 0x7F, 0x00);
    }

    #[test]
    fn hdma_indirect_pointer() {
        let mut s = sys();
        wr(&mut s, 0x4300, 0x40); // DMAP bit 6: the indirect form
        wr(&mut s, 0x4301, 0x02);
        wr(&mut s, 0x4302, 0x00);
        wr(&mut s, 0x4303, 0x50);
        wr(&mut s, 0x4307, 0x7E);
        wr(&mut s, 0x420C, 0x01);
        // The table = the header 01 then the pointer 0x1234; the unit
        // reads from the indirect bank.
        let mut io = TestIo::fed(&[0x01, 0x34, 0x12, 0x55]);
        s.hdma_line_with(&mut io);
        assert_eq!(io.reads, vec![0x5000, 0x5001, 0x5002, 0x7E_1234]);
        assert_eq!(io.writes, vec![(0x2102, 0x55)]);
        assert_eq!(rd(&mut s, 0x4308), 0x35, "the indirect pointer advanced");
        assert_eq!(rd(&mut s, 0x4309), 0x12);
    }

    #[test]
    fn hdma_freezes_over_vblank() {
        let mut s = sys();
        wr(&mut s, 0x4301, 0x02);
        wr(&mut s, 0x4302, 0x00);
        wr(&mut s, 0x4303, 0x50);
        wr(&mut s, 0x420C, 0x01);
        s.notify_line("vblank", Level::High, Tick(0));
        let mut io = TestIo::fed(&[0x01, 0x7F]);
        s.hdma_line_with(&mut io);
        assert!(io.reads.is_empty(), "the tables freeze over VBlank");
        s.notify_line("vblank", Level::Low, Tick(1));
        s.hdma_line_with(&mut io);
        assert_eq!(io.reads, vec![0x5000, 0x5001]);
        assert_eq!(io.writes, vec![(0x2102, 0x7F)]);
    }

    #[test]
    fn hdma_rearm_reloads_table_pointer() {
        let mut s = sys();
        wr(&mut s, 0x4301, 0x02);
        wr(&mut s, 0x4302, 0x00);
        wr(&mut s, 0x4303, 0x50);
        wr(&mut s, 0x420C, 0x01);
        let mut io = TestIo::fed(&[0x00]);
        s.hdma_line_with(&mut io);
        assert!(s.ch[0].done);
        // The arm reloads the pointer from A1T.
        wr(&mut s, 0x420C, 0x01);
        assert!(!s.ch[0].done);
        assert_eq!(s.ch[0].table, 0x5000);
        let mut io = TestIo::fed(&[0x00]);
        s.hdma_line_with(&mut io);
        assert_eq!(io.reads, vec![0x5000]);
        assert!(s.ch[0].done);
    }

    #[test]
    fn ext_latch_strobe_on_wrio_low_edge() {
        let mut s = sys();
        wr(&mut s, 0x4201, 0x7F);
        assert!(!s.ext_strobe);
        wr(&mut s, 0x4201, 0x00);
        assert!(!s.ext_strobe, "the bit 7 held low strobes nothing");
        wr(&mut s, 0x4201, 0x80);
        assert!(!s.ext_strobe, "the set edge strobes nothing");
        wr(&mut s, 0x4201, 0x00);
        assert!(s.ext_strobe, "the 1-to-0 edge strobes the external latch");
    }

    #[test]
    fn snapshot_round_trip_and_mismatch() {
        let mut s = sys();
        wr(&mut s, 0x4200, 0x81);
        wr(&mut s, 0x4203, 0x05);
        wr(&mut s, 0x4301, 0x55);
        wr(&mut s, 0x4302, 0x34);
        wr(&mut s, 0x4303, 0x12);
        wr(&mut s, 0x4304, 0x7E);
        wr(&mut s, 0x4307, 0x02);
        wr(&mut s, 0x420C, 0x01);
        wr(&mut s, 0x430A, 0x83); // after the arm: the arm resets the counter
        s.notify_line("nmi_pend", Level::High, Tick(0));
        s.joy = [0x1234, 0xC300];
        s.v = 17;
        s.timeup_flag = true;
        s.irq_out = true;
        let snap = s.snapshot();
        assert_eq!(snap.len(), SNAP_LENGTH);
        let mut twin = sys();
        twin.restore(&snap).expect("restore");
        assert_eq!(twin.snapshot(), snap, "the round trip is byte-identical");
        assert_eq!(twin.ch[0].a1t, 0x1234);
        assert_eq!(twin.ch[0].a1tb, 0x7E);
        assert_eq!(twin.ch[0].dasb, 0x02);
        assert_eq!(twin.ch[0].ntrl, 0x83);
        assert!(twin.ch[0].fetch);
        assert_eq!(twin.joy[0], 0x1234);
        assert_eq!(twin.v, 17);
        assert!(twin.timeup_flag);
        // A damaged chunk is rejected.
        let mut twin2 = sys();
        assert!(twin2.restore(&snap[..snap.len() - 1]).is_err());
        let mut twin3 = sys();
        let mut wrong = snap.clone();
        wrong[0] = b'X';
        assert!(twin3.restore(&wrong).is_err());
        wrong[0] = b'S';
        wrong[4] = 9;
        assert!(twin3.restore(&wrong).is_err(), "the version is checked");
    }
}
