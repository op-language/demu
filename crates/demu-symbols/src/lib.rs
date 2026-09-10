//! Reader for `.opl` and `.linked.opl` symbol tables.
//!
//! These files are JSON dumps of the compiler's object file behind an
//! envelope `{version, ...}`. This crate defines its own subset schema:
//! the envelope version, the target, and the sections with symbols. The
//! reader checks the version, ignores unknown fields, and builds a lookup
//! table by name and by address.

use std::collections::BTreeMap;

use demu_config::model::parse_number;

/// The envelope version this reader accepts.
pub const SUPPORTED_VERSION: u32 = 1;

/// The subset schema of one section.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct Section {
    /// The section name.
    #[serde(default)]
    pub name: String,
    /// The origin address, decimal or `0x` hex.
    #[serde(default)]
    pub org: String,
    /// The bank number.
    #[serde(default)]
    pub bank: u32,
    /// The symbols in this section.
    #[serde(default)]
    pub symbols: Vec<Symbol>,
}

/// The subset schema of one symbol.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct Symbol {
    /// The symbol name.
    #[serde(default)]
    pub name: String,
    /// The offset inside the section.
    #[serde(default)]
    pub offset: u32,
    /// The symbol size in bytes.
    #[serde(default)]
    pub size: u32,
    /// The symbol kind: function, variable, or label.
    #[serde(default)]
    pub kind: String,
}

/// The subset schema of the whole file.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct SymbolTable {
    /// The envelope version.
    pub version: u32,
    /// The target triple.
    #[serde(default)]
    pub target: String,
    /// The sections.
    #[serde(default)]
    pub sections: Vec<Section>,
}

/// A parse failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SymbolsError {
    /// The file is not valid JSON.
    #[error("failed to parse symbol table: {0}")]
    Json(String),

    /// The envelope version is not supported.
    #[error("symbol table version {found} is not supported (expected {expected})")]
    Version {
        /// The version in the file.
        found: u32,
        /// The version the reader accepts.
        expected: u32,
    },
}

impl SymbolTable {
    /// Parse a symbol table from JSON text.
    ///
    /// # Errors
    ///
    /// Returns an error when the text is not valid JSON or carries an
    /// unsupported envelope version.
    pub fn parse(text: &str) -> Result<Self, SymbolsError> {
        let table: Self =
            serde_json::from_str(text).map_err(|e| SymbolsError::Json(e.to_string()))?;
        if table.version != SUPPORTED_VERSION {
            return Err(SymbolsError::Version {
                found: table.version,
                expected: SUPPORTED_VERSION,
            });
        }
        Ok(table)
    }

    /// One resolved symbol entry.
    pub fn entries(&self) -> impl Iterator<Item = SymbolEntry> + '_ {
        self.sections.iter().flat_map(|section| {
            let org = demu_config::model::parse_number(&section.org).unwrap_or(0) as u32;
            section.symbols.iter().map(move |symbol| SymbolEntry {
                name: symbol.name.clone(),
                address: org.wrapping_add(symbol.offset),
                size: symbol.size,
                kind: symbol.kind.clone(),
                section: section.name.clone(),
            })
        })
    }
}

/// One symbol with its resolved absolute address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolEntry {
    /// The symbol name.
    pub name: String,
    /// The absolute address.
    pub address: u32,
    /// The size in bytes.
    pub size: u32,
    /// The kind.
    pub kind: String,
    /// The section name.
    pub section: String,
}

/// The lookup index over a parsed symbol table.
#[derive(Debug, Clone, Default)]
pub struct SymbolLookup {
    by_name: BTreeMap<String, SymbolEntry>,
    by_address: BTreeMap<u32, Vec<SymbolEntry>>,
}

impl SymbolLookup {
    /// Build a lookup index over a parsed table.
    pub fn new(table: &SymbolTable) -> Self {
        let mut lookup = Self::default();
        for entry in table.entries() {
            lookup.by_name.insert(entry.name.clone(), entry.clone());
            lookup
                .by_address
                .entry(entry.address)
                .or_default()
                .push(entry);
        }
        lookup
    }

