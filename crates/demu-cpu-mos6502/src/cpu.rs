//! The 6502 core: registers, timing, and the interrupt machinery.
//!
//! One domain tick is one master-crystal period. One CPU cycle is the
//! configured divider of the domain (12 on the RP2A03, 16 on the RP2A07).
//! Every bus access of an instruction lands on a cycle boundary and
//! carries its exact tick, so watchpoints and DMA see the true access
//! time. The core reschedules itself after every instruction and reads
//! the reset vector from the bus at boot, so the cartridge decides the
//! entry point.
//!
//! The core is the NMOS 6502 of the RP2A03 and RP2A07: the decimal
//! flag of ADC and SBC works, and the undocumented opcodes follow the
//! real NMOS behavior when the `undocumented` quirk flag is on.

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
            // The reset sequence decrements the stack pointer three
            // times with dummy pushes, which lands at 0xFD.
            s: 0,
            pc: 0,
        }
    }
}

/// The 6502 core.
pub struct Mos6502 {
    name: String,
    /// The name of the bus this CPU accesses.
    pub bus_name: String,
    /// Domain ticks per CPU cycle: 12 on the RP2A03, 16 on the RP2A07.
    pub cpu_div: u64,
    /// The register file.
    pub regs: Regs,
    /// The current level of the IRQ line.
    pub irq_line: bool,
    /// The current level of the NMI line, for edge detection.
    pub(crate) nmi_line: bool,
    /// A latched NMI edge waiting for the next instruction boundary.
    pub(crate) nmi_edge: bool,
    /// A DMA holds the ready line; the core idles until its release.
    pub halted_dma: bool,
    /// The `undocumented` quirk flag: unofficial opcodes follow the
    /// NMOS behavior instead of the safe two-cycle NOP.
    pub undocumented: bool,
    /// The core hit a jam opcode and waits for a reset.
    pub jammed: bool,
    /// The power-on reset sequence waits on the next visit.
    pub reset_pending: bool,
    pub(crate) next: Option<Tick>,
    last_tick: Option<Tick>,
}

impl std::fmt::Debug for Mos6502 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mos6502")
            .field("name", &self.name)
            .field("regs", &self.regs)
            .field("pal", &(self.cpu_div == 16))
            .field("irq_line", &self.irq_line)
            .field("nmi_edge", &self.nmi_edge)
            .field("halted_dma", &self.halted_dma)
            .field("undocumented", &self.undocumented)
            .finish()
    }
}

