//! Serde types for the TOML DSL sections.
//!
//! These types mirror the document exactly as written. Validation turns
//! them into the resolved [`crate::SystemConfig`] value. No field of these
//! types carries an invariant; that is the job of the resolved model.

use std::collections::BTreeMap;

/// The `[extends]` table.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct ExtendsSpec {
    /// The base config: a bundled triple name or a file path.
    pub base: String,
}

/// The `[system]` table.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SystemSpec {
    /// The machine name.
    pub name: String,
    /// The target triple.
    pub triple: String,
    /// A human-readable description.
    #[serde(default)]
    pub description: String,
}

/// The `[clock.<name>]` table.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ClockSpec {
    /// The crystal frequency as `num/den` or an integer.
    pub frequency: String,
    /// An optional provenance note.
    #[serde(default)]
    pub origin: String,
}

/// The `[[device]]` table.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct DeviceSpec {
    /// The unique device instance name.
    pub name: String,
    /// The registry kind key.
    pub kind: String,
    /// The clock domain the device lives on.
    #[serde(default)]
    pub domain: String,
    /// Kind-specific typed parameters.
    #[serde(default)]
    pub params: BTreeMap<String, toml::Value>,
    /// Named integer dividers the device publishes.
    #[serde(default)]
    pub dividers: BTreeMap<String, u32>,
}

/// The `[[bus]]` table.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BusSpec {
    /// The bus name.
    pub name: String,
    /// The address width in bits.
    #[serde(default = "default_address_bits")]
    pub address_bits: u32,
    /// The device that owns this bus for DMA, if any.
    #[serde(default)]
    pub owner: Option<String>,
    /// The parent bus whose address decoder this bus borrows.
    #[serde(default)]
    pub parent: Option<String>,
}

fn default_address_bits() -> u32 {
    16
}

/// The window address range kind, mirroring the `op-target` memory kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WindowKind {
    /// Read-only memory.
    Rom,
    /// Read-write memory.
    Ram,
    /// Character or tile memory.
    Chr,
}

/// The `[[window]]` table.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WindowSpec {
    /// The window name.
    pub name: String,
    /// The bus this window lives on.
    pub bus: String,
    /// The device this window maps to.
    pub device: String,
    /// The base address, parsed from decimal or `0x` hex.
    pub base: String,
    /// The window size in bytes, parsed from decimal or `0x` hex.
    pub size: String,
    /// The window kind.
    #[serde(default)]
    pub kind: Option<WindowKind>,
    /// The mirror stride, or 0 for no mirroring.
    #[serde(default)]
    pub mirror_stride: Option<String>,
    /// The mirror count.
    #[serde(default)]
    pub mirror_count: Option<u32>,
    /// The overlay group name. Windows in one group may overlap.
    #[serde(default)]
    pub overlay: Option<String>,
    /// The read priority inside the overlay group.
    #[serde(default)]
    pub overlay_read_priority: Option<u32>,
    /// The write priority inside the overlay group.
    #[serde(default)]
    pub overlay_write_priority: Option<u32>,
    /// The bus-conflict policy for this window.
    #[serde(default)]
    pub conflict: Option<ConflictPolicy>,
}

/// What happens when two devices drive one window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConflictPolicy {
    /// The first window in declaration order wins.
    First,
    /// Both devices answer; the AND of the bytes wins.
    And,
    /// The last window in declaration order wins.
    LastWins,
}

/// The `[[connection]]` table.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ConnectionSpec {
    /// The source endpoint as `device.line`.
    pub from: String,
    /// The target endpoint as `device.line`.
    pub to: String,
    /// The connection semantics.
    pub semantics: ConnectionSemantics,
    /// The direction: `forward` (from drives to), `bidir`.
    #[serde(default)]
    pub direction: Option<ConnectionDirection>,
}

/// The connection semantics from the technical design.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConnectionSemantics {
    /// An interrupt request edge.
    Irq,
    /// A DMA handoff.
    DmaRequest,
    /// A frame or scanline boundary signal.
    DisplaySync,
    /// A free-running port pair across domains.
    AsyncPort,
}

/// The direction of a connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConnectionDirection {
    /// The source drives the target.
    Forward,
    /// Both sides drive.
    Bidir,
}

/// The `[[signal]]` table: a group of lines with shared electrical
/// semantics.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SignalSpec {
    /// The signal group name.
    pub name: String,
    /// The lines in the group as `device.line` paths.
    pub lines: Vec<String>,
    /// Open-collector semantics: drivers pull low only.
    #[serde(default)]
    pub open_collector: bool,
}

/// A boot state for one device.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum BootSpec {
    /// A ROM device loads a file.
    Rom {
        /// The path of the ROM image file.
        rom: String,
    },
    /// A RAM device takes a fill pattern.
    Ram {
        /// `zero` or `random(seed)`.
        pattern: String,
    },
    /// A hex dump.
    Hex {
        /// The hex data as one string.
        hex: String,
    },
    /// A CPU device takes register presets.
    Cpu {
        /// Register name to value, values as decimal or `0x` hex strings.
        registers: BTreeMap<String, String>,
    },
}

