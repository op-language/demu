//! The resolved, validated configuration model.
//!
//! A `SystemConfig` value exists only after every layer accepted the
//! document. The resolution turns strings into validated newtypes and
//! cross-references into direct links.

use std::collections::BTreeMap;

use crate::error::ConfigError;
use crate::newtypes::{DeviceName, Frequency, KindName, LinePath};

/// A resolved clock domain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClockDomain {
    /// The domain name.
    pub name: String,
    /// The crystal frequency.
    pub frequency: Frequency,
    /// The provenance note.
    pub origin: String,
}

/// A resolved device instance.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceInstance {
    /// The unique device name.
    pub name: DeviceName,
    /// The registry kind key.
    pub kind: KindName,
    /// The clock domain the device lives on.
    pub domain: String,
    /// Kind-specific parameters, validated by the kind at assembly time.
    pub params: BTreeMap<String, toml::Value>,
    /// Named integer dividers the device publishes.
    pub dividers: BTreeMap<String, u32>,
}

/// The window kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WindowKind {
    /// Read-only memory.
    Rom,
    /// Read-write memory.
    Ram,
    /// Character or tile memory.
    Chr,
}

/// The bus-conflict policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictPolicy {
    /// The first window in declaration order wins.
    First,
    /// Both devices answer; the AND of the bytes wins.
    And,
    /// The last window in declaration order wins.
    LastWins,
}

/// One mirror of a window: a repeat at a stride.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mirror {
    /// The stride in bytes.
    pub stride: u32,
    /// The number of repeats.
    pub count: u32,
}

/// A resolved address window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    /// The window name.
    pub name: String,
    /// The bus this window lives on.
    pub bus: String,
    /// The device this window maps to.
    pub device: DeviceName,
    /// The base address.
    pub base: u32,
    /// The window size in bytes.
    pub size: u32,
    /// The window kind.
    pub kind: Option<WindowKind>,
    /// The mirror layout, if any.
    pub mirror: Option<Mirror>,
    /// The overlay group name.
    pub overlay: Option<String>,
    /// The read priority inside the overlay group.
    pub overlay_read_priority: Option<u32>,
    /// The write priority inside the overlay group.
    pub overlay_write_priority: Option<u32>,
    /// The bus-conflict policy.
    pub conflict: Option<ConflictPolicy>,
}

impl Window {
    /// The inclusive last address of the primary window.
    pub fn end(&self) -> u32 {
        self.base + self.size - 1
    }
}

/// A resolved bus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bus {
    /// The bus name.
    pub name: String,
    /// The address width in bits.
    pub address_bits: u32,
    /// The device that owns this bus for DMA, if any.
    pub owner: Option<DeviceName>,
    /// The parent bus whose address decoder this bus borrows.
    pub parent: Option<String>,
}

/// The connection semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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

/// The connection direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionDirection {
    /// The source drives the target.
    Forward,
    /// Both sides drive.
    Bidir,
}

/// A resolved connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connection {
    /// The source endpoint.
    pub from: LinePath,
    /// The target endpoint.
    pub to: LinePath,
    /// The semantics.
    pub semantics: ConnectionSemantics,
    /// The direction, defaulting to forward.
    pub direction: ConnectionDirection,
}

/// A declared signal group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignalGroup {
    /// The group name.
    pub name: String,
    /// The lines in the group.
    pub lines: Vec<LinePath>,
    /// Open-collector semantics.
    pub open_collector: bool,
}

/// The boot-state kind of one device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootState {
    /// A ROM image file.
    Rom {
        /// The path of the ROM image.
        path: String,
    },
    /// A RAM fill pattern.
    Ram {
        /// `zero` or `random(<seed>)`.
        pattern: RamPattern,
    },
    /// A fixed hex dump.
    Hex {
        /// The decoded bytes.
        bytes: Vec<u8>,
    },
    /// CPU register presets.
    Cpu {
        /// Register name to value.
        registers: BTreeMap<String, u64>,
    },
}

/// The RAM fill pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RamPattern {
    /// Fill with zero.
    Zero,
    /// Fill from a deterministic seeded generator.
    Random {
        /// The seed.
        seed: u64,
    },
}

