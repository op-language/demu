//! The daemon session engine and the `demu daemon` execution mode.
//!
//! The engine hosts many emulator sessions inside one process. Every
//! session owns one worker thread; the machine and its service live
//! entirely inside that thread, because [`Machine`] holds
//! `Vec<Box<dyn Device>>` and is not `Send` by design. Commands travel
//! to a worker as an ordered queue of closures, and every command reply
//! returns over its own one-shot `mpsc` channel.
//!
//! The `demu daemon` execution mode hosts this engine behind an MCP
//! Streamable HTTP server (see [`mcp`]); the machine work never joins
//! the HTTP runtime.
//!
//! [`Machine`]: demu_core::machine::Machine

pub mod confinement;
mod log;
mod mcp;
pub mod registry;
pub mod session;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use registry::Registry;

/// The default MCP bind address: the loopback interface, on the port
/// that honors the MOS 6502.
const DEFAULT_HTTP_SOCKET: std::net::SocketAddr =
    std::net::SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 6502);

/// The default concurrent-session cap.
const DEFAULT_SESSION_LIMIT: usize = 8;

/// The usage text of the daemon mode.
const DAEMON_USAGE: &str = "\
demu daemon - host emulator sessions for MCP clients

Usage:
  demu daemon [--http <addr:port>] [--session-limit <n>] [--data-root <dir>]

Options:
  --http <addr:port>       MCP endpoint bind address (default: 127.0.0.1:6502)
  --session-limit <n>      concurrent session cap (default: 8)
  --data-root <dir>        root of the default session output directories
                           (default: $XDG_DATA_HOME/demu else ~/.local/share/demu)
  -h, --help               show this help
";

/// A daemon startup or serve failure.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub(crate) enum DaemonError {
    /// The listener bind, the signal setup, or the runtime failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// One parsed daemon invocation.
struct DaemonOptions {
    /// The MCP bind address.
    http: std::net::SocketAddr,
    /// The concurrent-session cap.
    session_limit: usize,
    /// The session data root override.
    data_root: Option<PathBuf>,
}

fn parse_options(args: &[String]) -> Result<DaemonOptions, String> {
    let mut http = DEFAULT_HTTP_SOCKET;
    let mut session_limit = DEFAULT_SESSION_LIMIT;
    let mut data_root = None;
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--http" => {
                i += 1;
                let raw = args.get(i).ok_or("missing value for --http")?;
                http = raw
                    .parse()
                    .map_err(|_| format!("--http expects <addr>:<port>, got {raw}"))?;
            }
            "--session-limit" => {
                i += 1;
                let raw = args.get(i).ok_or("missing value for --session-limit")?;
                session_limit = raw
                    .parse()
                    .map_err(|_| format!("--session-limit expects a count, got {raw}"))?;
                if session_limit == 0 {
                    return Err("--session-limit must be at least 1".to_owned());
                }
            }
            "--data-root" => {
                i += 1;
                data_root = Some(PathBuf::from(
                    args.get(i).ok_or("missing value for --data-root")?,
                ));
            }
            "-h" | "--help" => {
                print!("{DAEMON_USAGE}");
                return Err(String::new());
            }
            other => return Err(format!("unexpected daemon argument: {other}")),
        }
        i += 1;
    }
    Ok(DaemonOptions {
        http,
        session_limit,
        data_root,
    })
}

/// Resolve the session data root: the `--data-root` override, then
/// `$XDG_DATA_HOME/demu`, then `$HOME/.local/share/demu`.
fn resolve_data_root(data_root: Option<&Path>) -> Result<PathBuf, String> {
    use std::ffi::OsStr;

    if let Some(dir) = data_root {
        return Ok(dir.to_path_buf());
    }
    // The XDG spec makes a relative `XDG_DATA_HOME` or `HOME` unset:
    // a relative value falls through to the next candidate instead of
    // anchoring the root on the process CWD.
    let absolute = |value: &OsStr| !value.is_empty() && Path::new(value).is_absolute();
    let xdg = std::env::var_os("XDG_DATA_HOME").filter(|value| absolute(value));
    if let Some(dir) = xdg {
        return Ok(PathBuf::from(dir).join("demu"));
    }
    let home = std::env::var_os("HOME")
        .filter(|value| absolute(value))
        .ok_or("cannot determine the data root: set XDG_DATA_HOME or HOME, or pass --data-root")?;
    Ok(PathBuf::from(home).join(".local/share/demu"))
}

/// Destroy every live session and report the count.
///
/// The destroy calls join worker threads, so this runs outside the
/// async runtime, after the acceptor has stopped.
fn destroy_sessions(registry: &Registry) -> usize {
    let mut destroyed = 0;
    for (name, _) in registry.list() {
        match registry.destroy(&name) {
            Ok(()) => destroyed += 1,
            Err(error) => log::emit("session destroy failed", &format!("{name}: {error}")),
        }
    }
    destroyed
}

/// The `demu daemon` execution mode: host the session engine behind
/// the MCP Streamable HTTP server.
///
/// Option errors and bind failures exit with code 2; SIGINT and
/// SIGTERM stop the acceptor, destroy the sessions, and exit 0.
pub(crate) fn run(args: &[String]) -> ExitCode {
    let options = match parse_options(args) {
        Ok(options) => options,
        Err(message) => {
            if message.is_empty() {
                // --help printed its text already.
                return ExitCode::SUCCESS;
            }
            eprintln!("error: {message}");
            eprintln!("{DAEMON_USAGE}");
            return ExitCode::from(2);
        }
    };
    let data_root = match resolve_data_root(options.data_root.as_deref()) {
        Ok(dir) => dir,
        Err(message) => {
            log::emit("error", &message);
            return ExitCode::from(2);
        }
    };
    let registry = Arc::new(Registry::new(options.session_limit));
    log::emit(
        "starting",
        &format!(
            "sessions<={} data-root={}",
            options.session_limit,
            data_root.display()
        ),
    );
    let serve = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime.block_on(mcp::serve(registry.clone(), options.http, data_root)),
        Err(error) => Err(DaemonError::Io(error)),
    };
    let destroyed = destroy_sessions(&registry);
    match serve {
        Ok(()) => {
            log::emit("shutdown", &format!("{destroyed} session(s) destroyed"));
            ExitCode::SUCCESS
        }
        Err(error) => {
            log::emit("error", &error.to_string());
            ExitCode::from(2)
        }
    }
}
