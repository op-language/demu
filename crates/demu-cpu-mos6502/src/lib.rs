//! The 6502 CPU device family.
//!
//! The crate exports the [`CpuDevice`] enum with the `Mos6502` core of
//! the NES (the RP2A03 and RP2A07 of the Nintendo 8-bit family, and the
//! plain MOS 6502 alias). The core steps per bus access: every memory
//! access is stamped with its exact domain tick, one CPU cycle apart.
//! The core reads the reset vector through the bus at power-on, and it
//! handles the NMI edge, the level-sensitive IRQ, and the DMA hold.
//!
//! Kinds registered here:
//!
//! - `rp2A03.cpu` — the NTSC core, 12 domain ticks per CPU cycle.
//! - `rp2A07.cpu` — the PAL core, 16 domain ticks per CPU cycle.
//! - `mos6502.cpu` — the region-parameterized alias kind; a `pal`
//!   parameter selects the PAL divider. Quirk flag: `undocumented`, on
//!   all three kinds.

pub mod cpu;
pub mod exec;

use demu_core::device::MachineCtx;
use demu_core::frequency::Tick;
use demu_core::{Device, Snapshot, SnapshotError};

use demu_config::model::BootState;

pub use cpu::Mos6502;

/// The 6502 CPU device family: the NES cores and the plain alias.
#[derive(Debug)]
pub enum CpuDevice {
    /// The 6502 core of the NES.
    Mos6502(Mos6502),
}

impl Device for CpuDevice {
    fn name(&self) -> &str {
        match self {
            CpuDevice::Mos6502(c) => c.name(),
        }
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn cpu(&self) -> Option<&dyn demu_core::CpuDebug> {
        match self {
            CpuDevice::Mos6502(c) => Some(c),
        }
    }

    fn cpu_mut(&mut self) -> Option<&mut dyn demu_core::CpuDebug> {
        match self {
            CpuDevice::Mos6502(c) => Some(c),
        }
    }

    fn reset(&mut self) {
        match self {
            CpuDevice::Mos6502(c) => c.reset(),
        }
    }

    fn boot(&mut self, state: Option<&BootState>) {
        match self {
            CpuDevice::Mos6502(c) => c.boot(state),
        }
    }

    fn snapshot(&self) -> Snapshot {
        match self {
            CpuDevice::Mos6502(c) => c.snapshot(),
        }
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        match self {
            CpuDevice::Mos6502(c) => c.restore(data),
        }
    }

    fn notify_line(&mut self, line: &str, level: demu_core::Level, tick: Tick) {
        match self {
            CpuDevice::Mos6502(c) => c.notify_line(line, level, tick),
        }
    }

    fn next_event(&self) -> Option<Tick> {
        match self {
            CpuDevice::Mos6502(c) => c.next_event(),
        }
    }

    fn tick(&mut self, tick: Tick, ctx: &mut MachineCtx) {
        match self {
            CpuDevice::Mos6502(c) => c.tick(tick, ctx),
        }
    }
}

/// The interrupt and control line names the 6502 core listens to.
///
/// The NMI latches on a high edge, the IRQ level applies while high
/// and the interrupt disable is clear, and the ready line drops for a
/// DMA.
pub const CONTROL_LINES: &[&str] = &["nmi", "irq", "halt"];