/// The parsed RAM pattern grammar: `zero` or `random(seed)`.
pub fn parse_ram_pattern(raw: &str) -> Result<RamPattern, ConfigError> {
    if raw == "zero" {
        return Ok(RamPattern::Zero);
    }
    if let Some(seed_text) = raw.strip_prefix("random(") {
        let seed_text = seed_text
            .strip_suffix(')')
            .ok_or_else(|| ConfigError::InvalidValue {
                path: "boot.pattern".to_string(),
                message: format!("unclosed random(): {raw}"),
            })?;
        let seed = parse_number(seed_text).ok_or_else(|| ConfigError::InvalidValue {
            path: "boot.pattern".to_string(),
            message: format!("random seed is not a number: {seed_text}"),
        })?;
        return Ok(RamPattern::Random { seed });
    }
    Err(ConfigError::InvalidValue {
        path: "boot.pattern".to_string(),
        message: format!("unknown RAM pattern: {raw}"),
    })
}

/// Parse a decimal or `0x`-hex number.
pub fn parse_number(raw: &str) -> Option<u64> {
    let raw = raw.trim();
    if let Some(hex) = raw.strip_prefix("0x").or_else(|| raw.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).ok()
    } else {
        raw.parse::<u64>().ok()
    }
}

/// Parse the `num/den` frequency form or a bare integer.
pub fn parse_frequency(raw: &str) -> Result<Frequency, ConfigError> {
    let raw = raw.trim();
    let (num_text, den_text) = match raw.split_once('/') {
        Some((n, d)) => (n, d),
        None => (raw, "1"),
    };
    let num = parse_number(num_text).ok_or_else(|| ConfigError::InvalidValue {
        path: "clock.frequency".to_string(),
        message: format!("numerator is not a number: {num_text}"),
    })?;
    let den = parse_number(den_text).ok_or_else(|| ConfigError::InvalidValue {
        path: "clock.frequency".to_string(),
        message: format!("denominator is not a number: {den_text}"),
    })?;
    Frequency::new(num, den)
}

/// A resolved quirk value.
#[derive(Debug, Clone, PartialEq)]
pub enum QuirkValue {
    /// A boolean flag.
    Bool(bool),
    /// An integer value.
    Int(i64),
    /// A string value.
    Str(String),
}

impl QuirkValue {
    /// Interpret the value as a boolean flag.
    ///
    /// `true`, and any nonzero integer, are true. Strings are false.
    pub fn as_flag(&self) -> bool {
        match self {
            QuirkValue::Bool(b) => *b,
            QuirkValue::Int(i) => *i != 0,
            QuirkValue::Str(_) => false,
        }
    }
}

/// The fully resolved system configuration.
///
/// The resolver produces this value only when the document passed every
/// validation layer. Every cross-reference is direct, so consumers never
/// re-check names.
#[derive(Debug, Clone, PartialEq)]
pub struct SystemConfig {
    /// The machine name.
    pub name: String,
    /// The target triple.
    pub triple: String,
    /// The description.
    pub description: String,
    /// The clock domains keyed by name.
    pub clocks: BTreeMap<String, ClockDomain>,
    /// The devices in declaration order.
    pub devices: Vec<DeviceInstance>,
    /// The devices keyed by name.
    pub devices_by_name: BTreeMap<DeviceName, usize>,
    /// The buses in declaration order.
    pub buses: Vec<Bus>,
    /// The windows in declaration order.
    pub windows: Vec<Window>,
    /// The connections in declaration order.
    pub connections: Vec<Connection>,
    /// The signal groups in declaration order.
    pub signals: Vec<SignalGroup>,
    /// The boot states keyed by device name.
    pub boot: BTreeMap<DeviceName, BootState>,
    /// The quirk flags keyed by `kind.quirk` or bare name.
    pub quirks: BTreeMap<String, QuirkValue>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frequency_forms() {
        assert_eq!(
            parse_frequency("236250000/11").unwrap(),
            Frequency::new(236250000, 11).unwrap()
        );
        assert_eq!(
            parse_frequency("1024000").unwrap(),
            Frequency::from_hz(1024000).unwrap()
        );
        assert!(parse_frequency("1/0").is_err());
        assert!(parse_frequency("zero").is_err());
    }

    #[test]
    fn number_forms() {
        assert_eq!(parse_number("0x8000"), Some(0x8000));
        assert_eq!(parse_number("32768"), Some(32768));
        assert_eq!(parse_number("-1"), None);
        assert_eq!(parse_number(""), None);
    }

    #[test]
    fn ram_patterns() {
        assert_eq!(parse_ram_pattern("zero").unwrap(), RamPattern::Zero);
        assert_eq!(
            parse_ram_pattern("random(42)").unwrap(),
            RamPattern::Random { seed: 42 }
        );
        assert_eq!(
            parse_ram_pattern("random(0xFF)").unwrap(),
            RamPattern::Random { seed: 0xFF }
        );
        assert!(parse_ram_pattern("random()").is_err());
        assert!(parse_ram_pattern("ones").is_err());
    }
}
