//! The Z80 core: registers, timing, contention, and interrupts.
//!
//! One CPU T-state is `cpu_div` domain ticks; the divider comes from
//! the config and defaults to 3 domain ticks per T-state. An opcode
//! fetch costs 4 T-states, a memory access 3, and a port access 4 with
//! its automatic wait state; the instruction tests cover the published
//! totals. Every access lands on a T-state boundary and carries its
//! exact tick, so triggers and bus watchers see the true access time.
//! The core reschedules itself after every instruction.
//!
//! Interrupt handling follows the published Z80: a latched NMI edge
//! jumps to 0x0066 in 11 T-states; the maskable interrupt samples the
//! line at instruction boundaries and runs the selected mode 0, 1, or
//! 2. EI defers maskable acceptance to the end of the following
//! instruction. HALT parks the core until an accepted interrupt or an
//! NMI wakes it, and the core drives the `halt` line while parked.
//!
//! Memory-contended machines (the Sinclair group) declare the
//! contended ranges and the delay-window phase in the CPU parameters.
//! The applied delay follows the published ULA pattern 6, 5, 4, 3, 2,
//! 1, 0, 0, indexed by the frame T-state counted from the last
//! interrupt edge, so a contended access at the window start waits up
//! to 6 T-states and a period's last two T-states wait none. The `IO`
//! contention of the Sinclair models keys on the high byte of the
//! port and on port 0xFE itself.

use demu_core::device::AccessWidth;
use demu_core::device::MachineCtx;
use demu_core::frequency::Tick;
use demu_core::signal::Level;
use demu_core::{Device, Snapshot, SnapshotError};

use demu_config::model::BootState;

/// Flag bit 0: carry.
pub const FLAG_C: u8 = 0x01;
/// Flag bit 1: add versus subtract.
pub const FLAG_N: u8 = 0x02;
/// Flag bit 2: parity or overflow.
pub const FLAG_P: u8 = 0x04;
/// Flag bit 3: the undocumented copy of operand bit 3.
pub const FLAG_X3: u8 = 0x08;
/// Flag bit 4: half carry.
pub const FLAG_H: u8 = 0x10;
/// Flag bit 5: the undocumented copy of operand bit 5.
pub const FLAG_X5: u8 = 0x20;
/// Flag bit 6: zero.
pub const FLAG_Z: u8 = 0x40;
/// Flag bit 7: sign.
pub const FLAG_S: u8 = 0x80;

/// The published ULA contention delay table, indexed by the frame
/// T-state modulo 8: an access at period offset 0 waits 6 T-states,
/// and the last two offsets wait none.
pub(crate) const CONTENTION: [u64; 8] = [6, 5, 4, 3, 2, 1, 0, 0];

/// A half-open address range like the `0x4000-0x8000` of the
/// contention parameters; the second value carries the exclusive
/// bound, so `0xE000-0x10000` names the top 16 KiB page.
pub(crate) type Range = (u32, u32);

/// The register file. The flag byte carries the six published bits
/// plus the two undocumented copy bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Regs {
    /// The accumulator and the flag byte: the high and low bytes of
    /// the AF pair.
    pub a: u8,
    pub f: u8,
    /// The BC pair.
    pub b: u8,
    pub c: u8,
    /// The DE pair.
    pub d: u8,
    pub e: u8,
    /// The HL pair.
    pub h: u8,
    pub l: u8,
    /// The IX and IY index pairs.
    pub ix: u16,
    pub iy: u16,
    /// The stack pointer.
    pub sp: u16,
    /// The program counter.
    pub pc: u16,
    /// The interrupt page register I.
    pub i: u8,
    /// The refresh counter R; bit 7 copies the I bit 7, bits 0-6
    /// increment per opcode fetch.
    pub r: u8,
    /// The shadow accumulator and flag byte of the AF' pair.
    pub a2: u8,
    pub f2: u8,
    /// The shadow BC pair.
    pub b2: u8,
    pub c2: u8,
    /// The shadow DE pair.
    pub d2: u8,
    pub e2: u8,
    /// The shadow HL pair.
    pub h2: u8,
    pub l2: u8,
}

impl Default for Regs {
    fn default() -> Self {
        Self {
            a: 0,
            f: 0,
            b: 0,
            c: 0,
            d: 0,
            e: 0,
            h: 0,
            l: 0,
            ix: 0,
            iy: 0,
            // The published power-on form: the stack pointer reads as
            // 0xFFFF and the rest of the file reads as zero.
            sp: 0xFFFF,
            pc: 0,
            i: 0,
            r: 0,
            a2: 0,
            f2: 0,
            b2: 0,
            c2: 0,
            d2: 0,
            e2: 0,
            h2: 0,
            l2: 0,
        }
    }
}

