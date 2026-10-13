//! Bundled configuration loading.
//!
//! Canonical machine configs live in `configs/<triple>.toml` at the
//! workspace root and are embedded with `include_str!` so that a release
//! binary carries all bundled machines.
//!
//! A bare selector (a triple name with no path separators) resolves in
//! a fixed order: first against the machine files installed under the
//! application config directory `$XDG_CONFIG_HOME/demu/configs` (else
//! `$HOME/.config/demu/configs`), then against the bundled copies, and
//! finally as a file path. A selector with a path separator is a file
//! path only, so an explicit `--config` path always names exactly one
//! file. An installed copy shadows a bundled triple of the same name.

use std::path::{Path, PathBuf};

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

/// The Nintendo SNES.
pub const SNES: &str = include_str!("../../../configs/wdc65c816-nintendo-snes.toml");

/// The Apple IIgs.
pub const APPLE_IIGS: &str = include_str!("../../../configs/wdc65c816-apple-iigs.toml");

/// The Atari Lynx.
pub const LYNX: &str = include_str!("../../../configs/vl65nc02-atari-lynx.toml");

/// The PC Engine.
pub const PCENGINE: &str = include_str!("../../../configs/mos6502-nec-pcengine.toml");

/// The Sega SG-1000.
pub const SG1000: &str = include_str!("../../../configs/z80-sega-sg1000.toml");

/// The Sega Master System.
pub const MASTERSYSTEM: &str = include_str!("../../../configs/z80-sega-mastersystem.toml");

/// The Sega Game Gear.
pub const GAMEGEAR: &str = include_str!("../../../configs/z80-sega-gamegear.toml");

/// The Sinclair ZX80.
pub const ZX80: &str = include_str!("../../../configs/z80-sinclair-zx80.toml");

/// The Sinclair ZX81.
pub const ZX81: &str = include_str!("../../../configs/z80-sinclair-zx81.toml");

/// The Sinclair ZX Spectrum 48K.
pub const SPECTRUM: &str = include_str!("../../../configs/z80-sinclair-spectrum.toml");

/// The Sinclair ZX Spectrum 128K, the banked extension of the 48K.
pub const SPECTRUM128: &str = include_str!("../../../configs/z80-sinclair-spectrum128.toml");

/// The Texas Instruments TI-85.
pub const TI85: &str = include_str!("../../../configs/z80-ti-85.toml");

/// The Sega Mega Drive, the 68000 view.
pub const GENESIS_68K: &str = include_str!("../../../configs/m68000-sega-genesis.toml");

/// The Sega Mega Drive, the sound view.
pub const GENESIS_Z80: &str = include_str!("../../../configs/z80-sega-genesis.toml");

/// The Neo Geo AES, the 68000 view.
pub const NEOGEO_68K: &str = include_str!("../../../configs/m68000-neogeo-aes.toml");

/// The Neo Geo AES, the sound view.
pub const NEOGEO_Z80: &str = include_str!("../../../configs/z80-neogeo-aes.toml");

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
    BundledConfig {
        triple: "wdc65c816-nintendo-snes",
        text: SNES,
    },
    BundledConfig {
        triple: "wdc65c816-apple-iigs",
        text: APPLE_IIGS,
    },
    BundledConfig {
        triple: "vl65nc02-atari-lynx",
        text: LYNX,
    },
    BundledConfig {
        triple: "mos6502-nec-pcengine",
        text: PCENGINE,
    },
    BundledConfig {
        triple: "z80-sega-sg1000",
        text: SG1000,
    },
    BundledConfig {
        triple: "z80-sega-mastersystem",
        text: MASTERSYSTEM,
    },
    BundledConfig {
        triple: "z80-sega-gamegear",
        text: GAMEGEAR,
    },
    BundledConfig {
        triple: "z80-sinclair-zx80",
        text: ZX80,
    },
    BundledConfig {
        triple: "z80-sinclair-zx81",
        text: ZX81,
    },
    BundledConfig {
        triple: "z80-sinclair-spectrum",
        text: SPECTRUM,
    },
    BundledConfig {
        triple: "z80-sinclair-spectrum128",
        text: SPECTRUM128,
    },
    BundledConfig {
        triple: "z80-ti-85",
        text: TI85,
    },
    BundledConfig {
        triple: "m68000-sega-genesis",
        text: GENESIS_68K,
    },
    BundledConfig {
        triple: "z80-sega-genesis",
        text: GENESIS_Z80,
    },
    BundledConfig {
        triple: "m68000-neogeo-aes",
        text: NEOGEO_68K,
    },
    BundledConfig {
        triple: "z80-neogeo-aes",
        text: NEOGEO_Z80,
    },
];

/// The machine-config directory of the demu application config home.
///
/// The XDG base-directory spec names `<config-home>/demu` for the app
/// config home, and demu reads machine files from its `configs`
/// subdirectory. `XDG_CONFIG_HOME` wins when it is set and absolute;
/// otherwise `$HOME/.config` applies when `HOME` is set and absolute.
/// A relative or unset value falls through to the next candidate, per
/// the spec. `None` means demu serves bundled configs and file paths
/// only.
pub fn user_config_dir() -> Option<PathBuf> {
    let configs = |key: &str| -> Option<PathBuf> {
        let value = std::env::var_os(key)?;
        if value.is_empty() || !Path::new(&value).is_absolute() {
            return None;
        }
        Some(PathBuf::from(value).join("demu").join("configs"))
    };
    // The XDG spec makes a relative `XDG_CONFIG_HOME` or `HOME` unset:
    // a relative value falls through to the next candidate instead of
    // anchoring the directory on the process CWD.
    configs("XDG_CONFIG_HOME").or_else(|| configs("HOME"))
}

