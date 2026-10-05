//! Validation layers.
//!
//! The layers run in order: schema, device graph, semantic, lint. Errors
//! stop the pipeline; warnings from the lint layer do not.

use rustc_hash::FxHashSet;

use crate::error::ConfigError;
use crate::model::{
    BootState, QuirkValue, SystemConfig, parse_frequency, parse_number, parse_ram_pattern,
};
use crate::newtypes::{DeviceName, LinePath};
use crate::raw::{BootSpec, ConnectionSemantics, RawConfig};
use crate::registry::{DomainRequirement, core_registry};

/// One lint finding: a warning that does not block construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LintWarning {
    /// The stable warning code, for example `unused_clock`.
    pub code: String,
    /// The human-readable message.
    pub message: String,
}

/// The report of one validation run.
#[derive(Debug, Clone, Default)]
pub struct ValidationReport {
    /// Blocking errors, from all layers.
    pub errors: Vec<ConfigError>,
    /// Non-blocking warnings from the lint layer.
    pub warnings: Vec<LintWarning>,
}

impl ValidationReport {
    /// True when the run found no errors.
    pub fn is_ok(&self) -> bool {
        self.errors.is_empty()
    }
}

/// Run all four validation layers and produce the resolved config.
///
/// This is the one entry point that every consumer calls. The result is
/// either a full [`SystemConfig`] or a report with all findings.
pub fn validate(raw: &RawConfig) -> Result<(SystemConfig, Vec<LintWarning>), ValidationReport> {
    let mut report = ValidationReport::default();
    if check_schema(raw, &mut report.errors).is_none() {
        return Err(report);
    }
    let partial = match check_graph(raw, &mut report.errors) {
        Some(p) => p,
        None => return Err(report),
    };
    check_semantic(raw, Some(&partial), &mut report.errors);
    if !report.is_ok() {
        return Err(report);
    }

    let config = finish(partial);
    let warnings = check_lint(raw, &config);
    Ok((config, warnings))
}

/// Build the final `SystemConfig` from the validated partial state.
fn finish(partial: PartialConfig) -> SystemConfig {
    let devices_by_name = partial
        .devices
        .iter()
        .enumerate()
        .map(|(i, (name, _))| (name.clone(), i))
        .collect();
    SystemConfig {
        name: partial.name,
        triple: partial.triple,
        description: partial.description,
        clocks: partial.clocks.into_iter().collect(),
        devices: partial.devices.into_iter().map(|(_, d)| d).collect(),
        devices_by_name,
        buses: partial.buses,
        windows: partial.windows,
        connections: partial.connections,
        signals: partial.signals,
        boot: partial.boots.into_iter().collect(),
        quirks: partial.quirks.into_iter().collect(),
    }
}

/// The graph layer output: names resolved, links checked, but boot states
/// still pending their file checks.
#[derive(Debug, Default)]
struct PartialConfig {
    name: String,
    triple: String,
    description: String,
    clocks: Vec<(String, crate::model::ClockDomain)>,
    devices: Vec<(DeviceName, crate::model::DeviceInstance)>,
    buses: Vec<crate::model::Bus>,
    windows: Vec<crate::model::Window>,
    connections: Vec<crate::model::Connection>,
    signals: Vec<crate::model::SignalGroup>,
    boots: Vec<(DeviceName, BootState)>,
    quirks: Vec<(String, QuirkValue)>,
}

// ---------------------------------------------------------------------------
// Layer 1: schema
// ---------------------------------------------------------------------------

