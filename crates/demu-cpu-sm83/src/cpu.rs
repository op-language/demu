//! The SM83 core: registers, timing, and the interrupt machinery.
//!
//! One domain tick is one T-cycle; one M-cycle is four T-cycles. Every
//! bus access of an instruction lands on an M-cycle boundary and carries
//! its exact tick, so watchpoints and DMA see the true access time. The
//! CPU reschedules itself after every instruction and idles when halted
//! or stopped; a signal-line edge that raises a pending interrupt wakes
//! it at the next M-cycle.

use demu_core::device::MachineCtx;
use demu_core::frequency::Tick;
use demu_core::signal::Level;
use demu_core::{Device, Snapshot, SnapshotError};

use demu_config::model::BootState;

/// The register file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Regs {
    /// The accumulator.
    pub a: u8,
    /// The flags: Z, N, H, C in bits 7, 6, 5, 4.
    pub f: u8,
    pub b: u8,
    pub c: u8,
    pub d: u8,
    pub e: u8,
    pub h: u8,
    pub l: u8,
    /// The stack pointer.
    pub sp: u16,
    /// The instruction pointer.
    pub pc: u16,
}

/// The power-on boot register values a real boot ROM leaves behind.
///
/// The cartridge reads these to detect the machine: DMG leaves `A = 1`,
/// the CGB leaves `A = 0x11`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BootRegs {
    pub a: u8,
    pub f: u8,
    pub b: u8,
    pub c: u8,
    pub d: u8,
    pub e: u8,
    pub h: u8,
    pub l: u8,
    pub sp: u16,
}

/// The DMG hand-off register values.
pub const BOOT_REGS_DMG: BootRegs = BootRegs {
    a: 0x01,
    f: 0xB0,
    b: 0x00,
    c: 0x13,
    d: 0x00,
    e: 0xD8,
    h: 0x01,
    l: 0x4D,
    sp: 0xFFFE,
};

/// The CGB hand-off register values.
pub const BOOT_REGS_CGB: BootRegs = BootRegs {
    a: 0x11,
    f: 0xB0,
    b: 0x00,
    c: 0x00,
    d: 0x00,
    e: 0x08,
    h: 0x00,
    l: 0x00,
    sp: 0xFFFE,
};

/// The SM83 core.
pub struct Sm83 {
    name: String,
    /// The name of the bus this CPU accesses.
    pub bus_name: String,
    /// The register file.
    pub regs: Regs,
    /// The interrupt master enable.
    pub ime: bool,
    /// The delayed EI: set by `ei`, applied before the next instruction.
    pub(crate) ime_pending: bool,
    /// The instruction right after `EI` runs before any service.
    pub(crate) suppress_dispatch: bool,
    /// The interrupt flags, register `IF` at `0xFF0F`.
    pub iflags: u8,
    /// The interrupt enables, register `IE` at `0xFFFF`.
    pub ienable: u8,
    /// Halt state: true while the CPU waits for an interrupt.
    pub halted: bool,
    /// Stop state: true while the CPU waits for a joypad press (DMG).
    pub stopped: bool,
    /// Domain ticks left in the speed-switch pause (CGB).
    pub pause_dots: u64,
    /// Domain dots per T-cycle: 1 at normal speed, 2 at double speed.
    pub ticks_per_t: u32,
    /// Quirk: the DMG OAM corruption bug.
    pub oam_bug: bool,
    /// Quirk: the halt bug of a halt with a pending interrupt and IME 0.
    pub(crate) halt_bug: bool,
    /// The divider chain unfreezes on the core's next visit.
    pub(crate) resume_notify_stop: bool,
    /// The tick of the next visit, maintained by the core.
    next: Option<Tick>,
    /// The last processed tick; stale visits are skipped.
    last_tick: Option<Tick>,
    /// The power-on register set for a bootless boot.
    boot_regs: BootRegs,
}