    /// Look up one symbol by name.
    pub fn by_name(&self, name: &str) -> Option<&SymbolEntry> {
        self.by_name.get(name)
    }

    /// The symbols that start at one address.
    pub fn by_address(&self, address: u32) -> Option<&[SymbolEntry]> {
        self.by_address.get(&address).map(Vec::as_slice)
    }

    /// Resolve a trigger argument that may be a symbol name or an address
    /// text. A symbol name wins; the address form is decimal or `0x` hex.
    pub fn resolve(&self, token: &str) -> Option<u32> {
        if let Some(entry) = self.by_name.get(token) {
            return Some(entry.address);
        }
        parse_number(token).and_then(|v| u32::try_from(v).ok())
    }

    /// All symbols, ordered by name.
    pub fn names(&self) -> Vec<&str> {
        self.by_name.keys().map(String::as_str).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> &'static str {
        r#"{
            "version": 1,
            "target": "sm83-nintendo-gameboy",
            "sections": [
                {
                    "name": "code",
                    "org": "0x150",
                    "bank": 0,
                    "symbols": [
                        {"name": "main", "offset": 0, "size": 16, "kind": "function"},
                        {"name": "loop", "offset": 8, "size": 0, "kind": "label"}
                    ]
                },
                {
                    "name": "ram",
                    "org": "0xC000",
                    "bank": 0,
                    "symbols": [
                        {"name": "frame_counter", "offset": 4, "size": 2, "kind": "variable"}
                    ]
                }
            ]
        }"#
    }

    #[test]
    fn parse_and_lookup_by_name() {
        let table = SymbolTable::parse(sample()).expect("sample parses");
        assert_eq!(table.version, 1);
        assert_eq!(table.sections.len(), 2);

        let lookup = SymbolLookup::new(&table);
        let main = lookup.by_name("main").expect("main resolves");
        assert_eq!(main.address, 0x150);
        let frame = lookup.by_name("frame_counter").expect("frame resolves");
        assert_eq!(frame.address, 0xC004);
        assert_eq!(frame.kind, "variable");
    }

    #[test]
    fn lookup_by_address() {
        let table = SymbolTable::parse(sample()).unwrap();
        let lookup = SymbolLookup::new(&table);
        let at = lookup.by_address(0x158).expect("loop is at 0x158");
        assert_eq!(at[0].name, "loop");
        assert!(lookup.by_address(0x999).is_none());
    }

    #[test]
    fn resolve_accepts_name_and_address_forms() {
        let table = SymbolTable::parse(sample()).unwrap();
        let lookup = SymbolLookup::new(&table);
        assert_eq!(lookup.resolve("main"), Some(0x150));
        assert_eq!(lookup.resolve("0x150"), Some(0x150));
        assert_eq!(lookup.resolve("336"), Some(336));
        assert_eq!(lookup.resolve("no_such"), None);
    }

    #[test]
    fn version_mismatch_is_rejected() {
        let text = r#"{"version": 2, "target": "x", "sections": []}"#;
        match SymbolTable::parse(text) {
            Err(SymbolsError::Version { found, expected }) => {
                assert_eq!(found, 2);
                assert_eq!(expected, SUPPORTED_VERSION);
            }
            other => panic!("expected Version error, got {other:?}"),
        }
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let text = r#"{
            "version": 1,
            "target": "x",
            "future_field": {"a": 1},
            "sections": [{"name": "s", "org": "0", "bank": 0, "symbols": [],
                          "extra": true}]
        }"#;
        let table = SymbolTable::parse(text).expect("unknown fields are ignored");
        assert_eq!(table.sections[0].name, "s");
    }

    #[test]
    fn names_are_ordered() {
        let table = SymbolTable::parse(sample()).unwrap();
        let lookup = SymbolLookup::new(&table);
        assert_eq!(lookup.names(), vec!["frame_counter", "loop", "main"]);
    }
}