fn check_schema(raw: &RawConfig, errors: &mut Vec<ConfigError>) -> Option<()> {
    let system = raw.system.as_ref()?;
    if system.name.trim().is_empty() {
        errors.push(ConfigError::Schema {
            path: "system.name".into(),
            message: "must not be empty".into(),
        });
    }
    if system.triple.trim().is_empty() {
        errors.push(ConfigError::Schema {
            path: "system.triple".into(),
            message: "must not be empty".into(),
        });
    }
    if raw.clocks.is_empty() {
        errors.push(ConfigError::Schema {
            path: "clock".into(),
            message: "at least one clock domain is required".into(),
        });
    }
    if raw.device.is_empty() {
        errors.push(ConfigError::Schema {
            path: "device".into(),
            message: "at least one device is required".into(),
        });
    }
    if raw.bus.is_empty() {
        errors.push(ConfigError::Schema {
            path: "bus".into(),
            message: "at least one bus is required".into(),
        });
    }
    for (name, clock) in &raw.clocks {
        if let Err(e) = parse_frequency(&clock.frequency) {
            errors.push(ConfigError::Schema {
                path: format!("clock.{name}.frequency"),
                message: e.to_string(),
            });
        }
    }
    for (i, d) in raw.device.iter().enumerate() {
        if d.name.trim().is_empty() {
            errors.push(ConfigError::Schema {
                path: format!("device[{i}].name"),
                message: "must not be empty".into(),
            });
        }
        if d.kind.trim().is_empty() {
            errors.push(ConfigError::Schema {
                path: format!("device[{i}].kind"),
                message: "must not be empty".into(),
            });
        }
        if d.domain.trim().is_empty() {
            errors.push(ConfigError::Schema {
                path: format!("device[{i}].domain"),
                message: "must name a clock domain".into(),
            });
        }
    }
    for w in raw.window.iter() {
        if parse_number(&w.base).is_none() {
            errors.push(ConfigError::Schema {
                path: format!("window[{}].base", w.name),
                message: format!("not a number: {}", w.base),
            });
        }
        if parse_number(&w.size).is_none() {
            errors.push(ConfigError::Schema {
                path: format!("window[{}].size", w.name),
                message: format!("not a number: {}", w.size),
            });
        }
        if let Some(count) = w.mirror_count {
            if w.mirror_stride.is_none() {
                errors.push(ConfigError::Schema {
                    path: format!("window[{}].mirror_stride", w.name),
                    message: "mirror_count requires mirror_stride".into(),
                });
            }
            if count_overflow(count, w.mirror_stride.as_deref()) {
                errors.push(ConfigError::Schema {
                    path: format!("window[{}].mirror_count", w.name),
                    message: "mirror layout overflows the address space".into(),
                });
            }
        } else if w.mirror_stride.is_some() {
            errors.push(ConfigError::Schema {
                path: format!("window[{}].mirror_count", w.name),
                message: "mirror_stride requires mirror_count".into(),
            });
        }
    }
    for (i, s) in raw.signal.iter().enumerate() {
        if s.lines.is_empty() {
            errors.push(ConfigError::Schema {
                path: format!("signal[{i}].lines"),
                message: "must list at least one line".into(),
            });
        }
    }
    if errors.is_empty() { Some(()) } else { None }
}

fn count_overflow(count: u32, stride: Option<&str>) -> bool {
    let stride = match stride.and_then(parse_number) {
        Some(s) if s > 0 => s,
        _ => return true,
    };
    u64::from(count).saturating_mul(stride) > u64::from(u32::MAX)
}

// ---------------------------------------------------------------------------
// Layer 2: device graph
// ---------------------------------------------------------------------------

