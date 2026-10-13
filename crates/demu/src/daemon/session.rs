//! The session worker: one emulator session on one thread.
//!
//! The worker boots its [`MachineService`] inside the thread, mirrors
//! the CLI bootstrap on submitted data, and serves an ordered queue of
//! command closures until every queue sender is gone.

use std::sync::Arc;
use std::sync::mpsc;

use demu_shell::service::{CommandError, MachineService};
use serde_json::Value;

use super::confinement::{ConfinementDir, ConfinementError};

/// The maximum admitted length of a session name, in characters.
const MAX_NAME_LENGTH: usize = 64;

/// A validated session name.
///
/// The name doubles as a path component under the session data root,
/// so it admits only ASCII letters, digits, hyphen, and underscore, in
/// one to 64 characters.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionName(String);

/// A session name validation failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SessionNameError {
    /// The name carries a character outside the admitted set.
    #[error("session name {raw:?} must contain only ASCII letters, digits, hyphen, and underscore")]
    Character {
        /// The rejected raw text.
        raw: String,
    },
    /// The name is empty or longer than the maximum.
    #[error("session name must be 1 to {max} characters, got {length}")]
    Length {
        /// The rejected length.
        length: usize,
        /// The maximum admitted length.
        max: usize,
    },
}

impl SessionName {
    /// The one canonical constructor: validate the raw text and build
    /// the name.
    pub fn new(raw: &str) -> Result<Self, SessionNameError> {
        let length = raw.chars().count();
        if !(1..=MAX_NAME_LENGTH).contains(&length) {
            return Err(SessionNameError::Length {
                length,
                max: MAX_NAME_LENGTH,
            });
        }
        if !raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(SessionNameError::Character {
                raw: raw.to_owned(),
            });
        }
        Ok(Self(raw.to_owned()))
    }

    /// The name as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SessionName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for SessionName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for SessionName {
    type Error = SessionNameError;

    fn try_from(raw: String) -> Result<Self, Self::Error> {
        Self::new(&raw)
    }
}

/// The configuration source of one session.
#[derive(Debug, Clone)]
pub enum ConfigSource {
    /// Submitted TOML configuration text.
    Toml(String),
    /// A bundled configuration triple.
    Triple(String),
}

/// The requested shape of one emulator session.
#[derive(Debug, Clone)]
pub struct SessionSpec {
    /// The validated session name.
    pub name: SessionName,
    /// The configuration source.
    pub config: ConfigSource,
    /// The cartridge ROM image, sent over the wire.
    pub rom: Option<Vec<u8>>,
    /// The symbol table text, parsed over the wire.
    pub symbols_text: Option<String>,
    /// The output directory for the session's files.
    pub output_dir: std::path::PathBuf,
}

/// The result of one successful session creation.
#[derive(Debug, Clone)]
pub struct SessionCreated {
    /// The session name.
    pub name: SessionName,
    /// The canonical root of the session output directory.
    pub output_dir: std::path::PathBuf,
    /// The configuration lint warnings.
    pub warnings: Vec<demu_config::LintWarning>,
}