impl std::fmt::Debug for Sm83 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sm83")
            .field("name", &self.name)
            .field("regs", &self.regs)
            .field("ime", &self.ime)
            .field("iflags", &self.iflags)
            .field("ienable", &self.ienable)
            .field("halted", &self.halted)
            .field("stopped", &self.stopped)
            .field("ticks_per_t", &self.ticks_per_t)
            .finish()
    }
}

impl Sm83 {
    /// A core from its configuration entry.
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
                    .unwrap_or_else(|| "main".to_string())
            });
        let triple = config.triple.clone();
        let mut cpu = Self {
            name: device.name.to_string(),
            bus_name,
            regs: Regs::default(),
            ime: false,
            ime_pending: false,
            suppress_dispatch: false,
            iflags: 0xE1,
            ienable: 0x00,
            halted: false,
            stopped: false,
            pause_dots: 0,
            ticks_per_t: 1,
            oam_bug: true,
            halt_bug: true,
            next: None,
            last_tick: None,
            resume_notify_stop: false,
            boot_regs: BOOT_REGS_DMG,
        };
        cpu.apply_quirks(config);
        cpu.power_on_regs(&triple);
        cpu
    }

    /// Apply this machine's quirk flags by name.
    fn apply_quirks(&mut self, config: &demu_config::model::SystemConfig) {
        for (name, value) in &config.quirks {
            match name.as_str() {
                "sm83.oam_bug" => self.oam_bug = value.as_flag(),
                "sm83.halt_undoc" => self.halt_bug = value.as_flag(),
                _ => {}
            }
        }
    }

    /// The power-on register state, by machine family.
    fn power_on_regs(&mut self, triple: &str) {
        self.boot_regs = if triple.contains("gameboy-color") {
            BOOT_REGS_CGB
        } else {
            BOOT_REGS_DMG
        };
        // `IF` holds the four hardware request bits after reset and `IE`
        // is clear; the register file itself starts empty and the boot
        // ROM programs it.
        self.regs = Regs::default();
        self.ime = false;
        self.ime_pending = false;
        self.halted = false;
        self.stopped = false;
        self.pause_dots = 0;
        self.iflags = 0xE1;
        self.ienable = 0x00;
        self.ticks_per_t = 1;
    }

    /// The joined 16-bit value of two register bytes.
    pub fn pair(hi: u8, lo: u8) -> u16 {
        (u16::from(hi) << 8) | u16::from(lo)
    }

    /// Set `next` after an instruction at `tick` that took `m` M-cycles.
    pub fn schedule_after(&mut self, tick: Tick, m: u32) {
        let dots_per_m = 4 * u64::from(self.ticks_per_t);
        self.next = Some(Tick(tick.0 + dots_per_m * u64::from(m)));
    }

    /// Enter halt mode. The CPU wakes when an interrupt becomes pending.
    pub fn enter_halt(&mut self) {
        self.halted = true;
        self.next = None;
    }

    /// Enter stop mode (DMG) or the speed-switch pause (CGB).
    ///
    /// A pause lasts `pause_dots` dots from the STOP instruction end;
    /// a bare stop idles until an interrupt arrives.
    pub(crate) fn enter_stop(&mut self, pause_dots: u64, at: Tick) {
        if pause_dots > 0 {
            self.pause_dots = pause_dots;
            self.next = Some(Tick(at.0 + pause_dots + 8));
        } else {
            self.stopped = true;
            self.next = None;
        }
        self.last_tick = None;
    }

    /// The divider chain unfreezes on the core's next visit.
    pub(crate) fn arm_resume_notify(&mut self) {
        self.resume_notify_stop = true;
    }

    /// Leave stop or the pause.
    pub fn exit_stop(&mut self, tick: Tick) {
        self.stopped = false;
        self.pause_dots = 0;
        self.last_tick = Some(tick);
    }

    /// A CPU-side read: the internal `IF` and `IE` registers answer
    /// directly, everything else goes on the bus.
    pub(crate) fn read8(&self, addr: u16, bus: &mut Bus) -> u8 {
        match addr {
            0xFF0F => self.iflags,
            0xFFFF => self.ienable,
            _ => bus.rd(addr),
        }
    }

    /// A CPU-side write: the internal `IF` and `IE` registers are local.
    pub(crate) fn write8(&mut self, addr: u16, value: u8, bus: &mut Bus) {
        match addr {
            0xFF0F => self.iflags = value & 0x1F,
            0xFFFF => self.ienable = value,
            _ => bus.wr(addr, value),
        }
    }

    /// One 16-bit increment or decrement drives the address bus with the
    /// register value. An access into OAM during OAM scan triggers the
    /// DMG OAM corruption bug.
    pub(crate) fn idu_op(&mut self, addr: u16, bus: &mut Bus) {
        if self.oam_bug && (0xFE00..=0xFEFF).contains(&addr) {
            bus.notify("oam_bug", Level::High, bus.cycles());
        }
    }
}