/// The Z80 core.
pub struct Z80 {
    name: String,
    /// The name of the memory bus this CPU accesses.
    pub bus_name: String,
    /// The name of the port bus this CPU accesses with IN and OUT.
    pub io_bus_name: String,
    /// Domain ticks per CPU T-state.
    pub cpu_div: u64,
    /// The configured divider, restored by `reset`.
    pub(crate) base_div: u64,
    /// The register file.
    pub regs: Regs,
    /// Interrupt flip-flop 1: maskable interrupts accepted when set.
    pub iff1: bool,
    /// Interrupt flip-flop 2: shadowed by NMI and restored by RETN.
    pub iff2: bool,
    /// The interrupt mode: 0, 1, or 2.
    pub im: u8,
    /// The core parked itself with a HALT instruction.
    pub halted: bool,
    /// The current level of the INT line.
    pub(crate) int_level: bool,
    /// The accepted INT level: the line must drop before the next
    /// level acceptance, so a held-high line services once.
    pub(crate) int_taken: bool,
    /// One deferred maskable acceptance after an EI.
    pub(crate) ei_delay: bool,
    /// The current level of the NMI line, for edge detection.
    pub(crate) nmi_line: bool,
    /// A latched NMI edge waiting for the next instruction boundary.
    pub(crate) nmi_edge: bool,
    /// The power-on reset sequence waits on the next visit.
    pub reset_pending: bool,
    /// The undocumented-opcode gate: the `undocumented` quirk flag.
    pub undocumented: bool,
    /// The contended memory ranges.
    pub(crate) mem_ranges: Vec<Range>,
    /// The contended port high-byte ranges.
    pub(crate) io_ranges: Vec<Range>,
    /// The frame T-state where the contention window opens.
    pub(crate) contend_from: u64,
    /// The frame T-state where the contention window closes.
    pub(crate) contend_to: u64,
    /// The frame T-state of the delay-table phase.
    pub(crate) contend_base: u64,
    /// The domain tick of the last INT rising edge, the frame anchor.
    pub(crate) frame_anchor: Option<u64>,
    /// The banked upper-RAM page of the 128K models is contended when
    /// the `contend_page` line reads high.
    pub(crate) contend_page: bool,
    pub(crate) next: Option<Tick>,
    last_tick: Option<Tick>,
}

impl std::fmt::Debug for Z80 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Z80")
            .field("name", &self.name)
            .field("regs", &self.regs)
            .field("cpu_div", &self.cpu_div)
            .field("iff1", &self.iff1)
            .field("iff2", &self.iff2)
            .field("im", &self.im)
            .field("halted", &self.halted)
            .finish()
    }
}

/// Parse a comma-separated list of `start-end` hexadecimal ranges.
///
/// A malformed entry drops; the config parameters stay declarative
/// strings and the shapes appear in the crate docs.
fn parse_ranges(text: &str) -> Vec<Range> {
    let hex = |s: &str| -> Option<u32> {
        let s = s.trim().trim_start_matches("0x").trim_start_matches("0X");
        u32::from_str_radix(s, 16).ok()
    };
    let mut out = Vec::new();
    for piece in text.split(',') {
        let Some((lo, hi)) = piece.trim().split_once('-') else {
            continue;
        };
        let (Some(lo), Some(hi)) = (hex(lo), hex(hi)) else {
            continue;
        };
        out.push((lo, hi));
    }
    out
}

impl Z80 {
    /// A core from its configuration entry.
    ///
    /// The `bus` parameter names the memory bus and the `io_bus`
    /// parameter the port bus; both fall back to the `io` bus name and
    /// the first configured bus. The `cpu` divider entry sets the
    /// domain ticks per T-state and defaults to 3. The contention
    /// parameters `contended`, `io_contend`, `contend_from`,
    /// `contend_to`, and `contend_base` shape the Sinclair delay; a
    /// machine without them never contends.
    pub fn new(
        device: &demu_config::model::DeviceInstance,
        config: &demu_config::model::SystemConfig,
    ) -> Self {
        let bus_name = device
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
        let io_bus_name = device
            .params
            .get("io_bus")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| {
                config
                    .buses
                    .iter()
                    .find(|b| b.name == "io")
                    .map(|b| b.name.clone())
                    .unwrap_or_else(|| bus_name.clone())
            });
        let base_div = device
            .dividers
            .get("cpu")
            .copied()
            .map(|d| u64::from(if d == 0 { 1 } else { d }))
            .unwrap_or(3);
        let mut cpu = Self {
            name: device.name.to_string(),
            bus_name,
            io_bus_name,
            cpu_div: base_div,
            base_div,
            regs: Regs::default(),
            iff1: false,
            iff2: false,
            im: 0,
            halted: false,
            int_level: false,
            int_taken: false,
            ei_delay: false,
            nmi_line: false,
            nmi_edge: false,
            reset_pending: false,
            undocumented: false,
            mem_ranges: device
                .params
                .get("contended")
                .and_then(|v| v.as_str())
                .map(parse_ranges)
                .unwrap_or_default(),
            io_ranges: device
                .params
                .get("io_contend")
                .and_then(|v| v.as_str())
                .map(parse_ranges)
                .unwrap_or_default(),
            contend_from: device
                .params
                .get("contend_from")
                .and_then(|v| v.as_integer())
                .map(|v| u64::try_from(v).unwrap_or(u64::MAX))
                .unwrap_or(u64::MAX),
            contend_to: device
                .params
                .get("contend_to")
                .and_then(|v| v.as_integer())
                .map(|v| u64::try_from(v).unwrap_or(u64::MAX))
                .unwrap_or(u64::MAX),
            contend_base: device
                .params
                .get("contend_base")
                .and_then(|v| v.as_integer())
                .map(|v| u64::try_from(v).unwrap_or(0))
                .unwrap_or(0),
            frame_anchor: None,
            contend_page: false,
            next: None,
            last_tick: None,
        };
        for (name, value) in &config.quirks {
            if name.as_str().ends_with(".undocumented")
                && name.rsplit_once('.').map(|(f, _)| f) == Some(device.kind.family())
            {
                cpu.undocumented = value.as_flag();
            }
        }
        cpu
    }

    /// Schedule the next core visit `t` T-states after `tick`.
    pub fn schedule_after(&mut self, tick: Tick, t: u32) {
        self.next = Some(Tick(tick.0 + self.cpu_div * u64::from(t)));
    }

    /// The first T-state boundary strictly after `tick`. The core is
    /// mid-instruction when a line wakes it, so a boundary at the
    /// current tick would re-schedule the visit the scheduler has
    /// running and would loop the queue.
    fn next_cycle_after(&self, tick: Tick) -> Option<Tick> {
        Some(Tick((tick.0 / self.cpu_div + 1) * self.cpu_div))
    }

    /// Wake a parked or idle core at the next T-state boundary.
    fn wake(&mut self, tick: Tick) {
        if self.next.is_none() {
            self.next = self.next_cycle_after(tick);
            self.last_tick = None;
        }
    }

    /// Advance the refresh counter one opcode fetch. Bits 0-6 count
    /// and bit 7 copies the I bit 7.
    pub(crate) fn r_bump(&mut self) {
        self.regs.r = (self.regs.i & 0x80) | ((self.regs.r + 1) & 0x7F);
    }

    /// True when a maskable interrupt can be accepted at this
    /// boundary: the line reads high, IFF1 is set, the level was not
    /// accepted already, and no EI delay guards the boundary.
    pub(crate) fn int_acceptable(&self) -> bool {
        self.int_level && self.iff1 && !self.int_taken && !self.ei_delay
    }

    /// True when the frame anchor is captured and a range covers the
    /// frame T-state `t`. A test-audit helper: the access path uses
    /// its copied shape.
    #[cfg(test)]
    pub(crate) fn delay_at(&self, t: u64) -> u64 {
        if self.frame_anchor.is_none() || self.contend_from >= self.contend_to {
            return 0;
        }
        if t < self.contend_from || t >= self.contend_to {
            return 0;
        }
        CONTENTION[((t.wrapping_sub(self.contend_base)) & 7) as usize]
    }

    /// True when the address `a` sits in the contended memory set:
    /// a declared range, or the banked upper RAM page when the
    /// `contend_page` line reads high. A test-audit helper: the
    /// access path uses its copied shape.
    #[cfg(test)]
    pub(crate) fn mem_contended(&self, a: u16) -> bool {
        let a = u32::from(a);
        if self.mem_ranges.iter().any(|(lo, hi)| a >= *lo && a < *hi) {
            return true;
        }
        self.contend_page && a >= 0xC000
    }

    /// True when the port `p` contends: a declared high-byte range, or
    /// the ULA port 0xFE itself. A test-audit helper.
    #[cfg(test)]
    pub(crate) fn io_contended(&self, p: u16) -> bool {
        let high = u32::from(p >> 8);
        self.io_ranges
            .iter()
            .any(|(lo, hi)| high >= *lo && high < *hi)
            || (p & 0xFF) == 0xFE
    }
}

