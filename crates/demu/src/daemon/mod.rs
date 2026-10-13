//! The daemon session engine.
//!
//! The engine hosts many emulator sessions inside one process. Every
//! session owns one worker thread; the machine and its service live
//! entirely inside that thread, because [`Machine`] holds
//! `Vec<Box<dyn Device>>` and is not `Send` by design. Commands travel
//! to a worker as an ordered queue of closures, and every command reply
//! returns over its own one-shot `mpsc` channel.
//!
//! [`Machine`]: demu_core::machine::Machine

// The engine is exercised through its in-process tests and consumed by
// the daemon mode once that mode wires into `main`. The compiler
// cannot reach the engine from `main` yet, so the dead-code
// suppression stays scoped to this module tree.
#![allow(dead_code)]

pub mod confinement;
pub mod registry;
pub mod session;
