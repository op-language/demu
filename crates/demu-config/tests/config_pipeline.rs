//! Integration tests for the configuration pipeline.
//!
//! Each test loads a TOML document and checks one behavior of the
//! resolution or of a validation layer, through the public entry points.

use demu_config::bundled::resolve_source;
use demu_config::{
    ConfigError, DeviceName, Frequency, RawConfig, SystemConfig, parse, smoke_config, validate,
};

fn base_config() -> RawConfig {
    RawConfig::parse(
        r#"
[system]
name = "test"
triple = "smoke"
description = "a test machine"

[clock.main]
frequency = "4194304"

[[device]]
name = "cpu0"
kind = "smoke.cpu"
domain = "main"

[[device]]
name = "ram0"
kind = "smoke.ram"
domain = "main"

[[bus]]
name = "bus0"
address_bits = 16
owner = "cpu0"

[[window]]
name = "w0"
bus = "bus0"
device = "ram0"
base = "0x0000"
size = "0x8000"

[boot.ram0]
pattern = "zero"
"#,
    )
    .expect("base config parses")
}

#[test]
fn smoke_machine_resolves() {
    let config = smoke_config().expect("smoke machine is valid");
    assert_eq!(config.triple, "smoke");
    assert_eq!(config.devices.len(), 3);
    let domain = config.clocks.get("main").expect("main domain");
    assert_eq!(domain.frequency.num(), 4194304);
    assert_eq!(domain.frequency.den(), 1);
    assert!(
        config
            .devices_by_name
            .contains_key(&DeviceName::new("cpu0").unwrap())
    );
}

#[test]
fn frequency_reduces_exact_rationals() {
    let f = Frequency::new(236250000, 11).unwrap();
    assert_eq!((f.num(), f.den()), (236250000, 11));
    assert_eq!(
        Frequency::new(9, 3).unwrap(),
        Frequency::from_hz(3).unwrap()
    );
    assert!(Frequency::new(1, 0).is_err());
    assert!(Frequency::new(0, 1).is_err());
}

#[test]
fn unknown_kind_is_rejected() {
    let mut raw = base_config();
    raw.device[0].kind = "nope.cpu".into();
    let report = validate(&raw).unwrap_err();
    assert!(report.errors.iter().any(|e| matches!(
        e,
        ConfigError::UnknownReference {
            kind: "device kind",
            ..
        }
    )));
}

#[test]
fn unknown_domain_is_rejected() {
    let mut raw = base_config();
    raw.device[0].domain = "phantom".into();
    let report = validate(&raw).unwrap_err();
    assert!(report.errors.iter().any(|e| matches!(
        e,
        ConfigError::UnknownReference {
            kind: "clock domain",
            ..
        }
    )));
}

#[test]
fn duplicate_device_is_rejected() {
    let mut raw = base_config();
    raw.device.push(raw.device[0].clone());
    let report = validate(&raw).unwrap_err();
    assert!(report.errors.iter().any(|e| matches!(
        e,
        ConfigError::DuplicateDevice(name) if name == "cpu0"
    )));
}

#[test]
fn cpu_kind_must_match_triple_family() {
    let mut raw = base_config();
    raw.system.as_mut().unwrap().triple = "z80-smoke".into();
    let report = validate(&raw).unwrap_err();
    assert!(
        report
            .errors
            .iter()
            .any(|e| matches!(e, ConfigError::Semantic { .. }))
    );
}

#[test]
fn the_pcengine_triple_accepts_the_huc6280_kind() {
    let mut raw = base_config();
    raw.system.as_mut().unwrap().triple = "mos6502-nec-pcengine".into();
    raw.device[0].kind = "huc6280.cpu".to_string();
    let (config, _) = validate(&raw).expect("the real chip kind passes");
    assert_eq!(config.devices[0].kind.as_str(), "huc6280.cpu");
    // Other triples keep the strict family rule.
    raw.system.as_mut().unwrap().triple = "z80-smoke".into();
    assert!(validate(&raw).is_err());
}

