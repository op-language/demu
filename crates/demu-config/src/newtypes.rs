//! Newtypes for the configuration DSL.
//!
//! Every newtype validates in its canonical constructor, exposes no public
//! inner field, and converts through the constructor. An invalid value
//! cannot exist as an instance.

use std::fmt;

use crate::error::ConfigError;

/// A rational frequency `num/den` in Hz, always reduced and positive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Frequency {
    num: u64,
    den: u64,
}

impl Frequency {
    /// Construct a reduced rational frequency from `num/den`.
    pub fn new(num: u64, den: u64) -> Result<Self, ConfigError> {
        if num == 0 || den == 0 {
            return Err(ConfigError::FrequencyZero { num, den });
        }
        let g = gcd(num, den);
        Ok(Self {
            num: num / g,
            den: den / g,
        })
    }

    /// Construct an integer-Hz frequency, the trivial rational `n/1`.
    pub fn from_hz(hz: u64) -> Result<Self, ConfigError> {
        Self::new(hz, 1)
    }

    /// The numerator of the reduced fraction.
    pub fn num(&self) -> u64 {
        self.num
    }

    /// The denominator of the reduced fraction.
    pub fn den(&self) -> u64 {
        self.den
    }
}

impl fmt::Display for Frequency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.den == 1 {
            write!(f, "{}", self.num)
        } else {
            write!(f, "{}/{}", self.num, self.den)
        }
    }
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// A stable device-kind key, for example `"sm83.cpu"`.
///
/// The grammar is `<family>.<kind>` where both parts are non-empty ASCII
/// alphanumeric-or-dash words. The grammar keeps kind keys unambiguous in
/// configs, in registry tables, and in error messages.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct KindName(String);

impl KindName {
    /// Construct a kind key from a raw string.
    pub fn new(raw: &str) -> Result<Self, ConfigError> {
        let (family, kind) = raw
            .split_once('.')
            .ok_or_else(|| ConfigError::InvalidKindName(raw.to_string()))?;
        let valid = |part: &str| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        };
        if !valid(family) || !valid(kind) {
            return Err(ConfigError::InvalidKindName(raw.to_string()));
        }
        Ok(Self(raw.to_string()))
    }

    /// The kind key as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The family part before the dot.
    pub fn family(&self) -> &str {
        self.0.split('.').next().unwrap_or(&self.0)
    }
}

impl fmt::Display for KindName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for KindName {
    type Error = ConfigError;

    fn try_from(raw: String) -> Result<Self, Self::Error> {
        Self::new(&raw)
    }
}

impl AsRef<str> for KindName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// The unique name of one device instance in a system configuration.
///
/// The grammar is a non-empty string of ASCII letters, digits, underscore,
/// and dash, starting with a letter or underscore. Device names appear in
/// endpoints and boot tables, so the grammar keeps references unambiguous.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DeviceName(String);

impl DeviceName {
    /// Construct a device name from a raw string.
    pub fn new(raw: &str) -> Result<Self, ConfigError> {
        let valid = !raw.is_empty()
            && raw.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
            && raw
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
        if !valid {
            return Err(ConfigError::InvalidDeviceName(raw.to_string()));
        }
        Ok(Self(raw.to_string()))
    }

    /// The device name as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DeviceName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for DeviceName {
    type Error = ConfigError;

    fn try_from(raw: String) -> Result<Self, Self::Error> {
        Self::new(&raw)
    }
}

impl AsRef<str> for DeviceName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// The name of one signal line on a device.
///
/// The grammar matches [`DeviceName`]. Line names appear in connection
/// endpoints as `device.line`, so both parts share one grammar.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LineName(String);

impl LineName {
    /// Construct a line name from a raw string.
    pub fn new(raw: &str) -> Result<Self, ConfigError> {
        DeviceName::new(raw).map(|d| Self(d.as_str().to_string()))
    }

    /// The line name as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for LineName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for LineName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// An endpoint reference of the form `device.line`.
///
/// The canonical constructor parses and validates both parts. The typed
/// pair keeps connection endpoints checked at construction instead of at
/// use time.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LinePath {
    device: DeviceName,
    line: LineName,
}

impl LinePath {
    /// Construct an endpoint from `device` and `line` names.
    pub fn new(device: DeviceName, line: LineName) -> Self {
        Self { device, line }
    }

    /// Parse an endpoint from the raw `device.line` form.
    pub fn parse(raw: &str) -> Result<Self, ConfigError> {
        let (device, line) = raw
            .split_once('.')
            .ok_or_else(|| ConfigError::InvalidLinePath(raw.to_string()))?;
        Ok(Self {
            device: DeviceName::new(device)?,
            line: LineName::new(line)?,
        })
    }

    /// The device part.
    pub fn device(&self) -> &DeviceName {
        &self.device
    }

    /// The line part.
    pub fn line(&self) -> &LineName {
        &self.line
    }
}

impl fmt::Display for LinePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.device, self.line)
    }
}

impl TryFrom<String> for LinePath {
    type Error = ConfigError;

    fn try_from(raw: String) -> Result<Self, Self::Error> {
        Self::parse(&raw)
    }
}

impl AsRef<str> for LinePath {
    fn as_ref(&self) -> &str {
        &self.device.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frequency_reduces_and_rejects_zero() {
        let f = Frequency::new(236250000, 11).unwrap();
        assert_eq!((f.num(), f.den()), (236250000, 11));

        let f = Frequency::new(8, 4).unwrap();
        assert_eq!((f.num(), f.den()), (2, 1));

        assert!(Frequency::new(0, 1).is_err());
        assert!(Frequency::new(1, 0).is_err());
    }

    #[test]
    fn frequency_display_shows_reduced_form() {
        assert_eq!(Frequency::new(1, 11).unwrap().to_string(), "1/11");
        assert_eq!(Frequency::from_hz(1024000).unwrap().to_string(), "1024000");
    }

    #[test]
    fn kind_name_grammar() {
        assert!(KindName::new("sm83.cpu").is_ok());
        assert!(KindName::new("rp2A03.cpu").is_ok());
        assert!(KindName::new("sm83cpu").is_err());
        assert!(KindName::new(".cpu").is_err());
        assert!(KindName::new("sm83.").is_err());
        assert!(KindName::new("sm83..cpu").is_err());
        assert!(KindName::new("sm 83.cpu").is_err());
    }

    #[test]
    fn kind_name_family() {
        let k = KindName::new("sm83.cpu").unwrap();
        assert_eq!(k.family(), "sm83");
        assert_eq!(k.as_str(), "sm83.cpu");
    }

    #[test]
    fn device_name_grammar() {
        assert!(DeviceName::new("ppu").is_ok());
        assert!(DeviceName::new("wr_ram_0").is_ok());
        assert!(DeviceName::new("2ppu").is_err());
        assert!(DeviceName::new("").is_err());
        assert!(DeviceName::new("ppu!").is_err());
    }

    #[test]
    fn line_path_parse() {
        let p = LinePath::parse("ppu.vblank").unwrap();
        assert_eq!(p.device().as_str(), "ppu");
        assert_eq!(p.line().as_str(), "vblank");
        assert_eq!(p.to_string(), "ppu.vblank");

        assert!(LinePath::parse("ppu").is_err());
        assert!(LinePath::parse("ppu.").is_err());
        assert!(LinePath::parse(".vblank").is_err());
    }
}
