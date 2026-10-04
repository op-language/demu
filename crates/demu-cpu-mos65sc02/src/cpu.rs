//! The 65SC02 core: registers, timing, and the interrupt machinery.
//!
//! One domain tick is one 16 MHz master-crystal period. One sequential
//! CPU cycle is 4 domain ticks; the sequential-disable edge that the
//! Mikey die raises from the memory-map control makes every cycle cost
//! one tick more. Every bus access of an instruction lands on a cycle
//! boundary and carries its exact tick, so watchpoints and DMA see the
//! true access time. The core reschedules itself after every
//! instruction and reads the reset vector from the bus at boot, so the
//! boot ROM decides the entry point.
//!
//! The core is the CMOS 65C02 of the Atari Lynx: the decimal flag of
//! ADC and SBC works in the CMOS form, branches pay one extra cycle
//! when taken and none on a page cross, and the WAI and STP states
//! park the core until the interrupt edge or the reset line.

use demu_core::device::MachineCtx;
use demu_core::frequency::Tick;
use demu_core::signal::Level;
use demu_core::{Device, Snapshot, SnapshotError};

use demu_config::model::BootState;

/// Flag bits of the status register.
pub const FLAG_C: u8 = 0x01;
pub const FLAG_Z: u8 = 0x02;
pub const FLAG_I: u8 = 0x04;
pub const FLAG_D: u8 = 0x08;
/// The break flag; set on a BRK stack push, clear on a hardware push.
pub const FLAG_B: u8 = 0x10;
/// The unused bit always reads as one.
pub const FLAG_UNUSED: u8 = 0x20;
pub const FLAG_V: u8 = 0x40;
pub const FLAG_N: u8 = 0x80;

/// The register file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Regs {
    /// The accumulator.
    pub a: u8,
    /// The X and Y index registers.
    pub x: u8,
    pub y: u8,
    /// The status flags (B and the unused bit carry the push form).
    pub p: u8,
    /// The stack pointer.
    pub s: u8,
    /// The program counter.
    pub pc: u16,
}

impl Default for Regs {
    fn default() -> Self {
        Self {
            a: 0,
            x: 0,
            y: 0,
            // After power-up the interrupt disable is set and the
            // unused bit reads as one.
            p: FLAG_I | FLAG_UNUSED,
            // The stack pointer starts at zero; the reset sequence's
            // three dummy pushes land the byte at 0xFD.
            s: 0,
            pc: 0,
        }
    }
}

/// The 65SC02 core.
pub struct Mos65Sc02 {
    name: String,
    /// The name of the bus this CPU accesses.
    pub bus_name: String,
    /// Domain ticks per CPU cycle: 4 sequential, 5 with the disable.
    pub cpu_div: u64,
    /// The configured cycle divider, restored by `reset`.
    pub(crate) base_div: u64,
    /// The register file.
    pub regs: Regs,
    /// The current level of the IRQ line.
    pub irq_line: bool,
    /// The current level of the NMI line, for edge detection.
    pub(crate) nmi_line: bool,
    /// A latched NMI edge waiting for the next instruction boundary.
    pub(crate) nmi_edge: bool,
    /// The sleep line holds the core while a device owns the bus.
    pub(crate) bus_hold: bool,
    /// The core parked itself with WAI and waits for the IRQ edge.
    pub(crate) waiting: bool,
    /// The core parked itself with STP and leaves only by a reset.
    pub(crate) stopped: bool,
    /// The power-on reset sequence waits on the next visit.
    pub reset_pending: bool,
    /// The flavor gate: the page-bit instructions SMB, RMB, BBS, and
    /// BBS run in the plain `mos65sc02.cpu` kind. The Lynx core of the
    /// VTI die omits them.
    pub bbr_smb: bool,
    /// The memory-map unit: the `vl65nc02.cpu` kind reads the map
    /// control of the Mikey chip back at every instruction and drives
    /// the RAM-overlay lines when the byte changes.
    pub(crate) map_unit: bool,
    /// The map-control byte the unit applied last.
    pub(crate) map_seen: u8,
    pub(crate) next: Option<Tick>,
    last_tick: Option<Tick>,
}