#[test]
fn device_off_bus_is_rejected() {
    let mut raw = base_config();
    raw.window.clear();
    let report = validate(&raw).unwrap_err();
    assert!(
        report
            .errors
            .iter()
            .any(|e| matches!(e, ConfigError::DeviceOffBus(_)))
    );
}

#[test]
fn window_overlap_without_overlay_is_rejected() {
    let mut raw = base_config();
    raw.window.push(demu_config::raw::WindowSpec::new(
        "w1", "bus0", "ram0", "0x100", "0x100",
    ));
    let report = validate(&raw).unwrap_err();
    assert!(
        report
            .errors
            .iter()
            .any(|e| matches!(e, ConfigError::WindowOverlap { .. }))
    );
}

#[test]
fn window_beyond_bus_width_is_rejected() {
    let mut raw = base_config();
    raw.window[0].size = "0x20000".into();
    let report = validate(&raw).unwrap_err();
    assert!(
        report
            .errors
            .iter()
            .any(|e| matches!(e, ConfigError::InvalidValue { .. }))
    );
}

#[test]
fn unknown_quirk_is_rejected() {
    let mut raw = base_config();
    // The base config declares no quirk flags, so the registry accepts any
    // quirk name of the selected families: the kind `smoke.cpu` declares
    // `smoke.trace`. Test both directions.
    raw.quirks
        .0
        .insert("smoke.trace".into(), toml::Value::Boolean(true));
    let (config, _) = validate(&raw).expect("declared quirk is accepted");
    assert!(config.quirks["smoke.trace"].as_flag());

    let mut raw = base_config();
    raw.quirks
        .0
        .insert("smoke.unknown_flag".into(), toml::Value::Boolean(true));
    let report = validate(&raw).unwrap_err();
    assert!(
        report
            .errors
            .iter()
            .any(|e| matches!(e, ConfigError::UnknownReference { kind: "quirk", .. }))
    );
}

#[test]
fn cross_domain_connection_requires_async_port() {
    let mut raw = base_config();
    raw.clocks
        .insert("aux".into(), demu_config::raw::ClockSpec::new("1024000"));
    raw.device.push(demu_config::raw::DeviceSpec::new(
        "ram1",
        "smoke.ram",
        "audio",
    ));
    raw.device[1].domain = "phantom".into();
    // First the graph layer must reject the unknown domain.
    let report = validate(&raw).unwrap_err();
    assert!(report.errors.iter().any(|e| matches!(
        e,
        ConfigError::UnknownReference {
            kind: "clock domain",
            ..
        }
    )));

    // With the domain fixed, an irq connection across domains must fail.
    let mut raw = base_config();
    raw.clocks
        .insert("second".into(), demu_config::raw::ClockSpec::new("1024000"));
    raw.device.push(demu_config::raw::DeviceSpec::new(
        "ram1",
        "smoke.ram",
        "second",
    ));
    raw.connection.push(demu_config::raw::ConnectionSpec::new(
        "cpu0.irq",
        "ram1.irq",
        demu_config::raw::ConnectionSemantics::Irq,
    ));
    let report = validate(&raw).unwrap_err();
    assert!(
        report
            .errors
            .iter()
            .any(|e| matches!(e, ConfigError::InvalidConnection { .. }))
    );
}

#[test]
fn async_port_connection_across_domains_is_accepted() {
    let mut raw = base_config();
    raw.clocks
        .insert("second".into(), demu_config::raw::ClockSpec::new("1024000"));
    raw.device.push(demu_config::raw::DeviceSpec::new(
        "ram1",
        "smoke.ram",
        "second",
    ));
    raw.bus.push(demu_config::raw::BusSpec {
        name: "bus1".into(),
        address_bits: 16,
        owner: Some("ram1".into()),
        parent: None,
    });
    raw.window.push(demu_config::raw::WindowSpec::new(
        "w1", "bus1", "ram1", "0x0000", "0x8000",
    ));
    raw.connection.push(demu_config::raw::ConnectionSpec::new(
        "cpu0.data",
        "ram1.data",
        demu_config::raw::ConnectionSemantics::AsyncPort,
    ));
    let (config, _) = validate(&raw).expect("async-port crossing is accepted");
    assert_eq!(config.connections.len(), 1);
}

