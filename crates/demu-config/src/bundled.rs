//! Bundled configuration loading.
//!
//! Canonical machine configs live in `configs/<triple>.toml` at the
//! workspace root and are embedded with `include_str!` so that a release
//! binary carries all bundled machines.

use std::path::PathBuf;

use crate::error::ConfigError;
use crate::raw::RawConfig;

/// The smoke machine: a trivial synthetic system for framework tests.
pub const SMOKE: &str = include_str!("../../../configs/smoke.toml");

/// The monochrome Game Boy.
pub const GAMEBOY: &str = include_str!("../../../configs/sm83-nintendo-gameboy.toml");

/// The Game Boy Color.
pub const GAMEBOY_COLOR: &str = include_str!("../../../configs/sm83-nintendo-gameboy-color.toml");

/// A bundled config source: the triple name and the embedded text.
pub struct BundledConfig {
    /// The triple name that selects this config.
    pub triple: &'static str,
    /// The embedded TOML text.
    pub text: &'static str,
}

/// All bundled canonical configs. New system support extends the list.
pub const BUNDLED: &[BundledConfig] = &[
    BundledConfig {
        triple: "smoke",
        text: SMOKE,
    },
    BundledConfig {
        triple: "sm83-nintendo-gameboy",
        text: GAMEBOY,
    },
    BundledConfig {
        triple: "sm83-nintendo-gameboy-color",
        text: GAMEBOY_COLOR,
    },
];

/// Resolve a `--config` argument: a bundled triple name or a file path.
pub fn resolve_source(selector: &str) -> Result<RawConfig, ConfigError> {
    if let Some(bundled) = BUNDLED.iter().find(|b| b.triple == selector) {
        return RawConfig::parse(bundled.text);
    }
    let path = PathBuf::from(selector);
    let text = std::fs::read_to_string(&path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            ConfigError::NotFound(path)
        } else {
            ConfigError::TomlParse {
                message: format!("failed to read {}: {e}", path.display()),
            }
        }
    })?;
    RawConfig::parse(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smoke_config_loads() {
        let raw = RawConfig::parse(SMOKE).expect("smoke config parses");
        let system = raw.system.as_ref().expect("smoke has [system]");
        assert_eq!(system.triple, "smoke");
        assert!(!raw.clocks.is_empty());
        assert!(!raw.device.is_empty());
    }

    #[test]
    fn bundled_listing_contains_smoke() {
        assert!(BUNDLED.iter().any(|b| b.triple == "smoke"));
    }

    #[test]
    fn unknown_selector_reports_not_found() {
        match resolve_source("no-such-triple") {
            Err(ConfigError::NotFound(_)) => {}
            other => panic!("expected NotFound, got {other:?}"),
        }
    }
}
