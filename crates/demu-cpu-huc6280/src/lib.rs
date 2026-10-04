//! The HuC6280 CPU device family.
//!
//! The crate exports the [`CpuDevice`] enum with the Hudson Soft
//! HuC6280 core of the PC Engine. The die is a CMOS 65C02 superset: on
//! top of the core instruction set it carries the MPR mapping unit,
//! the timer, the interrupt controller, and the video strobes that
//! talk to the VDC. Every memory access is stamped with its exact
//! domain tick, one CPU cycle apart, and the mapping unit translates
//! each 64K logical address onto the 8K physical bank the die names.
//!
//! Kinds registered here:
//!
//! - `huc6280.cpu` — the PC Engine core, 3 domain ticks per CPU cycle,
//!   overridable with a `dividers.cpu` entry. Control lines: `nmi`,
//!   `irq1`, `irq2`.

pub mod cpu;
pub mod exec;

#[cfg(test)]
#[path = "exec_tests.rs"]
mod tests;

pub use cpu::Huc6280;
use demu_core::device::MachineCtx;
use demu_core::frequency::Tick;
use demu_core::{Device, Snapshot, SnapshotError};

use demu_config::model::BootState;

/// The HuC6280 CPU device family: the PC Engine core.
#[derive(Debug)]
pub enum CpuDevice {
    /// The HuC6280 core of the PC Engine.
    Huc6280(Huc6280),
}

impl Device for CpuDevice {
    fn name(&self) -> &str {
        match self {
            CpuDevice::Huc6280(c) => c.name(),
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
            CpuDevice::Huc6280(c) => Some(c),
        }
    }

    fn cpu_mut(&mut self) -> Option<&mut dyn demu_core::CpuDebug> {
        match self {
            CpuDevice::Huc6280(c) => Some(c),
        }
    }

    fn reset(&mut self) {
        match self {
            CpuDevice::Huc6280(c) => c.reset(),
        }
    }

    fn boot(&mut self, state: Option<&BootState>) {
        match self {
            CpuDevice::Huc6280(c) => c.boot(state),
        }
    }

    fn snapshot(&self) -> Snapshot {
        match self {
            CpuDevice::Huc6280(c) => c.snapshot(),
        }
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        match self {
            CpuDevice::Huc6280(c) => c.restore(data),
        }
    }

    fn notify_line(&mut self, line: &str, level: demu_core::Level, tick: Tick) {
        match self {
            CpuDevice::Huc6280(c) => c.notify_line(line, level, tick),
        }
    }

    fn next_event(&self) -> Option<Tick> {
        match self {
            CpuDevice::Huc6280(c) => c.next_event(),
        }
    }

    fn tick(&mut self, tick: Tick, ctx: &mut MachineCtx) {
        match self {
            CpuDevice::Huc6280(c) => c.tick(tick, ctx),
        }
    }
}

/// The interrupt and control lines the core answers.
///
/// The NMI latches on a high edge. The IRQ1 and IRQ2 levels feed the
/// die's interrupt controller, which masks them through its disable
/// register and the interrupt disable flag; the timer source the die
/// raises itself never crosses a line.
pub const CONTROL_LINES: &[&str] = &["nmi", "irq1", "irq2"];