impl std::fmt::Debug for Mos65Sc02 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mos65Sc02")
            .field("name", &self.name)
            .field("regs", &self.regs)
            .field("cpu_div", &self.cpu_div)
            .field("bus_hold", &self.bus_hold)
            .field("waiting", &self.waiting)
            .field("stopped", &self.stopped)
            .finish()
    }
}

impl Mos65Sc02 {
    /// A core from its configuration entry.
    ///
    /// The kind sets the flavor: `vl65nc02.cpu` drops the page-bit
    /// instructions of the VTI die, `mos65sc02.cpu` keeps them. The
    /// `cpu` divider entry, when present, overrides the default of 4
    /// domain ticks per cycle.
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
        let vl = device.kind.as_str().starts_with("vl65nc02");
        let base_div = device
            .dividers
            .get("cpu")
            .copied()
            .map(|d| u64::from(if d == 0 { 1 } else { d }))
            .unwrap_or(4);
        let mut cpu = Self {
            name: device.name.to_string(),
            bus_name,
            cpu_div: base_div,
            base_div,
            regs: Regs::default(),
            irq_line: false,
            nmi_line: false,
            nmi_edge: false,
            bus_hold: false,
            waiting: false,
            stopped: false,
            reset_pending: false,
            bbr_smb: !vl,
            map_unit: vl,
            map_seen: 0xFF,
            next: None,
            last_tick: None,
        };
        // The `bbr_smb` quirk forces the page-bit instructions when the
        // kind's default flavor lacks them.
        for (name, value) in &config.quirks {
            if name.as_str().ends_with(".bbr_smb")
                && name.rsplit_once('.').map(|(f, _)| f) == Some(device.kind.family())
            {
                cpu.bbr_smb = value.as_flag();
            }
        }
        cpu
    }

    /// The map-control register of the Mikey chip.
    pub const MAP_CTL: u16 = 0xFFF9;

    /// Read the map control back and apply a change: the four
    /// RAM-overlay lines re-drive, and the sequential-disable bit
    /// adjusts the cycle length. The read runs at the instruction
    /// boundary, so the register write takes effect from the next
    /// access onward, as the hardware memory-map unit does.
    pub(crate) fn sync_map(&mut self, tick: Tick, ctx: &mut MachineCtx) {
        let ctl = ctx.peek(&self.bus_name, demu_core::Address(u32::from(Self::MAP_CTL)));
        if ctl == self.map_seen {
            return;
        }
        self.map_seen = ctl;
        let level = |bit: u32| {
            if (u32::from(ctl) >> bit) & 1 == 1 {
                Level::High
            } else {
                Level::Low
            }
        };
        // The high-speed bit (bit 7, the sequential-disable form)
        // applies to the core itself.
        self.notify_line("seq_dis", level(7), tick);
        // The space bits: a set bit moves the space to the RAM
        // underneath. The lines ride the declared connections to the
        // devices they gate.
        ctx.notify("suzy_ram", level(0), tick);
        ctx.notify("mikey_ram", level(1), tick);
        ctx.notify("rom_ram", level(2), tick);
        ctx.notify("vec_ram", level(3), tick);
    }

    /// Schedule the next core visit `m` CPU cycles after `tick`.
    pub fn schedule_after(&mut self, tick: Tick, m: u32) {
        self.next = Some(Tick(tick.0 + self.cpu_div * u64::from(m)));
    }

    /// The first cycle boundary strictly after `tick`. The core is
    /// mid-instruction when a line wakes it, so a boundary at the
    /// current tick would re-schedule the visit the scheduler has
    /// running and would loop the queue.
    fn next_cycle_after(&self, tick: Tick) -> Option<Tick> {
        Some(Tick((tick.0 / self.cpu_div + 1) * self.cpu_div))
    }

    /// Wake a held or idle core at the next cycle boundary.
    fn wake(&mut self, tick: Tick) {
        if self.bus_hold {
            return;
        }
        if self.next.is_none() {
            self.next = self.next_cycle_after(tick);
            self.last_tick = None;
        }
    }
}

/// A bus access helper bound to one instruction's cycle counter.
///
/// Every access stamps its tick as the start tick plus one cycle per
/// preceding access. The helper also forwards notifications and event
/// reports, because the machine context is exclusively borrowed here.
pub struct Access<'a, 'c> {
    ctx: &'c mut MachineCtx<'a>,
    tick: Tick,
    k: u32,
    cpu_div: u64,
    bus_name: String,
}