/// A bus access helper bound to one instruction's T-state counter.
///
/// Every access stamps its tick as the start tick plus one T-state
/// width per preceding access plus the applied contention delay. The
/// helper also forwards notifications and event reports, because the
/// machine context is exclusively borrowed here.
pub struct Access<'a, 'c> {
    ctx: &'c mut MachineCtx<'a>,
    tick: Tick,
    /// T-states consumed so far.
    k: u64,
    cpu_div: u64,
    bus_name: String,
    io_bus: String,
    anchor: Option<u64>,
    pub(crate) from: u64,
    pub(crate) to: u64,
    pub(crate) base: u64,
    page_bit: bool,
    mem_r: [Range; 4],
    mem_n: u8,
    io_r: [Range; 4],
    io_n: u8,
}

impl<'a, 'c> Access<'a, 'c> {
    /// An access helper for one instruction at its start tick. The
    /// contention shape copies out of the core, so the instruction
    /// functions borrow the core and the helper independently.
    pub fn for_cpu(cpu: &Z80, tick: Tick, ctx: &'c mut MachineCtx<'a>) -> Self {
        let mut mem_r = [(0u32, 0u32); 4];
        let mut mem_n = 0u8;
        for r in cpu.mem_ranges.iter().take(4) {
            mem_r[mem_n as usize] = *r;
            mem_n += 1;
        }
        let mut io_r = [(0u32, 0u32); 4];
        let mut io_n = 0u8;
        for r in cpu.io_ranges.iter().take(4) {
            io_r[io_n as usize] = *r;
            io_n += 1;
        }
        Self {
            ctx,
            tick,
            k: 0,
            cpu_div: cpu.cpu_div,
            bus_name: cpu.bus_name.clone(),
            io_bus: cpu.io_bus_name.clone(),
            anchor: cpu.frame_anchor,
            from: cpu.contend_from,
            to: cpu.contend_to,
            base: cpu.contend_base,
            page_bit: cpu.contend_page,
            mem_r,
            mem_n,
            io_r,
            io_n,
        }
    }

    /// The frame T-state of the access at the current counter, floor
    /// divided from domain ticks of the frame anchor.
    pub(crate) fn t_state(&self) -> u64 {
        let Some(anchor) = self.anchor else {
            return u64::MAX;
        };
        let now = self.tick.0 + self.cpu_div * self.k;
        if now < anchor {
            return u64::MAX;
        }
        (now - anchor) / self.cpu_div
    }

    /// The contention delay for a memory access at the current
    /// counter over `a`.
    pub(crate) fn mem_delay(&self, a: u16) -> u64 {
        if self.mem_n == 0 && !self.page_bit {
            return 0;
        }
        let a = u32::from(a);
        let ranged = self.mem_r[..self.mem_n as usize]
            .iter()
            .any(|(lo, hi)| a >= *lo && a < *hi);
        if !ranged && !(self.page_bit && a >= 0x0000_C000) {
            return 0;
        }
        let t = self.t_state();
        if t == u64::MAX || t < self.from || t >= self.to {
            return 0;
        }
        CONTENTION[((t.wrapping_sub(self.base)) & 7) as usize]
    }

    /// The contention delay for a port access over `p`: the high-byte
    /// ranges, or the ULA port 0xFE.
    pub(crate) fn io_delay(&self, p: u16) -> u64 {
        let high = u32::from(p >> 8);
        let ranged = self.io_r[..self.io_n as usize]
            .iter()
            .any(|(lo, hi)| high >= *lo && high < *hi);
        if !ranged && (p & 0xFF) != 0xFE {
            return 0;
        }
        let t = self.t_state();
        if t == u64::MAX || t < self.from || t >= self.to {
            return 0;
        }
        CONTENTION[((t.wrapping_sub(self.base)) & 7) as usize]
    }