fn check_graph(raw: &RawConfig, errors: &mut Vec<ConfigError>) -> Option<PartialConfig> {
    let system = raw.system.as_ref()?;

    let mut clocks = Vec::new();
    for (name, spec) in &raw.clocks {
        // The schema layer already rejected unparseable frequencies.
        let Ok(frequency) = parse_frequency(&spec.frequency) else {
            return None;
        };
        clocks.push((
            name.clone(),
            crate::model::ClockDomain {
                name: name.clone(),
                frequency,
                origin: spec.origin.clone(),
            },
        ));
    }

    let registry = core_registry();
    let mut devices = Vec::new();
    let mut seen_names = FxHashSet::default();
    for (i, d) in raw.device.iter().enumerate() {
        let name = match DeviceName::new(&d.name) {
            Ok(n) => n,
            Err(e) => {
                errors.push(e);
                continue;
            }
        };
        let Ok(kind) = crate::newtypes::KindName::new(&d.kind) else {
            errors.push(ConfigError::Schema {
                path: format!("device[{i}].kind"),
                message: format!("not a valid kind key: {}", d.kind),
            });
            continue;
        };
        if !seen_names.insert(name.clone()) {
            errors.push(ConfigError::DuplicateDevice(d.name.clone()));
            continue;
        }
        let domain_ok = raw.clocks.contains_key(&d.domain);
        if !domain_ok {
            errors.push(ConfigError::UnknownReference {
                kind: "clock domain",
                name: d.domain.clone(),
            });
        }
        if let Some(info) = registry.get(&kind) {
            if info.domain == DomainRequirement::Required && d.domain.is_empty() {
                errors.push(ConfigError::Schema {
                    path: format!("device[{i}].domain"),
                    message: format!("kind {} requires a domain", info.kind),
                });
            }
        } else {
            errors.push(ConfigError::UnknownReference {
                kind: "device kind",
                name: d.kind.clone(),
            });
        }
        devices.push((
            name,
            crate::model::DeviceInstance {
                name: DeviceName::new(&d.name).ok()?,
                kind: crate::newtypes::KindName::new(&d.kind).ok()?,
                domain: d.domain.clone(),
                params: d.params.clone(),
                dividers: d.dividers.clone(),
            },
        ));
    }

    for b in raw.bus.iter() {
        if let Some(owner) = &b.owner
            && !raw.device.iter().any(|d| d.name == *owner)
        {
            errors.push(ConfigError::UnknownReference {
                kind: "device",
                name: owner.clone(),
            });
        }
        if let Some(parent) = &b.parent {
            if !raw.bus.iter().any(|p| p.name == *parent) {
                errors.push(ConfigError::UnknownReference {
                    kind: "bus",
                    name: parent.clone(),
                });
            }
            if b.address_bits != 0 && parent == &b.name {
                errors.push(ConfigError::InvalidConnection {
                    from: b.name.clone(),
                    to: b.name.clone(),
                    reason: "a bus cannot be its own parent".into(),
                });
            }
        }
    }

    for w in raw.window.iter() {
        if !raw.bus.iter().any(|b| b.name == w.bus) {
            errors.push(ConfigError::UnknownReference {
                kind: "bus",
                name: w.bus.clone(),
            });
        }
        if !raw.device.iter().any(|d| d.name == w.device) {
            errors.push(ConfigError::UnknownReference {
                kind: "device",
                name: w.device.clone(),
            });
        }
    }

    let mut connections = Vec::new();
    for (i, c) in raw.connection.iter().enumerate() {
        let from = match LinePath::parse(&c.from) {
            Ok(p) => p,
            Err(e) => {
                errors.push(e);
                continue;
            }
        };
        let to = match LinePath::parse(&c.to) {
            Ok(p) => p,
            Err(e) => {
                errors.push(e);
                continue;
            }
        };
        if from.device().as_str() == to.device().as_str() && from.line() == to.line() {
            errors.push(ConfigError::InvalidConnection {
                from: c.from.clone(),
                to: c.to.clone(),
                reason: "an endpoint cannot connect to itself".into(),
            });
            continue;
        }
        for (p, label) in [(&from, "from"), (&to, "to")] {
            if !raw.device.iter().any(|d| d.name == p.device().as_str()) {
                errors.push(ConfigError::UnknownReference {
                    kind: "device",
                    name: p.device().as_str().to_string(),
                });
                errors.push(ConfigError::Schema {
                    path: format!("connection[{i}].{label}"),
                    message: format!("endpoint references unknown device {}", p.device()),
                });
            }
        }
        connections.push(crate::model::Connection {
            from,
            to,
            semantics: match c.semantics {
                ConnectionSemantics::Irq => crate::model::ConnectionSemantics::Irq,
                ConnectionSemantics::DmaRequest => crate::model::ConnectionSemantics::DmaRequest,
                ConnectionSemantics::DisplaySync => crate::model::ConnectionSemantics::DisplaySync,
                ConnectionSemantics::AsyncPort => crate::model::ConnectionSemantics::AsyncPort,
            },
            direction: match c.direction {
                Some(crate::raw::ConnectionDirection::Forward) | None => {
                    crate::model::ConnectionDirection::Forward
                }
                Some(crate::raw::ConnectionDirection::Bidir) => {
                    crate::model::ConnectionDirection::Bidir
                }
            },
        });
    }

    let mut signals = Vec::new();
    for (i, s) in raw.signal.iter().enumerate() {
        let mut lines = Vec::new();
        for l in &s.lines {
            match LinePath::parse(l) {
                Ok(p) => lines.push(p),
                Err(e) => {
                    errors.push(ConfigError::Schema {
                        path: format!("signal[{i}].lines"),
                        message: e.to_string(),
                    });
                }
            }
        }
        signals.push(crate::model::SignalGroup {
            name: s.name.clone(),
            lines,
            open_collector: s.open_collector,
        });
    }

    // A connection from device A to device B is legal only when the two
    // devices share a domain, or the semantics is async-port.
    for c in &connections {
        let from_domain = raw
            .device
            .iter()
            .find(|d| d.name == c.from.device().as_str())
            .map(|d| d.domain.as_str());
        let to_domain = raw
            .device
            .iter()
            .find(|d| d.name == c.to.device().as_str())
            .map(|d| d.domain.as_str());
        if let (Some(fd), Some(td)) = (from_domain, to_domain)
            && fd != td
            && c.semantics != crate::model::ConnectionSemantics::AsyncPort
        {
            errors.push(ConfigError::InvalidConnection {
                from: c.from.to_string(),
                to: c.to.to_string(),
                reason: format!("crosses domains '{fd}' and '{td}'; use async-port semantics"),
            });
        }
    }

    let mut boots = Vec::new();
    for (device, spec) in &raw.boot {
        if !raw.device.iter().any(|d| &d.name == device) {
            errors.push(ConfigError::UnknownReference {
                kind: "device",
                name: device.clone(),
            });
            continue;
        }
        let state = match boot_state(spec) {
            Ok(state) => state,
            Err(e) => {
                errors.push(e);
                continue;
            }
        };
        let Some(name) = DeviceName::new(device).ok() else {
            continue;
        };
        boots.push((name, state));
    }

    let mut quirks = Vec::new();
    for (name, value) in &raw.quirks.0 {
        let quirk_value = match value {
            toml::Value::Boolean(b) => QuirkValue::Bool(*b),
            toml::Value::Integer(i) => QuirkValue::Int(*i),
            toml::Value::String(s) => QuirkValue::Str(s.clone()),
            other => {
                errors.push(ConfigError::InvalidValue {
                    path: format!("quirks.{name}"),
                    message: format!("quirk values are bool, integer, or string, got {other}"),
                });
                continue;
            }
        };
        quirks.push((name.clone(), quirk_value));
    }

    // Quirk names must be declared by the device kinds that the config
    // selects. The key form is `<family>.<quirk>`.
    for (name, _) in &quirks {
        let Some((family, quirk)) = name.split_once('.') else {
            errors.push(ConfigError::UnknownReference {
                kind: "quirk",
                name: name.clone(),
            });
            continue;
        };
        // The config selects the family, and the registry declares the
        // quirk on some kind of that family.
        let family_selected = devices.iter().any(|(_, d)| d.kind.family() == family);
        let quirk_declared = core_registry()
            .kinds()
            .iter()
            .any(|info| info.kind.family() == family && info.quirks.contains(&quirk));
        if !family_selected || !quirk_declared {
            errors.push(ConfigError::UnknownReference {
                kind: "quirk",
                name: name.clone(),
            });
        }
    }

    if errors.is_empty() {
        let mut windows = Vec::with_capacity(raw.window.len());
        for w in &raw.window {
            // The schema layer already rejected unparseable ranges.
            let base = parse_number(&w.base).and_then(|v| u32::try_from(v).ok());
            let size = parse_number(&w.size).and_then(|v| u32::try_from(v).ok());
            let mirror = match (w.mirror_stride.as_deref(), w.mirror_count) {
                (Some(s), Some(c)) => parse_number(s)
                    .and_then(|v| u32::try_from(v).ok())
                    .map(|stride| crate::model::Mirror { stride, count: c }),
                _ => None,
            };
            let (Some(base), Some(size)) = (base, size) else {
                errors.push(ConfigError::Schema {
                    path: format!("window[{}].base", windows.len()),
                    message: "range is not a 32-bit number".into(),
                });
                return None;
            };
            windows.push(crate::model::Window {
                name: w.name.clone(),
                bus: w.bus.clone(),
                device: DeviceName::new(&w.device).ok()?,
                base,
                size,
                kind: w.kind.map(window_kind),
                mirror,
                overlay: w.overlay.clone(),
                overlay_read_priority: w.overlay_read_priority,
                overlay_write_priority: w.overlay_write_priority,
                conflict: w.conflict.map(conflict_policy),
            });
        }
        return Some(PartialConfig {
            name: system.name.clone(),
            triple: system.triple.clone(),
            description: system.description.clone(),
            clocks,
            devices,
            buses: raw
                .bus
                .iter()
                .map(|b| crate::model::Bus {
                    name: b.name.clone(),
                    address_bits: b.address_bits,
                    owner: b.owner.as_deref().and_then(|o| DeviceName::new(o).ok()),
                    parent: b.parent.clone(),
                })
                .collect(),
            windows,
            connections,
            signals,
            boots,
            quirks,
        });
    }
    None
}