/// A session management failure.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SessionError {
    /// A session with the same name exists already.
    #[error("session {0} already exists")]
    Duplicate(SessionName),

    /// The session limit is reached.
    #[error("the session limit of {limit} is reached")]
    LimitReached {
        /// The configured session limit.
        limit: usize,
    },

    /// The configuration text does not parse or resolve.
    #[error(transparent)]
    Config(#[from] demu_config::ConfigError),

    /// The configuration does not validate.
    #[error("configuration validation failed")]
    Validation(demu_config::ValidationReport),

    /// The output directory could not be created or anchored.
    #[error(transparent)]
    OutputDir(#[from] ConfinementError),

    /// The machine failed to assemble or boot.
    #[error(transparent)]
    Machine(#[from] demu_core::machine::MachineError),

    /// The configuration declares no cartridge device for the ROM bytes.
    #[error("the configuration declares no cartridge device")]
    NoCartridge,

    /// The symbol table text does not parse.
    #[error(transparent)]
    Symbols(#[from] demu_symbols::SymbolsError),

    /// No session runs under that name.
    #[error("no session named {0}")]
    Unknown(SessionName),

    /// The session worker died before or while serving a command.
    #[error("the session worker failed: {0}")]
    Worker(String),

    /// The daemon engine hit an internal fault.
    #[error("session engine failure: {0}")]
    Internal(String),

    /// A command executed on the machine failed.
    #[error(transparent)]
    Command(#[from] demu_shell::service::CommandError),
}

/// One queued command for a session worker.
///
/// The closure runs on the worker thread against the session's machine
/// service and its confinement directory, and replies once over the
/// paired one-shot channel. One closure transports one command across
/// the thread boundary; the engine itself keeps plain data types.
pub type SessionJob =
    Box<dyn FnOnce(&mut MachineService, &ConfinementDir) -> Result<Value, CommandError> + Send>;

/// One queued job with its one-shot reply channel.
pub(crate) struct WorkerJob {
    /// The command closure.
    pub(crate) job: SessionJob,
    /// The reply channel; the worker sends exactly one result.
    pub(crate) reply: mpsc::Sender<Result<Value, CommandError>>,
}

/// The boot instructions for one worker thread.
pub(crate) struct WorkerInstruction {
    /// The validated configuration.
    config: demu_config::SystemConfig,
    /// The configuration text tagged into media files.
    source_text: String,
    /// The over-the-wire cartridge image, if any.
    rom: Option<Vec<u8>>,
    /// The over-the-wire symbol table text, if any.
    symbols_text: Option<String>,
    /// The confinement root of the session output directory.
    confined: ConfinementDir,
}

impl WorkerInstruction {
    /// Assemble the boot instructions of one session worker.
    pub(crate) fn new(
        config: demu_config::SystemConfig,
        source_text: String,
        rom: Option<Vec<u8>>,
        symbols_text: Option<String>,
        confined: ConfinementDir,
    ) -> Self {
        Self {
            config,
            source_text,
            rom,
            symbols_text,
            confined,
        }
    }
}

/// Resolve a session configuration source to its raw form and its
/// text.
///
/// A submitted TOML text parses as-is and applies verbatim as the
/// session's media tag; a bundled triple resolves to the bundled text
/// form.
pub(crate) fn resolve_source(
    source: &ConfigSource,
) -> Result<(demu_config::RawConfig, String), SessionError> {
    match source {
        ConfigSource::Toml(text) => Ok((demu_config::RawConfig::parse(text)?, text.clone())),
        ConfigSource::Triple(triple) => {
            let raw = demu_config::bundled::resolve_source(triple)?;
            let text = raw.to_toml_text()?;
            Ok((raw, text))
        }
    }
}

/// Spawn one session worker thread.
///
/// The worker assembles and boots its machine inside the thread, and
/// reports the boot result once over `boot_reply`. The thread exits
/// when the boot fails or when every queue sender is gone.
pub(crate) fn spawn_worker(
    instruction: WorkerInstruction,
    boot_reply: mpsc::Sender<Result<(), SessionError>>,
    queue: mpsc::Receiver<WorkerJob>,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("demu-session".to_owned())
        .spawn(move || run_worker(instruction, boot_reply, queue))
}

/// The worker loop: bootstrap, then serve queued jobs until every
/// queue sender is gone.
fn run_worker(
    instruction: WorkerInstruction,
    boot_reply: mpsc::Sender<Result<(), SessionError>>,
    queue: mpsc::Receiver<WorkerJob>,
) {
    let mut service = MachineService::new();
    let boot = bootstrap(&mut service, &instruction);
    let booted = boot.is_ok();
    if boot_reply.send(boot).is_err() || !booted {
        // A failed boot, or a vanished creator, registers nothing.
        return;
    }
    while let Ok(WorkerJob { job, reply }) = queue.recv() {
        let result = job(&mut service, &instruction.confined);
        // A gone caller leaves nobody to reply to; the worker keeps
        // serving the session.
        let _ = reply.send(result);
    }
}

/// Build the session machine inside the worker thread.
///
/// The flow mirrors the CLI session bootstrap: the configuration text,
/// then the machine factory for the media commands, then the machine
/// with any over-the-wire cartridge image, then the over-the-wire
/// symbol table.
fn bootstrap(
    service: &mut MachineService,
    instruction: &WorkerInstruction,
) -> Result<(), SessionError> {
    service.set_config_source(instruction.source_text.clone());
    service.set_machine_factory(Arc::new(|config| {
        let mut machine =
            demu_core::Machine::assemble(config.clone(), &crate::assemble::demu_ctors())?;
        machine.boot();
        crate::assemble::apply_nes_cartridge_wiring(&mut machine);
        Ok(machine)
    }));
    let mut machine =
        demu_core::Machine::assemble(instruction.config.clone(), &crate::assemble::demu_ctors())?;
    if let Some(bytes) = &instruction.rom {
        // A ROM payload overrides the cartridge boot state before the
        // boot applies every boot entry.
        let cart = machine
            .config
            .devices
            .iter()
            .find(|d| crate::assemble::is_cart_kind(d.kind.as_str()))
            .map(|d| d.name.to_string())
            .ok_or(SessionError::NoCartridge)?;
        machine.override_boot(
            &cart,
            demu_config::model::BootState::Hex {
                bytes: bytes.clone(),
            },
        )?;
    }
    machine.boot();
    crate::assemble::apply_nes_cartridge_wiring(&mut machine);
    service.load_machine(machine);
    if let Some(text) = &instruction.symbols_text {
        let table = demu_symbols::SymbolTable::parse(text)?;
        service.load_symbols(&table);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_accept_the_documented_charset() {
        for raw in ["a", "gb-dev_01", "A9_zZ"] {
            let name = SessionName::new(raw).expect("the name validates");
            assert_eq!(name.as_str(), raw);
            assert_eq!(name.to_string(), raw);
        }
        SessionName::new(&"x".repeat(64)).expect("64 characters pass");
        let name = SessionName::try_from("moved-name".to_owned()).expect("try_from delegates");
        assert_eq!(name.as_str(), "moved-name");
    }

    #[test]
    fn names_reject_bad_charset_and_length() {
        assert!(matches!(
            SessionName::new(""),
            Err(SessionNameError::Length { .. })
        ));
        assert!(matches!(
            SessionName::new(&"x".repeat(65)),
            Err(SessionNameError::Length { .. })
        ));
        assert!(matches!(
            SessionName::new("has space"),
            Err(SessionNameError::Character { .. })
        ));
        assert!(matches!(
            SessionName::new("sláinte"),
            Err(SessionNameError::Character { .. })
        ));
        assert!(matches!(
            SessionName::new("../etc"),
            Err(SessionNameError::Character { .. })
        ));
    }
}
