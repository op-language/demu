//! Structured errors for configuration parsing, resolution, and
//! validation.
//!
//! Every failure mode has one variant so that callers can match on the
//! error kind instead of parsing message strings.

use std::path::PathBuf;

/// All failure modes of the configuration pipeline.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigError {
    /// The TOML text is not parseable.
    #[error("failed to parse config: {message}")]
    TomlParse {
        /// Human-readable parse error text.
        message: String,
    },

    /// A required field is absent or a typed field failed to deserialize.
    #[error("schema error at {path}: {message}")]
    Schema {
        /// A field path inside the document, for example `device[2].kind`.
        path: String,
        /// Human-readable description of the schema problem.
        message: String,
    },

    /// A raw value could not become a validated newtype.
    #[error("{path}: invalid value: {message}")]
    InvalidValue {
        /// A field path inside the document.
        path: String,
        /// Human-readable description of the constraint that failed.
        message: String,
    },

    /// A frequency must have positive numerator and denominator.
    #[error("frequency {num}/{den} is invalid: both parts must be positive")]
    FrequencyZero {
        /// The rejected numerator.
        num: u64,
        /// The rejected denominator.
        den: u64,
    },

    /// A kind key does not match the `<family>.<kind>` grammar.
    #[error("invalid kind name: {0}")]
    InvalidKindName(String),

    /// A device name does not match the device-name grammar.
    #[error("invalid device name: {0}")]
    InvalidDeviceName(String),

    /// An endpoint does not match the `device.line` grammar.
    #[error("invalid line path: {0}")]
    InvalidLinePath(String),

    /// A reference to a named entity that the config does not declare.
    #[error("unknown {kind} reference '{name}'")]
    UnknownReference {
        /// What kind of entity was referenced, for example `device`.
        kind: &'static str,
        /// The missing name.
        name: String,
    },

    /// Two devices carry the same name.
    #[error("duplicate device name '{0}'")]
    DuplicateDevice(String),

    /// A device is not attached to any bus.
    #[error("device '{0}' is not on any bus")]
    DeviceOffBus(String),

    /// A connection crosses a domain without a declared async-port pair or
    /// connects a device to itself.
    #[error("invalid connection between {from} and {to}: {reason}")]
    InvalidConnection {
        /// The source endpoint text.
        from: String,
        /// The target endpoint text.
        to: String,
        /// The reason the connection is invalid.
        reason: String,
    },

    /// The extends chain visits a config twice.
    #[error("extends cycle detected: {chain}")]
    ExtendsCycle {
        /// The visited chain, newest first.
        chain: String,
    },

    /// A bundled or file config referenced by extends is missing.
    #[error("config not found: {0}")]
    NotFound(PathBuf),

    /// The file referenced by a boot ROM state is missing.
    #[error("boot ROM file not found: {0}")]
    BootRomNotFound(PathBuf),

    /// A boot ROM file does not fit the device capacity.
    #[error("boot ROM file {file} is {actual} bytes but device {device} holds {capacity}")]
    BootRomSize {
        /// The device name.
        device: String,
        /// The file path text.
        file: String,
        /// The file size in bytes.
        actual: u64,
        /// The device capacity in bytes.
        capacity: u64,
    },

    /// The CPU kind family does not match the triple's CPU family.
    #[error("semantic error: {message}")]
    Semantic {
        /// Human-readable description of the violated machine rule.
        message: String,
    },

    /// Two windows overlap without a declared overlay.
    #[error("windows {a} and {b} overlap at {range} without an overlay declaration")]
    WindowOverlap {
        /// The first window name.
        a: String,
        /// The second window name.
        b: String,
        /// The overlapping range as `0xSTART-0xEND`.
        range: String,
    },
}
