//! The SM83 CPU device family.
//!
//! The crate exports the [`CpuDevice`] enum with the `Sm83` core of the
//! Nintendo Game Boy (the Sharp LR35902). The core steps per bus access:
//! every memory access is stamped with its exact domain tick, one M-cycle
//! apart. The core handles halt, stop, interrupts, the EI delay, and the
//! speed switch of the Game Boy Color.
//!
//! Kinds registered here:
//!
//! - `sm83.cpu` — the SM83 core. Quirk flags: `oam_bug`, `wave_ram_undoc`
//!   (declared here for configuration compatibility; the audio device
//!   applies the wave RAM behavior), `halt_undoc`.

pub mod cpu;
pub mod exec;

use demu_core::device::MachineCtx;
use demu_core::frequency::Tick;
use demu_core::{Device, Snapshot, SnapshotError};

use demu_config::model::BootState;

pub use cpu::Sm83;

/// The SM83 CPU device family: one kind today, more as systems land.
pub enum CpuDevice {
    /// The Game Boy SM83 core.
    Sm83(Sm83),
}

impl Device for CpuDevice {
    fn name(&self) -> &str {
        match self {
            CpuDevice::Sm83(c) => c.name(),
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
            CpuDevice::Sm83(c) => Some(c),
        }
    }

    fn cpu_mut(&mut self) -> Option<&mut dyn demu_core::CpuDebug> {
        match self {
            CpuDevice::Sm83(c) => Some(c),
        }
    }

    fn reset(&mut self) {
        match self {
            CpuDevice::Sm83(c) => c.reset(),
        }
    }

    fn boot(&mut self, state: Option<&BootState>) {
        match self {
            CpuDevice::Sm83(c) => c.boot(state),
        }
    }

    fn snapshot(&self) -> Snapshot {
        match self {
            CpuDevice::Sm83(c) => c.snapshot(),
        }
    }

    fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        match self {
            CpuDevice::Sm83(c) => c.restore(data),
        }
    }

    fn notify_line(&mut self, line: &str, level: demu_core::Level, tick: Tick) {
        match self {
            CpuDevice::Sm83(c) => c.notify_line(line, level, tick),
        }
    }

    fn next_event(&self) -> Option<Tick> {
        match self {
            CpuDevice::Sm83(c) => c.next_event(),
        }
    }

    fn tick(&mut self, tick: Tick, ctx: &mut MachineCtx) {
        match self {
            CpuDevice::Sm83(c) => c.tick(tick, ctx),
        }
    }
}

/// The CPU interrupt line names and their `IF` bits.
pub const IRQ_LINES: &[(&str, u8)] = &[
    ("vblank", 0x01),
    ("stat", 0x02),
    ("timer", 0x04),
    ("serial", 0x08),
    ("joypad", 0x10),
];