impl<'a, 'c> Access<'a, 'c> {
    /// An access helper for one instruction at its start tick.
    pub fn new(ctx: &'c mut MachineCtx<'a>, tick: Tick, bus_name: String, cpu_div: u64) -> Self {
        Self {
            ctx,
            tick,
            k: 0,
            cpu_div,
            bus_name,
        }
    }

    /// The stamp of the access at the current cycle index.
    #[inline]
    pub fn stamp(&self) -> Tick {
        Tick(self.tick.0 + self.cpu_div * u64::from(self.k))
    }

    /// Read one byte at cycle `k`, then advance the index.
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

    /// Write one byte at cycle `k`, then advance the index.
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

    /// Skip one internal cycle.
    #[inline]
    pub fn wait(&mut self) {
        self.k += 1;
    }

    /// The number of cycles consumed so far.
    pub fn cycles(&self) -> u32 {
        self.k
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
    /// Read one byte; access stamping stays with the implementation.
    fn rd(&mut self, addr: u16) -> u8;
    /// Write one byte.
    fn wr(&mut self, addr: u16, value: u8);
    /// Skip one internal cycle.
    fn wait(&mut self);
    /// The number of cycles consumed so far.
    fn cycles(&self) -> u32;
    /// Report one runtime event from the core.
    fn emit(&mut self, event: demu_core::triggers::TriggerEvent);
}

impl Cycles for Access<'_, '_> {
    fn rd(&mut self, addr: u16) -> u8 {
        Access::rd(self, addr)
    }

    fn wr(&mut self, addr: u16, value: u8) {
        Access::wr(self, addr, value)
    }

    fn wait(&mut self) {
        Access::wait(self)
    }

    fn cycles(&self) -> u32 {
        Access::cycles(self)
    }

    fn emit(&mut self, event: demu_core::triggers::TriggerEvent) {
        Access::emit(self, event)
    }
}

impl Device for Mos65Sc02 {
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
        self.irq_line = false;
        self.nmi_line = false;
        self.nmi_edge = false;
        self.bus_hold = false;
        self.waiting = false;
        self.stopped = false;
        self.reset_pending = false;
        self.next = None;
        self.last_tick = None;
        // The map unit re-reads the control on the first visit.
        self.map_seen = 0xFF;
    }

    fn boot(&mut self, state: Option<&BootState>) {
        self.reset();
        if let Some(BootState::Cpu { registers }) = state {
            let debug: &mut dyn demu_core::CpuDebug = self;
            for (name, value) in registers {
                debug.set_register(name, *value);
            }
        }
        // The power-on reset sequence runs on the first visit and reads
        // the reset vector through the bus.
        self.reset_pending = true;
        self.next = Some(Tick(0));
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(31);
        out.extend_from_slice(b"SC02");
        out.push(1);
        out.extend_from_slice(&[
            self.regs.a,
            self.regs.x,
            self.regs.y,
            self.regs.s,
            self.regs.p,
        ]);
        out.extend_from_slice(&self.regs.pc.to_le_bytes());
        out.push(u8::from(self.irq_line));
        out.push(u8::from(self.nmi_line));
        out.push(u8::from(self.nmi_edge));
        out.push(u8::from(self.bus_hold));
        out.push(u8::from(self.waiting));
        out.push(u8::from(self.stopped));
        out.push(u8::from(self.reset_pending));
        out.push(u8::from(self.bbr_smb));
        out.push(self.map_seen);
        out.extend_from_slice(&self.cpu_div.to_le_bytes());
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        let expected = 4 + 1 + 5 + 2 + 9 + 8;
        if data.len() != expected {
            return Err(SnapshotError::Mismatch(format!(
                "mos65sc02 chunk must be {expected} bytes, got {}",
                data.len()
            )));
        }
        if &data[..4] != b"SC02" {
            return Err(SnapshotError::Mismatch("bad mos65sc02 tag".into()));
        }
        if data[4] != 1 {
            return Err(SnapshotError::Mismatch(
                "unsupported mos65sc02 chunk version".into(),
            ));
        }
        self.regs.a = data[5];
        self.regs.x = data[6];
        self.regs.y = data[7];
        self.regs.s = data[8];
        self.regs.p = data[9] | FLAG_UNUSED;
        self.regs.pc = u16::from_le_bytes(data[10..12].try_into().expect("2 bytes"));
        self.irq_line = data[12] != 0;
        self.nmi_line = data[13] != 0;
        self.nmi_edge = data[14] != 0;
        self.bus_hold = data[15] != 0;
        self.waiting = data[16] != 0;
        self.stopped = data[17] != 0;
        self.reset_pending = data[18] != 0;
        self.bbr_smb = data[19] != 0;
        self.map_seen = data[20];
        self.cpu_div = u64::from_le_bytes(data[21..29].try_into().expect("8 bytes")).clamp(1, 64);
        self.next = None;
        self.last_tick = None;
        Ok(())
    }

