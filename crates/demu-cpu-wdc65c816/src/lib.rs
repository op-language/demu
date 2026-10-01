//! The WDC 65C816 CPU device family.
//!
//! The crate exports the [`CpuDevice`] enum with the [`Wdc65c816`]
//! core. The core runs the full documented opcode table in both
//! operating modes: the power-on emulation mode (the 8-bit registers
//! of the 6502, the page-one stack, and the 6502 vector map) and the
//! native mode (the 16-bit C, X, Y, and S registers, the direct-page
//! and bank registers, and the native vector map). Every memory
//! access of an instruction is stamped with its exact domain tick,
//! one CPU cycle apart; the core reads the reset vector through the
//! bus at power-on, and it handles the NMI edge, the level-sensitive
//! IRQ, the `WAI` and `STP` instruction states, the DMA hold, and a
//! reset line.
//!
//! Kinds registered by the assembly layer for this core:
//!
//! - `wdc65c816.cpu` — the SNES-style core; the default divider is 6
//!   domain ticks per CPU cycle. The `cpu` divider entry overrides
//!   the default (10 on the IIgs core).

pub mod cpu;
pub mod exec;

use demu_core::device::MachineCtx;
use demu_core::frequency::Tick;
use demu_core::signal::Level;
use demu_core::{Device, Snapshot, SnapshotError};

use demu_config::model::BootState;

pub use cpu::Wdc65c816;

/// The 65C816 CPU device family.
#[derive(Debug)]
pub enum CpuDevice {
    /// The 65C816 core.
    Wdc65c816(Wdc65c816),
}

impl Device for CpuDevice {
    fn name(&self) -> &str {
        match self {
            CpuDevice::Wdc65c816(c) => c.name(),
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
            CpuDevice::Wdc65c816(c) => c.cpu(),
        }
    }

    fn cpu_mut(&mut self) -> Option<&mut dyn demu_core::CpuDebug> {
        match self {
            CpuDevice::Wdc65c816(c) => c.cpu_mut(),
        }
    }

    fn reset(&mut self) {
        match self {
            CpuDevice::Wdc65c816(c) => c.reset(),
        }
    }

    fn boot(&mut self, state: Option<&BootState>) {
        match self {
            CpuDevice::Wdc65c816(c) => c.boot(state),
        }
    }

    fn snapshot(&self) -> Snapshot {
        match self {
            CpuDevice::Wdc65c816(c) => c.snapshot(),
        }
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        match self {
            CpuDevice::Wdc65c816(c) => c.restore(data),
        }
    }

    fn notify_line(&mut self, line: &str, level: Level, tick: Tick) {
        match self {
            CpuDevice::Wdc65c816(c) => c.notify_line(line, level, tick),
        }
    }

    fn next_event(&self) -> Option<Tick> {
        match self {
            CpuDevice::Wdc65c816(c) => c.next_event(),
        }
    }

    fn tick(&mut self, tick: Tick, ctx: &mut MachineCtx) {
        match self {
            CpuDevice::Wdc65c816(c) => c.tick(tick, ctx),
        }
    }
}

/// The interrupt and control line names the 65C816 core listens to.
///
/// The NMI latches on a high edge, the IRQ level applies while high
/// and the interrupt disable is clear, the ready line drops for a
/// DMA, and the reset line re-enters the power-on sequence (the wake
/// of a `STP` core belongs to it).
pub const CONTROL_LINES: &[&str] = &["nmi", "irq", "halt", "reset"];
