//! Interactive shell, JSON-lines protocol, and Unix-socket server for demu.
//!
//! The crate exposes one command set through two renderers:
//!
//! - [`text`] — the human REPL over any `BufRead` reader and writer.
//! - [`json`] — the JSON-lines protocol on stdio or a Unix-domain socket.
//!
//! Both renderers call the same [`service::MachineService`]; neither holds
//! device access.

pub mod json;
pub mod protocol;
pub mod service;
pub mod socket;
pub mod text;

pub use protocol::{ErrorCode, ProtocolError, Request, Response};
pub use service::{CommandError, CommandOutput, MachineService};

/// Parse one text command line into `(command, args-text)`.
pub fn split_command(line: &str) -> (String, String) {
    let line = line.trim();
    match line.split_once(' ') {
        Some((cmd, rest)) => (cmd.to_string(), rest.trim().to_string()),
        None => (line.to_string(), String::new()),
    }
}