impl Mos6502 {
    /// A core from its configuration entry.
    ///
    /// The kind decides the region: `rp2A07.cpu` and `mos6502.cpu`
    /// with a `pal` parameter select the PAL divider of 16. The `cpu`
    /// divider entry, when present, overrides the region default.
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
        let pal = device.kind.as_str().starts_with("rp2A07")
            || device
                .params
                .get("pal")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
        let mut cpu = Self {
            name: device.name.to_string(),
            bus_name,
            cpu_div: if pal { 16 } else { 12 },
            regs: Regs::default(),
            irq_line: false,
            nmi_line: false,
            nmi_edge: false,
            halted_dma: false,
            undocumented: false,
            jammed: false,
            reset_pending: false,
            next: None,
            last_tick: None,
        };
        if let Some(div) = device.dividers.get("cpu") {
            cpu.cpu_div = u64::from(*div);
        }
        for (name, value) in &config.quirks {
            if name.as_str().ends_with(".undocumented")
                && name.rsplit_once('.').map(|(f, _)| f) == Some(device.kind.family())
            {
                cpu.undocumented = value.as_flag();
            }
        }
        cpu
    }

    /// The status byte in push form: the unused bit reads as one and
    /// the break flag states the push source.
    pub fn push_p(&self, break_flag: bool) -> u8 {
        self.regs.p | FLAG_UNUSED | if break_flag { FLAG_B } else { 0 }
    }

    /// Schedule the next core visit `m` CPU cycles after `tick`.
    pub fn schedule_after(&mut self, tick: Tick, m: u32) {
        self.next = Some(Tick(tick.0 + self.cpu_div * u64::from(m)));
    }

    /// The first cycle boundary at or after `tick`.
    fn next_boundary(&self, tick: Tick) -> Option<Tick> {
        let boundary = tick.0.div_ceil(self.cpu_div) * self.cpu_div;
        Some(Tick(boundary))
    }

    /// Wake a halted or idle core at the next cycle boundary.
    fn wake(&mut self, tick: Tick) {
        if self.halted_dma {
            return;
        }
        if self.next.is_none() {
            self.next = self.next_boundary(tick);
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

impl Device for Mos6502 {
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
        self.irq_line = false;
        self.nmi_line = false;
        self.nmi_edge = false;
        self.halted_dma = false;
        self.jammed = false;
        self.reset_pending = false;
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
        // The power-on reset sequence runs on the first visit and reads
        // the reset vector through the bus.
        self.reset_pending = true;
        self.next = Some(Tick(0));
    }

    fn snapshot(&self) -> Snapshot {
        let mut out = Vec::with_capacity(26);
        out.extend_from_slice(b"6502");
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
        out.push(u8::from(self.halted_dma));
        out.push(u8::from(self.jammed));
        out.push(u8::from(self.reset_pending));
        out.extend_from_slice(&self.cpu_div.to_le_bytes());
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        let expected = 4 + 1 + 5 + 2 + 6 + 8;
        if data.len() != expected {
            return Err(SnapshotError::Mismatch(format!(
                "mos6502 chunk must be {expected} bytes, got {}",
                data.len()
            )));
        }
        if &data[..4] != b"6502" {
            return Err(SnapshotError::Mismatch("bad mos6502 tag".into()));
        }
        if data[4] != 1 {
            return Err(SnapshotError::Mismatch(
                "unsupported mos6502 chunk version".into(),
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
        self.halted_dma = data[15] != 0;
        self.jammed = data[16] != 0;
        self.reset_pending = data[17] != 0;
        self.cpu_div = u64::from_le_bytes(data[18..26].try_into().expect("8 bytes")).clamp(1, 64);
        self.next = None;
        self.last_tick = None;
        Ok(())
    }

    fn notify_line(&mut self, line: &str, level: Level, tick: Tick) {
        match line {
            "nmi" => {
                let was = self.nmi_line;
                self.nmi_line = level == Level::High;
                // The NMOS NMI input is edge sensitive: a second pulse
                // while the line stays asserted does not re-latch.
                if self.nmi_line && !was {
                    self.nmi_edge = true;
                    self.wake(tick);
                }
            }
            "irq" => {
                self.irq_line = level == Level::High;
                if self.irq_line {
                    self.wake(tick);
                }
            }
            "halt" => {
                if level == Level::High {
                    // The ready line drops: the core idles until the
                    // release wakes it.
                    self.halted_dma = true;
                    self.next = None;
                } else {
                    self.halted_dma = false;
                    self.last_tick = None;
                    self.next = self.next_boundary(tick);
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
        if self.halted_dma {
            self.next = None;
            return;
        }
        crate::exec::step(self, tick, ctx);
    }
}

impl Mos6502 {
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
}

impl demu_core::CpuDebug for Mos6502 {
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
            frequency = "236250000/11"
            "#
        );
        let (config, _) = demu_config::validate(
            &demu_config::RawConfig::parse(&raw).expect("test config parses"),
        )
        .expect("test config validates");
        config.devices[0].clone()
    }

    #[test]
    fn region_sets_the_cycle_divider() {
        let ntsc = Mos6502::new(&instance("rp2A03.cpu"), &config_of("rp2A03.cpu"));
        assert_eq!(ntsc.cpu_div, 12);
        let pal = Mos6502::new(&instance("rp2A07.cpu"), &config_of("rp2A07.cpu"));
        assert_eq!(pal.cpu_div, 16);
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
            frequency = "236250000/11"
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
    fn power_on_state() {
        let mut cpu = Mos6502::new(&instance("rp2A03.cpu"), &config_of("rp2A03.cpu"));
        cpu.reset();
        cpu.boot(None);
        assert_eq!(cpu.regs.p & FLAG_I, FLAG_I);
        assert_eq!(cpu.regs.p & FLAG_UNUSED, FLAG_UNUSED);
        assert_eq!(cpu.regs.s, 0x00);
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
        let mut cpu = Mos6502::new(&instance("rp2A03.cpu"), &config_of("rp2A03.cpu"));
        cpu.boot(None);
        cpu.notify_line("nmi", Level::High, Tick(0));
        assert!(cpu.nmi_edge);
        cpu.notify_line("nmi", Level::High, Tick(12));
        assert!(cpu.take_nmi());
        let pending = cpu.interrupt_pending();
        assert!(!pending, "the edge consumed, a held high does not relatch");
        cpu.notify_line("nmi", Level::Low, Tick(24));
        cpu.notify_line("nmi", Level::High, Tick(24));
        assert!(cpu.interrupt_pending(), "a new edge latches again");
    }
}
