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

/// The NES, NTSC.
pub const NES_NTSC: &str = include_str!("../../../configs/rp2A03-nintendo-nes-ntsc.toml");

/// The NES, PAL.
pub const NES_PAL: &str = include_str!("../../../configs/rp2A07-nintendo-nes-pal.toml");

/// The plain 6502 view of the NTSC NES.
pub const NES_NTSC_6502: &str = include_str!("../../../configs/mos6502-nintendo-nes-ntsc.toml");

/// The plain 6502 view of the PAL NES.
pub const NES_PAL_6502: &str = include_str!("../../../configs/mos6502-nintendo-nes-pal.toml");

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
    BundledConfig {
        triple: "rp2A03-nintendo-nes-ntsc",
        text: NES_NTSC,
    },
    BundledConfig {
        triple: "rp2A07-nintendo-nes-pal",
        text: NES_PAL,
    },
    BundledConfig {
        triple: "mos6502-nintendo-nes-ntsc",
        text: NES_NTSC_6502,
    },
    BundledConfig {
        triple: "mos6502-nintendo-nes-pal",
        text: NES_PAL_6502,
    },
];

/// Resolve a `--config` argument: a bundled triple name or a file path.
///
/// The `[extends]` chain resolves against the bundled configs and files
/// before the document returns; the caller validates the merged result.
pub fn resolve_source(selector: &str) -> Result<RawConfig, ConfigError> {
    let raw = load_raw(selector)?;
    crate::extends::resolve(&raw, &load_raw)
}

fn load_raw(selector: &str) -> Result<RawConfig, ConfigError> {
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
