//! The 65SC02 CPU device family.
//!
//! The crate exports the [`CpuDevice`] enum with the CMOS 65SC02 core.
//! The Lynx core is the `vl65nc02.cpu` kind: a VTI 65C02 die inside the
//! Mikey chip. It carries STZ, the CMOS bit test, and the push and pull
//! index instructions, it omits the page-bit instructions SMB, RMB, BBS,
//! and BBR, and its Mikey memory-map unit steers the RAM overlays of the
//! Suzy, Mikey, ROM, and vector spaces over the `suzy_ram`, `mikey_ram`,
//! `rom_ram`, and `vec_ram` lines. A sequential access costs 4 domain
//! ticks per CPU cycle; the sequential-disable bit of the memory-map
//! control costs 5.
//!
//! The `mos65sc02.cpu` kind is the plain Rockwell-form core with the
//! page-bit instructions and no memory-map unit. No bundled config
//! selects it yet; user machines can.
//!
//! Kinds registered here:
//!
//! - `vl65nc02.cpu` — the Lynx core, 4 domain ticks per CPU cycle.
//! - `mos65sc02.cpu` — the plain core, 4 domain ticks per CPU cycle.

pub mod cpu;
pub mod exec;

#[cfg(test)]
#[path = "exec_tests.rs"]
mod tests;

pub use cpu::Mos65Sc02;
use demu_core::device::MachineCtx;
use demu_core::frequency::Tick;
use demu_core::{Device, Snapshot, SnapshotError};

use demu_config::model::BootState;

/// The 65SC02 CPU device family.
#[derive(Debug)]
pub enum CpuDevice {
    /// The VL65NC02 core of the Atari Lynx (the Mikey die).
    Vl65Nc02(Mos65Sc02),
    /// The plain 65SC02 core with the page-bit instructions.
    Mos65Sc02(Mos65Sc02),
}

impl Device for CpuDevice {
    fn name(&self) -> &str {
        match self {
            CpuDevice::Vl65Nc02(c) => c.name(),
            CpuDevice::Mos65Sc02(c) => c.name(),
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
            CpuDevice::Vl65Nc02(c) => Some(c),
            CpuDevice::Mos65Sc02(c) => Some(c),
        }
    }

    fn cpu_mut(&mut self) -> Option<&mut dyn demu_core::CpuDebug> {
        match self {
            CpuDevice::Vl65Nc02(c) => Some(c),
            CpuDevice::Mos65Sc02(c) => Some(c),
        }
    }

    fn reset(&mut self) {
        match self {
            CpuDevice::Vl65Nc02(c) => c.reset(),
            CpuDevice::Mos65Sc02(c) => c.reset(),
        }
    }

    fn boot(&mut self, state: Option<&BootState>) {
        match self {
            CpuDevice::Vl65Nc02(c) => c.boot(state),
            CpuDevice::Mos65Sc02(c) => c.boot(state),
        }
    }

    fn snapshot(&self) -> Snapshot {
        match self {
            CpuDevice::Vl65Nc02(c) => c.snapshot(),
            CpuDevice::Mos65Sc02(c) => c.snapshot(),
        }
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        match self {
            CpuDevice::Vl65Nc02(c) => c.restore(data),
            CpuDevice::Mos65Sc02(c) => c.restore(data),
        }
    }

    fn notify_line(&mut self, line: &str, level: demu_core::Level, tick: Tick) {
        match self {
            CpuDevice::Vl65Nc02(c) => c.notify_line(line, level, tick),
            CpuDevice::Mos65Sc02(c) => c.notify_line(line, level, tick),
        }
    }

    fn next_event(&self) -> Option<Tick> {
        match self {
            CpuDevice::Vl65Nc02(c) => c.next_event(),
            CpuDevice::Mos65Sc02(c) => c.next_event(),
        }
    }

    fn tick(&mut self, tick: Tick, ctx: &mut MachineCtx) {
        match self {
            CpuDevice::Vl65Nc02(c) => c.tick(tick, ctx),
            CpuDevice::Mos65Sc02(c) => c.tick(tick, ctx),
        }
    }
}

/// The control lines the core answers.
pub const CONTROL_LINES: &[&str] = &["nmi", "irq", "sleep"];

/// The memory-map lines the `vl65nc02.cpu` kind drives.
pub const DRIVEN_LINES: &[&str] = &["suzy_ram", "mikey_ram", "rom_ram", "vec_ram"];
