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

/// The Atari 2600.
pub const ATARI_2600: &str = include_str!("../../../configs/mos6502-atari-2600.toml");

/// The Atari 5200.
pub const ATARI_5200: &str = include_str!("../../../configs/mos6502-atari-5200.toml");

/// The Atari 7800.
pub const ATARI_7800: &str = include_str!("../../../configs/mos6502-atari-7800.toml");

/// The Atari 800, NTSC.
pub const ATARI_800_NTSC: &str = include_str!("../../../configs/mos6502-atari-800-ntsc.toml");

/// The Atari 800, PAL.
pub const ATARI_800_PAL: &str = include_str!("../../../configs/mos6502-atari-800-pal.toml");

/// The Commodore 64, NTSC.
pub const COMMODORE_64: &str = include_str!("../../../configs/mos6502-commodore-64.toml");

/// The Commodore 64, PAL.
pub const COMMODORE_64_PAL: &str = include_str!("../../../configs/mos6502-commodore-64-pal.toml");

/// The Apple II.
pub const APPLE_II: &str = include_str!("../../../configs/mos6502-apple-ii.toml");

/// The Apple IIc.
pub const APPLE_IIC: &str = include_str!("../../../configs/mos6502-apple-iic.toml");

/// The Apple IIe.
pub const APPLE_IIE: &str = include_str!("../../../configs/mos6502-apple-iie.toml");

/// The Apple IIe, enhanced.
pub const APPLE_IIE_ENHANCED: &str =
    include_str!("../../../configs/mos6502-apple-iie-enhanced.toml");

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
    BundledConfig {
        triple: "mos6502-atari-2600",
        text: ATARI_2600,
    },
    BundledConfig {
        triple: "mos6502-atari-5200",
        text: ATARI_5200,
    },
    BundledConfig {
        triple: "mos6502-atari-7800",
        text: ATARI_7800,
    },
    BundledConfig {
        triple: "mos6502-atari-800-ntsc",
        text: ATARI_800_NTSC,
    },
    BundledConfig {
        triple: "mos6502-atari-800-pal",
        text: ATARI_800_PAL,
    },
    BundledConfig {
        triple: "mos6502-commodore-64",
        text: COMMODORE_64,
    },
    BundledConfig {
        triple: "mos6502-commodore-64-pal",
        text: COMMODORE_64_PAL,
    },
    BundledConfig {
        triple: "mos6502-apple-ii",
        text: APPLE_II,
    },
    BundledConfig {
        triple: "mos6502-apple-iic",
        text: APPLE_IIC,
    },
    BundledConfig {
        triple: "mos6502-apple-iie",
        text: APPLE_IIE,
    },
    BundledConfig {
        triple: "mos6502-apple-iie-enhanced",
        text: APPLE_IIE_ENHANCED,
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
