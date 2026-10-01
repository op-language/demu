//! The 65C816 core: registers, timing, and the interrupt machinery.
//!
//! One domain tick is one master-crystal period. One CPU cycle is the
//! configured divider of the domain (6 on the SNES core, 10 on the
//! IIgs). Every bus access of an instruction lands on a cycle boundary
//! and carries its exact tick, so watchpoints and DMA see the true
//! access time. The core reschedules itself after every instruction
//! and reads the reset vector from the bus at boot, so the cartridge
//! decides the entry point.
//!
//! The core runs both operating modes of the WDC 65C816. Power-on
//! selects emulation mode (E = 1): 8-bit A, X, and Y, the stack on
//! page one, and the 6502 vector map. The `XCE` sequence switches to
//! native mode, which exposes the 16-bit C accumulator, the 16-bit
//! X/Y/S, the direct-page and bank registers, and the 65C02-style
//! pointer walks. The instruction set is complete: all 256 bytes run
//! as documented `65C816` opcodes in both modes.

use demu_core::device::MachineCtx;
use demu_core::frequency::Tick;
use demu_core::signal::Level;
use demu_core::{Device, Snapshot, SnapshotError};

use demu_config::model::BootState;

/// The status flag set, native-mode layout: `N V M X D I Z C`.
pub const FLAG_C: u8 = 0x01;
pub const FLAG_Z: u8 = 0x02;
pub const FLAG_I: u8 = 0x04;
pub const FLAG_D: u8 = 0x08;
/// Bit four of the native status stores the index-select flag X. In
/// emulation mode the pushed status carries the break bit in the same
/// position (the emulation-mode select bits read as set).
pub const FLAG_X: u8 = 0x10;
/// Bit five of the native status stores the accumulator select M. In
/// emulation mode the pushed status carries its unused-bit one in
/// this position.
pub const FLAG_M: u8 = 0x20;
pub const FLAG_V: u8 = 0x40;
pub const FLAG_N: u8 = 0x80;

/// The snapshot chunk tag.
const SNAP_TAG: &[u8; 4] = b"5816";
/// The snapshot chunk version.
const SNAP_VERSION: u8 = 1;
/// The snapshot chunk length in bytes: 4 tag, 1 version, 16
/// register bytes, 7 control bits, 8 divider.
const SNAP_LENGTH: usize = 4 + 1 + 16 + 7 + 8;

/// The register file.
///
/// The accumulator is the combined 16-bit `C`: `A` is the low byte and
/// `B` the high one. `X` and `Y` are 16-bit storages whose low byte is
/// the whole 8-bit register. `S` carries the page-one high byte in
/// emulation mode. `p` stores the flag set with `M` on bit five and
/// `X` on bit four; the push and pull forms in emulation mode reshape
/// those two bits per the chip's status layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Regs {
    /// The combined accumulator; `A` is the low byte.
    pub c: u16,
    /// The X index at its storage width.
    pub x: u16,
    /// The Y index at its storage width.
    pub y: u16,
    /// The status flags in the native layout.
    pub p: u8,
    /// The stack pointer.
    pub s: u16,
    /// The direct page register.
    pub d: u16,
    /// The data bank register.
    pub dbr: u8,
    /// The program bank register.
    pub pbr: u8,
    /// The program counter within the program bank.
    pub pc: u16,
    /// True in emulation mode (6502 style).
    pub e: bool,
}

impl Default for Regs {
    fn default() -> Self {
        Self {
            c: 0,
            x: 0,
            y: 0,
            // After power-up the interrupt disable is set and emulation
            // mode locks the M and X selects to eight-bit registers.
            p: FLAG_I | FLAG_M | FLAG_X,
            // The reset sequence decrements the low byte three times
            // with dummy pushes, which lands at 0xFD on page one.
            s: 0x0100,
            d: 0,
            dbr: 0,
            pbr: 0,
            pc: 0,
            e: true,
        }
    }
}

/// The 65C816 core.
pub struct Wdc65c816 {
    name: String,
    /// The name of the bus this CPU accesses.
    pub bus_name: String,
    /// Domain ticks per CPU cycle.
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
    /// The core sits in `STP` and waits for a reset.
    pub stopped: bool,
    /// The core sits in `WAI` and waits for an interrupt.
    pub waiting: bool,
    /// The power-on reset sequence waits on the next visit.
    pub reset_pending: bool,
    pub(crate) next: Option<Tick>,
    last_tick: Option<Tick>,
}