/// A bus access helper bound to one instruction's M-cycle counter.
///
/// Every access stamps its tick as the start tick plus four dots per
/// M-cycle. The helper also forwards notifications and event reports,
/// because the machine context is exclusively borrowed here.
pub struct Bus<'a, 'c> {
    ctx: &'c mut MachineCtx<'a>,
    tick: Tick,
    /// The M-cycle index of the next access.
    k: u32,
    bus_name: String,
    dots_per_m: u64,
}

impl<'a, 'c> Bus<'a, 'c> {
    /// A bus helper for one instruction at its start tick.
    pub fn new(
        ctx: &'c mut MachineCtx<'a>,
        tick: Tick,
        bus_name: String,
        ticks_per_t: u32,
    ) -> Self {
        Self {
            ctx,
            tick,
            k: 0,
            bus_name,
            dots_per_m: 4 * u64::from(ticks_per_t),
        }
    }

    /// The stamp of the access at the current M-cycle index.
    #[inline]
    fn stamp(&self) -> Tick {
        Tick(self.tick.0 + self.dots_per_m * u64::from(self.k))
    }

    /// Read one byte at M-cycle `k`, then advance the index.
    #[inline]
    pub fn rd(&mut self, addr: u16) -> u8 {
        let at = self.stamp();
        self.k += 1;
        self.ctx.read(
            &self.bus_name,
            demu_core::device::AccessWidth::Byte,
            demu_core::Address(u32::from(addr)),
            at,
        ) as u8
    }

    /// Write one byte at M-cycle `k`, then advance the index.
    #[inline]
    pub fn wr(&mut self, addr: u16, value: u8) {
        let at = self.stamp();
        self.k += 1;
        self.ctx.write(
            &self.bus_name,
            demu_core::device::AccessWidth::Byte,
            demu_core::Address(u32::from(addr)),
            u32::from(value),
            at,
        );
    }

    /// Skip one internal M-cycle.
    #[inline]
    pub fn wait(&mut self) {
        self.k += 1;
    }

    /// The start tick of the instruction this helper serves.
    pub fn tick(&self) -> Tick {
        self.tick
    }

    /// The M-cycle count consumed so far.
    pub fn cycles(&self) -> u32 {
        self.k
    }

    /// Deliver a signal-line edge from the CPU at M-cycle `k`.
    pub fn notify(&mut self, line: &str, level: Level, at_cycle: u32) {
        let at = Tick(self.tick.0 + self.dots_per_m * u64::from(at_cycle));
        self.ctx.notify(line, level, at);
    }

    /// Report a runtime event from the CPU.
    pub fn emit(&mut self, event: demu_core::triggers::TriggerEvent) {
        self.ctx.emit(event);
    }
}

