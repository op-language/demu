//! The `demu` binary: CLI parsing and mode dispatch.

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

mod assemble;

use demu_shell::service::MachineService;
use demu_shell::{json, socket, text};

/// The usage text.
const USAGE: &str = "\
demu - a cycle-accurate debugging emulator

Usage:
  demu [OPTIONS] [ROM]
  demu config --check <file|triple>

Options:
  --config <triple|file>   system configuration
  --symbols <file.opl>     symbol table
  --dump <path>            memory dump at exit
  --sentinel <addr>:<val>  compare byte at exit, set the exit code
  --json                   JSON-lines protocol on stdio
  --serve <path>           Unix-domain-socket server mode
  -h, --help               show this help
";

/// One parsed CLI invocation.
struct Cli {
    config: Option<String>,
    symbols: Option<String>,
    dump: Option<String>,
    sentinel: Option<(u32, u8)>,
    json: bool,
    serve: Option<String>,
    rom: Option<String>,
    check: Option<String>,
}

fn parse_args(args: &[String]) -> Result<Cli, String> {
    // The subcommand form: `demu config --check <selector>`.
    if args.first().map(String::as_str) == Some("config") {
        let rest = &args[1..];
        if rest.first().map(String::as_str) != Some("--check") {
            return Err("usage: demu config --check <file|triple>".to_string());
        }
        let selector = rest
            .get(1)
            .ok_or("usage: demu config --check <file|triple>")?;
        return Ok(Cli {
            config: None,
            symbols: None,
            dump: None,
            sentinel: None,
            json: false,
            serve: None,
            rom: None,
            check: Some(selector.clone()),
        });
    }
    let mut cli = Cli {
        config: None,
        symbols: None,
        dump: None,
        sentinel: None,
        json: false,
        serve: None,
        rom: None,
        check: None,
    };
    let mut i = 0usize;
    let mut positional_seen = false;
    while i < args.len() {
        match args[i].as_str() {
            "--config" => {
                i += 1;
                cli.config = Some(args.get(i).ok_or("missing value for --config")?.clone());
            }
            "--symbols" => {
                i += 1;
                cli.symbols = Some(args.get(i).ok_or("missing value for --symbols")?.clone());
            }
            "--dump" => {
                i += 1;
                cli.dump = Some(args.get(i).ok_or("missing value for --dump")?.clone());
            }
            "--sentinel" => {
                i += 1;
                let raw = args.get(i).ok_or("missing value for --sentinel")?;
                cli.sentinel = Some(parse_sentinel(raw)?);
            }
            "--json" => cli.json = true,
            "--serve" => {
                i += 1;
                cli.serve = Some(args.get(i).ok_or("missing value for --serve")?.clone());
            }
            "-h" | "--help" => {
                print!("{USAGE}");
                return Err(String::new());
            }
            _ => {
                if positional_seen {
                    return Err(format!("unexpected argument: {}", args[i]));
                }
                positional_seen = true;
                cli.rom = Some(args[i].clone());
            }
        }
        i += 1;
    }
    Ok(cli)
}