impl std::fmt::Debug for Wdc65c816 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Wdc65c816")
            .field("name", &self.name)
            .field("regs", &self.regs)
            .field("cpu_div", &self.cpu_div)
            .field("irq_line", &self.irq_line)
            .field("nmi_edge", &self.nmi_edge)
            .field("halted_dma", &self.halted_dma)
            .field("stopped", &self.stopped)
            .field("waiting", &self.waiting)
            .finish()
    }
}

impl Wdc65c816 {
    /// A core from its configuration entry.
    ///
    /// The default divider is 6 (the SNES core); a `cpu` divider entry
    /// overrides it. The bus parameter falls back to the first bus of
    /// the system and then to `cpu`.
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
        let mut cpu = Self {
            name: device.name.to_string(),
            bus_name,
            cpu_div: 6,
            regs: Regs::default(),
            irq_line: false,
            nmi_line: false,
            nmi_edge: false,
            halted_dma: false,
            stopped: false,
            waiting: false,
            reset_pending: false,
            next: None,
            last_tick: None,
        };
        if let Some(div) = device.dividers.get("cpu") {
            cpu.cpu_div = u64::from(*div);
        }
        cpu
    }

    /// The visible accumulator width: 16 bits only in native mode
    /// with the M flag clear.
    pub fn m_wide(&self) -> bool {
        !self.regs.e && self.regs.p & FLAG_M == 0
    }

    /// The visible index width: 16 bits only in native mode with the
    /// X flag clear.
    pub fn x_wide(&self) -> bool {
        !self.regs.e && self.regs.p & FLAG_X == 0
    }

    /// The data mask of the current accumulator or memory width.
    pub fn m_mask(&self) -> u16 {
        if self.m_wide() { 0xFFFF } else { 0x00FF }
    }

    /// The data mask of the current index width.
    pub fn x_mask(&self) -> u16 {
        if self.x_wide() { 0xFFFF } else { 0x00FF }
    }

    /// The status byte in push form. The break flag lives only in the
    /// pushed copy: bit four carries it, and bit five (the select) is
    /// forced in emulation mode. A hardware IRQ or NMI pushes the
    /// break bit clear; BRK, COP, and PHP push it set in both modes.
    /// Native mode keeps the stored carry/decimal and index-form bits;
    /// emulation forces the index select to one.
    pub fn push_p(&self, break_flag: bool) -> u8 {
        let base = self.regs.p & !FLAG_X;
        let break_bit = if break_flag { FLAG_X } else { 0 };
        if self.regs.e {
            base | FLAG_M | break_bit
        } else {
            base | break_bit
        }
    }

    /// Store a status byte pulled from the stack. Emulation mode
    /// forces the hidden select bits back to one, and a set index
    /// select then truncates the index registers.
    pub fn pull_p(&mut self, value: u8) {
        self.regs.p = if self.regs.e {
            value | FLAG_M | FLAG_X
        } else {
            value
        };
        if self.regs.p & FLAG_X != 0 {
            self.regs.x &= 0x00FF;
            self.regs.y &= 0x00FF;
        }
    }

    /// A register write lands inside the native emulation-mode
    /// invariants: the stack points at page one and the relocatable
    /// registers keep their forced forms.
    pub fn set_stack(&mut self, stack: u16) {
        self.regs.s = if self.regs.e {
            0x0100 | (stack & 0x00FF)
        } else {
            stack
        };
    }

    /// Schedule the next core visit `m` CPU cycles after `tick`.
    pub fn schedule_after(&mut self, tick: Tick, m: u32) {
        self.next = Some(Tick(tick.0 + self.cpu_div * u64::from(m)));
    }

    /// The first cycle boundary at or after `tick`.
    fn next_boundary(&self, tick: Tick) -> Option<Tick> {
        Some(Tick(tick.0.div_ceil(self.cpu_div) * self.cpu_div))
    }

    /// Wake the core at the next cycle boundary.
    ///
    /// A stopped core stays down. A `WAI` core resumes; the served
    /// interrupt then runs its full sequence, per the data sheet's
    /// "additional cycles" note for the restart.
    fn wake(&mut self, tick: Tick) {
        if self.halted_dma || self.stopped {
            return;
        }
        self.waiting = false;
        if self.next.is_none() {
            self.next = self.next_boundary(tick);
            self.last_tick = None;
        }
    }

    /// The full reset path a hardware reset line takes: the register
    /// power-on state and a pending reset sequence that reads the
    /// vector on the next visit.
    fn reset_now(&mut self, tick: Tick) {
        let div = self.cpu_div;
        let name = self.name.clone();
        let bus_name = self.bus_name.clone();
        let last_tick = self.last_tick;
        *self = Self {
            name,
            bus_name,
            cpu_div: div,
            regs: Regs::default(),
            irq_line: false,
            nmi_line: false,
            nmi_edge: false,
            halted_dma: false,
            stopped: false,
            waiting: false,
            reset_pending: true,
            next: self.next_boundary(tick),
            last_tick,
        };
    }
}

