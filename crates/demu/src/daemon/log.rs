//! Daemon lifecycle logging on stderr.
//!
//! Every event goes through one locked stderr writer, following the
//! binary's stderr diagnostics style. The systemd journal captures the
//! stream for `journalctl --user -u demu`.

use std::io::Write;

/// Write one daemon lifecycle event to stderr.
///
/// Write failures are ignored: a closed journal must never prevent the
/// daemon from shutting down.
pub(crate) fn emit(event: &str, detail: &str) {
    let stderr = std::io::stderr();
    let _ = writeln!(stderr.lock(), "demu-daemon: {event}: {detail}");
}