    /// The stamp of an access `extra` T-states past the counter.
    #[inline]
    fn stamp(&self, extra: u64) -> Tick {
        Tick(self.tick.0 + self.cpu_div * (self.k + extra))
    }

    /// Read one byte of memory; a 3 T-state access plus the delay.
    #[inline]
    pub fn rd(&mut self, addr: u16) -> u8 {
        let d = self.mem_delay(addr);
        let at = self.stamp(d);
        self.k += 3 + d;
        self.ctx.read(
            &self.bus_name,
            AccessWidth::Byte,
            demu_core::Address(u32::from(addr)),
            at,
        ) as u8
    }

    /// Write one byte of memory; a 3 T-state access plus the delay.
    #[inline]
    pub fn wr(&mut self, addr: u16, value: u8) {
        let d = self.mem_delay(addr);
        let at = self.stamp(d);
        self.k += 3 + d;
        self.ctx.write(
            &self.bus_name,
            AccessWidth::Byte,
            demu_core::Address(u32::from(addr)),
            u32::from(value),
            at,
        );
    }

    /// Fetch one opcode byte; a 4 T-state cycle plus the delay.
    #[inline]
    pub fn m1(&mut self, addr: u16) -> u8 {
        let (at, d) = self.stamp_for(addr);
        self.k += 4 + d;
        self.ctx.read(
            &self.bus_name,
            AccessWidth::Byte,
            demu_core::Address(u32::from(addr)),
            at,
        ) as u8
    }

    /// The stamp of an M1 fetch with its same delay shape as a read
    /// but a 4 T-state width; the pair carries the applied delay.
    #[inline]
    fn stamp_for(&self, addr: u16) -> (Tick, u64) {
        let d = if self.mem_n == 0 && !self.page_bit {
            0
        } else {
            let a = u32::from(addr);
            let ranged = self.mem_r[..self.mem_n as usize]
                .iter()
                .any(|(lo, hi)| a >= *lo && a < *hi);
            if ranged || (self.page_bit && a >= 0x0000_C000) {
                let t = self.t_state();
                if t == u64::MAX || t < self.from || t >= self.to {
                    0
                } else {
                    CONTENTION[((t.wrapping_sub(self.base)) & 7) as usize]
                }
            } else {
                0
            }
        };
        (self.stamp(d), d)
    }

    /// Read one port; a 4 T-state cycle with its automatic wait and
    /// the I/O contention delay.
    #[inline]
    pub fn rd_io(&mut self, port: u16) -> u8 {
        let d = self.io_delay(port);
        let at = self.stamp(d);
        self.k += 4 + d;
        self.ctx.read(
            &self.io_bus,
            AccessWidth::Byte,
            demu_core::Address(0x1_0000 | u32::from(port)),
            at,
        ) as u8
    }

    /// Write one port; a 4 T-state cycle with its automatic wait and
    /// the I/O contention delay.
    #[inline]
    pub fn wr_io(&mut self, port: u16, value: u8) {
        let d = self.io_delay(port);
        let at = self.stamp(d);
        self.k += 4 + d;
        self.ctx.write(
            &self.io_bus,
            AccessWidth::Byte,
            demu_core::Address(0x1_0000 | u32::from(port)),
            u32::from(value),
            at,
        );
    }

    /// Pay `t` internal T-states.
    #[inline]
    pub fn wait(&mut self, t: u32) {
        self.k += u64::from(t);
    }

    /// The number of T-states consumed so far.
    pub fn cycles(&self) -> u32 {
        self.k as u32
    }

    /// Report a runtime event from the core.
    pub fn emit(&mut self, event: demu_core::triggers::TriggerEvent) {
        self.ctx.emit(event);
    }
}

/// The bus abstraction of one instruction's accesses: the machine
/// context stamps exact ticks, and the instruction tests run over a
/// plain byte map with the same step function.
pub trait Cycles {
    /// Fetch one opcode byte (4 T-states).
    fn m1(&mut self, addr: u16) -> u8;
    /// Read one byte of memory (3 T-states).
    fn rd(&mut self, addr: u16) -> u8;
    /// Write one byte of memory (3 T-states).
    fn wr(&mut self, addr: u16, value: u8);
    /// Read one port (4 T-states with the automatic wait).
    fn rd_io(&mut self, port: u16) -> u8;
    /// Write one port (4 T-states with the automatic wait).
    fn wr_io(&mut self, port: u16, value: u8);
    /// Pay internal T-states.
    fn wait(&mut self, t: u32);
    /// The number of T-states consumed so far.
    fn cycles(&self) -> u32;
    /// Report one runtime event from the core.
    fn emit(&mut self, event: demu_core::triggers::TriggerEvent);
}

impl Cycles for Access<'_, '_> {
    fn m1(&mut self, addr: u16) -> u8 {
        Access::m1(self, addr)
    }

    fn rd(&mut self, addr: u16) -> u8 {
        Access::rd(self, addr)
    }

    fn wr(&mut self, addr: u16, value: u8) {
        Access::wr(self, addr, value)
    }

    fn rd_io(&mut self, port: u16) -> u8 {
        Access::rd_io(self, port)
    }

    fn wr_io(&mut self, port: u16, value: u8) {
        Access::wr_io(self, port, value)
    }

    fn wait(&mut self, t: u32) {
        Access::wait(self, t)
    }

    fn cycles(&self) -> u32 {
        Access::cycles(self)
    }

    fn emit(&mut self, event: demu_core::triggers::TriggerEvent) {
        Access::emit(self, event)
    }
}