/// The bus-access shape of one instruction's cycle counter.
///
/// The production [`Access`] stamps every access one domain divider
/// apart; the tests drive the same instruction code over a plain
/// memory map. Every method advances the cycle counter once, so the
/// instruction cycle totals stay exact in both back ends.
pub trait Cycles {
    /// Read one byte at the current cycle.
    fn rd(&mut self, addr: u32) -> u8;
    /// Write one byte at the current cycle.
    fn wr(&mut self, addr: u32, value: u8);
    /// Skip one internal cycle.
    fn wait(&mut self);
    /// The number of cycles consumed so far.
    fn cycles(&self) -> u32;
    /// Report a runtime event from the core.
    fn emit(&mut self, _event: demu_core::triggers::TriggerEvent) {}
}

/// A bus access helper bound to one instruction's cycle counter.
///
/// Every access stamps its tick as the start tick plus one cycle per
/// preceding access. The helper also forwards event reports, because
/// the machine context is exclusively borrowed here.
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
}

impl Cycles for Access<'_, '_> {
    /// Read one byte at cycle `k`, then advance the index.
    #[inline]
    fn rd(&mut self, addr: u32) -> u8 {
        let at = self.stamp();
        self.k += 1;
        self.ctx.read(
            &self.bus_name,
            demu_core::device::AccessWidth::Byte,
            demu_core::Address(addr),
            at,
        ) as u8
    }

    /// Write one byte at cycle `k`, then advance the index.
    #[inline]
    fn wr(&mut self, addr: u32, value: u8) {
        let at = self.stamp();
        self.k += 1;
        self.ctx.write(
            &self.bus_name,
            demu_core::device::AccessWidth::Byte,
            demu_core::Address(addr),
            u32::from(value),
            at,
        );
    }

    /// Skip one internal cycle.
    #[inline]
    fn wait(&mut self) {
        self.k += 1;
    }

    /// The number of cycles consumed so far.
    fn cycles(&self) -> u32 {
        self.k
    }

    /// Report a runtime event from the core.
    fn emit(&mut self, event: demu_core::triggers::TriggerEvent) {
        self.ctx.emit(event);
    }
}