impl Device for Sm83 {
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
        self.ime = false;
        self.ime_pending = false;
        self.suppress_dispatch = false;
        self.iflags = 0xE1;
        self.ienable = 0;
        self.halted = false;
        self.stopped = false;
        self.pause_dots = 0;
        self.ticks_per_t = 1;
        self.next = None;
        self.last_tick = None;
        self.resume_notify_stop = false;
        self.suppress_dispatch = false;
    }

    fn boot(&mut self, state: Option<&BootState>) {
        let presets = match state {
            Some(BootState::Cpu { registers }) => Some(registers.clone()),
            _ => None,
        };
        self.reset();
        // Without presets the core starts the boot ROM at zero.
        self.regs = Regs::default();
        self.regs.sp = self.boot_regs.sp;
        if let Some(presets) = presets {
            let debug: &mut dyn demu_core::CpuDebug = self;
            for (name, value) in presets {
                debug.set_register(&name, value);
            }
        }
        self.next = Some(Tick(0));
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(48);
        out.extend_from_slice(b"SM83");
        out.push(1);
        out.extend_from_slice(&[
            self.regs.a,
            self.regs.f,
            self.regs.b,
            self.regs.c,
            self.regs.d,
            self.regs.e,
            self.regs.h,
            self.regs.l,
        ]);
        out.extend_from_slice(&self.regs.sp.to_le_bytes());
        out.extend_from_slice(&self.regs.pc.to_le_bytes());
        out.push(u8::from(self.ime));
        out.push(u8::from(self.ime_pending));
        out.push(self.iflags);
        out.push(self.ienable);
        out.push(u8::from(self.halted));
        out.push(u8::from(self.stopped));
        out.extend_from_slice(&self.pause_dots.to_le_bytes());
        out.extend_from_slice(&self.ticks_per_t.to_le_bytes());
        out.push(u8::from(self.resume_notify_stop));
        out.push(u8::from(self.suppress_dispatch));
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        let expected = 4 + 1 + 8 + 2 + 2 + 6 + 8 + 4 + 2;
        if data.len() != expected {
            return Err(SnapshotError::Mismatch(format!(
                "sm83 chunk must be {expected} bytes, got {}",
                data.len()
            )));
        }
        if &data[..4] != b"SM83" {
            return Err(SnapshotError::Mismatch("bad sm83 tag".into()));
        }
        if data[4] != 1 {
            return Err(SnapshotError::Mismatch(
                "unsupported sm83 chunk version".into(),
            ));
        }
        let bytes: [u8; 8] = data[5..13].try_into().expect("8 bytes");
        self.regs.a = bytes[0];
        self.regs.f = bytes[1] & 0xF0;
        self.regs.b = bytes[2];
        self.regs.c = bytes[3];
        self.regs.d = bytes[4];
        self.regs.e = bytes[5];
        self.regs.h = bytes[6];
        self.regs.l = bytes[7];
        self.regs.sp = u16::from_le_bytes(data[13..15].try_into().expect("2 bytes"));
        self.regs.pc = u16::from_le_bytes(data[15..17].try_into().expect("2 bytes"));
        self.ime = data[17] != 0;
        self.ime_pending = data[18] != 0;
        // The full byte round-trips: the upper bits read as one on the
        // real hardware and the boot value carries them.
        self.iflags = data[19];
        self.ienable = data[20];
        self.halted = data[21] != 0;
        self.stopped = data[22] != 0;
        self.pause_dots = u64::from_le_bytes(data[23..31].try_into().expect("8 bytes"));
        self.ticks_per_t =
            u32::from_le_bytes(data[31..35].try_into().expect("4 bytes")).clamp(1, 2);
        self.resume_notify_stop = data[35] != 0;
        self.suppress_dispatch = data[36] != 0;
        self.last_tick = None;
        Ok(())
    }

    fn notify_line(&mut self, line: &str, level: demu_core::Level, tick: Tick) {
        match line {
            // The core performs its own speed switch on STOP; a notified
            // switch would double-apply.
            "speed" | "stop" | "oam_bug" => {}
            other => {
                if level == demu_core::Level::High {
                    raise_irq(self, other, tick);
                }
            }
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
        if self.resume_notify_stop {
            ctx.notify("stop", demu_core::Level::Low, tick);
            self.resume_notify_stop = false;
        }
        if self.ime_pending {
            self.ime = true;
            self.ime_pending = false;
            // The instruction after EI runs; the service waits one more
            // boundary.
            self.suppress_dispatch = true;
        }
        if self.halted || self.stopped {
            // No wake happened; the core idles from here.
            self.next = None;
            return;
        }
        crate::exec::step(self, tick, ctx);
    }
}