    fn notify_line(&mut self, line: &str, level: Level, tick: Tick) {
        match line {
            "nmi" => {
                let was = self.nmi_line;
                self.nmi_line = level == Level::High;
                // The CMOS NMI input is edge sensitive: a second pulse
                // while the line stays asserted does not re-latch.
                if self.nmi_line && !was {
                    self.nmi_edge = true;
                    self.waiting = false;
                    self.wake(tick);
                }
            }
            "irq" => {
                self.irq_line = level == Level::High;
                if self.irq_line {
                    self.waiting = false;
                    self.wake(tick);
                }
            }
            "sleep" => {
                // The Mikey or Suzy die took the bus for sprite work.
                // High holds the core; the release wakes it at the next
                // cycle boundary.
                if level == Level::High {
                    self.bus_hold = true;
                    self.next = None;
                } else {
                    self.bus_hold = false;
                    self.last_tick = None;
                    self.next = self.next_cycle_after(tick);
                }
            }
            "seq_dis" => {
                // The sequential disable of the memory-map control
                // makes every cycle cost one tick more.
                if level == Level::High {
                    self.cpu_div = self.base_div + 1;
                } else {
                    self.cpu_div = self.base_div;
                }
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
        if self.bus_hold {
            self.next = None;
            return;
        }
        crate::exec::step(self, tick, ctx);
    }
}

impl Mos65Sc02 {
    /// True when an interrupt can run at this instruction boundary:
    /// a latched NMI edge, or an asserted IRQ line with the interrupt
    /// disable clear.
    pub fn interrupt_pending(&self) -> bool {
        self.nmi_edge || (self.irq_line && self.regs.p & FLAG_I == 0)
    }

    /// Consume one latched NMI edge.
    pub fn take_nmi(&mut self) -> bool {
        std::mem::take(&mut self.nmi_edge)
    }

    /// The pushed form of the status register: the unused bit reads as
    /// one, and the break flag states the software-interrupt form.
    pub fn push_p(&self, software: bool) -> u8 {
        let mut p = self.regs.p | FLAG_UNUSED;
        if software {
            p |= FLAG_B;
        }
        p
    }
}

impl demu_core::CpuDebug for Mos65Sc02 {
    fn registers(&self) -> std::collections::BTreeMap<String, u64> {
        let mut map = std::collections::BTreeMap::new();
        map.insert("A".to_string(), u64::from(self.regs.a));
        map.insert("X".to_string(), u64::from(self.regs.x));
        map.insert("Y".to_string(), u64::from(self.regs.y));
        map.insert("S".to_string(), u64::from(self.regs.s));
        map.insert("SP".to_string(), u64::from(u16::from(self.regs.s) | 0x100));
        map.insert("P".to_string(), u64::from(self.regs.p));
        map.insert("PC".to_string(), u64::from(self.regs.pc));
        map
    }

    fn set_register(&mut self, name: &str, value: u64) -> bool {
        let lo = value as u8;
        match name.to_ascii_uppercase().as_str() {
            "A" => self.regs.a = lo,
            "X" => self.regs.x = lo,
            "Y" => self.regs.y = lo,
            "S" | "SP" => self.regs.s = lo,
            "P" => self.regs.p = lo | FLAG_UNUSED,
            "PC" => self.regs.pc = value as u16,
            _ => return false,
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instance(kind: &str) -> demu_config::model::DeviceInstance {
        let family = kind.split_once('.').map(|(f, _)| f).unwrap_or(kind);
        let raw = format!(
            r#"
            [system]
            name = "test"
            triple = "test"
            [[bus]]
            name = "cpu"
            address_bits = 16
            owner = "cpu0"
            [[device]]
            name = "cpu0"
            kind = "{kind}"
            domain = "main"
            [clock.main]
            frequency = "16000000"
            [quirks]
            "{family}.bbr_smb" = true
            "#
        );
        let (config, _) = demu_config::validate(
            &demu_config::RawConfig::parse(&raw).expect("test config parses"),
        )
        .expect("test config validates");
        config.devices[0].clone()
    }

    fn config_of(kind: &str) -> demu_config::model::SystemConfig {
        let raw = format!(
            r#"
            [system]
            name = "test"
            triple = "test"
            [[bus]]
            name = "cpu"
            address_bits = 16
            owner = "cpu0"
            [clock.main]
            frequency = "16000000"
            [[device]]
            name = "cpu0"
            kind = "{kind}"
            domain = "main"
            "#
        );
        demu_config::validate(&demu_config::RawConfig::parse(&raw).expect("parse"))
            .expect("validate")
            .0
    }

    #[test]
    fn kind_sets_the_flavor() {
        let vl = Mos65Sc02::new(&instance("vl65nc02.cpu"), &config_of("vl65nc02.cpu"));
        assert!(!vl.bbr_smb);
        assert_eq!(vl.cpu_div, 4);
        let plain = Mos65Sc02::new(&instance("mos65sc02.cpu"), &config_of("mos65sc02.cpu"));
        assert!(plain.bbr_smb);
    }

    #[test]
    fn sequential_disable_bumps_the_cycle() {
        let mut vl = Mos65Sc02::new(&instance("vl65nc02.cpu"), &config_of("vl65nc02.cpu"));
        assert_eq!(vl.cpu_div, 4);
        vl.notify_line("seq_dis", Level::High, Tick(10));
        assert_eq!(vl.cpu_div, 5);
        vl.notify_line("seq_dis", Level::Low, Tick(20));
        assert_eq!(vl.cpu_div, 4);
        // Restoring keeps the applied form: the control rides the
        // snapshot so a twin continues identically.
        vl.notify_line("seq_dis", Level::High, Tick(30));
        let saved = vl.snapshot();
        let mut twin = Mos65Sc02::new(&instance("vl65nc02.cpu"), &config_of("vl65nc02.cpu"));
        twin.restore(&saved).expect("round trip");
        assert_eq!(twin.cpu_div, 5);
        assert_eq!(twin.regs.s, vl.regs.s);
    }

    #[test]
    fn power_on_state() {
        let mut cpu = Mos65Sc02::new(&instance("vl65nc02.cpu"), &config_of("vl65nc02.cpu"));
        cpu.reset();
        cpu.boot(None);
        assert_eq!(cpu.regs.p & FLAG_I, FLAG_I);
        assert_eq!(cpu.regs.p & FLAG_UNUSED, FLAG_UNUSED);
        assert_eq!(cpu.next, Some(Tick(0)));
        // The boot preset overrides the power-on form.
        let presets = demu_config::model::BootState::Cpu {
            registers: [("s".to_string(), 0x40)].into_iter().collect(),
        };
        cpu.boot(Some(&presets));
        assert_eq!(cpu.regs.s, 0x40);
    }

    #[test]
    fn nmi_latches_once_per_edge() {
        let mut cpu = Mos65Sc02::new(&instance("vl65nc02.cpu"), &config_of("vl65nc02.cpu"));
        cpu.boot(None);
        cpu.notify_line("nmi", Level::High, Tick(0));
        assert!(cpu.nmi_edge);
        cpu.notify_line("nmi", Level::High, Tick(16));
        assert!(cpu.take_nmi());
        assert!(
            !cpu.interrupt_pending(),
            "the edge consumed, a held high does not relatch"
        );
        cpu.notify_line("nmi", Level::Low, Tick(32));
        cpu.notify_line("nmi", Level::High, Tick(32));
        assert!(cpu.interrupt_pending(), "a new edge latches again");
    }
}
