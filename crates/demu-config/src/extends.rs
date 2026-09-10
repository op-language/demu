//! `[extends]` resolution.
//!
//! A user config names one base config. The base can be a bundled triple
//! name or a file path. The merge replaces named devices, windows,
//! connections, signal groups, boot states, and quirk flags, and appends
//! new ones. The merged document re-runs full validation.

use crate::error::ConfigError;
use crate::raw::RawConfig;

/// Load the extends chain from the given document.
///
/// The chain visits each document once. A document that appears twice is a
/// cycle. The merged result is the root document with every ancestor
/// applied underneath.
pub fn resolve(
    base: &RawConfig,
    load_base: &dyn Fn(&str) -> Result<RawConfig, ConfigError>,
) -> Result<RawConfig, ConfigError> {
    let mut chain: Vec<String> = Vec::new();
    merge_chain(base, load_base, &mut chain)
}

fn merge_chain(
    base: &RawConfig,
    load_base: &dyn Fn(&str) -> Result<RawConfig, ConfigError>,
    chain: &mut Vec<String>,
) -> Result<RawConfig, ConfigError> {
    let Some(extends) = &base.extends else {
        return Ok(base.clone());
    };
    let key = extends.base.clone();
    if chain.iter().any(|k| k == &key) {
        return Err(ConfigError::ExtendsCycle {
            chain: chain.iter().rev().cloned().collect::<Vec<_>>().join(" <- "),
        });
    }
    chain.push(key.clone());
    if chain.len() > MAX_EXTEND_DEPTH {
        return Err(ConfigError::ExtendsCycle {
            chain: chain.join(" <- "),
        });
    }
    let parent = load_base(&key)?;
    let parent_merged = merge_chain(&parent, load_base, chain)?;
    chain.pop();
    Ok(merge(&parent_merged, base))
}

/// The extend chain depth limit. Configs are small; this bound keeps a
/// broken chain from exhausting memory.
const MAX_EXTEND_DEPTH: usize = 32;

fn merge(parent: &RawConfig, child: &RawConfig) -> RawConfig {
    let mut merged = parent.clone();

    if child.system.is_some() {
        merged.system = child.system.clone();
    }
    merged.extends = None;

    // Named overrides replace; new entries append.
    for (name, clock) in &child.clocks {
        merged.clocks.insert(name.clone(), clock.clone());
    }
    for device in &child.device {
        if let Some(slot) = merged.device.iter_mut().find(|d| d.name == device.name) {
            *slot = device.clone();
        } else {
            merged.device.push(device.clone());
        }
    }
    for bus in &child.bus {
        if let Some(slot) = merged.bus.iter_mut().find(|b| b.name == bus.name) {
            *slot = bus.clone();
        } else {
            merged.bus.push(bus.clone());
        }
    }
    for window in &child.window {
        if let Some(slot) = merged.window.iter_mut().find(|w| w.name == window.name) {
            *slot = window.clone();
        } else {
            merged.window.push(window.clone());
        }
    }
    for signal in &child.signal {
        if let Some(slot) = merged.signal.iter_mut().find(|s| s.name == signal.name) {
            *slot = signal.clone();
        } else {
            merged.signal.push(signal.clone());
        }
    }

    // Connection lists replace as a whole: partial connection lists leave
    // the wiring ambiguous.
    if !child.connection.is_empty() {
        merged.connection = child.connection.clone();
    }

    // Boot states are keyed by device; a child key overrides the parent
    // state for that device.
    for (device, boot) in &child.boot {
        merged.boot.insert(device.clone(), boot.clone());
    }
    for (name, value) in &child.quirks.0 {
        merged.quirks.0.insert(name.clone(), value.clone());
    }

    merged
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::raw::{ClockSpec, DeviceSpec, ExtendsSpec, SystemSpec};

    fn config_with(base: &str, device: &str, kind: &str) -> RawConfig {
        RawConfig {
            system: Some(SystemSpec::new("test", base)),
            extends: Some(ExtendsSpec {
                base: base.to_string(),
            }),
            clocks: [("main".to_string(), ClockSpec::new("1000"))].into(),
            device: vec![DeviceSpec::new(device, kind, "main")],
            ..RawConfig::default()
        }
    }

    fn loader(_key: &str) -> Result<RawConfig, ConfigError> {
        Ok(RawConfig {
            system: Some(SystemSpec::new("parent", "smoke")),
            extends: None,
            clocks: [("main".into(), ClockSpec::new("2000"))].into(),
            device: vec![DeviceSpec::new("ram0", "smoke.ram", "main")],
            ..RawConfig::default()
        })
    }

    #[test]
    fn child_overrides_parent_device() {
        let child = config_with("smoke", "cpu0", "smoke.cpu");
        let merged = merge_chain(&child, &loader, &mut Vec::new()).unwrap();
        assert_eq!(
            merged
                .device
                .iter()
                .map(|d| d.name.as_str())
                .collect::<Vec<_>>(),
            vec!["ram0", "cpu0"]
        );
        assert_eq!(merged.clocks["main"].frequency, "1000");
        assert!(merged.extends.is_none());
    }

    #[test]
    fn parent_keeps_entries_child_does_not_touch() {
        let mut child = config_with("smoke", "cpu0", "smoke.cpu");
        child.device.clear();
        child.system = Some(SystemSpec::new("child", "smoke"));
        let merged = merge_chain(&child, &loader, &mut Vec::new()).unwrap();
        assert_eq!(merged.device[0].name, "ram0");
        assert_eq!(merged.system.as_ref().unwrap().name, "child");
    }

    #[test]
    fn self_extends_is_a_cycle() {
        let a = config_with("smoke", "cpu0", "smoke.cpu");
        let loader_self = |key: &str| -> Result<RawConfig, ConfigError> {
            let mut base = config_with(key, "cpu0", "smoke.cpu");
            base.extends = Some(ExtendsSpec {
                base: key.to_string(),
            });
            Ok(base)
        };
        let err = merge_chain(&a, &loader_self, &mut Vec::new()).unwrap_err();
        assert!(matches!(err, ConfigError::ExtendsCycle { .. }));
    }

    #[test]
    fn depth_limit_stops_longs_chains() {
        let child = config_with("smoke", "cpu0", "smoke.cpu");
        let loader_deep = |_key: &str| -> Result<RawConfig, ConfigError> {
            let mut c = config_with("smoke", "cpu0", "smoke.cpu");
            c.extends = Some(ExtendsSpec {
                base: "deeper".to_string(),
            });
            Ok(c)
        };
        let err = merge_chain(&child, &loader_deep, &mut Vec::new()).unwrap_err();
        assert!(matches!(err, ConfigError::ExtendsCycle { .. }));
    }
}