/// Raise one interrupt request line from a connected device.
///
/// A halt or stop exits on the M-cycle after the request, and the
/// interrupt service runs there too when IME allows it.
pub fn raise_irq(cpu: &mut Sm83, line: &str, tick: Tick) {
    let bit = crate::IRQ_LINES
        .iter()
        .find(|(l, _)| *l == line)
        .map(|(_, b)| *b)
        .unwrap_or(0);
    if bit == 0 {
        return;
    }
    cpu.iflags |= bit;
    if cpu.iflags & cpu.ienable != 0 && (cpu.halted || cpu.stopped) {
        cpu.halted = false;
        cpu.stopped = false;
        cpu.pause_dots = 0;
        cpu.next = Some(Tick(tick.0 + 4));
    }
}

impl Sm83 {
    /// The highest-priority pending interrupt bits, or `None`.
    pub fn pending_interrupt(&self) -> Option<u8> {
        let bits = self.iflags & self.ienable;
        if bits == 0 { None } else { Some(bits) }
    }
}

impl demu_core::CpuDebug for Sm83 {
    fn registers(&self) -> std::collections::BTreeMap<String, u64> {
        let mut map = std::collections::BTreeMap::new();
        map.insert("A".to_string(), u64::from(self.regs.a));
        map.insert("F".to_string(), u64::from(self.regs.f));
        map.insert("B".to_string(), u64::from(self.regs.b));
        map.insert("C".to_string(), u64::from(self.regs.c));
        map.insert("D".to_string(), u64::from(self.regs.d));
        map.insert("E".to_string(), u64::from(self.regs.e));
        map.insert("H".to_string(), u64::from(self.regs.h));
        map.insert("L".to_string(), u64::from(self.regs.l));
        map.insert(
            "AF".to_string(),
            u64::from(Self::pair(self.regs.a, self.regs.f)),
        );
        map.insert(
            "BC".to_string(),
            u64::from(Self::pair(self.regs.b, self.regs.c)),
        );
        map.insert(
            "DE".to_string(),
            u64::from(Self::pair(self.regs.d, self.regs.e)),
        );
        map.insert(
            "HL".to_string(),
            u64::from(Self::pair(self.regs.h, self.regs.l)),
        );
        map.insert("SP".to_string(), u64::from(self.regs.sp));
        map.insert("PC".to_string(), u64::from(self.regs.pc));
        map.insert("IME".to_string(), u64::from(self.ime));
        map.insert("IF".to_string(), u64::from(self.iflags));
        map.insert("IE".to_string(), u64::from(self.ienable));
        map
    }

    fn set_register(&mut self, name: &str, value: u64) -> bool {
        let lo = value as u8;
        match name.to_ascii_uppercase().as_str() {
            "A" => self.regs.a = lo,
            "F" => self.regs.f = lo & 0xF0,
            "B" => self.regs.b = lo,
            "C" => self.regs.c = lo,
            "D" => self.regs.d = lo,
            "E" => self.regs.e = lo,
            "H" => self.regs.h = lo,
            "L" => self.regs.l = lo,
            "AF" => {
                self.regs.a = (value >> 8) as u8;
                self.regs.f = lo & 0xF0;
            }
            "BC" => {
                self.regs.b = (value >> 8) as u8;
                self.regs.c = lo;
            }
            "DE" => {
                self.regs.d = (value >> 8) as u8;
                self.regs.e = lo;
            }
            "HL" => {
                self.regs.h = (value >> 8) as u8;
                self.regs.l = lo;
            }
            "SP" => self.regs.sp = value as u16,
            "PC" => self.regs.pc = value as u16,
            "IME" => self.ime = value != 0,
            "IF" => self.iflags = lo & 0x1F,
            "IE" => self.ienable = lo,
            _ => return false,
        }
        true
    }
}
