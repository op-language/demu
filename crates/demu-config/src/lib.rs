//! The system-configuration DSL for demu.
//!
//! The crate parses TOML machine definitions, resolves `[extends]` chains,
//! validates them in four layers (schema, device graph, semantic, lint),
//! and produces a resolved [`SystemConfig`] that later construction stages
//! consume directly.
//!
//! Entry points:
//!
//! - [`raw::RawConfig::parse`] — parse the document, schema layer only.
//! - [`extends::resolve`] — resolve an extends chain.
//! - [`validate::validate`] — run all layers and produce the resolved
//!   config.
//! - [`bundled::resolve_source`] — resolve a `--config` selector (an
//!   installed or a bundled triple name, or a file path).
//! - [`registry::core_registry`] — the kind registry contract.
//!
//! A `SystemConfig` value exists only when every layer accepted the
//! document. Consumers never re-check names or references.

pub mod bundled;
pub mod error;
pub mod extends;
pub mod model;
pub mod newtypes;
pub mod raw;
pub mod registry;
pub mod validate;

pub use error::ConfigError;
pub use model::SystemConfig;
pub use newtypes::{DeviceName, Frequency, KindName, LineName, LinePath};
pub use raw::RawConfig;
pub use registry::{DomainRequirement, KindInfo, KindRegistry, core_registry};
pub use validate::{LintWarning, ValidationReport, validate};

/// Parse a raw configuration from TOML text at the crate root.
pub fn parse(text: &str) -> Result<RawConfig, ConfigError> {
    RawConfig::parse(text)
}

/// The default machine for framework tests: the bundled smoke config.
pub fn smoke_config() -> Result<SystemConfig, ConfigError> {
    let raw = RawConfig::parse(bundled::SMOKE)?;
    let (config, _) = validate(&raw).map_err(|report| {
        report
            .errors
            .into_iter()
            .next()
            .unwrap_or(ConfigError::Semantic {
                message: "validation failed without a specific error".into(),
            })
    })?;
    Ok(config)
}
