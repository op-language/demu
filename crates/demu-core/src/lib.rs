//! The deterministic simulation core of demu.
//!
//! The crate owns the exact time model, the device traits, the bus model,
//! the scheduler, boot states, save states, and the trigger and expression
//! engines. Everything here is synchronous and single-threaded by design:
//! the simulation must be deterministic.
//!
//! Entry points:
//!
//! - [`Machine`] — assemble and run a machine from a resolved
//!   configuration.
//! - [`Frequency`] — the exact rational time type with the ns conversion.
//! - [`Scheduler`] — the event queues with cross-domain ordering.
//! - [`TriggerRegistry`] — the trigger kinds and their conditions.
//! - [`expr`] — the condition expression language.
//! - [`savestate`] — the versioned save-state format.

pub mod boot;
pub mod bus;
pub mod device;
pub mod expr;
pub mod frequency;
pub mod machine;
pub mod savestate;
pub mod scheduler;
pub mod script;
pub mod signal;
pub mod triggers;

pub use bus::{Bus, BusWindow, ConflictPolicy, Mirror};
pub use device::{
    AccessWidth, AudioSource, BusAccess, CpuDebug, Decode, DecodeHit, Device, Frame, InputSink,
    MachineCtx, MappedDevice, PatternSource, Route, Snapshot, SnapshotError, VideoLayer,
    VideoSource, Window,
};
pub use frequency::{Address, Divider, Frequency, Tick};
pub use machine::{DeviceCtor, DeviceCtors, Machine, RunHook, smoke_ctors};
pub use scheduler::{ClockDomain, Event, Scheduler};
pub use script::{InputRunError, InputScheduler, ScriptError, ScriptEvent, run_until_with_input};
pub use signal::{DriveMode, Level, SignalLine};
pub use triggers::{Trigger, TriggerError, TriggerEvent, TriggerKind, TriggerRegistry};