#[test]
fn boot_state_forms_resolve() {
    let mut raw = base_config();
    raw.boot.insert(
        "ram0".into(),
        demu_config::raw::BootSpec::Hex {
            hex: "DE AD BE EF".into(),
        },
    );
    raw.boot.insert(
        "cpu0".into(),
        demu_config::raw::BootSpec::Cpu {
            registers: [("ip".into(), "0x0100".into())].into(),
        },
    );
    let (config, _) = validate(&raw).expect("boot states resolve");
    match &config.boot[&DeviceName::new("ram0").unwrap()] {
        demu_config::model::BootState::Hex { bytes } => {
            assert_eq!(bytes, &[0xDE, 0xAD, 0xBE, 0xEF])
        }
        other => panic!("expected hex boot state, got {other:?}"),
    }
    match &config.boot[&DeviceName::new("cpu0").unwrap()] {
        demu_config::model::BootState::Cpu { registers } => {
            assert_eq!(registers["ip"], 0x0100)
        }
        other => panic!("expected cpu boot state, got {other:?}"),
    }
}

#[test]
fn boot_state_errors_are_specific() {
    let mut raw = base_config();
    raw.boot.insert(
        "ram0".into(),
        demu_config::raw::BootSpec::Ram {
            pattern: "frobnicate".into(),
        },
    );
    let report = validate(&raw).unwrap_err();
    assert!(
        report
            .errors
            .iter()
            .any(|e| matches!(e, ConfigError::InvalidValue { .. }))
    );

    let mut raw = base_config();
    raw.boot.insert(
        "ram0".into(),
        demu_config::raw::BootSpec::Hex { hex: "abc".into() },
    );
    let report = validate(&raw).unwrap_err();
    assert!(
        report
            .errors
            .iter()
            .any(|e| matches!(e, ConfigError::InvalidValue { .. }))
    );
}

#[test]
fn parse_reports_toml_errors() {
    let err = parse("this is [not toml");
    assert!(matches!(err, Err(ConfigError::TomlParse { .. })));
}

#[test]
fn config_round_trips_through_serialize() {
    let raw = base_config();
    let text = toml::to_string_pretty(&raw).expect("raw config serializes");
    let reparsed = RawConfig::parse(&text).expect("serialized config re-parses");
    assert_eq!(reparsed, raw);
}

#[test]
fn lint_layer_reports_unused_clock() {
    let mut raw = base_config();
    raw.clocks
        .insert("unused".into(), demu_config::raw::ClockSpec::new("1000"));
    let (config, warnings) = validate(&raw).expect("unused clock is a warning only");
    assert!(!config.clocks.is_empty());
    assert!(
        warnings
            .iter()
            .any(|w| w.code == "unused_clock" && w.message.contains("unused"))
    );
}

/// Serializing a resolved config is out of scope here; the helper
/// below only confirms that the resolved graph is self-consistent.
#[test]
fn resolved_graph_is_consistent() {
    let config: SystemConfig = smoke_config().expect("smoke resolves");
    for device in &config.devices {
        assert!(config.clocks.contains_key(&device.domain));
        assert!(config.devices_by_name.contains_key(&device.name));
        assert_eq!(
            config.devices_by_name[&device.name],
            config
                .devices
                .iter()
                .position(|d| d.name == device.name)
                .unwrap()
        );
    }
    for window in &config.windows {
        assert!(config.buses.iter().any(|b| b.name == window.bus));
        assert!(config.devices_by_name.contains_key(&window.device));
    }
}