fn window_kind(kind: crate::raw::WindowKind) -> crate::model::WindowKind {
    match kind {
        crate::raw::WindowKind::Rom => crate::model::WindowKind::Rom,
        crate::raw::WindowKind::Ram => crate::model::WindowKind::Ram,
        crate::raw::WindowKind::Chr => crate::model::WindowKind::Chr,
    }
}

fn conflict_policy(policy: crate::raw::ConflictPolicy) -> crate::model::ConflictPolicy {
    match policy {
        crate::raw::ConflictPolicy::First => crate::model::ConflictPolicy::First,
        crate::raw::ConflictPolicy::And => crate::model::ConflictPolicy::And,
        crate::raw::ConflictPolicy::LastWins => crate::model::ConflictPolicy::LastWins,
    }
}

fn boot_state(spec: &BootSpec) -> Result<BootState, ConfigError> {
    match spec {
        BootSpec::Rom { rom } => Ok(BootState::Rom { path: rom.clone() }),
        BootSpec::Ram { pattern } => Ok(BootState::Ram {
            pattern: parse_ram_pattern(pattern)?,
        }),
        BootSpec::Hex { hex } => {
            let cleaned: String = hex.chars().filter(|c| !c.is_whitespace()).collect();
            if !cleaned.len().is_multiple_of(2) {
                return Err(ConfigError::InvalidValue {
                    path: "boot.hex".into(),
                    message: "hex dump has an odd number of digits".into(),
                });
            }
            let mut bytes = Vec::with_capacity(cleaned.len() / 2);
            for pair in cleaned.as_bytes().rchunks(2) {
                let text = std::str::from_utf8(pair).unwrap_or_default();
                let byte = u8::from_str_radix(text, 16).map_err(|_| ConfigError::InvalidValue {
                    path: "boot.hex".into(),
                    message: "hex dump contains non-hex digits".into(),
                })?;
                bytes.push(byte);
            }
            bytes.reverse();
            Ok(BootState::Hex { bytes })
        }
        BootSpec::Cpu { registers } => {
            let mut resolved = std::collections::BTreeMap::new();
            for (name, value) in registers {
                let parsed = parse_number(value).ok_or_else(|| ConfigError::InvalidValue {
                    path: format!("boot.registers.{name}"),
                    message: format!("register value is not a number: {value}"),
                })?;
                resolved.insert(name.clone(), parsed);
            }
            Ok(BootState::Cpu {
                registers: resolved,
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Layer 3: semantic
// ---------------------------------------------------------------------------

fn check_semantic(raw: &RawConfig, partial: Option<&PartialConfig>, errors: &mut Vec<ConfigError>) {
    let Some(partial) = partial else { return };

    // The CPU kind family must match the triple's CPU family: the part
    // before the first dash of the triple. A triple without a dash, like
    // the synthetic "smoke", carries no CPU family constraint.
    let system = raw.system.as_ref().expect("schema layer checked [system]");
    let triple_cpu = triple_cpu_prefix(&system.triple);
    let triple_extra = triple_cpu_exception(&system.triple);
    for (_, device) in &partial.devices {
        if triple_cpu.is_empty() {
            continue;
        }
        if device.kind.as_str().ends_with(".cpu")
            && !device.kind.family().eq_ignore_ascii_case(&triple_cpu)
            && !triple_extra.is_some_and(|extra| {
                extra
                    .iter()
                    .any(|family| device.kind.family().eq_ignore_ascii_case(family))
            })
        {
            errors.push(ConfigError::Semantic {
                message: format!(
                    "triple '{}' names CPU family '{}' but device '{}' uses kind '{}'",
                    system.triple, triple_cpu, device.name, device.kind
                ),
            });
        }
    }

    // Every device must take part in the bus fabric: through a window
    // that maps it, by owning a bus (the CPU role), or by being named
    // in another device's parameters (the die-read data chips).
    for (name, _) in &partial.devices {
        let mapped = raw.window.iter().any(|w| {
            DeviceName::new(&w.device)
                .map(|d| d == *name)
                .unwrap_or(false)
        });
        let owns = raw.bus.iter().any(|b| {
            b.owner
                .as_deref()
                .and_then(|o| DeviceName::new(o).ok())
                .as_ref()
                == Some(name)
        });
        if !mapped && !owns && !device_named_in_params(raw, name) {
            errors.push(ConfigError::DeviceOffBus(name.to_string()));
        }
    }

    // Windows must fit the bus address width and not overlap outside
    // declared overlays.
    for (i, w) in partial.windows.iter().enumerate() {
        let Some(bus) = partial.buses.iter().find(|b| b.name == w.bus) else {
            continue;
        };
        let width = bus.address_bits;
        let max = if width >= 32 {
            u32::MAX
        } else {
            (1u32 << width) - 1
        };
        if w.end() > max {
            errors.push(ConfigError::InvalidValue {
                path: format!("window[{i}].size"),
                message: format!(
                    "range 0x{:X}-0x{:X} exceeds the {}-bit address space of bus '{}'",
                    w.base,
                    w.end(),
                    width,
                    w.bus
                ),
            });
        }
    }

    for i in 0..partial.windows.len() {
        for j in (i + 1)..partial.windows.len() {
            let (a, b) = (&partial.windows[i], &partial.windows[j]);
            if a.bus != b.bus || a.name == b.name {
                continue;
            }
            let overlaps = a.base <= b.end() && b.base <= a.end();
            if overlaps && a.overlay.is_none() && b.overlay.is_none() {
                errors.push(ConfigError::WindowOverlap {
                    a: a.name.clone(),
                    b: b.name.clone(),
                    range: format!("0x{:X}-0x{:X}", a.base.max(b.base), a.end().min(b.end())),
                });
            }
        }
    }
}

fn triple_cpu_prefix(triple: &str) -> String {
    match triple.split_once('-') {
        Some((cpu, _)) => cpu.to_string(),
        // A triple without a dash, like the synthetic "smoke", has no CPU
        // family constraint.
        None => String::new(),
    }
}

/// The CPU kind families a triple accepts beyond its own prefix.
///
/// The PC Engine triple names the 6502 family but the machine carries
/// the Hudson Soft HuC6280 die, so the config names the real chip kind
/// while the triple keeps the compatibility form. The dual-CPU
/// machines (the Mega Drive and the Neo Geo AES) carry both the 68000
/// and the Z80 cores in either view of the machine.
fn triple_cpu_exception(triple: &str) -> Option<&'static [&'static str]> {
    match triple {
        "mos6502-nec-pcengine" => Some(&["huc6280"]),
        "m68000-sega-genesis" | "z80-sega-genesis" | "m68000-neogeo-aes" | "z80-neogeo-aes" => {
            Some(&["m68000", "z80"])
        }
        _ => None,
    }
}

/// True when some other device's parameter names this device: the
/// data chips a die reads through its parameters (the video die's ROM
/// sources, the sound chip's sample images) take part in the fabric
/// without owning a window.
fn device_named_in_params(raw: &RawConfig, name: &DeviceName) -> bool {
    raw.device.iter().any(|d| {
        d.name.as_str() != name.as_str()
            && d.params.values().any(|v| {
                v.as_str()
                    .is_some_and(|s| DeviceName::new(s).as_ref() == Ok(name))
            })
    })
}

// ---------------------------------------------------------------------------
// Layer 4: lint
// ---------------------------------------------------------------------------

fn check_lint(raw: &RawConfig, config: &SystemConfig) -> Vec<LintWarning> {
    let mut warnings = Vec::new();
    for name in raw.clocks.keys() {
        let used = raw.device.iter().any(|d| &d.domain == name);
        if !used {
            warnings.push(LintWarning {
                code: "unused_clock".into(),
                message: format!("clock domain '{name}' has no devices"),
            });
        }
    }
    for device in &config.devices {
        let mapped = raw.window.iter().any(|w| {
            DeviceName::new(&w.device)
                .map(|d| d == device.name)
                .unwrap_or(false)
        });
        let owns = raw.bus.iter().any(|b| {
            b.owner
                .as_deref()
                .and_then(|o| DeviceName::new(o).ok())
                .as_ref()
                == Some(&device.name)
        });
        if !mapped && !owns && !device_named_in_params(raw, &device.name) {
            warnings.push(LintWarning {
                code: "orphan_device".into(),
                message: format!("device '{}' has no window on any bus", device.name),
            });
        }
    }
    let mut seen_labels = FxHashSet::default();
    for s in &raw.signal {
        if !seen_labels.insert(s.name.clone()) {
            warnings.push(LintWarning {
                code: "duplicate_label".into(),
                message: format!("signal group '{}' is declared twice", s.name),
            });
        }
    }
    warnings
}