/// Resolve a `--config` argument: an installed or bundled triple name
/// or a file path.
///
/// The `[extends]` chain resolves through the same order and files;
/// the caller validates the merged result.
pub fn resolve_source(selector: &str) -> Result<RawConfig, ConfigError> {
    resolve_source_in(selector, user_config_dir().as_deref())
}

/// Resolve a selector against an explicit machine-config directory.
///
/// The override replaces the [`user_config_dir`] lookup (an empty dir
/// disables it), which makes resolution deterministic for callers that
/// own their config root and keeps the search testable without
/// touching process environment.
pub fn resolve_source_in(
    selector: &str,
    config_dir: Option<&Path>,
) -> Result<RawConfig, ConfigError> {
    let raw = load_raw(selector, config_dir)?;
    crate::extends::resolve(&raw, &|key| load_raw(key, config_dir))
}

fn load_raw(selector: &str, config_dir: Option<&Path>) -> Result<RawConfig, ConfigError> {
    // The config-directory search names a bare triple only; any
    // separator makes the selector a file path and every lookup joins
    // the `.toml` suffix or takes the selector as written.
    if let Some(dir) = config_dir
        && is_bare_name(selector)
    {
        let path = if selector.ends_with(".toml") {
            dir.join(selector)
        } else {
            dir.join(format!("{selector}.toml"))
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => return RawConfig::parse(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(ConfigError::TomlParse {
                    message: format!("failed to read {}: {e}", path.display()),
                });
            }
        }
    }
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

/// Whether the selector names a triple instead of a file path: a
/// non-empty text with no path separators.
fn is_bare_name(selector: &str) -> bool {
    !selector.is_empty() && !selector.contains('/') && !selector.contains('\\')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A config directory cleared of any prior test artifact.
    fn temp_dir(case: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("demu-config-user-dir-{case}"));
        std::fs::remove_dir_all(&path).ok();
        std::fs::create_dir_all(&path).expect("the config dir creates");
        path
    }

    /// A machine document that only the schema layer parses, with the
    /// system triple naming its origin.
    fn stub_config(triple: &str) -> String {
        format!("[system]\nname = \"stub\"\ntriple = \"{triple}\"\n")
    }

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

    #[test]
    fn a_bare_selector_resolves_from_the_config_dir() {
        let dir = temp_dir("bare");
        std::fs::write(dir.join("user-config.toml"), stub_config("user-dir-config"))
            .expect("write");
        let raw = resolve_source_in("user-config", Some(&dir)).expect("resolves");
        assert_eq!(raw.system.expect("[system]").triple, "user-dir-config");
    }

    #[test]
    fn a_toml_suffixed_selector_resolves_from_the_config_dir() {
        let dir = temp_dir("suffixed");
        std::fs::write(dir.join("user-config.toml"), stub_config("user-dir-config"))
            .expect("write");
        // The selector as written joins without a doubled suffix.
        let raw = resolve_source_in("user-config.toml", Some(&dir)).expect("resolves");
        assert_eq!(raw.system.expect("[system]").triple, "user-dir-config");
    }

    #[test]
    fn the_installed_copy_shadows_the_bundled_triple() {
        let dir = temp_dir("shadow");
        std::fs::write(dir.join("smoke.toml"), stub_config("shadow-test")).expect("write");
        let raw = resolve_source_in("smoke", Some(&dir)).expect("resolves");
        assert_eq!(raw.system.expect("[system]").triple, "shadow-test");
        // The next selector falls back to the bundled copy.
        let raw = resolve_source_in("smoke", None).expect("resolves");
        assert_eq!(raw.system.expect("[system]").triple, "smoke");
    }

    #[test]
    fn a_path_selector_skips_the_config_dir() {
        let dir = temp_dir("path");
        std::fs::write(dir.join("my-config.toml"), stub_config("user-dir-config")).expect("write");
        let other = temp_dir("path-other");
        let file = other.join("my-config.toml");
        std::fs::write(&file, stub_config("path-config")).expect("write");
        // A selector with a separator is a file path only.
        let raw = resolve_source_in(&file.display().to_string(), Some(&dir)).expect("resolves");
        assert_eq!(raw.system.expect("[system]").triple, "path-config");
    }

    #[test]
    fn an_extends_base_resolves_from_the_config_dir() {
        let dir = temp_dir("extends");
        std::fs::write(dir.join("base.toml"), stub_config("base-config")).expect("write");
        std::fs::write(
            dir.join("child.toml"),
            "[system]\nname = \"child\"\ntriple = \"child-config\"\n[extends]\nbase = \"base\"\n",
        )
        .expect("write");
        // The child resolves by its bare name and the extends base in
        // the chain goes through the same search; a miss would error.
        let raw = resolve_source_in("child", Some(&dir)).expect("resolves");
        assert_eq!(raw.system.expect("[system]").triple, "child-config");
    }
}