impl Device for Wdc65c816 {
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
        self.stopped = false;
        self.waiting = false;
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
        let mut out = Vec::with_capacity(SNAP_LENGTH);
        out.extend_from_slice(SNAP_TAG);
        out.push(SNAP_VERSION);
        out.extend_from_slice(&self.regs.c.to_le_bytes());
        out.extend_from_slice(&self.regs.x.to_le_bytes());
        out.extend_from_slice(&self.regs.y.to_le_bytes());
        out.extend_from_slice(&self.regs.s.to_le_bytes());
        out.extend_from_slice(&self.regs.d.to_le_bytes());
        out.push(self.regs.dbr);
        out.push(self.regs.pbr);
        out.push(self.regs.p);
        out.push(u8::from(self.regs.e));
        out.extend_from_slice(&self.regs.pc.to_le_bytes());
        out.push(u8::from(self.irq_line));
        out.push(u8::from(self.nmi_line));
        out.push(u8::from(self.nmi_edge));
        out.push(u8::from(self.halted_dma));
        out.push(u8::from(self.stopped));
        out.push(u8::from(self.waiting));
        out.push(u8::from(self.reset_pending));
        out.extend_from_slice(&self.cpu_div.to_le_bytes());
        out
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        if data.len() != SNAP_LENGTH {
            return Err(SnapshotError::Mismatch(format!(
                "wdc65c816 chunk must be {SNAP_LENGTH} bytes, got {}",
                data.len()
            )));
        }
        if &data[..4] != SNAP_TAG {
            return Err(SnapshotError::Mismatch("bad wdc65c816 tag".into()));
        }
        if data[4] != SNAP_VERSION {
            return Err(SnapshotError::Mismatch(
                "unsupported wdc65c816 chunk version".into(),
            ));
        }
        self.regs.c = u16::from_le_bytes(data[5..7].try_into().expect("2 bytes"));
        self.regs.x = u16::from_le_bytes(data[7..9].try_into().expect("2 bytes"));
        self.regs.y = u16::from_le_bytes(data[9..11].try_into().expect("2 bytes"));
        self.regs.s = u16::from_le_bytes(data[11..13].try_into().expect("2 bytes"));
        self.regs.d = u16::from_le_bytes(data[13..15].try_into().expect("2 bytes"));
        self.regs.dbr = data[15];
        self.regs.pbr = data[16];
        self.regs.p = data[17];
        self.regs.e = data[18] != 0;
        self.regs.pc = u16::from_le_bytes(data[19..21].try_into().expect("2 bytes"));
        self.irq_line = data[21] != 0;
        self.nmi_line = data[22] != 0;
        self.nmi_edge = data[23] != 0;
        self.halted_dma = data[24] != 0;
        self.stopped = data[25] != 0;
        self.waiting = data[26] != 0;
        self.reset_pending = data[27] != 0;
        self.cpu_div = u64::from_le_bytes(data[28..36].try_into().expect("8 bytes")).clamp(1, 64);
        if self.regs.e {
            // Re-apply the emulation-mode register invariants.
            self.regs.s = 0x0100 | (self.regs.s & 0x00FF);
            self.regs.d &= 0x00FF;
            self.regs.x &= 0x00FF;
            self.regs.y &= 0x00FF;
            self.regs.p |= FLAG_M | FLAG_X;
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
                // The NMI input is edge sensitive: a second pulse
                // while the line stays asserted does not re-latch.
                if self.nmi_line && !was {
                    self.nmi_edge = true;
                    self.wake(tick);
                }
            }
            "irq" => {
                self.irq_line = level == Level::High;
                if self.irq_line {
                    // The ready line releases a WAI core even when the
                    // interrupt disable is set; the handler runs from
                    // the interrupt check at the instruction boundary.
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
            "reset" if level == Level::High => {
                // A hardware reset line: stop the core, take the
                // power-on register state, and service the vector.
                self.reset_now(tick);
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
        if self.stopped || self.waiting {
            self.next = None;
            return;
        }
        crate::exec::step(self, tick, ctx);
    }
}

impl Wdc65c816 {
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

impl demu_core::CpuDebug for Wdc65c816 {
    fn registers(&self) -> std::collections::BTreeMap<String, u64> {
        let mut map = std::collections::BTreeMap::new();
        map.insert("A".to_string(), u64::from(self.regs.c & 0x00FF));
        map.insert("B".to_string(), u64::from(self.regs.c >> 8));
        map.insert("C".to_string(), u64::from(self.regs.c));
        map.insert("X".to_string(), u64::from(self.regs.x & self.x_mask()));
        map.insert("Y".to_string(), u64::from(self.regs.y & self.x_mask()));
        map.insert("S".to_string(), u64::from(self.regs.s));
        map.insert("SP".to_string(), u64::from(self.regs.s));
        map.insert("D".to_string(), u64::from(self.regs.d));
        map.insert("DB".to_string(), u64::from(self.regs.dbr));
        map.insert("DBR".to_string(), u64::from(self.regs.dbr));
        map.insert("KB".to_string(), u64::from(self.regs.pbr));
        map.insert("PBR".to_string(), u64::from(self.regs.pbr));
        // The status readback shows the operating mode's forced
        // select bits: emulation runs with M and X set, native keeps
        // the stored selects. Bit 4 of the internal form is the index
        // select; the pushed byte's break flag lives only in the
        // stack copy.
        map.insert(
            "P".to_string(),
            u64::from(if self.regs.e {
                self.regs.p | FLAG_X | FLAG_M
            } else {
                self.regs.p
            }),
        );
        map.insert("PC".to_string(), u64::from(self.regs.pc));
        map.insert("E".to_string(), u64::from(self.regs.e));
        map
    }

    fn set_register(&mut self, name: &str, value: u64) -> bool {
        match name.to_ascii_uppercase().as_str() {
            "A" => self.regs.c = (self.regs.c & 0xFF00) | ((value as u16) & 0x00FF),
            "C" => self.regs.c = value as u16,
            "B" => self.regs.c = (self.regs.c & 0x00FF) | (((value as u16) & 0x00FF) << 8),
            "X" => self.regs.x = (self.regs.x & !self.x_mask()) | ((value as u16) & self.x_mask()),
            "Y" => self.regs.y = (self.regs.y & !self.x_mask()) | ((value as u16) & self.x_mask()),
            "S" | "SP" => self.set_stack(value as u16),
            "D" => {
                self.regs.d = if self.regs.e {
                    value as u16 & 0xFF
                } else {
                    value as u16
                }
            }
            "DB" | "DBR" => self.regs.dbr = value as u8,
            "KB" | "PBR" => self.regs.pbr = value as u8,
            "P" => {
                let value = if self.regs.e {
                    value as u8 | FLAG_M | FLAG_X
                } else {
                    value as u8
                };
                self.regs.p = value;
            }
            "PC" => self.regs.pc = value as u16,
            "E" => {
                if value != 0 && !self.regs.e {
                    // Entering emulation: force the register forms of
                    // the emulation model.
                    self.regs.e = true;
                    self.regs.p |= FLAG_M | FLAG_X;
                    self.regs.s = 0x0100 | (self.regs.s & 0x00FF);
                    self.regs.d &= 0x00FF;
                    self.regs.x &= 0x00FF;
                    self.regs.y &= 0x00FF;
                } else if value == 0 {
                    self.regs.e = false;
                }
            }
            _ => return false,
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use demu_core::CpuDebug;

    /// An instance for the tests. The kinds are registered by the
    /// assembly layer, so the tests build the instance record directly
    /// instead of walking the config validator.
    fn instance(divider: u32) -> demu_config::model::DeviceInstance {
        demu_config::model::DeviceInstance {
            name: demu_config::newtypes::DeviceName::new("cpu0").expect("device name"),
            kind: demu_config::newtypes::KindName::new("wdc65c816.cpu").expect("kind name"),
            domain: "main".to_string(),
            params: Default::default(),
            dividers: [("cpu".to_string(), divider)].into_iter().collect(),
        }
    }

    fn config() -> demu_config::model::SystemConfig {
        demu_config::model::SystemConfig {
            name: "test".to_string(),
            triple: "wdc65c816".to_string(),
            description: String::new(),
            clocks: Default::default(),
            devices: Vec::new(),
            devices_by_name: Default::default(),
            buses: vec![demu_config::model::Bus {
                name: "cpu".to_string(),
                address_bits: 24,
                owner: None,
                parent: None,
            }],
            windows: Vec::new(),
            connections: Vec::new(),
            signals: Vec::new(),
            boot: Default::default(),
            quirks: Default::default(),
        }
    }

    #[test]
    fn divider_defaults_to_snes_and_overrides() {
        let cpu = Wdc65c816::new(&instance(6), &config());
        assert_eq!(cpu.cpu_div, 6);
        let cpu = Wdc65c816::new(&instance(10), &config());
        assert_eq!(cpu.cpu_div, 10);
    }

    #[test]
    fn power_on_state() {
        let mut cpu = Wdc65c816::new(&instance(6), &config());
        cpu.reset();
        cpu.boot(None);
        assert!(cpu.regs.e, "power-on selects emulation mode");
        assert_eq!(cpu.regs.p & FLAG_I, FLAG_I);
        assert_eq!(cpu.regs.p & FLAG_M, FLAG_M);
        assert_eq!(cpu.regs.p & FLAG_X, FLAG_X);
        assert_eq!(cpu.regs.s & 0x0100, 0x0100);
        assert_eq!(cpu.regs.d, 0);
        assert_eq!(cpu.regs.dbr, 0);
        assert_eq!(cpu.regs.pbr, 0);
        assert_eq!(cpu.next, Some(Tick(0)));
        // The boot preset overrides the power-on form.
        let presets = demu_config::model::BootState::Cpu {
            registers: [("s".to_string(), 0x01E8)].into_iter().collect(),
        };
        cpu.boot(Some(&presets));
        assert_eq!(cpu.regs.s, 0x01E8);
    }

    #[test]
    fn nmi_latches_once_per_edge() {
        let mut cpu = Wdc65c816::new(&instance(6), &config());
        cpu.boot(None);
        cpu.notify_line("nmi", Level::High, Tick(0));
        assert!(cpu.nmi_edge);
        cpu.notify_line("nmi", Level::High, Tick(6));
        assert!(cpu.take_nmi());
        let pending = cpu.interrupt_pending();
        assert!(!pending, "the edge consumed, a held high does not relatch");
        cpu.notify_line("nmi", Level::Low, Tick(12));
        cpu.notify_line("nmi", Level::High, Tick(12));
        assert!(cpu.interrupt_pending(), "a new edge latches again");
    }

    #[test]
    fn snapshot_round_trip() {
        let mut cpu = Wdc65c816::new(&instance(10), &config());
        cpu.reset();
        cpu.boot(None);
        cpu.regs.c = 0x1234;
        cpu.regs.x = 0x0456;
        cpu.regs.d = 0x0200;
        cpu.regs.pbr = 0x01;
        cpu.regs.e = false;
        let snap = cpu.snapshot();
        let mut twin = Wdc65c816::new(&instance(10), &config());
        twin.restore(&snap).expect("restore");
        assert_eq!(twin.snapshot(), snap);
        assert_eq!(twin.cpu_div, 10);
    }

    #[test]
    fn restore_rejects_foreign_chunks() {
        let mut cpu = Wdc65c816::new(&instance(6), &config());
        // A short chunk.
        let short = vec![0u8; SNAP_LENGTH - 1];
        assert!(matches!(
            cpu.restore(&short),
            Err(SnapshotError::Mismatch(_))
        ));
        // A wrong tag.
        let mut wrong = cpu.snapshot();
        wrong[3] ^= 0xFF;
        assert!(matches!(
            cpu.restore(&wrong),
            Err(SnapshotError::Mismatch(_))
        ));
        // A future version.
        let mut future = cpu.snapshot();
        future[4] = 2;
        assert!(matches!(
            cpu.restore(&future),
            Err(SnapshotError::Mismatch(_))
        ));
    }

    #[test]
    fn restore_applies_emulation_invariants() {
        let mut cpu = Wdc65c816::new(&instance(6), &config());
        // A hand-written chunk with a native stack form and a widened
        // index register while the emulation bit is set.
        let mut snap = vec![0u8; SNAP_LENGTH];
        snap[..4].copy_from_slice(SNAP_TAG);
        snap[4] = SNAP_VERSION;
        snap[11] = 0xFD; // s low byte
        snap[13] = 0x80; // d low byte
        snap[17] = FLAG_I;
        snap[18] = 1; // e
        cpu.restore(&snap).expect("restore");
        assert_eq!(cpu.regs.s, 0x0100 | 0xFD);
        assert_eq!(cpu.regs.d, 0x0080);
        assert_eq!(cpu.regs.p & (FLAG_M | FLAG_X), FLAG_M | FLAG_X);
    }

    #[test]
    fn register_map_round_trip() {
        let mut cpu = Wdc65c816::new(&instance(6), &config());
        cpu.regs.e = false;
        cpu.regs.p = 0;
        assert!(cpu.set_register("c", 0x1357));
        assert!(cpu.set_register("x", 0x2468));
        assert!(cpu.set_register("y", 0x0159));
        assert!(cpu.set_register("d", 0x0300));
        assert!(cpu.set_register("dbr", 0x04));
        assert!(cpu.set_register("pbr", 0x05));
        assert!(cpu.set_register("pc", 0x8000));
        let regs = cpu.registers();
        assert_eq!(regs.get("A"), Some(&0x57));
        assert_eq!(regs.get("B"), Some(&0x13));
        assert_eq!(regs.get("C"), Some(&0x1357));
        assert_eq!(regs.get("X"), Some(&0x2468));
        assert_eq!(regs.get("Y"), Some(&0x0159));
        assert_eq!(regs.get("D"), Some(&0x0300));
        assert_eq!(regs.get("DBR"), Some(&0x04));
        assert_eq!(regs.get("PBR"), Some(&0x05));
        assert_eq!(regs.get("PC"), Some(&0x8000));
        // The emulation bit setter applies the entry invariants.
        assert!(cpu.set_register("e", 1));
        assert!(cpu.regs.e);
        assert_eq!(cpu.regs.x, 0x0068, "the index truncates into eight bits");
        assert_eq!(cpu.regs.s & 0xFF00, 0x0100);
        // An unknown name reads false.
        assert!(!cpu.set_register("ip", 0));
    }
}