#[test]
fn the_z80_triples_accept_their_kinds() {
    // The z80 family kinds validate against the z80 triples.
    for (triple, kind) in [
        ("z80-sega-sg1000", "z80.cpu"),
        ("z80-sega-mastersystem", "z80.cpu"),
        ("z80-sega-gamegear", "z80.cpu"),
        ("z80-sinclair-zx80", "z80.cpu"),
        ("z80-sinclair-zx81", "z80.cpu"),
        ("z80-sinclair-spectrum", "z80.cpu"),
        ("z80-ti-85", "z80.cpu"),
    ] {
        let mut raw = base_config();
        raw.system.as_mut().unwrap().triple = triple.into();
        raw.device[0].kind = kind.to_string();
        let (config, _) = validate(&raw).expect("the z80 kinds pass on their triples");
        assert_eq!(config.devices[0].kind.as_str(), kind);
    }
    // A z80 CPU kind stays wrong on a non-z80 triple.
    let mut raw = base_config();
    raw.system.as_mut().unwrap().triple = "sm83-nintendo-gameboy".into();
    raw.device[0].kind = "z80.cpu".to_string();
    assert!(validate(&raw).is_err());
}

#[test]
fn the_spectrum128_variant_extends_the_48k_machine() {
    // The variant resolves through the bundled selector and merges
    // the banked memory controller over the 48K base.
    let raw = resolve_source("z80-sinclair-spectrum128").expect("variant resolves");
    let (config, warnings) = validate(&raw).expect("the variant validates");
    assert!(warnings.is_empty(), "the variant raises no lints");
    assert_eq!(
        config.clocks["main"].frequency.to_string(),
        "3546895",
        "the PAL crystal replaced the 48K clock"
    );
    let ula = config
        .devices
        .iter()
        .find(|d| d.kind.as_str() == "zx.ula")
        .expect("the ULA survives the extend");
    assert_eq!(
        ula.params.get("line_t").and_then(|v| v.as_integer()),
        Some(228),
        "the 128K scanline length replaced the 48K form"
    );
    let door = config
        .devices
        .iter()
        .find(|d| d.kind.as_str() == "zx.bankram")
        .expect("the memory controller joined");
    assert_eq!(
        door.name.as_str(),
        "ram_a",
        "the controller replaced the plain RAM"
    );
    // The CPU contention window follows the published 128K
    // delay-table phase.
    let cpu = config
        .devices
        .iter()
        .find(|d| d.kind.as_str() == "z80.cpu")
        .expect("the cpu");
    assert_eq!(
        cpu.params.get("contend_base").and_then(|v| v.as_integer()),
        Some(14361),
        "the 128K contention base replaced the 48K base"
    );
}

#[test]
fn the_bundled_z80_configs_validate() {
    for triple in [
        "z80-sega-sg1000",
        "z80-sega-mastersystem",
        "z80-sega-gamegear",
        "z80-sinclair-zx80",
        "z80-sinclair-zx81",
        "z80-sinclair-spectrum",
        "z80-sinclair-spectrum128",
        "z80-ti-85",
    ] {
        let raw = resolve_source(triple).unwrap_or_else(|_| panic!("{triple} resolves"));
        let (config, warnings) = validate(&raw).unwrap_or_else(|_| panic!("{triple} validates"));
        let _ = config;
        assert!(warnings.is_empty(), "{triple} raises lints: {warnings:?}");
    }
}

#[test]
fn every_bundled_config_validates() {
    use demu_config::bundled::BUNDLED;
    for bundled in BUNDLED {
        let raw = demu_config::bundled::resolve_source(bundled.triple)
            .unwrap_or_else(|e| panic!("{} resolves: {e}", bundled.triple));
        demu_config::validate(&raw)
            .unwrap_or_else(|report| panic!("{} validates: {:?}", bundled.triple, report.errors));
    }
}
