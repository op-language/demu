//! The Z80 CPU device family.
//!
//! The crate exports the [`CpuDevice`] enum with the NMOS Z80 core. The
//! core runs every documented instruction of the Z80 with the published
//! cycle table: one opcode fetch costs 4 domain ticks per CPU cycle
//! divisor unit, one memory read or write costs 3, and one port access
//! costs 4 with its automatic wait state. Interrupts run in modes 0,
//! 1, and 2; the non-maskable interrupt latches per edge.
//!
//! The undocumented forms sit behind the `undocumented` quirk flag: the
//! index-register halves, the SLL shift, and the `OUT (C),0` form run
//! with their published effects when the flag is set, and run
//! effect-free when it is clear; the `IN (C)` flag-only form and the
//! `OUT (C),0` store then fall to the unknown-form 8 T-state fallback
//! with no bus access.
//!
//! The core answers four lines: `nmi` (edge triggered), `int` (level
//! sampled at instruction boundaries), and `contend_page` (the banked
//! upper RAM of the banked Sinclair models). It drives the `halt` line
//! while a HALT instruction parks it, so a display generator can watch
//! the parked core.
//!
//! Kinds registered here:
//!
//! - `z80.cpu` — the NMOS Z80 core; the `cpu` divider entry sets the
//!   domain ticks per CPU cycle and defaults to 3.
//!
//! ZX-Spectrum contention: the `contended` and `io_contend` parameter
//! entries name the contended address and high-port-byte ranges, and
//! `contend_from`, `contend_to`, and `contend_base` name the frame
//! window and the phase of the delay table. The applied delay is the
//! published pattern 6, 5, 4, 3, 2, 1, 0, 0 indexed by the frame
//! t-state.

pub mod cpu;
pub mod exec;

#[cfg(test)]
#[path = "exec_tests.rs"]
mod tests;

pub use cpu::Z80;
use demu_core::device::MachineCtx;
use demu_core::frequency::Tick;
use demu_core::{Device, Snapshot, SnapshotError};

use demu_config::model::BootState;

/// The Z80 CPU device family.
#[derive(Debug)]
pub enum CpuDevice {
    /// The NMOS Z80 core.
    Z80(Z80),
}

impl Device for CpuDevice {
    fn name(&self) -> &str {
        match self {
            CpuDevice::Z80(c) => c.name(),
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
            CpuDevice::Z80(c) => Some(c),
        }
    }

    fn cpu_mut(&mut self) -> Option<&mut dyn demu_core::CpuDebug> {
        match self {
            CpuDevice::Z80(c) => Some(c),
        }
    }

    fn reset(&mut self) {
        match self {
            CpuDevice::Z80(c) => c.reset(),
        }
    }

    fn boot(&mut self, state: Option<&BootState>) {
        match self {
            CpuDevice::Z80(c) => c.boot(state),
        }
    }

    fn snapshot(&self) -> Snapshot {
        match self {
            CpuDevice::Z80(c) => c.snapshot(),
        }
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        match self {
            CpuDevice::Z80(c) => c.restore(data),
        }
    }

    fn notify_line(&mut self, line: &str, level: demu_core::Level, tick: Tick) {
        match self {
            CpuDevice::Z80(c) => c.notify_line(line, level, tick),
        }
    }

    fn next_event(&self) -> Option<Tick> {
        match self {
            CpuDevice::Z80(c) => c.next_event(),
        }
    }

    fn tick(&mut self, tick: Tick, ctx: &mut MachineCtx) {
        match self {
            CpuDevice::Z80(c) => c.tick(tick, ctx),
        }
    }
}

/// The control lines the core answers.
pub const CONTROL_LINES: &[&str] = &["nmi", "int", "contend_page", "stall", "reset"];

/// The line the core drives while a HALT parks it.
pub const DRIVEN_LINES: &[&str] = &["halt"];