fn parse_sentinel(raw: &str) -> Result<(u32, u8), String> {
    let (addr_text, value_text) = raw
        .split_once(':')
        .ok_or_else(|| format!("--sentinel expects <addr>:<value>, got {raw}"))?;
    let addr = demu_config::model::parse_number(addr_text)
        .ok_or_else(|| format!("sentinel address is not a number: {addr_text}"))?;
    let value = demu_config::model::parse_number(value_text)
        .ok_or_else(|| format!("sentinel value is not a number: {value_text}"))?;
    Ok((
        u32::try_from(addr).map_err(|_| format!("sentinel address {addr:#x} exceeds 32 bits"))?,
        u8::try_from(value).map_err(|_| format!("sentinel value {value} exceeds a byte"))?,
    ))
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cli = match parse_args(&args) {
        Ok(cli) => cli,
        Err(message) => {
            if message.is_empty() {
                // --help printed its text already.
                return ExitCode::SUCCESS;
            }
            eprintln!("error: {message}");
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };

    if let Some(selector) = &cli.check {
        return run_config_check(selector);
    }

    run_session(&cli)
}

/// The `config --check` mode: validate a config and report.
fn run_config_check(selector: &str) -> ExitCode {
    let raw = match demu_config::bundled::resolve_source(selector) {
        Ok(raw) => raw,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };
    match demu_config::validate(&raw) {
        Ok((config, warnings)) => {
            for w in &warnings {
                println!("warning: {} - {}", w.code, w.message);
            }
            println!(
                "ok: {} ({}) devices={} clocks={}",
                config.name,
                config.triple,
                config.devices.len(),
                config.clocks.len()
            );
            ExitCode::SUCCESS
        }
        Err(report) => {
            for e in &report.errors {
                eprintln!("error: {e}");
            }
            for w in &report.warnings {
                eprintln!("warning: {} - {}", w.code, w.message);
            }
            ExitCode::from(2)
        }
    }
}

/// The interactive or machine session.
fn run_session(cli: &Cli) -> ExitCode {
    let selector = cli.config.as_deref().unwrap_or("smoke");
    let raw = match demu_config::bundled::resolve_source(selector) {
        Ok(raw) => raw,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };
    let (config, warnings) = match demu_config::validate(&raw) {
        Ok(pair) => pair,
        Err(report) => {
            for e in &report.errors {
                eprintln!("error: {e}");
            }
            return ExitCode::from(2);
        }
    };
    let _ = warnings;
    let mut service = MachineService::new();
    match demu_core::Machine::assemble(config, &assemble::demu_ctors()) {
        Ok(machine) => service.load_machine(machine),
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    }
    // The positional ROM path overrides the cartridge boot state.
    let cart_name = {
        let machine = service.machine().expect("machine is loaded");
        machine
            .config
            .devices
            .iter()
            .find(|d| d.kind.as_str() == assemble::CART_KIND)
            .map(|d| d.name.to_string())
    };
    match (&cli.rom, cart_name) {
        (Some(rom), Some(name)) => {
            let machine = service.machine_mut().expect("machine was loaded");
            if let Err(e) = machine.override_boot(
                &name,
                demu_config::model::BootState::Rom { path: rom.clone() },
            ) {
                eprintln!("error: {e}");
                return ExitCode::from(2);
            }
            machine.boot();
        }
        (Some(_), None) => {
            // A ROM path with no cartridge device in the configuration.
            eprintln!("error: the configuration declares no cartridge device");
            return ExitCode::from(2);
        }
        (None, _) => {
            service.machine_mut().expect("machine was loaded").boot();
        }
    }
    if let Some(path) = &cli.symbols {
        match std::fs::read_to_string(path) {
            Ok(text) => match demu_symbols::SymbolTable::parse(&text) {
                Ok(table) => service.load_symbols(&table),
                Err(e) => {
                    eprintln!("error: {e}");
                    return ExitCode::from(2);
                }
            },
            Err(e) => {
                eprintln!("error: failed to read {path}: {e}");
                return ExitCode::from(2);
            }
        }
    }
    service.dump_path = cli.dump.clone().map(PathBuf::from);
    service.sentinel = cli.sentinel;

    if let Some(socket_path) = &cli.serve {
        let path = PathBuf::from(socket_path);
        if let Err(e) = socket::serve(&mut service, &path) {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
        return ExitCode::SUCCESS;
    }

    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut input = stdin.lock();
    let mut output = stdout.lock();
    let result = if cli.json {
        json::run(&mut service, &mut input, &mut output)
    } else {
        text::run(&mut service, &mut input, &mut output)
    };
    if let Err(e) = result {
        eprintln!("error: {e}");
        return ExitCode::from(2);
    }
    finish_session(&service)
}

/// Write the dump, check the sentinel, and decide the exit code.
fn finish_session(service: &MachineService) -> ExitCode {
    match service.write_dump() {
        Ok(None) => {}
        Ok(Some((addr, value))) => {
            if let Some((_, expected)) = service.sentinel {
                if value == expected {
                    writeln!(
                        std::io::stderr(),
                        "sentinel {addr:#x} = {value:#x} matches {expected:#x}"
                    )
                    .ok();
                } else {
                    writeln!(
                        std::io::stderr(),
                        "sentinel {addr:#x} = {value:#x} does not match {expected:#x}"
                    )
                    .ok();
                    return ExitCode::FAILURE;
                }
            }
        }
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
pub mod tests {
    use super::*;

    fn machine_for(triple: &str) -> demu_core::Machine {
        let raw = demu_config::bundled::resolve_source(triple).expect("bundled config");
        let (config, _) = demu_config::validate(&raw).expect("config is valid");
        demu_core::Machine::assemble(config, &assemble::demu_ctors()).expect("machine assembles")
    }

    fn demo_rom() -> Vec<u8> {
        let path =
            "/home/dave/Projects/op-language/dmg-demo/target/sm83-nintendo-gameboy/dmg-demo.gb";
        if let Ok(data) = std::fs::read(path)
            && data.len() >= 0x150
        {
            return data;
        }
        // A minimal fallback cart: jump to a halt loop.
        let mut rom = vec![0u8; 0x8000];
        rom[0x100..0x108].copy_from_slice(&[0x00, 0xC3, 0x50, 0x01, 0x00, 0x00, 0x00, 0x00]);
        rom[0x150..0x152].copy_from_slice(&[0x76, 0x76]);
        rom[0x14D] = 0; // header checksum not validated by the loader
        rom
    }

    pub fn service_with_rom() -> MachineService {
        let mut service = MachineService::new();
        service.load_machine(
            demu_core::Machine::assemble(config_for_gameboy(), &crate::assemble::demu_ctors())
                .expect("machine assembles"),
        );
        let machine = service.machine_mut().expect("loaded");
        if let Err(e) = machine.override_boot(
            "cart",
            demu_config::model::BootState::Hex { bytes: demo_rom() },
        ) {
            panic!("boot override failed: {e}");
        }
        machine.boot();
        service
    }

    pub fn config_for_gameboy() -> demu_config::model::SystemConfig {
        let raw =
            demu_config::bundled::resolve_source("sm83-nintendo-gameboy").expect("bundled config");
        let (config, _) = demu_config::validate(&raw).expect("config is valid");
        config
    }

    #[test]
    fn bundled_configs_load() {
        let machine = machine_for("sm83-nintendo-gameboy");
        assert_eq!(machine.config.name, "Nintendo Game Boy");
        let machine = machine_for("sm83-nintendo-gameboy-color");
        assert_eq!(machine.config.triple, "sm83-nintendo-gameboy-color");
    }

    #[test]
    fn machine_boots_and_runs() {
        let mut service = service_with_rom();
        service.run(300_000).expect("run works");
        let machine = service.machine().expect("machine");
        let cpu = machine
            .device_by_name("cpu0")
            .and_then(|d| d.cpu())
            .expect("cpu exposes registers");
        let regs = cpu.registers();
        // The boot ROM ran past its setup by this point.
        assert!(regs["SP"] == 0xFFFE, "the boot stack pointer is in place");
    }

    #[test]
    fn frames_count_across_runs() {
        let mut service = service_with_rom();
        service.run(600_000).expect("run works");
        let before = frames_of(&service);
        service.run(300_000).expect("run works");
        let after = frames_of(&service);
        assert!(after > before, "display frames advance across run commands");
    }

    fn frames_of(service: &MachineService) -> u64 {
        let machine = service.machine().expect("machine");
        machine
            .devices
            .iter()
            .filter_map(|d| d.video())
            .map(|v| v.frames())
            .next()
            .unwrap_or(0)
    }
}

#[cfg(test)]
pub mod state_tests {
    use crate::tests::{config_for_gameboy, service_with_rom};

    #[test]
    fn save_and_restore_round_trips_machine_state() {
        let mut service = service_with_rom();
        service.run(300_000).expect("run works");
        let mut buf = Vec::new();
        let machine = service.machine_mut().expect("machine");
        machine.save(&mut buf).expect("save works");

        let mut restored =
            demu_core::Machine::assemble(config_for_gameboy(), &crate::assemble::demu_ctors())
                .expect("machine assembles");
        restored.load(&buf).expect("restore works");
        let mut buf2 = Vec::new();
        restored.save(&mut buf2).expect("save works");
        if buf.len() != buf2.len() {
            panic!(
                "chunk lengths differ: {len1} vs {len2}",
                len1 = buf.len(),
                len2 = buf2.len()
            );
        }
        let diff = buf
            .iter()
            .zip(&buf2)
            .enumerate()
            .find(|(_, (a, b))| a != b)
            .map(|(i, (a, b))| format!("first diff at {i}: {a:#04x} vs {b:#04x}"));
        assert!(
            diff.is_none(),
            "save after restore is byte-identical: {diff:?}",
        );
        assert_eq!(restored.now(), machine.now());
        assert_eq!(restored.now(), machine.now());
    }

    #[test]
    fn bundled_gameboy_configs_validate() {
        for triple in ["sm83-nintendo-gameboy", "sm83-nintendo-gameboy-color"] {
            let raw = demu_config::bundled::resolve_source(triple).expect("bundled config");
            demu_config::validate(&raw).expect("config is valid");
        }
    }

    #[test]
    fn restore_mid_boot_reproduces_registers() {
        let mut service = service_with_rom();
        service.run(150_000).expect("run works");
        let machine = service.machine_mut().expect("machine");
        let pc_before = machine
            .device_by_name("cpu0")
            .and_then(|d| d.cpu())
            .and_then(|c| c.registers().get("PC").copied())
            .expect("cpu");
        let mut buf = Vec::new();
        machine.save(&mut buf).expect("save works");
        machine.load(&buf).expect("restore works");
        let pc_after = machine
            .device_by_name("cpu0")
            .and_then(|d| d.cpu())
            .and_then(|c| c.registers().get("PC").copied())
            .expect("cpu");
        assert_eq!(pc_before, pc_after, "PC survives the round trip");
    }
}