/// The `[quirks]` table: named quirk flags with typed values.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct QuirksSpec(pub BTreeMap<String, toml::Value>);

/// One raw configuration document, exactly as parsed from TOML.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct RawConfig {
    /// The `[system]` table.
    pub system: Option<SystemSpec>,
    /// The `[extends]` table.
    #[serde(default)]
    pub extends: Option<ExtendsSpec>,
    /// The `[clock.<name>]` tables.
    #[serde(default, rename = "clock")]
    pub clocks: BTreeMap<String, ClockSpec>,
    /// The `[[device]]` tables.
    #[serde(default)]
    pub device: Vec<DeviceSpec>,
    /// The `[[bus]]` tables.
    #[serde(default)]
    pub bus: Vec<BusSpec>,
    /// The `[[window]]` tables.
    #[serde(default)]
    pub window: Vec<WindowSpec>,
    /// The `[[connection]]` tables.
    #[serde(default)]
    pub connection: Vec<ConnectionSpec>,
    /// The `[[signal]]` tables.
    #[serde(default)]
    pub signal: Vec<SignalSpec>,
    /// The `[boot.<device>]` tables.
    #[serde(default)]
    pub boot: BTreeMap<String, BootSpec>,
    /// The `[quirks]` table.
    #[serde(default)]
    pub quirks: QuirksSpec,
}

impl RawConfig {
    /// Parse a raw configuration from TOML text.
    ///
    /// This is the schema layer only: the parse succeeds when the TOML is
    /// well-formed and the shapes match. Further checks happen in
    /// resolution and validation.
    pub fn parse(text: &str) -> Result<Self, crate::error::ConfigError> {
        toml::from_str(text).map_err(|e| crate::error::ConfigError::TomlParse {
            message: e.message().to_string(),
        })
    }

    /// Render the document back to TOML text.
    ///
    /// The render is the canonical merged form: an `extends` chain has
    /// already resolved to its merged raw document, so the text a media
    /// file carries reconstructs the machine without the base file.
    ///
    /// # Errors
    ///
    /// Returns a [`crate::error::ConfigError`] when a value cannot
    /// serialize.
    pub fn to_toml_text(&self) -> Result<String, crate::error::ConfigError> {
        toml::to_string(self).map_err(|e| crate::error::ConfigError::TomlParse {
            message: format!("failed to serialize the configuration: {e}"),
        })
    }
}

impl SystemSpec {
    /// A default `[system]` table for a bare test document.
    pub fn new(name: &str, triple: &str) -> Self {
        Self {
            name: name.to_string(),
            triple: triple.to_string(),
            description: String::new(),
        }
    }
}

impl ClockSpec {
    /// A default `[clock.<name>]` table from an integer-Hz string.
    pub fn new(frequency: &str) -> Self {
        Self {
            frequency: frequency.to_string(),
            origin: String::new(),
        }
    }
}

impl DeviceSpec {
    /// A default `[[device]]` table.
    pub fn new(name: &str, kind: &str, domain: &str) -> Self {
        Self {
            name: name.to_string(),
            kind: kind.to_string(),
            domain: domain.to_string(),
            params: BTreeMap::new(),
            dividers: BTreeMap::new(),
        }
    }
}

impl BusSpec {
    /// A default `[[bus]]` table.
    pub fn new(name: &str, address_bits: u32) -> Self {
        Self {
            name: name.to_string(),
            address_bits,
            owner: None,
            parent: None,
        }
    }
}

impl WindowSpec {
    /// A default `[[window]]` table with a hex range.
    pub fn new(name: &str, bus: &str, device: &str, base: &str, size: &str) -> Self {
        Self {
            name: name.to_string(),
            bus: bus.to_string(),
            device: device.to_string(),
            base: base.to_string(),
            size: size.to_string(),
            kind: None,
            mirror_stride: None,
            mirror_count: None,
            overlay: None,
            overlay_read_priority: None,
            overlay_write_priority: None,
            conflict: None,
        }
    }
}

impl ConnectionSpec {
    /// A default `[[connection]]` table.
    pub fn new(from: &str, to: &str, semantics: ConnectionSemantics) -> Self {
        Self {
            from: from.to_string(),
            to: to.to_string(),
            semantics,
            direction: None,
        }
    }
}

#[cfg(test)]
mod render_tests {
    use super::RawConfig;

    #[test]
    fn the_rendered_text_round_trips_through_parse_and_validation() {
        for triple in ["sm83-nintendo-gameboy", "rp2A03-nintendo-nes-ntsc"] {
            let raw = crate::bundled::resolve_source(triple).expect("bundled raw");
            let text = raw.to_toml_text().expect("renders");
            let back = RawConfig::parse(&text).expect("the rendered text parses");
            let (config, _) = crate::validate(&back).expect("the rendered text validates");
            assert_eq!(config.triple, triple, "{triple} round trips");
        }
    }
}