impl Device for Z80 {
    fn name(&self) -> &str {
        &self.name
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn cpu(&self) -> Option<&dyn demu_core::CpuDebug> {
        Some(self)
    }

    fn cpu_mut(&mut self) -> Option<&mut dyn demu_core::CpuDebug> {
        Some(self)
    }

    fn reset(&mut self) {
        self.regs = Regs::default();
        self.cpu_div = self.base_div;
        self.iff1 = false;
        self.iff2 = false;
        self.im = 0;
        self.halted = false;
        self.int_level = false;
        self.int_taken = false;
        self.ei_delay = false;
        self.nmi_line = false;
        self.nmi_edge = false;
        self.reset_pending = false;
        self.frame_anchor = None;
        self.contend_page = false;
        self.next = None;
        self.last_tick = None;
    }

    fn boot(&mut self, state: Option<&BootState>) {
        self.reset();
        if let Some(BootState::Cpu { registers }) = state {
            let debug: &mut dyn demu_core::CpuDebug = self;
            for (name, value) in registers {
                debug.set_register(name, *value);
            }
        }
        // The power-on reset sequence runs on the first visit.
        self.reset_pending = true;
        self.next = Some(Tick(0));
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(64);
        out.extend_from_slice(b"Z80A");
        out.push(1);
        // The register file: the current file then the shadow file.
        out.extend_from_slice(&[
            self.regs.a,
            self.regs.f,
            self.regs.b,
            self.regs.c,
            self.regs.d,
            self.regs.e,
            self.regs.h,
            self.regs.l,
            self.regs.i,
            self.regs.r,
            self.regs.a2,
            self.regs.f2,
            self.regs.b2,
            self.regs.c2,
            self.regs.d2,
            self.regs.e2,
            self.regs.h2,
            self.regs.l2,
        ]);
        out.extend_from_slice(&self.regs.ix.to_le_bytes());
        out.extend_from_slice(&self.regs.iy.to_le_bytes());
        out.extend_from_slice(&self.regs.sp.to_le_bytes());
        out.extend_from_slice(&self.regs.pc.to_le_bytes());
        // The control state: iff1, iff2, im, halted, int level, int
        // taken, ei delay, nmi line, nmi edge, reset pending,
        // undocumented gate, contend page bit, last tick presence.
        out.extend_from_slice(&[
            u8::from(self.iff1),
            u8::from(self.iff2),
            self.im,
            u8::from(self.halted),
            u8::from(self.int_level),
            u8::from(self.int_taken),
            u8::from(self.ei_delay),
            u8::from(self.nmi_line),
            u8::from(self.nmi_edge),
            u8::from(self.reset_pending),
            u8::from(self.undocumented),
            u8::from(self.contend_page),
        ]);
        out.extend_from_slice(&self.cpu_div.to_le_bytes());
        out.extend_from_slice(&self.contend_from.to_le_bytes());
        out.extend_from_slice(&self.contend_to.to_le_bytes());
        out.extend_from_slice(&self.contend_base.to_le_bytes());
        match self.frame_anchor {
            Some(anchor) => {
                out.push(1);
                out.extend_from_slice(&anchor.to_le_bytes());
            }
            None => out.push(0),
        }
        out.push(self.mem_ranges.len() as u8);
        for (lo, hi) in &self.mem_ranges {
            out.extend_from_slice(&lo.to_le_bytes());
            out.extend_from_slice(&hi.to_le_bytes());
        }
        out.push(self.io_ranges.len() as u8);
        for (lo, hi) in &self.io_ranges {
            out.extend_from_slice(&lo.to_le_bytes());
            out.extend_from_slice(&hi.to_le_bytes());
        }
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        // The minimum form with two range slots empty: 4 tag + 1
        // version + 18 registers + 8 pairs + 12 control bytes + 4 u64
        // fields + 1 anchor flag + 1 range count + 1 range count.
        if data.len() < 78 {
            return Err(SnapshotError::Mismatch(format!(
                "z80 chunk must be at least 78 bytes, got {}",
                data.len()
            )));
        }
        if &data[..4] != b"Z80A" {
            return Err(SnapshotError::Mismatch("bad z80 tag".into()));
        }
        if data[4] != 1 {
            return Err(SnapshotError::Mismatch(
                "unsupported z80 chunk version".into(),
            ));
        }
        let mut i = 5usize;
        let next = |n: usize, i: &mut usize| -> Result<&[u8], SnapshotError> {
            let end = *i + n;
            let slice = data
                .get(*i..end)
                .ok_or_else(|| SnapshotError::Mismatch("truncated z80 chunk".to_string()))?;
            *i = end;
            Ok(slice)
        };
        let regs_bytes = next(18, &mut i)?;
        self.regs.a = regs_bytes[0];
        self.regs.f = regs_bytes[1];
        self.regs.b = regs_bytes[2];
        self.regs.c = regs_bytes[3];
        self.regs.d = regs_bytes[4];
        self.regs.e = regs_bytes[5];
        self.regs.h = regs_bytes[6];
        self.regs.l = regs_bytes[7];
        self.regs.i = regs_bytes[8];
        self.regs.r = regs_bytes[9];
        self.regs.a2 = regs_bytes[10];
        self.regs.f2 = regs_bytes[11];
        self.regs.b2 = regs_bytes[12];
        self.regs.c2 = regs_bytes[13];
        self.regs.d2 = regs_bytes[14];
        self.regs.e2 = regs_bytes[15];
        self.regs.h2 = regs_bytes[16];
        self.regs.l2 = regs_bytes[17];
        let pairs = next(8, &mut i)?;
        self.regs.ix = u16::from_le_bytes(pairs[0..2].try_into().expect("2 bytes"));
        self.regs.iy = u16::from_le_bytes(pairs[2..4].try_into().expect("2 bytes"));
        self.regs.sp = u16::from_le_bytes(pairs[4..6].try_into().expect("2 bytes"));
        self.regs.pc = u16::from_le_bytes(pairs[6..8].try_into().expect("2 bytes"));
        let control = next(12, &mut i)?;
        self.iff1 = control[0] != 0;
        self.iff2 = control[1] != 0;
        self.im = control[2].clamp(0, 2);
        self.halted = control[3] != 0;
        self.int_level = control[4] != 0;
        self.int_taken = control[5] != 0;
        self.ei_delay = control[6] != 0;
        self.nmi_line = control[7] != 0;
        self.nmi_edge = control[8] != 0;
        self.reset_pending = control[9] != 0;
        self.undocumented = control[10] != 0;
        self.contend_page = control[11] != 0;
        let div = u64::from_le_bytes(next(8, &mut i)?.try_into().expect("8 bytes"));
        self.cpu_div = div.clamp(1, 64);
        self.contend_from = u64::from_le_bytes(next(8, &mut i)?.try_into().expect("8 bytes"));
        self.contend_to = u64::from_le_bytes(next(8, &mut i)?.try_into().expect("8 bytes"));
        self.contend_base = u64::from_le_bytes(next(8, &mut i)?.try_into().expect("8 bytes"));
        let anchor_flag = next(1, &mut i)?[0];
        self.frame_anchor = if anchor_flag == 1 {
            Some(u64::from_le_bytes(
                next(8, &mut i)?.try_into().expect("8 bytes"),
            ))
        } else {
            None
        };
        let mem_n = next(1, &mut i)?[0] as usize;
        self.mem_ranges.clear();
        for _ in 0..mem_n {
            let b = next(8, &mut i)?;
            self.mem_ranges.push((
                u32::from_le_bytes(b[0..4].try_into().expect("4 bytes")),
                u32::from_le_bytes(b[4..8].try_into().expect("4 bytes")),
            ));
        }
        let io_n = next(1, &mut i)?[0] as usize;
        self.io_ranges.clear();
        for _ in 0..io_n {
            let b = next(8, &mut i)?;
            self.io_ranges.push((
                u32::from_le_bytes(b[0..4].try_into().expect("4 bytes")),
                u32::from_le_bytes(b[4..8].try_into().expect("4 bytes")),
            ));
        }
        if i != data.len() {
            return Err(SnapshotError::Mismatch(format!(
                "z80 chunk has {} trailing bytes",
                data.len() - i
            )));
        }
        self.next = None;
        self.last_tick = None;
        Ok(())
    }

    fn notify_line(&mut self, line: &str, level: Level, tick: Tick) {
        match line {
            "nmi" => {
                let was = self.nmi_line;
                self.nmi_line = level == Level::High;
                // The NMI input is edge sensitive: a held-high line
                // latches one service only.
                if self.nmi_line && !was {
                    self.nmi_edge = true;
                    self.wake(tick);
                }
            }
            "int" => {
                self.int_level = level == Level::High;
                if self.int_level {
                    self.frame_anchor = Some(tick.0);
                    self.wake(tick);
                } else {
                    // The line dropped: the next high level services.
                    self.int_taken = false;
                }
            }
            "contend_page" => {
                self.contend_page = level == Level::High;
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
        crate::exec::step(self, tick, ctx);
    }
}

impl Z80 {
    /// Consume one latched NMI edge.
    pub(crate) fn take_nmi(&mut self) -> bool {
        std::mem::take(&mut self.nmi_edge)
    }
}

impl demu_core::CpuDebug for Z80 {
    fn registers(&self) -> std::collections::BTreeMap<String, u64> {
        let mut map = std::collections::BTreeMap::new();
        let r = &self.regs;
        map.insert("A".to_string(), u64::from(r.a));
        map.insert("F".to_string(), u64::from(r.f));
        map.insert("B".to_string(), u64::from(r.b));
        map.insert("C".to_string(), u64::from(r.c));
        map.insert("D".to_string(), u64::from(r.d));
        map.insert("E".to_string(), u64::from(r.e));
        map.insert("H".to_string(), u64::from(r.h));
        map.insert("L".to_string(), u64::from(r.l));
        map.insert("I".to_string(), u64::from(r.i));
        map.insert("R".to_string(), u64::from(r.r));
        map.insert(
            "AF".to_string(),
            u64::from(u16::from(r.a) << 8 | u16::from(r.f)),
        );
        map.insert(
            "BC".to_string(),
            u64::from(u16::from(r.b) << 8 | u16::from(r.c)),
        );
        map.insert(
            "DE".to_string(),
            u64::from(u16::from(r.d) << 8 | u16::from(r.e)),
        );
        map.insert(
            "HL".to_string(),
            u64::from(u16::from(r.h) << 8 | u16::from(r.l)),
        );
        map.insert("IX".to_string(), u64::from(r.ix));
        map.insert("IY".to_string(), u64::from(r.iy));
        map.insert("IXH".to_string(), u64::from(r.ix >> 8));
        map.insert("IXL".to_string(), u64::from(r.ix & 0xFF));
        map.insert("IYH".to_string(), u64::from(r.iy >> 8));
        map.insert("IYL".to_string(), u64::from(r.iy & 0xFF));
        map.insert("SP".to_string(), u64::from(r.sp));
        map.insert("PC".to_string(), u64::from(r.pc));
        map.insert("IM".to_string(), u64::from(self.im));
        map.insert("IFF1".to_string(), u64::from(self.iff1));
        map.insert("IFF2".to_string(), u64::from(self.iff2));
        map
    }

    fn set_register(&mut self, name: &str, value: u64) -> bool {
        let low = value as u8;
        match name.to_ascii_uppercase().as_str() {
            "A" => self.regs.a = low,
            "F" => self.regs.f = low,
            "B" => self.regs.b = low,
            "C" => self.regs.c = low,
            "D" => self.regs.d = low,
            "E" => self.regs.e = low,
            "H" => self.regs.h = low,
            "L" => self.regs.l = low,
            "I" => self.regs.i = low,
            "R" => self.regs.r = low,
            "AF" => {
                self.regs.a = (value >> 8) as u8;
                self.regs.f = value as u8;
            }
            "BC" => {
                self.regs.b = (value >> 8) as u8;
                self.regs.c = value as u8;
            }
            "DE" => {
                self.regs.d = (value >> 8) as u8;
                self.regs.e = value as u8;
            }
            "HL" => {
                self.regs.h = (value >> 8) as u8;
                self.regs.l = value as u8;
            }
            "IX" => self.regs.ix = value as u16,
            "IY" => self.regs.iy = value as u16,
            "IXH" => self.regs.ix = self.regs.ix & 0x00FF | ((value as u16) << 8),
            "IXL" => self.regs.ix = self.regs.ix & 0xFF00 | u16::from(value as u8),
            "IYH" => self.regs.iy = self.regs.iy & 0x00FF | ((value as u16) << 8),
            "IYL" => self.regs.iy = self.regs.iy & 0xFF00 | u16::from(value as u8),
            "SP" => self.regs.sp = value as u16,
            "PC" => self.regs.pc = value as u16,
            "IM" => self.im = (value as u8).clamp(0, 2),
            "IFF1" => self.iff1 = value & 1 != 0,
            "IFF2" => self.iff2 = value & 1 != 0,
            _ => return false,
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use demu_core::CpuDebug as _;

    fn instance() -> demu_config::model::DeviceInstance {
        let raw = r#"
            [system]
            name = "test"
            triple = "z80-test"
            [[bus]]
            name = "cpu"
            address_bits = 16
            owner = "cpu0"
            [[bus]]
            name = "io"
            address_bits = 16
            owner = "cpu0"
            [clock.main]
            frequency = "3500000"
            [[device]]
            name = "cpu0"
            kind = "z80.cpu"
            domain = "main"
            [device.params]
            bus = "cpu"
            io_bus = "io"
            contended = "0x4000-0x8000,0xE000-0x10000"
            io_contend = "0x40-0x80"
            [device.dividers]
            cpu = 1
            [quirks]
            "z80.undocumented" = true
            "#;
        let (config, _) =
            demu_config::validate(&demu_config::RawConfig::parse(raw).expect("test config parses"))
                .expect("test config validates");
        config.devices[0].clone()
    }

    fn config_of() -> demu_config::model::SystemConfig {
        let raw = r#"
            [system]
            name = "test"
            triple = "z80-test"
            [[bus]]
            name = "cpu"
            address_bits = 16
            owner = "cpu0"
            [[bus]]
            name = "io"
            address_bits = 16
            owner = "cpu0"
            [clock.main]
            frequency = "3500000"
            [[device]]
            name = "cpu0"
            kind = "z80.cpu"
            domain = "main"
            [device.params]
            bus = "cpu"
            io_bus = "io"
            contended = "0x4000-0x8000,0xE000-0x10000"
            io_contend = "0x40-0x80"
            [device.dividers]
            cpu = 1
            [quirks]
            "z80.undocumented" = true
            "#;
        demu_config::validate(&demu_config::RawConfig::parse(raw).expect("parse"))
            .expect("validate")
            .0
    }

    #[test]
    fn the_config_shapes_the_core() {
        let config = config_of();
        let cpu = Z80::new(&instance(), &config);
        assert_eq!(cpu.cpu_div, 1);
        assert_eq!(cpu.bus_name, "cpu");
        assert_eq!(cpu.io_bus_name, "io");
        assert!(cpu.undocumented);
        assert_eq!(cpu.mem_ranges, vec![(0x4000, 0x8000), (0xE000, 0x10000)]);
        assert_eq!(cpu.io_ranges, vec![(0x40, 0x80)]);
        assert!(cpu.mem_contended(0x5000));
        assert!(cpu.mem_contended(0xF000));
        assert!(!cpu.mem_contended(0x1000));
        assert!(cpu.io_contended(0x4100));
        assert!(cpu.io_contended(0x00FE));
        assert!(!cpu.io_contended(0x0100));
    }

    #[test]
    fn a_default_core_has_no_contention() {
        let raw = r#"
            [system]
            name = "test"
            triple = "z80-test"
            [[bus]]
            name = "cpu"
            address_bits = 16
            owner = "cpu0"
            [clock.main]
            frequency = "3500000"
            [[device]]
            name = "cpu0"
            kind = "z80.cpu"
            domain = "main"
            [device.dividers]
            cpu = 3
            "#;
        let config = demu_config::validate(&demu_config::RawConfig::parse(raw).expect("parse"))
            .expect("validate")
            .0;
        let cpu = Z80::new(&config.devices[0].clone(), &config);
        assert_eq!(cpu.cpu_div, 3);
        assert!(cpu.mem_ranges.is_empty());
        assert!(!cpu.mem_contended(0x5000));
        assert_eq!(cpu.delay_at(0), 0);
    }

    #[test]
    fn the_contention_window_follows_the_published_table() {
        let raw = r#"
            [system]
            name = "test"
            triple = "z80-test"
            [[bus]]
            name = "cpu"
            address_bits = 16
            owner = "cpu0"
            [clock.main]
            frequency = "3500000"
            [[device]]
            name = "cpu0"
            kind = "z80.cpu"
            domain = "main"
            [device.params]
            contended = "0x4000-0x8000"
            contend_from = 14336
            contend_to = 57344
            contend_base = 14344
            "#;
        let config = demu_config::validate(&demu_config::RawConfig::parse(raw).expect("parse"))
            .expect("validate")
            .0;
        let mut cpu = Z80::new(&config.devices[0].clone(), &config);
        // The window is open and the anchor captured; the delay reads
        // the published table through the exact phase.
        cpu.frame_anchor = Some(0);
        assert_eq!(cpu.delay_at(14344), 6);
        assert_eq!(cpu.delay_at(14345), 5);
        assert_eq!(cpu.delay_at(14350), 0);
        assert_eq!(cpu.delay_at(14351), 0);
        assert_eq!(cpu.delay_at(14352), 6);
        assert_eq!(cpu.delay_at(0), 0, "the window is closed before 14336");
        assert_eq!(cpu.delay_at(14336), 6, "the first window cycle wraps to 6");
        assert_eq!(cpu.delay_at(57344), 0, "the window closed at 57344");
    }

    #[test]
    fn nmi_latches_once_per_edge() {
        let config = config_of();
        let mut cpu = Z80::new(&instance(), &config);
        cpu.boot(None);
        cpu.notify_line("nmi", Level::High, Tick(0));
        assert!(cpu.nmi_edge);
        cpu.notify_line("nmi", Level::High, Tick(16));
        assert!(cpu.take_nmi());
        assert!(!cpu.int_acceptable(), "no maskable latching here");
        cpu.notify_line("nmi", Level::Low, Tick(32));
        cpu.notify_line("nmi", Level::High, Tick(32));
        assert!(cpu.nmi_edge, "a new edge latches again");
    }

    #[test]
    fn the_int_level_rearms_on_the_trailing_edge() {
        let config = config_of();
        let mut cpu = Z80::new(&instance(), &config);
        cpu.boot(None);
        cpu.iff1 = true;
        cpu.notify_line("int", Level::High, Tick(0));
        assert!(cpu.int_acceptable());
        cpu.int_taken = true;
        assert!(!cpu.int_acceptable(), "the level already serviced");
        cpu.notify_line("int", Level::Low, Tick(8));
        assert!(!cpu.int_acceptable(), "the line dropped");
        cpu.notify_line("int", Level::High, Tick(16));
        assert!(cpu.int_acceptable(), "the level rearmed");
        assert_eq!(cpu.frame_anchor, Some(16), "the rising edge is the anchor");
    }

    #[test]
    fn power_on_state_and_boot_presets() {
        let config = config_of();
        let mut cpu = Z80::new(&instance(), &config);
        cpu.reset();
        cpu.boot(None);
        assert_eq!(cpu.regs.sp, 0xFFFF);
        assert!(!cpu.iff1);
        assert_eq!(cpu.im, 0);
        assert_eq!(cpu.next, Some(Tick(0)));
        assert!(cpu.reset_pending);
        let presets = demu_config::model::BootState::Cpu {
            registers: [("SP".to_string(), 0xFF40), ("PC".to_string(), 0x1234)]
                .into_iter()
                .collect(),
        };
        cpu.boot(Some(&presets));
        assert_eq!(cpu.regs.sp, 0xFF40);
        assert_eq!(cpu.regs.pc, 0x1234);
    }

    #[test]
    fn snapshot_round_trips() {
        let config = config_of();
        let mut cpu = Z80::new(&instance(), &config);
        cpu.boot(None);
        cpu.regs = Regs {
            a: 0x11,
            a2: 0x12,
            f: FLAG_C | FLAG_Z,
            f2: FLAG_S,
            b: 0x22,
            b2: 0x10,
            c: 0x33,
            c2: 0x20,
            d: 0x44,
            d2: 0x30,
            e: 0x55,
            e2: 0x03,
            h: 0x66,
            h2: 0x02,
            l: 0x77,
            l2: 0x01,
            ix: 0x1234,
            iy: 0x5678,
            sp: 0x9ABC,
            pc: 0x0DEF,
            i: 0x40,
            r: 0x0F,
        };
        cpu.iff1 = true;
        cpu.iff2 = true;
        cpu.im = 2;
        cpu.halted = true;
        cpu.contend_page = true;
        cpu.frame_anchor = Some(777);
        cpu.notify_line("nmi", Level::High, Tick(1));
        let saved = cpu.snapshot();
        let mut twin = Z80::new(&instance(), &config);
        twin.restore(&saved).expect("round trip");
        assert_eq!(twin.regs.a, 0x11);
        assert_eq!(twin.regs.f, FLAG_C | FLAG_Z);
        assert_eq!(twin.regs.ix, 0x1234);
        assert_eq!(twin.regs.pc, 0x0DEF);
        assert!(twin.iff1 && twin.iff2);
        assert_eq!(twin.im, 2);
        assert!(twin.halted && twin.contend_page);
        assert!(twin.nmi_edge);
        assert_eq!(twin.frame_anchor, Some(777));
        assert_eq!(twin.mem_ranges, vec![(0x4000, 0x8000), (0xE000, 0x10000)]);
    }

    #[test]
    fn restore_rejects_bad_lengths() {
        let config = config_of();
        let mut cpu = Z80::new(&instance(), &config);
        let saved = cpu.snapshot();
        assert!(cpu.restore(&saved[..saved.len() - 1]).is_err());
        assert!(cpu.restore(&[]).is_err());
        let mut tagged = saved.clone();
        tagged[0] = b'X';
        assert!(cpu.restore(&tagged).is_err());
        let mut version = saved;
        version[4] = 9;
        assert!(cpu.restore(&version).is_err());
    }

    #[test]
    fn register_names_round_trip_through_the_debug_interface() {
        let config = config_of();
        let mut cpu = Z80::new(&instance(), &config);
        let set: &mut dyn demu_core::CpuDebug = &mut cpu;
        assert!(set.set_register("HL", 0x1234));
        assert!(set.set_register("A", 0x42));
        assert!(set.set_register("IXH", 0x99));
        assert!(set.set_register("IM", 2));
        assert!(!set.set_register("NOPE", 1));
        let regs = cpu.registers();
        assert_eq!(regs["HL"], 0x1234);
        assert_eq!(regs["A"], 0x42);
        assert_eq!(regs["IXH"], 0x99);
        assert_eq!(regs["IM"], 2);
        assert_eq!(regs.len(), 25);
    }
}
