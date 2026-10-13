//! The `demu` binary: CLI parsing and mode dispatch.

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

mod assemble;
mod daemon;

use demu_shell::service::MachineService;
use demu_shell::{json, socket, text};

/// The usage text.
const USAGE: &str = "\
demu - a cycle-accurate debugging emulator

Usage:
  demu [OPTIONS] [ROM]
  demu config --check <file|triple>
  demu daemon [--http <addr:port>] [--session-limit <n>] [--data-root <dir>]
              [--allow-host <name>[,...]]

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

/// One CLI routing decision.
enum Route<'a> {
    /// The daemon subcommand: it parses its own options.
    Daemon(&'a [String]),
    /// The generic parser handles the rest.
    Session(&'a [String]),
}

/// Route the `daemon` subcommand before the generic parser, so the
/// daemon options (`--http`, `--session-limit`, `--data-root`) never
/// reach the ROM parser.
fn route(args: &[String]) -> Route<'_> {
    if args.first().map(String::as_str) == Some("daemon") {
        Route::Daemon(&args[1..])
    } else {
        Route::Session(args)
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match route(&args) {
        Route::Daemon(rest) => daemon::run(rest),
        Route::Session(all) => run_mode(all),
    }
}

/// The generic CLI mode: `config --check` or the one-machine session.
fn run_mode(args: &[String]) -> ExitCode {
    let cli = match parse_args(args) {
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
    // The session machine assembles from this same source; the media
    // commands tag files with the configuration text.
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
    let raw_text = match raw.to_toml_text() {
        Ok(text) => text,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };
    service.set_config_source(raw_text);
    service.set_machine_factory(std::sync::Arc::new(|config| {
        let mut machine = demu_core::Machine::assemble(config.clone(), &assemble::demu_ctors())?;
        machine.boot();
        assemble::apply_nes_cartridge_wiring(&mut machine);
        Ok(machine)
    }));
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
            .find(|d| crate::assemble::is_cart_kind(d.kind.as_str()))
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
            assemble::apply_nes_cartridge_wiring(machine);
        }
        (Some(_), None) => {
            // A ROM path with no cartridge device in the configuration.
            eprintln!("error: the configuration declares no cartridge device");
            return ExitCode::from(2);
        }
        (None, _) => {
            let machine = service.machine_mut().expect("machine was loaded");
            machine.boot();
            assemble::apply_nes_cartridge_wiring(machine);
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
mod cli_tests {
    use super::*;

    #[test]
    fn the_daemon_subcommand_routes_to_the_daemon_parser() {
        assert!(matches!(
            route(&[
                "daemon".to_owned(),
                "--http".to_owned(),
                "127.0.0.1:6502".to_owned()
            ]),
            Route::Daemon(_)
        ));
        assert!(matches!(route(&["boot.rom".to_owned()]), Route::Session(_)));
    }

    #[test]
    fn the_daemon_options_reach_the_daemon_parser() {
        // Through the dispatch: the daemon parser owns `--session-limit`
        // and rejects a zero cap with exit 2, so the option arrived
        // instead of dying in the generic parser as an unexpected
        // argument.
        let code = match route(&[
            "daemon".to_owned(),
            "--session-limit".to_owned(),
            "0".to_owned(),
        ]) {
            Route::Daemon(rest) => daemon::run(rest),
            Route::Session(_) => panic!("the daemon subcommand must route to the daemon"),
        };
        assert_eq!(code, ExitCode::from(2));
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use demu_core::Tick;
    use demu_shell::text::CommandOutputText as _;

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
        assemble::apply_nes_cartridge_wiring(machine);
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
    /// The script text for the media tests: four joypad events at exact
    /// ticks of the Game Boy's single domain.
    const GB_SCRIPT: &str = "\
# a scripted play window
#main:100000 joypad press a
#main:200000 joypad release a
#main:300000 joypad press b   # holds through the window
#main:500000 joypad release b
";

    /// A temp path cleared of any prior test artifact.
    fn temp_media(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("demu-media-{name}"));
        std::fs::remove_file(&path).ok();
        path
    }

    /// A booted Game Boy on the configuration's own boot state.
    fn gb_machine() -> demu_core::Machine {
        let raw = demu_config::bundled::resolve_source("sm83-nintendo-gameboy").expect("config");
        let (config, _) = demu_config::validate(&raw).expect("valid");
        let mut machine =
            demu_core::Machine::assemble(config, &assemble::demu_ctors()).expect("assembles");
        machine.boot();
        assemble::apply_nes_cartridge_wiring(&mut machine);
        machine
    }

    #[test]
    fn a_rejected_input_event_fails_loudly() {
        // The script grammar validates the device and the domain; the
        // event text itself fails at injection time, mid-run.
        let mut service = service_with_rom();
        service
            .load_script("#main:1000 joypad press zzz\n")
            .expect("the script loads");
        let error = service
            .step(200_000)
            .expect_err("the unknown event text rejects");
        assert!(
            error.to_string().contains("rejected"),
            "the error names the rejection: {error}"
        );
    }

    #[test]
    fn mkv_input_carries_events_and_the_config() {
        let mut service = service_with_rom();
        let raw =
            demu_config::bundled::resolve_source("sm83-nintendo-gameboy").expect("raw config");
        service.set_config_source(raw.to_toml_text().expect("render"));
        let script = temp_media("input-script.txt");
        std::fs::write(&script, GB_SCRIPT).expect("script writes");
        let mkv = temp_media("input.mkv");
        demu_shell::text::execute(
            &mut service,
            "mkv-input",
            &format!("{} -o {}", script.display(), mkv.display()),
        )
        .expect("mkv-input");

        let read = demu_mkv::read_input_file(&mkv).expect("read back");
        assert_eq!(read.events.len(), 4, "the event blocks survive");
        assert_eq!(read.events[0].domain, "main");
        assert_eq!(read.events[0].device, "joypad");
        assert_eq!(read.events[0].event, "press a");
        assert_eq!(read.events[3].ticks, 500_000);
        // The embedded configuration reconstructs the machine.
        let raw_text = read.config_text.as_deref().expect("config tag");
        let raw_copy = demu_config::RawConfig::parse(raw_text).expect("parses");
        let (config, _) = demu_config::validate(&raw_copy).expect("validates");
        assert_eq!(config.triple, "sm83-nintendo-gameboy");
    }

    #[test]
    fn the_mkv_replay_reproduces_the_script_session_byte_identically() {
        // A: the script drives the machine directly.
        let mut machine_a = gb_machine();
        let config = machine_a.config.clone();
        let script = demu_core::script::InputScript::parse(GB_SCRIPT, &config)
            .expect("the script parses and validates");
        let mut scheduler_a = demu_core::script::InputScheduler::new(script);
        demu_core::script::run_until_with_input(
            &mut machine_a,
            &mut scheduler_a,
            Tick(600_000),
            &mut |_| false,
            true,
        )
        .expect("script session runs");
        let mut state_a = Vec::new();
        machine_a.save(&mut state_a).expect("save a");

        // The script converts to Matroska and comes back.
        let mkv = temp_media("replay.mkv");
        let config_text = demu_config::bundled::resolve_source("sm83-nintendo-gameboy")
            .expect("raw config")
            .to_toml_text()
            .expect("render");
        demu_mkv::write_input_file(&mkv, &config, &config_text, &{
            let script =
                demu_core::script::InputScript::parse(GB_SCRIPT, &config).expect("reparse");
            script.events().to_vec()
        })
        .expect("write input file");
        let source = demu_mkv::read_input_file(&mkv).expect("read back");
        let raw_text = source.config_text.as_deref().expect("config text");
        let raw_copy = demu_config::RawConfig::parse(raw_text).expect("the config text parses");
        let (config_b, _) = demu_config::validate(&raw_copy).expect("validates");
        let mut machine_b =
            demu_core::Machine::assemble(config_b, &assemble::demu_ctors()).expect("assembles");
        machine_b.boot();
        assemble::apply_nes_cartridge_wiring(&mut machine_b);
        let schedule = demu_core::script::InputScript::from_events(source.events);
        let mut scheduler_b = demu_core::script::InputScheduler::new(schedule);
        demu_core::script::run_until_with_input(
            &mut machine_b,
            &mut scheduler_b,
            Tick(600_000),
            &mut |_| false,
            true,
        )
        .expect("replay runs");
        let mut state_b = Vec::new();
        machine_b.save(&mut state_b).expect("save b");
        assert_eq!(
            state_a, state_b,
            "the Matroska replay reproduces the script session byte-identically"
        );
    }

    #[test]
    fn render_session_records_deterministic_aligned_frames() {
        let raw =
            demu_config::bundled::resolve_source("sm83-nintendo-gameboy").expect("raw config");
        let config_text = raw.to_toml_text().expect("render");

        let first = temp_media("render-a.mkv");
        let mut service = service_with_rom();
        service.set_config_source(config_text.clone());
        demu_shell::text::execute(&mut service, "render-session", "3 -o")
            .expect_err("the output file is required");
        demu_shell::text::execute(
            &mut service,
            "render-session",
            &format!("3 -o {}", first.display()),
        )
        .expect("render-session runs");

        let second = temp_media("render-b.mkv");
        let mut service_b = service_with_rom();
        service_b.set_config_source(config_text);
        demu_shell::text::execute(
            &mut service_b,
            "render-session",
            &format!("3 -o {}", second.display()),
        )
        .expect("render-session runs again");
        let bytes_a = std::fs::read(&first).expect("read a");
        let bytes_b = std::fs::read(&second).expect("read b");
        assert_eq!(
            bytes_a, bytes_b,
            "identical sessions record identical files"
        );

        // Three clusters, one per emulated display frame, with strictly
        // increasing nanosecond timestamps and a frame-period spacing.
        let clusters = demu_mkv::read_clusters(&first).expect("read clusters");
        assert_eq!(clusters.len(), 3, "one cluster per display frame");
        assert!(clusters[0].timestamp_ns > 0, "the first frame completed");
        for window in clusters.windows(2) {
            assert!(window[0].timestamp_ns < window[1].timestamp_ns);
            let delta = window[1].timestamp_ns - window[0].timestamp_ns;
            assert!(
                (10_000_000..=100_000_000).contains(&delta),
                "frame spacing {delta} ns"
            );
        }

        // Each video block decodes losslessly to the frame the equal
        // replay captures at that vblank: one frame per emulated frame.
        let mut replay = service_with_rom();
        for (index, cluster) in clusters.iter().enumerate() {
            replay.run_frame(1).expect("replay frame");
            let captured = replay
                .machine()
                .expect("machine")
                .devices
                .iter()
                .find_map(|d| d.video())
                .and_then(|v| v.frame())
                .expect("frame")
                .clone();
            let video_block = cluster
                .blocks
                .iter()
                .find(|(track, _)| *track == 2)
                .map(|(_, payload)| payload)
                .expect("video block");
            let decoded =
                demu_mkv::ffv1::decode_frame(captured.width, captured.height, video_block)
                    .expect("video decodes");
            assert_eq!(
                decoded, captured.pixels,
                "cluster {index} decodes to the emulated frame"
            );
        }
    }

    #[test]
    fn mkv_render_rebuilds_the_machine_and_replays_the_events() {
        // The input form carries the configuration and the events.
        let mkv = temp_media("render-input.mkv");
        {
            let config = gb_machine().config.clone();
            let script =
                demu_core::script::InputScript::parse(GB_SCRIPT, &config).expect("script parses");
            let config_text = demu_config::bundled::resolve_source("sm83-nintendo-gameboy")
                .expect("raw config")
                .to_toml_text()
                .expect("render");
            demu_mkv::write_input_file(&mkv, &config, &config_text, script.events())
                .expect("writes");
        }

        // The command rebuilds the machine from the file and replays.
        let mut service = MachineService::new();
        let raw = demu_config::bundled::resolve_source("sm83-nintendo-gameboy").expect("raw");
        service.set_config_source(raw.to_toml_text().expect("render"));
        service.set_machine_factory(std::sync::Arc::new(|config| {
            let mut machine =
                demu_core::Machine::assemble(config.clone(), &assemble::demu_ctors())?;
            machine.boot();
            assemble::apply_nes_cartridge_wiring(&mut machine);
            Ok(machine)
        }));
        let output = temp_media("render-output.mkv");
        // The four events span 500_000 ticks; at 16.74 ms a frame, 20
        // emulated display frames cover the window.
        let render = demu_shell::text::execute(
            &mut service,
            "mkv-render",
            &format!("{} 20 -o {}", mkv.display(), output.display()),
        )
        .expect("mkv-render runs");
        assert!(render.text().contains("20"));

        // The replayed state matches a direct script session at the
        // same vblank count. The twin repeats the recording loop
        // without the writer: one display frame per round, events at
        // their exact instants.
        let mut twin = gb_machine();
        {
            let config = twin.config.clone();
            let script =
                demu_core::script::InputScript::parse(GB_SCRIPT, &config).expect("script parses");
            let mut scheduler = demu_core::script::InputScheduler::new(script);
            for _ in 0..20 {
                let mut vblanks = 0u64;
                let mut hook = |event: &demu_core::TriggerEvent| {
                    if matches!(event, demu_core::TriggerEvent::Vblank) {
                        vblanks += 1;
                    }
                    vblanks >= 1
                };
                if scheduler.pending() == 0 {
                    twin.run_hook(Tick(u64::MAX), &mut hook, true);
                } else {
                    demu_core::script::run_until_with_input(
                        &mut twin,
                        &mut scheduler,
                        Tick(u64::MAX),
                        &mut hook,
                        true,
                    )
                    .expect("segment");
                }
            }
        }

        // The rendered machine and the twin agree.
        let rendered = service.machine().expect("machine");
        let mut rendered_state = Vec::new();
        rendered.save(&mut rendered_state).expect("save rendered");
        let mut twin_state = Vec::new();
        twin.save(&mut twin_state).expect("save twin");
        assert_eq!(
            rendered_state, twin_state,
            "the rendered replay matches the direct script session"
        );

        // The output carries the three tracks with the same cluster
        // count and strictly increasing timestamps.
        let clusters = demu_mkv::read_clusters(&output).expect("read clusters");
        assert_eq!(clusters.len(), 20);
        assert!(
            clusters
                .windows(2)
                .all(|w| w[0].timestamp_ns < w[1].timestamp_ns)
        );
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

#[cfg(test)]
pub mod mos_exec_tests {
    use crate::assemble;
    use demu_dev_video::NesPpu;
    use demu_shell::MachineService;

    /// Boot the NTSC machine with a hex cartridge image whose
    /// program runs at $C000 (the reset vector $C000).
    fn nes_with(program: &[u8]) -> MachineService {
        let raw =
            demu_config::bundled::resolve_source("rp2A03-nintendo-nes-ntsc").expect("bundled");
        let (config, _) = demu_config::validate(&raw).expect("validate");
        let mut service = MachineService::new();
        service.load_machine(
            demu_core::Machine::assemble(config, &assemble::demu_ctors()).expect("assemble"),
        );
        // An iNES image: the header, the 16-KiB program bank with the
        // program at $C000, the reset vector at the bank's end, and
        // the 8-KiB character bank.
        let mut image = vec![0u8; 16 + 16384 + 8192];
        image[0..4].copy_from_slice(b"NES\x1a");
        image[4] = 1;
        image[5] = 1;
        image[16..16 + program.len()].copy_from_slice(program);
        // The reset vector names the program's first byte at $C000.
        image[16 + 0x3FFC] = 0x00;
        image[16 + 0x3FFD] = 0xC0;
        let machine = service.machine_mut().expect("loaded");
        machine
            .override_boot("cart", demu_config::model::BootState::Hex { bytes: image })
            .expect("boot");
        machine.boot();
        assemble::apply_nes_cartridge_wiring(machine);
        service
    }

    fn run(service: &mut MachineService, cpu_ticks: u64) {
        let start = service.machine().expect("m").now().0;
        service.run(start + cpu_ticks * 12).expect("run");
    }

    fn cpu_reg(service: &mut MachineService, name: &str) -> u64 {
        let machine = service.machine().expect("m");
        machine
            .device_by_name("cpu0")
            .and_then(|d| d.cpu())
            .and_then(|c| c.registers().get(name).copied())
            .expect("cpu")
    }

    #[test]
    fn lda_immediate_and_cycles() {
        // SEI, TXS, LDA #$5A, then NOPs.
        let mut service = nes_with(&[0x78, 0x9A, 0xA9, 0x5A, 0xEA, 0xEA]);
        run(&mut service, 30);
        assert_eq!(cpu_reg(&mut service, "A"), 0x5A);
    }

    #[test]
    fn sta_absolute_hits_ppu_mask() {
        // STA $2001: the write reaches the PPU's mask register.
        let mut service = nes_with(&[0xA9, 0x1E, 0x8D, 0x01, 0x20, 0xEA]);
        run(&mut service, 30);
        let machine = service.machine().expect("m");
        let mask = machine
            .device_by_name("ppu")
            .and_then(|d| d.as_any().downcast_ref::<NesPpu>())
            .map(|ppu| ppu.nes_mask())
            .expect("ppu");
        assert_eq!(mask, 0x1E);
    }

    #[test]
    fn adc_binary_and_decimal() {
        // CLD, LDA #$50, ADC #$50: the binary sum 0xA0 with the carry.
        let mut service = nes_with(&[
            0xD8, // CLD
            0xA9, 0x50, // LDA #$50
            0x69, 0x50, // ADC #$50
            0xEA, 0xEA,
        ]);
        run(&mut service, 30);
        assert_eq!(cpu_reg(&mut service, "A"), 0xA0);
        assert_eq!(cpu_reg(&mut service, "P") & 0x01, 0x00);

        // The decimal form: 0x50 + 0x50 = 0x00 with the carry set.
        let mut service = nes_with(&[
            0xF8, // SED
            0xA9, 0x50, 0x69, 0x50, 0xEA, 0xEA,
        ]);
        run(&mut service, 30);
        assert_eq!(cpu_reg(&mut service, "A"), 0x00);
        assert_eq!(cpu_reg(&mut service, "P") & 0x01, 0x01);
    }

    #[test]
    fn sbc_decimal_borrow_chain() {
        // SED; LDA #$50; SEC; SBC #$50 = 0x00 with the carry.
        let mut service = nes_with(&[0xF8, 0xA9, 0x50, 0x38, 0xE9, 0x50, 0xEA, 0xEA]);
        run(&mut service, 30);
        assert_eq!(cpu_reg(&mut service, "A"), 0x00);
        assert_eq!(cpu_reg(&mut service, "P") & 0x01, 0x01);

        // SED; LDA #$00; SEC; SBC #$01 = 0x99 with the carry set.
        let mut service = nes_with(&[0xF8, 0xA9, 0x00, 0x38, 0xE9, 0x01, 0xEA, 0xEA]);
        run(&mut service, 30);
        assert_eq!(cpu_reg(&mut service, "A"), 0x99);
        assert_eq!(cpu_reg(&mut service, "P") & 0x01, 0x00);
    }

    #[test]
    fn rti_six_cycles_and_rts_six() {
        // The RTS: the JSR and the return complete in 12 cycles.
        let mut service = nes_with(&[0x20, 0x05, 0xC0, 0xEA, 0xEA, 0x60, 0xEA]);
        run(&mut service, 20);
        // The flow: the JSR at $C000, the RTS at $C005, the loop at
        // $C000: the cycle count stays in the window either way.
        let pc = service
            .machine()
            .and_then(|m| m.device_by_name("cpu0"))
            .and_then(|d| d.cpu())
            .and_then(|c| c.registers().get("PC").copied())
            .expect("cpu");
        assert!((0xC000..=0xC004).contains(&pc), "pc={pc:#06x}");
    }
}

#[cfg(test)]
pub mod dma_tests {
    use crate::assemble;
    use demu_dev_video::NesPpu;
    use demu_shell::MachineService;

    #[test]
    fn oam_dma_copies_page_to_oam() {
        // STA $2007 × 8 with the test bytes at $200, then the OAM DMA
        // from the $0200 page: the object memory carries the copies.
        let raw =
            demu_config::bundled::resolve_source("rp2A03-nintendo-nes-ntsc").expect("bundled");
        let (config, _) = demu_config::validate(&raw).expect("validate");
        let mut service = MachineService::new();
        service.load_machine(
            demu_core::Machine::assemble(config, &assemble::demu_ctors()).expect("assemble"),
        );
        {
            let machine = service.machine_mut().expect("loaded");
            machine.boot();
            assemble::apply_nes_cartridge_wiring(machine);
        }
        // Load $0200..$0208 through the debug writes.
        for (i, value) in [0x41u8, 0x42, 0x43, 0x44, 0x00, 0x55, 0x66, 0x77]
            .iter()
            .enumerate()
        {
            assert!(machine_ref_debug_write(
                &mut service,
                0x200 + i as u32,
                *value
            ));
        }
        // The transfer start: $4014 = $02.
        assert!(machine_ref_debug_write(&mut service, 0x4014, 0x02));
        // The transfer = ~513 cycles plus the alignment.
        service.run(700 * 12).expect("run");
        let machine = service.machine().expect("m");
        let oam = machine
            .devices
            .iter()
            .find_map(|d| {
                d.as_any()
                    .downcast_ref::<NesPpu>()
                    .map(|ppu| ppu.oam_ram()[0..8].to_vec())
            })
            .expect("ppu");
        assert_eq!(oam, vec![0x41, 0x42, 0x43, 0x44, 0x00, 0x55, 0x66, 0x77]);
    }

    fn machine_ref_debug_write(service: &mut MachineService, addr: u32, value: u8) -> bool {
        service
            .machine_mut()
            .is_some_and(|m| m.debug_write("cpu", demu_core::Address(addr), value))
    }
}

#[cfg(test)]
pub mod nes_machine_tests {
    use crate::assemble;
    use demu_core::Tick;
    use demu_core::machine::Machine;

    /// Assemble and boot one machine from a bundled triple with the
    /// demo ROM in the cartridge, or no image at all.
    fn machine_for(triple: &str, rom: Option<&[u8]>) -> Machine {
        let raw = demu_config::bundled::resolve_source(triple).expect("bundled config");
        let (config, _) = demu_config::validate(&raw).expect("validate");
        let mut machine = Machine::assemble(config, &assemble::demu_ctors()).expect("assemble");
        if let Some(bytes) = rom {
            machine
                .override_boot(
                    "cart",
                    demu_config::model::BootState::Hex {
                        bytes: bytes.to_vec(),
                    },
                )
                .expect("boot");
        }
        machine.boot();
        assemble::apply_nes_cartridge_wiring(&mut machine);
        machine
    }

    /// A minimal NROM image: the vectors and a boot loop at $C000.
    fn demo_rom() -> Vec<u8> {
        let mut rom = vec![0u8; 16 + 16384 + 8192];
        rom[0..4].copy_from_slice(b"NES\x1a");
        rom[4] = 1; // One 16-KiB program bank.
        rom[5] = 1; // One 8-KiB character bank.
        rom[16] = 0x78; // SEI at $C000
        rom[17] = 0x9A; // TXS
        rom[18] = 0x4C; // JMP $C000
        rom[19] = 0x00;
        rom[20] = 0xC0;
        rom[16 + 0x3FFC] = 0x00; // The reset vector's low byte, $FFFC = $C000
        rom[16 + 0x3FFD] = 0xC0;
        rom
    }

    #[test]
    fn nes_configs_boot_and_validate() {
        for triple in ["rp2A03-nintendo-nes-ntsc", "rp2A07-nintendo-nes-pal"] {
            let machine = machine_for(triple, None);
            assert_eq!(machine.config.clocks.len(), 1);
            assert_eq!(machine.devices.len(), 7);
        }
    }

    #[test]
    fn nescpu_reads_reset_vector_through_cart() {
        let mut machine = machine_for("rp2A03-nintendo-nes-ntsc", Some(&demo_rom()));
        machine.run_until(Tick(1200));
        let machine_ref = &machine;
        let pc = machine_ref
            .device_by_name("cpu0")
            .and_then(|d| d.cpu())
            .and_then(|c| c.registers().get("PC").copied())
            .expect("cpu");
        // The core jumped through the vector to $C000 and sits in the
        // loop back at the reset code.
        assert!((0xC000..=0xC002).contains(&pc), "pc={pc:#06x}");
    }

    #[test]
    fn nes_runs_deterministically() {
        let mut machine = machine_for("rp2A03-nintendo-nes-ntsc", Some(&demo_rom()));
        let mut twin = machine_for("rp2A03-nintendo-nes-ntsc", Some(&demo_rom()));
        machine.run_until(Tick(600_000));
        twin.run_until(Tick(600_000));
        let mut state = Vec::new();
        machine.save(&mut state).expect("save");
        let mut twin_state = Vec::new();
        twin.save(&mut twin_state).expect("save");
        assert_eq!(state, twin_state, "the runs produce identical state");
    }

    #[test]
    fn save_and_restore_reproduces_nes_state() {
        let mut machine = machine_for("rp2A03-nintendo-nes-ntsc", Some(&demo_rom()));
        machine.run_until(Tick(400_000));
        let mut buf = Vec::new();
        machine.save(&mut buf).expect("save");

        let mut restored = machine_for("rp2A03-nintendo-nes-ntsc", Some(&demo_rom()));
        restored.load(&buf).expect("load");
        let mut buf2 = Vec::new();
        restored.save(&mut buf2).expect("save");
        assert_eq!(buf, buf2, "save after restore is byte-identical");
        assert_eq!(restored.now(), machine.now());
        // The restored machine continues deterministically.
        // A second machine restored from the same save must continue
        // identically to the first restore: device state, frame
        // timing, and later saves all stay byte-equal. (A machine
        // that runs on without a restore is not a comparable twin:
        // re-entered runs re-seed its queue from each device's cached
        // next event, which bumps scheduler bookkeeping.)
        let mut twin = machine_for("rp2A03-nintendo-nes-ntsc", Some(&demo_rom()));
        twin.load(&buf).expect("load twin");
        restored.run_until(Tick(600_000));
        twin.run_until(Tick(600_000));
        let mut check = Vec::new();
        restored.save(&mut check).expect("save");
        let mut check2 = Vec::new();
        twin.save(&mut check2).expect("save");
        assert_eq!(check, check2, "the two restores continue identically");
    }

    #[test]
    fn pal_machine_runs_the_same_flow() {
        let mut machine = machine_for("rp2A07-nintendo-nes-pal", Some(&demo_rom()));
        machine.run_until(Tick(600_000));
        let regs = machine
            .device_by_name("cpu0")
            .and_then(|d| d.cpu())
            .expect("cpu")
            .registers();
        assert!(
            (0xC000..=0xC002).contains(&regs["PC"]),
            "the PAL core loops in the boot code"
        );
    }

    #[test]
    fn alias_config_selects_the_mos6502_cpu() {
        let raw = demu_config::bundled::resolve_source("mos6502-nintendo-nes-pal").expect("alias");
        let (config, _) = demu_config::validate(&raw).expect("validate");
        let cpu = config
            .devices
            .iter()
            .find(|d| d.kind.as_str().ends_with(".cpu"))
            .expect("cpu0");
        assert_eq!(cpu.kind.as_str(), "mos6502.cpu");
    }
}

#[cfg(test)]
pub mod atari_machine_tests {
    use crate::assemble;
    use demu_core::machine::Machine;
    use demu_core::{Device as _, Tick};

    /// Assemble one machine from a bundled triple.
    fn machine_for(triple: &str) -> Machine {
        let raw = demu_config::bundled::resolve_source(triple).expect("bundled config");
        let (config, _) = demu_config::validate(&raw).expect("validate");
        Machine::assemble(config, &assemble::demu_ctors()).expect("assemble")
    }

    /// Boot one machine with one synthetic image on the cart and, when
    /// present, a synthetic BIOS image.
    pub(super) fn booted(triple: &str, cart: Vec<u8>, bios: Option<Vec<u8>>) -> Machine {
        let mut machine = machine_for(triple);
        machine
            .override_boot("cart", demu_config::model::BootState::Hex { bytes: cart })
            .expect("boot cart");
        if let Some(bytes) = bios {
            let name = machine
                .config
                .devices
                .iter()
                .find(|d| d.kind.as_str() == "atari.rom")
                .map(|d| d.name.to_string())
                .expect("bios device");
            machine
                .override_boot(&name, demu_config::model::BootState::Hex { bytes })
                .expect("boot bios");
        }
        machine.boot();
        machine
    }

    pub(super) fn cpu_pc(machine: &Machine) -> u16 {
        let regs = machine
            .device_by_name("cpu0")
            .and_then(|d| d.cpu())
            .expect("cpu")
            .registers();
        u16::try_from(regs["PC"]).expect("pc fits")
    }

    /// The 2600 kernel: a reset that clears the TIA, writes the
    /// background and playfield colors, then loops over WSYNC.
    pub(super) fn vcs_program() -> Vec<u8> {
        let mut rom = vec![0u8; 4096];
        let code: &[u8] = &[
            0x78, // SEI
            0xA2, 0xFF, 0x9A, // LDX #$FF, TXS
            0xA9, 0x48, 0x8D, 0x09, 0x00, // LDA #$48, STA COLUBK: sky blue
            0xA9, 0x2E, 0x8D, 0x08, 0x00, // LDA #$2E, STA COLUPF: gold
            0xA9, 0xF0, 0x8D, 0x0D, 0x00, // LDA #$F0, STA PF0
            0xA9, 0xFF, 0x8D, 0x0E, 0x00, // LDA #$FF, STA PF1
            0x8D, 0x02, 0x00, // loop: STA WSYNC
            0x4C, 0x18, 0xF0, // JMP loop
        ];
        rom[..code.len()].copy_from_slice(code);
        rom[0x0FFC] = 0x00;
        rom[0x0FFD] = 0xF0;
        rom
    }

    #[test]
    fn vcs_boots_through_cart_vector() {
        let mut machine = booted("mos6502-atari-2600", vcs_program(), None);
        machine.run_until(Tick(2_000_000));
        let pc = cpu_pc(&machine);
        assert!(
            (0xF018..=0xF01E).contains(&pc),
            "the kernel loops at the WSYNC, pc={pc:#06x}"
        );
        // The TIA composed whole frames.
        let frames = machine
            .device_by_name("tia")
            .and_then(|d| d.video())
            .map(|v| v.frames())
            .expect("tia");
        assert!(frames >= 24, "frames={frames}");
    }

    #[test]
    fn vcs_playfield_renders_and_determines_screenshot() {
        let mut one = booted("mos6502-atari-2600", vcs_program(), None);
        one.run_until(Tick(2_000_000));
        let mut two = booted("mos6502-atari-2600", vcs_program(), None);
        two.run_until(Tick(2_000_000));
        let v = one
            .device_by_name("tia")
            .and_then(|d| d.video())
            .and_then(|v| v.dump_layer(demu_core::device::VideoLayer::Framebuffer))
            .expect("framebuffer");
        assert!(v.iter().any(|b| *b != 0), "the playfield paints pixels");
        let w = two
            .device_by_name("tia")
            .and_then(|d| d.video())
            .expect("video")
            .frame()
            .expect("frame")
            .pixels
            .clone();
        assert_eq!(v, w, "two runs compose identical frames");
    }

    #[test]
    fn vcs_saves_and_restores_mid_kernel() {
        let mut machine = booted("mos6502-atari-2600", vcs_program(), None);
        machine.run_until(Tick(1_000_000));
        let mut buf = Vec::new();
        machine.save(&mut buf).expect("save");
        let mut twin = machine_for("mos6502-atari-2600");
        twin.override_boot(
            "cart",
            demu_config::model::BootState::Hex {
                bytes: vcs_program(),
            },
        )
        .expect("boot cart");
        twin.boot();
        twin.load(&buf).expect("load");
        twin.run_until(Tick(2_000_000));
        machine.run_until(Tick(2_000_000));
        let a = machine
            .device_by_name("cpu0")
            .and_then(|d| d.cpu())
            .expect("cpu")
            .registers();
        let b = twin
            .device_by_name("cpu0")
            .and_then(|d| d.cpu())
            .expect("cpu")
            .registers();
        assert_eq!(a["PC"], b["PC"]);
        assert_eq!(a["A"], b["A"]);
    }

    /// The 5200 BIOS: the minimal power-up that jumps through the cart
    /// vector at $BFFE. The reset vector at $FFFC points into it.
    pub(super) fn bios5200() -> Vec<u8> {
        let mut rom = vec![0xFFu8; 0x800]; // $F800-$FFFF
        let reset: &[u8] = &[
            0x78, // SEI
            0xA2, 0xFF, 0x9A, // LDX #$FF, TXS
            0x6C, 0xFE, 0xBF, // JMP ($BFFE)
        ];
        rom[0x780..0x780 + reset.len()].copy_from_slice(reset);
        rom[0x7FC] = 0x80; // $FFFC low
        rom[0x7FD] = 0xFF; // $FFFD high
        rom
    }

    /// The 5200 cart: sets up one display list of two mode-two lines
    /// in the RAM at $2000, enables DMA, then idles with a JVB.
    pub(super) fn cart5200() -> Vec<u8> {
        let mut rom = vec![0u8; 32 * 1024];
        // The program: enable the display list DMA at $2016, paint
        // colors, then idle.
        let code: &[u8] = &[
            0xA9, 0x22, 0x8D, 0x00, 0xD4, // LDA #$22, STA DMACTL: normal width
            0xA9, 0x16, 0x8D, 0x02, 0xD4, // LDA #$16, STA DLISTL: $2016
            0xA9, 0x20, 0x8D, 0x03, 0xD4, // STA DLISTH
            0xA9, 0x2A, 0x8D, 0x16, 0xC0, // LDA #$2A, STA COLPF0
            0xA9, 0x54, 0x8D, 0x1A, 0xC0, // LDA #$54, STA COLBK
            0xA9, 0x42, 0x8D, 0x16, 0x20, // $2016 = $42: mode line with LMS
            0xA9, 0x20, 0x8D, 0x17, 0x20, // $2017 = $20: the scan low
            0x8D, 0x18, 0x20, // $2018 = $20: the scan high
            0xA9, 0x02, 0x8D, 0x19, 0x20, // $2019 = $02: one more mode line
            0xA9, 0x41, 0x8D, 0x1A, 0x20, // $201A = $41: JVB
            0xA9, 0x16, 0x8D, 0x1B, 0x20, // $201B = $16: the loop low
            0xA9, 0x20, 0x8D, 0x1C, 0x20, // $201C = $20: back to $2016
            0xA9, 0xAA, 0x8D, 0x20, 0x20, // $2020 = $AA: the pixel byte
            0xEA, 0x4C, 0x40, 0x40, // idle: NOP; JMP $4040, self
        ];
        rom[..code.len()].copy_from_slice(code);
        // The cart vectors sit at $BFFE/$BFFF: image offsets 0x7FFE
        // and 0x7FFF in a flat 32-KiB image based at $4000.
        rom[0x7FFD] = 0xFF; // The diagnostic flag: not set high.
        rom[0x7FFE] = 0x00;
        rom[0x7FFF] = 0x40; // JMP target: $4000
        rom
    }

    #[test]
    fn atari5200_boots_bios_then_cart() {
        let mut machine = booted("mos6502-atari-5200", cart5200(), Some(bios5200()));
        machine.run_until(Tick(10_000_000));
        let pc = cpu_pc(&machine);
        assert_eq!(pc, 0x4040, "the cart program idles, pc={pc:#06x}");
        // The DMACTL landed through the bus.
        let dmactl = machine
            .device_by_name("antic")
            .and_then(|d| d.as_any().downcast_ref::<demu_dev_video::Antic>())
            .map(|a| a.dmactl())
            .expect("antic");
        assert_eq!(dmactl & 0x20, 0x20, "the display list DMA armed");
    }

    #[test]
    fn atari5200_display_list_walks() {
        let mut machine = booted("mos6502-atari-5200", cart5200(), Some(bios5200()));
        machine.run_until(Tick(10_000_000));
        // The mode lines of one display window: poll line advances,
        // because a three-instruction list is active only briefly.
        let mut saw_mode = false;
        let mut scan_max = 0u16;
        for i in 0..300u64 {
            machine.run_until(Tick(11_000_000 + 456 * i));
            let a = machine
                .device_by_name("antic")
                .and_then(|d| d.as_any().downcast_ref::<demu_dev_video::Antic>())
                .expect("antic");
            saw_mode |= a.dl_mode().is_some();
            scan_max = scan_max.max(a.mem_scan());
        }
        assert!(saw_mode, "a mode line became active");
        assert!(scan_max > 0x2016, "the scan pointer advanced past the list");
        // Picture row zero paints the border and the pattern colors.
        let v = machine
            .device_by_name("antic")
            .and_then(|d| d.as_any().downcast_ref::<demu_dev_video::Antic>())
            .and_then(|a| a.video())
            .and_then(|v| v.dump_layer(demu_core::device::VideoLayer::Framebuffer))
            .expect("framebuffer");
        let row: std::collections::BTreeSet<_> = v[0..320 * 3]
            .chunks(3)
            .map(|c| (c[0], c[1], c[2]))
            .collect();
        assert!(row.len() > 1, "the mode line painted the picture row");
    }

    #[test]
    fn atari5200_saves_and_restores() {
        let mut machine = booted("mos6502-atari-5200", cart5200(), Some(bios5200()));
        machine.run_until(Tick(5_000_000));
        let mut buf = Vec::new();
        machine.save(&mut buf).expect("save");
        let mut twin = machine_for("mos6502-atari-5200");
        twin.override_boot(
            "cart",
            demu_config::model::BootState::Hex { bytes: cart5200() },
        )
        .expect("boot cart");
        let bios_name = twin
            .config
            .devices
            .iter()
            .find(|d| d.kind.as_str() == "atari.rom")
            .map(|d| d.name.to_string())
            .expect("bios");
        twin.override_boot(
            &bios_name,
            demu_config::model::BootState::Hex { bytes: bios5200() },
        )
        .expect("boot bios");
        twin.boot();
        twin.load(&buf).expect("load");
        twin.run_until(Tick(10_000_000));
        machine.run_until(Tick(10_000_000));
        let a = machine
            .device_by_name("cpu0")
            .and_then(|d| d.cpu())
            .expect("cpu")
            .registers();
        let b = twin
            .device_by_name("cpu0")
            .and_then(|d| d.cpu())
            .expect("cpu")
            .registers();
        assert_eq!(a["PC"], b["PC"]);
        let sa = machine
            .device_by_name("antic")
            .and_then(|d| d.as_any().downcast_ref::<demu_dev_video::Antic>())
            .map(|a| a.dlist())
            .expect("antic");
        let sb = twin
            .device_by_name("antic")
            .and_then(|d| d.as_any().downcast_ref::<demu_dev_video::Antic>())
            .map(|a| a.dlist())
            .expect("antic");
        assert_eq!(sa, sb);
    }

    /// The 7800 BIOS: the minimal power-up that hands execution to
    /// the cart through its start vector at $FFFC.
    pub(super) fn bios7800() -> Vec<u8> {
        let mut rom = vec![0xFFu8; 0x1000]; // $C000-$CFFF
        let code: &[u8] = &[
            0x78, 0xA2, 0xFF, 0x9A, // SEI, LDX #$FF, TXS
            0x6C, 0xFC, 0xFF, // JMP ($FFFC)
        ];
        rom[..7].copy_from_slice(code);
        rom
    }

    /// The 7800 cart: a 48-KiB flat image filling $4000-$FFFF. It
    /// sets up MARIA with a display list list of one zone, then idles.
    /// The reset vector at $FFFC (image offsets 0xBFFC/0xBFFD) points
    /// at the program start.
    pub(super) fn cart7800() -> Vec<u8> {
        let mut rom = vec![0u8; 48 * 1024];
        let code: &[u8] = &[
            0xA9, 0x22, 0x8D, 0x20, 0x00, // LDA #$22, STA BACKGRND
            0xA9, 0x84, 0x8D, 0x21, 0x00, // LDA #$84, STA P0C1: blue vs the red background
            0xA9, 0x40, 0x8D, 0x3C, 0x00, // LDA #$40, STA CTRL: normal DMA
            0xA9, 0x60, 0x8D, 0x30, 0x00, // LDA #$60, STA DPPL: DLL at $1860
            0xA9, 0x18, 0x8D, 0x2C, 0x00, // LDA #$18, STA DPPH
            // The display list list entry at $1860: one line, DL at
            // $1850; the bytes carry the address high then low.
            0xA9, 0x00, 0x8D, 0x60, 0x18, // LDA #$00, STA $1860: count 0
            0xA9, 0x18, 0x8D, 0x61, 0x18, // STA $1861: the DL address high
            0xA9, 0x50, 0x8D, 0x62, 0x18, // STA $1862: the DL address low
            // The display list header at $1850: four bytes, one data
            // byte at $1870.
            0xA9, 0x70, 0x8D, 0x50, 0x18, // LDA #$70, STA $1850 (addr lo)
            0xA9, 0x1F, 0x8D, 0x51, 0x18, // LDA #$1F, STA $1851: palette 0, width 1
            0xA9, 0x18, 0x8D, 0x52, 0x18, // STA $1852 (addr hi)
            0xA9, 0x40, 0x8D, 0x53, 0x18, // LDA #$40, STA $1853 (hpos)
            0xA9, 0x55, 0x8D, 0x70, 0x18, // LDA #$55, STA $1870: color code one = P0C1
            0x4C, 0x41, 0x40, // idle: JMP $4041, self
        ];
        rom[..code.len()].copy_from_slice(code);
        rom[0xBFFC] = 0x00; // $FFFC low
        rom[0xBFFD] = 0x40; // $FFFD high: $4000
        rom
    }

    #[test]
    fn atari7800_boots_bios_then_cart() {
        let mut machine = booted("mos6502-atari-7800", cart7800(), Some(bios7800()));
        machine.run_until(Tick(10_000_000));
        let pc = cpu_pc(&machine);
        assert!(pc == 0x4041, "the cart program idles, pc={pc:#06x}");
    }

    #[test]
    fn atari7800_maria_composes_frame() {
        let mut machine = booted("mos6502-atari-7800", cart7800(), Some(bios7800()));
        machine.run_until(Tick(10_000_000));
        let m = machine
            .device_by_name("maria")
            .and_then(|d| d.as_any().downcast_ref::<demu_dev_video::Maria>())
            .expect("maria");
        assert!(m.video().expect("video").frames() >= 10);
        let frame = m.video().expect("video").frame().expect("frame");
        assert_eq!((frame.width, frame.height), (320, 192));
        let fb = m
            .video()
            .and_then(|v| v.dump_layer(demu_core::device::VideoLayer::Framebuffer))
            .expect("framebuffer");
        assert_eq!(fb.len(), 320 * 192 * 3);
        // The count-zero zone walks on the raster before the visible
        // window and paints exactly the frame's first row: the entry's
        // one data byte covers columns 64..67 of the 160-column grid,
        // two screen pixels each, from x 128 through 135. The graphics
        // byte $55 selects color code one per pixel, so the region
        // shows P0C1 while everything else shows BACKGRND; the zero
        // terminator behind the zone keeps the lower rows plain.
        let bg = demu_dev_video::tia_color_rgb(0x22, 4);
        let fg = demu_dev_video::tia_color_rgb(0x84, 4);
        assert_ne!(bg, fg, "the fixture names two distinct TIA colors");
        let px = |x: usize, y: usize| {
            let base = (y * 320 + x) * 3;
            [fb[base], fb[base + 1], fb[base + 2]]
        };
        for x in 128..136 {
            assert_eq!(px(x, 0), fg, "x={x} shows the painted P0C1 region");
        }
        assert_eq!(
            px(127, 0),
            bg,
            "the column left of the region stays background"
        );
        assert_eq!(
            px(136, 0),
            bg,
            "the column right of the region stays background"
        );
        for y in 1..192 {
            for x in [0usize, 64, 127, 128, 136, 200, 319] {
                assert_eq!(px(x, y), bg, "({x},{y}) stays background");
            }
        }
    }

    #[test]
    fn atari7800_saves_and_restores() {
        let mut machine = booted("mos6502-atari-7800", cart7800(), Some(bios7800()));
        machine.run_until(Tick(5_000_000));
        let mut buf = Vec::new();
        machine.save(&mut buf).expect("save");
        let mut twin = machine_for("mos6502-atari-7800");
        twin.override_boot(
            "cart",
            demu_config::model::BootState::Hex { bytes: cart7800() },
        )
        .expect("boot cart");
        twin.override_boot(
            "bios",
            demu_config::model::BootState::Hex { bytes: bios7800() },
        )
        .expect("boot bios");
        twin.boot();
        twin.load(&buf).expect("load");
        twin.run_until(Tick(10_000_000));
        machine.run_until(Tick(10_000_000));
        let a = cpu_pc(&machine);
        let b = cpu_pc(&twin);
        assert_eq!(a, b);
    }
}

#[cfg(test)]
pub mod computer_machine_tests {
    use crate::assemble;
    use demu_core::Tick;
    use demu_core::machine::Machine;

    /// Assemble one machine from a bundled triple.
    fn machine_for(triple: &str) -> Machine {
        let raw = demu_config::bundled::resolve_source(triple).expect("bundled config");
        let (config, _) = demu_config::validate(&raw).expect("validate");
        Machine::assemble(config, &assemble::demu_ctors()).expect("assemble")
    }

    /// Boot one machine with one synthetic image on the named device.
    pub(super) fn booted_image(triple: &str, device: &str, bytes: Vec<u8>) -> Machine {
        let mut machine = machine_for(triple);
        machine
            .override_boot(device, demu_config::model::BootState::Hex { bytes })
            .expect("boot image");
        machine.boot();
        machine
    }

    pub(super) fn cpu_pc(machine: &Machine) -> u16 {
        let regs = machine
            .device_by_name("cpu0")
            .and_then(|d| d.cpu())
            .expect("cpu")
            .registers();
        u16::try_from(regs["PC"]).expect("pc fits")
    }

    /// The frames of the first device that reports a frame count.
    pub(super) fn frames_of(machine: &Machine, device: &str) -> u64 {
        machine
            .device_by_name(device)
            .and_then(|d| d.video())
            .map(|v| v.frames())
            .expect("frames")
    }

    /// The Atari 800 OS stub: the setup writes, one audio register and
    /// one control write per chip, then a WSYNC loop.
    pub(super) fn a800_os_image() -> Vec<u8> {
        let mut rom = vec![0u8; 0x2000];
        let code: &[u8] = &[
            0x78, // SEI
            0xA2, 0xFF, 0x9A, // LDX #$FF, TXS
            0xD8, // CLD
            0xA9, 0x3C, 0x8D, 0x02, 0xD3, // LDA #$3C, STA PACTL
            0xA9, 0x00, 0x8D, 0x0E, 0xD2, // LDA #$00, STA IRQEN: no irq source
            0xA9, 0xA8, 0x8D, 0x00, 0xD2, // LDA #$A8, STA AUDF1
            0xA9, 0xA8, 0x8D, 0x01, 0xD2, // LDA #$A8, STA AUDC1
            0xA9, 0x8A, 0x8D, 0x0C, 0xD2, // LDA #$8A, STA SKCTL
        ];
        rom[..code.len()].copy_from_slice(code);
        // The loop at $E01E: STA WSYNC ($D40A), JMP back.
        rom[0x001E..0x0024].copy_from_slice(&[0x8D, 0x0A, 0xD4, 0x4C, 0x1E, 0xE0]);
        // The reset vector at $FFFC maps to the last bytes of the 8K
        // window: image offset 0x1FFC.
        rom[0x1FFC] = 0x00;
        rom[0x1FFD] = 0xE0;
        rom
    }

    #[test]
    fn atari800_ntsc_boots_into_the_wsync_loop() {
        let mut machine = booted_image("mos6502-atari-800-ntsc", "os", a800_os_image());
        machine.run_until(Tick(2_000_000));
        let pc = cpu_pc(&machine);
        assert!(
            (0xE01E..=0xE021).contains(&pc),
            "the OS stub loops at the WSYNC, pc={pc:#06x}"
        );
        assert!(frames_of(&machine, "antic") >= 16);
    }

    #[test]
    fn atari800_pal_boots_and_the_ntsc_machine_frames_lead() {
        let mut pal = booted_image("mos6502-atari-800-pal", "os", a800_os_image());
        pal.run_until(Tick(2_000_000));
        let pc = cpu_pc(&pal);
        assert!(
            (0xE01E..=0xE021).contains(&pc),
            "the OS stub loops at the WSYNC, pc={pc:#06x}"
        );
        let mut ntsc = booted_image("mos6502-atari-800-ntsc", "os", a800_os_image());
        ntsc.run_until(Tick(2_000_000));
        // The PAL frame runs 312 lines over the color-clock domain of
        // 71_136 ticks; the NTSC frame runs 262 lines over 119_472
        // ticks, so over one shared budget the PAL machine composes
        // strictly more frames.
        let ntsc_frames = frames_of(&ntsc, "antic");
        let pal_frames = frames_of(&pal, "antic");
        assert!(
            pal_frames > ntsc_frames,
            "ntsc={ntsc_frames} pal={pal_frames}"
        );
    }

    /// The C64 RAM image: the boot code parks the IRQ compare and the
    /// flag enable, counts raster IRQs into $0200, and loops.
    pub(super) fn c64_ram_image() -> Vec<u8> {
        let mut ram = vec![0u8; 0x10000];
        let code: &[u8] = &[
            0x78, // C000: SEI
            0xA2, 0xFF, 0x9A, // LDX #$FF, TXS
            0xD8, // CLD
            0xA9, 0x32, 0x8D, 0x12, 0xD0, // LDA #$32, STA $D012 raster 50
            0xA9, 0x01, 0x8D, 0x1A, 0xD0, // LDA #$01, STA $D01A
            0x58, // CLI
            0xAD, 0x00, 0x02, // C010: LDA $0200
            0xC9, 0x0A, // CMP #$0A
            0x90, 0xF9, // BCC back to C010
            0xE6, 0x01, // INC $01 done flag
            0x4C, 0x19, 0xC0, // JMP C019 park
        ];
        ram[0xC000..0xC000 + code.len()].copy_from_slice(code);
        // C020: LDA $D019 ack, INC $0200 absolute, RTI
        ram[0xC020..0xC027].copy_from_slice(&[0xAD, 0x19, 0xD0, 0xEE, 0x00, 0x02, 0x40]);
        ram[0xFFFC] = 0x00;
        ram[0xFFFD] = 0xC0;
        ram[0xFFFE] = 0x20;
        ram[0xFFFF] = 0xC0;
        ram
    }

    #[test]
    fn commodore64_counts_raster_irqs_from_ram_boot() {
        let mut machine = booted_image("mos6502-commodore-64", "wram", c64_ram_image());
        machine.run_until(Tick(4_000_000));
        use demu_core::frequency::Address;
        let count = machine.peek("cpu", Address(0x0200));
        assert!(count >= 10, "raster irq count={count}");
        let done = machine.peek("cpu", Address(0x0001));
        assert_eq!(done, 0x01, "the counted loop finished");
        let pc = cpu_pc(&machine);
        assert!(
            (0xC019..=0xC01B).contains(&pc),
            "the park loop holds, pc={pc:#06x}"
        );
        // Two runs with one image compose identical frame counts.
        let mut two = booted_image("mos6502-commodore-64", "wram", c64_ram_image());
        two.run_until(Tick(4_000_000));
        assert_eq!(frames_of(&machine, "vic"), frames_of(&two, "vic"));
    }

    #[test]
    fn commodore64_saves_and_restores_mid_run() {
        let mut machine = booted_image("mos6502-commodore-64", "wram", c64_ram_image());
        machine.run_until(Tick(2_000_000));
        let mut buf = Vec::new();
        machine.save(&mut buf).expect("save");
        let mut twin = machine_for("mos6502-commodore-64");
        twin.override_boot(
            "wram",
            demu_config::model::BootState::Hex {
                bytes: c64_ram_image(),
            },
        )
        .expect("boot twin");
        twin.boot();
        twin.load(&buf).expect("restore");
        twin.run_until(Tick(4_000_000));
        machine.run_until(Tick(4_000_000));
        use demu_core::frequency::Address;
        assert_eq!(
            machine.peek("cpu", Address(0x0200)),
            twin.peek("cpu", Address(0x0200)),
            "the restored twin counts the same"
        );
        assert_eq!(cpu_pc(&machine), cpu_pc(&twin));
    }

    /// The Apple II monitor stub: the keys write text page bytes, the
    /// form switches flip pages, and the loop parks.
    pub(super) fn a2_monitor_image() -> Vec<u8> {
        let mut rom = vec![0u8; 0x1000];
        let code: &[u8] = &[
            0x78, // F000: SEI
            0xA2, 0xFF, 0x9A, // LDX #$FF, TXS
            0xD8, // CLD
            0xA9, 0x41, 0x8D, 0x00, 0x04, // LDA #'A', STA $0400
            0xA9, 0x42, 0x8D, 0x01, 0x04, // LDA #'B', STA $0401
            0xA9, 0x43, 0x8D, 0x02, 0x04, // LDA #'C', STA $0402
            0x8D, 0x51, 0xC0, // STA $C051 text mode
        ];
        rom[..code.len()].copy_from_slice(code);
        let loop_at = 0x17;
        rom[loop_at..loop_at + 3].copy_from_slice(&[0x4C, loop_at as u8, 0xF0]);
        rom[0x0FFC] = 0x00;
        rom[0x0FFD] = 0xF0;
        rom
    }

    #[test]
    fn apple_ii_boots_into_the_park_loop() {
        let mut machine = booted_image("mos6502-apple-ii", "monitor", a2_monitor_image());
        machine.run_until(Tick(2_000_000));
        let pc = cpu_pc(&machine);
        assert!(
            (0xF017..=0xF019).contains(&pc),
            "the monitor stub parks, pc={pc:#06x}"
        );
        assert!(frames_of(&machine, "video") >= 8);
        use demu_core::device::VideoLayer;
        let fb = machine
            .device_by_name("video")
            .and_then(|d| d.video())
            .and_then(|v| v.dump_layer(VideoLayer::Framebuffer))
            .expect("framebuffer");
        assert!(fb.iter().any(|b| *b != 0), "the text page paints pixels");
    }

    #[test]
    fn apple_disk2_answers_the_slot_window() {
        let mut machine = booted_image("mos6502-apple-ii", "monitor", a2_monitor_image());
        let mut disk = vec![0u8; 35 * 256];
        disk[0] = 0xAA;
        machine
            .override_boot("disk2", demu_config::model::BootState::Hex { bytes: disk })
            .expect("boot disk");
        machine.boot();
        machine.run_until(Tick(100_000));
        use demu_core::frequency::Address;
        // The controller latches a written byte for the next read; the
        // peek answers without side effects.
        machine.debug_write("cpu", Address(0xC0ED), 0x5A);
        assert_eq!(machine.peek("cpu", Address(0xC0EC)), 0x5A, "the latch");
        // The second drive keeps its own latch.
        machine.debug_write("cpu", Address(0xC0EB), 0);
        machine.debug_write("cpu", Address(0xC0ED), 0xA5);
        assert_eq!(machine.peek("cpu", Address(0xC0EC)), 0xA5, "drive two");
        machine.debug_write("cpu", Address(0xC0EA), 0);
        assert_eq!(
            machine.peek("cpu", Address(0xC0EC)),
            0x5A,
            "drive one again"
        );
        // A media read through the window picks up a latch from the
        // image only through the read path; the peek stays stable.
        assert_eq!(
            machine.peek("cpu", Address(0xC0EC)),
            0x5A,
            "peek has no side effect"
        );
    }

    #[test]
    fn commodore64_pal_boots_with_the_slow_crystal() {
        let mut machine = booted_image("mos6502-commodore-64-pal", "wram", c64_ram_image());
        machine.run_until(Tick(4_000_000));
        use demu_core::frequency::Address;
        let count = machine.peek("cpu", Address(0x0200));
        assert!(count >= 10, "raster irq count={count}");
        // The PAL domain carries one tick per CPU cycle, so the
        // budget equals about four seconds and the fifty-hertz VIC
        // composes about two hundred frames.
        let frames = frames_of(&machine, "vic");
        assert!((203..=205).contains(&frames), "pal frames={frames}");
    }

    #[test]
    fn apple_family_configs_all_boot() {
        for triple in [
            "mos6502-apple-ii",
            "mos6502-apple-iic",
            "mos6502-apple-iie",
            "mos6502-apple-iie-enhanced",
        ] {
            let mut machine = booted_image(triple, "monitor", a2_monitor_image());
            machine.run_until(Tick(1_000_000));
            assert!(frames_of(&machine, "video") >= 4, "triple={triple}");
            let pc = cpu_pc(&machine);
            assert!(
                (0xF017..=0xF019).contains(&pc),
                "triple={triple} pc={pc:#06x}"
            );
        }
    }

    #[test]
    fn apple_ii_saves_and_restores_mid_run() {
        let mut machine = booted_image("mos6502-apple-ii", "monitor", a2_monitor_image());
        machine.run_until(Tick(1_000_000));
        let mut buf = Vec::new();
        machine.save(&mut buf).expect("save");
        let mut twin = machine_for("mos6502-apple-ii");
        twin.override_boot(
            "monitor",
            demu_config::model::BootState::Hex {
                bytes: a2_monitor_image(),
            },
        )
        .expect("boot twin");
        twin.boot();
        twin.load(&buf).expect("restore");
        twin.run_until(Tick(2_000_000));
        machine.run_until(Tick(2_000_000));
        assert_eq!(cpu_pc(&machine), cpu_pc(&twin));
        use demu_core::device::VideoLayer;
        let a = machine
            .device_by_name("video")
            .and_then(|d| d.video())
            .and_then(|v| v.dump_layer(VideoLayer::Framebuffer))
            .expect("framebuffer");
        let b = twin
            .device_by_name("video")
            .and_then(|d| d.video())
            .and_then(|v| v.dump_layer(VideoLayer::Framebuffer))
            .expect("framebuffer");
        assert_eq!(a, b, "the restored twin composes the same frame");
    }
}

#[cfg(test)]
pub mod snes_machine_tests {
    use crate::assemble;
    use demu_core::Tick;
    use demu_core::machine::Machine;

    /// Assemble and boot one machine from a bundled triple, with the
    /// test cart image as the positional form.
    fn machine_for(triple: &str, rom: Option<&[u8]>) -> Machine {
        let raw = demu_config::bundled::resolve_source(triple).expect("bundled config");
        let (config, _) = demu_config::validate(&raw).expect("validate");
        let mut machine = Machine::assemble(config, &assemble::demu_ctors()).expect("assemble");
        if let Some(bytes) = rom {
            machine
                .override_boot(
                    "cart",
                    demu_config::model::BootState::Hex {
                        bytes: bytes.to_vec(),
                    },
                )
                .expect("boot");
        }
        machine.boot();
        machine
    }

    /// A LoROM test cart: the PPU setup, the audio-port walk, and one
    /// DMA burst that fills VRAM, then a park loop.
    fn test_cart() -> Vec<u8> {
        let mut rom = vec![0u8; 0x8000];
        // The reset vector points at $8000.
        rom[0x7FFC] = 0x00;
        rom[0x7FFD] = 0x80;
        // The 65C816 program.
        let code: [u8; 79] = [
            0x18, // CLC
            0xFB, // XCE: native mode, eight-bit registers
            0xE2, 0x30, // SEP #$30
            0x9C, 0x00, 0x21, // STZ $2100: the frame blank
            0xA9, 0x0F, // LDA #$0F
            0x8D, 0x00, 0x21, // STA $2100: on
            0xA9, 0x34, // LDA #$34
            0x8D, 0x40, 0x21, // STA $2140: the CPU writes port zero
            0xAD, 0x40, 0x21, // LDA $2140: the plain-latch readback
            0x8F, 0x00, 0x00, 0x7E, // STA $7E0000: the copy to WRAM
            0xC2, 0x20, // REP #$20: sixteen-bit A
            0xA9, 0x34, 0x11, // LDA #$1134: the two source bytes
            0x8F, 0x00, 0x10, 0x7E, // STA $7E1000: the source word
            0xA9, 0x00, 0x10, // LDA #$1000
            0x8D, 0x02, 0x43, // STA $4302: the A-bus source word
            0xE2, 0x20, // SEP #$20
            0xA9, 0x7E, // LDA #$7E
            0x8D, 0x04, 0x43, // STA $4304: the source bank
            0xC2, 0x20, // REP #$20
            0xA9, 0x02, 0x00, // LDA #$0002: two source bytes
            0x8D, 0x05, 0x43, // STA $4305: the byte count
            0xE2, 0x20, // SEP #$20
            0xA9, 0x01, // LDA #$01
            0x8D, 0x00, 0x43, // STA $4300: the mode-one pair form
            0xA9, 0x18, // LDA #$18
            0x8D, 0x01, 0x43, // STA $4301: the destination $2118
            0xA9, 0x80, // LDA #$80
            0x8D, 0x15, 0x21, // STA $2115: the address increments at $2119
            0xA9, 0x01, // LDA #$01
            0x8D, 0x0B, 0x42, // STA $420B: one burst of channel zero
            0x4C, 0x4C, 0x80, // JMP $804C: the park loop
        ];
        rom[..code.len()].copy_from_slice(&code);
        rom
    }

    fn cpu_pc(machine: &Machine) -> Option<u64> {
        machine
            .device_by_name("cpu0")
            .and_then(|d| d.cpu())
            .and_then(|c| c.registers().get("PC").copied())
    }

    #[test]
    fn snes_config_boots_two_domains() {
        let machine = machine_for("wdc65c816-nintendo-snes", None);
        assert_eq!(machine.config.clocks.len(), 2, "the two crystals");
        assert_eq!(machine.config.devices.len(), 7);
    }

    #[test]
    fn snes_reads_the_vector_and_walks_the_program() {
        let mut machine = machine_for("wdc65c816-nintendo-snes", Some(&test_cart()));
        machine.run_until(Tick(2_000_000));
        let pc = cpu_pc(&machine).expect("cpu");
        assert!(
            pc == 0x804C,
            "the core parks at the program end: pc={pc:#06x}"
        );
    }

    #[test]
    fn snes_dma_burst_fills_vram_and_the_channel_clears() {
        use demu_core::frequency::Address;
        let mut machine = machine_for("wdc65c816-nintendo-snes", Some(&test_cart()));
        machine.run_until(Tick(2_000_000));
        // The DMA's source bytes land in VRAM through the $2118/$2119
        // register pair.
        let vram = machine
            .device_by_name("ppu")
            .and_then(|d| d.video())
            .and_then(|v| v.dump_layer(demu_core::device::VideoLayer::Vram))
            .expect("vram");
        assert_eq!(&vram[..2], &[0x34, 0x11], "the burst copied the pair");
        // The burst enable cleared itself.
        assert_eq!(machine.peek("cpu", Address(0x420B)), 0);
        // The work RAM carries the port readback and the source bytes.
        assert_eq!(machine.peek("cpu", Address(0x7E_0000)), 0x00);
        assert_eq!(machine.peek("cpu", Address(0x7E_1000)), 0x34);
        assert_eq!(machine.peek("cpu", Address(0x7E_1001)), 0x11);
    }

    #[test]
    fn snes_sound_domain_free_runs_deterministically() {
        let cart = test_cart();
        let mut machine = machine_for("wdc65c816-nintendo-snes", Some(&cart));
        let mut twin = machine_for("wdc65c816-nintendo-snes", Some(&cart));
        machine.run_until(Tick(3_000_000));
        twin.run_until(Tick(3_000_000));
        let mut state = Vec::new();
        machine.save(&mut state).expect("save");
        let mut twin_state = Vec::new();
        twin.save(&mut twin_state).expect("save");
        assert_eq!(
            state, twin_state,
            "the async domains produce identical state"
        );
    }

    #[test]
    fn snes_save_and_restore_reproduces_state() {
        let cart = test_cart();
        let mut machine = machine_for("wdc65c816-nintendo-snes", Some(&cart));
        machine.run_until(Tick(2_500_000));
        let mut buf = Vec::new();
        machine.save(&mut buf).expect("save");

        let mut restored = machine_for("wdc65c816-nintendo-snes", Some(&cart));
        restored.load(&buf).expect("load");
        let mut buf2 = Vec::new();
        restored.save(&mut buf2).expect("save");
        assert_eq!(buf, buf2, "save after restore is byte-identical");
        assert_eq!(restored.now(), machine.now());

        // Two machines restored from the same save continue
        // identically: device state, both domain queues, and later
        // saves stay byte-equal. (A machine that runs on without a
        // restore is not a comparable twin: re-entered runs re-seed
        // its queue from each device's cached next event.)
        let mut twin = machine_for("wdc65c816-nintendo-snes", Some(&cart));
        twin.load(&buf).expect("load twin");
        restored.run_until(Tick(4_000_000));
        twin.run_until(Tick(4_000_000));
        let mut check = Vec::new();
        restored.save(&mut check).expect("save");
        let mut check2 = Vec::new();
        twin.save(&mut check2).expect("save");
        assert_eq!(check, check2, "the two restores continue identically");
    }
}

#[cfg(test)]
pub mod iigs_machine_tests {
    use crate::assemble;
    use demu_core::Tick;
    use demu_core::machine::Machine;

    /// Assemble and boot one machine from a bundled triple, with the
    /// test monitor image as the ROM boot state.
    fn machine_for(triple: &str, rom: Option<&[u8]>) -> Machine {
        let raw = demu_config::bundled::resolve_source(triple).expect("bundled config");
        let (config, _) = demu_config::validate(&raw).expect("validate");
        let mut machine = Machine::assemble(config, &assemble::demu_ctors()).expect("assemble");
        if let Some(bytes) = rom {
            machine
                .override_boot(
                    "rom",
                    demu_config::model::BootState::Hex {
                        bytes: bytes.to_vec(),
                    },
                )
                .expect("boot");
        }
        machine.boot();
        machine
    }

    /// The synthetic boot monitor: the reset code enters native mode,
    /// copies the ROM banner into the text page, walks the console
    /// switches, and parks in an idle branch.
    fn monitor_image() -> Vec<u8> {
        let mut rom = vec![0u8; 0x1000];
        // The banner at $FF20 in the high-ASCII text form.
        rom[0x0F20..0x0F28].copy_from_slice(&[0xC1, 0xD0, 0xD0, 0xCC, 0xC5, 0xA0, 0xC9, 0xC9]);
        // The program at $FFD0.
        let code: [u8; 29] = [
            0x18, // CLC
            0xFB, // XCE: native mode
            0xE2, 0x30, // SEP #$30
            0xA2, 0x00, // LDX #$00
            0xBD, 0x20, 0xFF, // LDA $FF20,X
            0xF0, 0x06, // BEQ $FFE1: the switch walk
            0x9D, 0x00, 0x04, // STA $0400,X
            0xE8, // INX
            0x80, 0xF5, // BRA $FFD6
            0xA9, 0x80, // LDA #$80
            0x8D, 0x29, 0xC0, // STA $C029: the new-video register
            0xA9, 0x3C, // LDA #$3C
            0x8D, 0x35, 0xC0, // STA $C035: the shadow inhibit byte
            0x80, 0xFE, // the park at $FFEB: BRA self
        ];
        rom[0x0FD0..0x0FED].copy_from_slice(&code);
        // The reset vector picks $FFD0.
        rom[0x0FFC] = 0xD0;
        rom[0x0FFD] = 0xFF;
        rom
    }

    fn cpu_pc(machine: &Machine) -> Option<u64> {
        machine
            .device_by_name("cpu0")
            .and_then(|d| d.cpu())
            .and_then(|c| c.registers().get("PC").copied())
    }

    #[test]
    fn iigs_config_boots_one_domain() {
        let machine = machine_for("wdc65c816-apple-iigs", None);
        assert_eq!(machine.config.clocks.len(), 1);
        assert_eq!(machine.config.devices.len(), 7);
    }

    #[test]
    fn iigs_boots_and_walks_the_console_switches() {
        use demu_core::frequency::Address;
        let mut machine = machine_for("wdc65c816-apple-iigs", Some(&monitor_image()));
        machine.run_until(Tick(500_000));
        // The banner landed in the text page.
        for (i, wanted) in [0xC1, 0xD0, 0xD0, 0xCC, 0xC5, 0xA0, 0xC9, 0xC9]
            .into_iter()
            .enumerate()
        {
            assert_eq!(
                machine.peek("cpu", Address(0x0400 + i as u32)),
                wanted,
                "text page byte {i}"
            );
        }
        // The switch walk left its registers.
        assert_eq!(machine.peek("cpu", Address(0x00C029)), 0x80);
        assert_eq!(machine.peek("cpu", Address(0x00C035)), 0x3C);
        // The core sits in the park branch.
        let pc = cpu_pc(&machine).expect("cpu");
        assert!(pc == 0xFFEB, "the monitor parks at $FFEB: pc={pc:#06x}");
    }

    #[test]
    fn iigs_runs_deterministically() {
        let monitor = monitor_image();
        let mut machine = machine_for("wdc65c816-apple-iigs", Some(&monitor));
        let mut twin = machine_for("wdc65c816-apple-iigs", Some(&monitor));
        machine.run_until(Tick(500_000));
        twin.run_until(Tick(500_000));
        let mut state = Vec::new();
        machine.save(&mut state).expect("save");
        let mut twin_state = Vec::new();
        twin.save(&mut twin_state).expect("save");
        assert_eq!(state, twin_state, "the runs produce identical state");
    }
}

#[cfg(test)]
pub mod vl65_machine_tests {
    use crate::assemble;
    use demu_core::Level;
    use demu_core::Tick;
    use demu_core::machine::Machine;

    /// Assemble the mini VTI-die machine: the core, the work RAM, the
    /// boot ROM, and the map control with the published wiring.
    fn mini_machine(boot_image: &[u8]) -> Machine {
        let mut hex = String::new();
        for b in boot_image {
            hex.push_str(&format!("{:02x}", b));
        }
        let raw = format!(
            r#"
            [system]
            name = "vl65-mini"
            triple = "test"
            description = "the mini VTI die machine of the framework tests"
            [clock.main]
            frequency = "16000000"
            [[device]]
            name = "cpu0"
            kind = "vl65nc02.cpu"
            domain = "main"
            [device.dividers]
            cpu = 4
            [[device]]
            name = "ram0"
            kind = "lynx.ram"
            domain = "main"
            [device.params]
            size = 65536
            [[device]]
            name = "bootrom"
            kind = "lynx.bootrom"
            domain = "main"
            [[device]]
            name = "mapctl"
            kind = "lynx.mapctl"
            domain = "main"
            [[bus]]
            name = "cpu"
            address_bits = 16
            owner = "cpu0"
            [[window]]
            name = "w_ram_mid"
            bus = "cpu"
            device = "ram0"
            base = "0x0000"
            size = "0xFC00"
            [[window]]
            name = "w_ram_rom"
            bus = "cpu"
            device = "ram0"
            base = "0xFE00"
            size = "0x01F8"
            [[window]]
            name = "w_ram_vec"
            bus = "cpu"
            device = "ram0"
            base = "0xFFFA"
            size = "0x0006"
            [[window]]
            name = "w_bootrom"
            bus = "cpu"
            device = "bootrom"
            base = "0xFE00"
            size = "0x0200"
            overlay = "lynx_boot"
            overlay_read_priority = 3
            overlay_write_priority = 0
            [[window]]
            name = "w_mapctl"
            bus = "cpu"
            device = "mapctl"
            base = "0xFFF8"
            size = "0x0002"
            overlay = "mapctl_regs"
            overlay_read_priority = 2
            overlay_write_priority = 2
            [[connection]]
            from = "cpu0.rom_ram"
            to = "bootrom.rom_ram"
            semantics = "display-sync"
            [[connection]]
            from = "cpu0.vec_ram"
            to = "bootrom.vec_ram"
            semantics = "display-sync"
            [boot.ram0]
            pattern = "zero"
            [boot.bootrom]
            hex = "{hex}"
            "#
        );
        let parsed = demu_config::RawConfig::parse(&raw).expect("mini config parses");
        let (config, warnings) = demu_config::validate(&parsed).expect("mini config validates");
        for w in warnings {
            eprintln!("warning: {} - {}", w.code, w.message);
        }
        Machine::assemble(config, &assemble::demu_ctors()).expect("mini machine assembles")
    }

    /// The image with the vector bytes of the machine's own flow.
    fn image_of(code: &[u8]) -> Vec<u8> {
        let mut image = vec![0x00u8; 512];
        let n = code.len().min(508);
        image[..n].copy_from_slice(&code[..n]);
        // The reset and the IRQ vectors point at the image start.
        let vectors: [u8; 4] = [0x00, 0xFE, 0x00, 0xFE];
        image[508..512].copy_from_slice(&vectors);
        image
    }

    fn cpu_pc_of(machine: &Machine) -> u16 {
        let regs = machine
            .device_by_name("cpu0")
            .and_then(|d| d.cpu())
            .expect("cpu")
            .registers();
        u16::try_from(regs["PC"]).expect("pc fits")
    }

    fn cpu_reg(machine: &Machine, name: &str) -> u64 {
        machine
            .device_by_name("cpu0")
            .and_then(|d| d.cpu())
            .expect("cpu")
            .registers()
            .get(name)
            .copied()
            .expect("register")
    }

    #[test]
    fn loader_copies_its_tail_then_parks_in_the_ram() {
        // The tail runs from the ROM page the first time, writes the
        // map control, and the park instruction then fetches from the
        // RAM underneath where the loader copied the tail.
        let loader: [u8; 16] = [
            0x78, // SEI
            0xD8, // CLD
            0xA2, 0x00, // LDX #$00
            0xBD, 0x10, 0xFE, // LDA $FE10,X
            0x9D, 0x10, 0xFE, // STA $FE10,X
            0xE8, // INX
            0xE0, 0x08, // CPX #$08
            0xD0, 0xF5, // BNE back
            0xEA, // NOP
        ];
        let mut image = image_of(&loader);
        // The tail at $FE10: the map write (the ROM and the vector
        // spaces off), then the park branch; the park's fetches come
        // from the copied RAM bytes.
        image[16..24].copy_from_slice(&[
            0xA9, 0x0C, // LDA #$0C
            0x8D, 0xF9, 0xFF, // STA $FFF9
            0x80, 0xFE, // BRA $FE15 from the copied tail
            0xEA,
        ]);

        let mut machine = mini_machine(&image);
        machine.boot();
        machine.run_until(Tick(4 * 300));
        // The map control kept the written form and the ROM slice
        // reads the RAM underneath now.
        assert_eq!(machine.peek("cpu", demu_core::Address(0xFFF9)), 0x0C);
        assert_eq!(
            machine.peek("cpu", demu_core::Address(0xFE10)),
            0xA9,
            "the copied tail is visible as the RAM view"
        );
        // The core parked at the branch: the taken BRA returns to its
        // own first byte.
        assert_eq!(cpu_pc_of(&machine), 0xFE15);
    }

    #[test]
    fn map_write_unmaps_the_boot_rom_slice_to_the_ram() {
        let loader: [u8; 15] = [
            0xA2, 0x00, // LDX #$00
            0xBD, 0x15, 0xFE, // LDA $FE15,X
            0x9D, 0x15, 0xFE, // STA $FE15,X
            0xE8, // INX
            0xE0, 0x08, // CPX #$08
            0xD0, 0xF5, // BNE back
            0xEA, 0xEA,
        ];
        let mut image = image_of(&loader);
        // The gap between the loader and the tail position stays a
        // NOP stream.
        image[15..21].fill(0xEA);
        // The tail at $FE15: the map write of the ROM space only, then
        // the stop state.
        image[21..28].copy_from_slice(&[
            0xA9, 0x04, // LDA #$04
            0x8D, 0xF9, 0xFF, // STA $FFF9
            0xDB, // STP
            0xEA,
        ]);

        let mut machine = mini_machine(&image);
        machine.boot();
        machine.run_until(Tick(4 * 200));
        assert_eq!(machine.peek("cpu", demu_core::Address(0xFFF9)), 0x04);
        // The ROM slice reads the RAM (zeros); the copied tail shows
        // through it at its own position.
        assert_eq!(machine.peek("cpu", demu_core::Address(0xFE00)), 0x00);
        assert_eq!(machine.peek("cpu", demu_core::Address(0xFE15)), 0xA9);
        // The core stopped on the copied STP byte: the run returned with
        // the park state holding the stop byte's position. A parked core
        // declares no next event, and the finish tick reports the
        // requested run boundary because the idle window let the primary
        // clock pass.
        assert!(machine.now() <= Tick(4 * 200), "the stopped run returned");
        assert_eq!(
            machine.device_by_name("cpu0").and_then(|d| d.next_event()),
            None,
            "the STP parked the core"
        );
        assert_eq!(cpu_pc_of(&machine), 0xFE1B);
    }

    #[test]
    fn the_irq_wake_releases_a_parked_core() {
        let image = image_of(&[0xEA, 0x58, 0xCB, 0xEA]); // NOP CLI WAI NOP
        let mut machine = mini_machine(&image);
        machine.boot();
        machine.run_until(Tick(100));
        // The WAI parked the core: no next event.
        assert_eq!(
            machine.device_by_name("cpu0").and_then(|d| d.next_event()),
            None
        );
        // The IRQ line rises through the test and the run picks the
        // parked core up for the service.
        {
            let Some(cpu) = machine.device_by_name_mut("cpu0") else {
                panic!("cpu0 exists");
            };
            cpu.notify_line("irq", Level::High, Tick(100));
        }
        machine.run_until(Tick(1000));
        // The level IRQ stays asserted with the disable clear: the
        // service loop ran and every push landed on the stack. The pc
        // keeps returning to the vector's target.
        assert_eq!(
            cpu_pc_of(&machine),
            0xFE00,
            "the service loop parks at the vector"
        );
        assert_ne!(cpu_reg(&machine, "SP"), 0xFF, "the pushes ran");
    }

    #[test]
    fn the_high_speed_bit_lengthens_every_cycle() {
        let boot_image = |ctl: u8| {
            image_of(&[
                0xA9, ctl, // LDA #ctl
                0x8D, 0xF9, 0xFF, // STA $FFF9
                0xA2, 0x00, // LDX #$00
                0xE8, // INX
                0xE0, 0xFF, // CPX #$FF
                0xD0, 0xFB, // BNE back
                0x80, 0xFE, // park
            ])
        };
        let mut normal = mini_machine(&boot_image(0x00));
        normal.boot();
        normal.run_until(Tick(2000));
        let mut high = mini_machine(&boot_image(0x80));
        high.boot();
        high.run_until(Tick(2000));
        // Both cores sit in the same counting loop; the high-speed
        // core consumed a tick more per cycle, so its loop counter
        // stays behind.
        assert!(
            cpu_reg(&normal, "X") > cpu_reg(&high, "X"),
            "the high-speed core lags"
        );
    }

    #[test]
    fn the_mini_runs_deterministically() {
        let image = image_of(&[
            0x78, 0xD8, 0xEA, 0xEA, 0xA2, 0x00, 0x18, 0xE8, 0xCA, 0xE0, 0x0A,
        ]);
        let run = || -> (Tick, Vec<u8>) {
            let mut machine = mini_machine(&image);
            machine.boot();
            machine.run_until(Tick(4000));
            let mut out = Vec::new();
            machine.save(&mut out).expect("save");
            (machine.now(), out)
        };
        let (t1, s1) = run();
        let (t2, s2) = run();
        assert_eq!(t1, t2);
        assert_eq!(s1, s2, "identical runs produce identical state");
    }

    #[test]
    fn save_and_restore_continues_identically() {
        let image = image_of(&[0x78, 0xD8, 0xEA, 0xEA, 0xE8, 0xCA]);
        let mut machine = mini_machine(&image);
        machine.boot();
        machine.run_until(Tick(2000));
        let mut buf = Vec::new();
        machine.save(&mut buf).expect("save");

        // Two machines restored from the same save continue
        // identically (the live machine's queue re-seeds from device
        // state, so it is not a comparable twin).
        let mut twin_a = mini_machine(&image);
        twin_a.load(&buf).expect("load twin a");
        let mut twin_b = mini_machine(&image);
        twin_b.load(&buf).expect("load twin b");
        twin_a.run_until(Tick(4000));
        twin_b.run_until(Tick(4000));
        let mut again = Vec::new();
        twin_a.save(&mut again).expect("save");
        let mut check = Vec::new();
        twin_b.save(&mut check).expect("save");
        assert_eq!(again, check, "the two restores continue identically");
    }

    /// Assemble and boot one machine from a bundled triple, with the
    /// boot image as the boot-state override of one device.
    fn bundled_machine(triple: &str, device: &str, image: Option<Vec<u8>>) -> Machine {
        let raw = demu_config::bundled::resolve_source(triple).expect("bundled config");
        let (config, _) = demu_config::validate(&raw).expect("validate");
        let mut machine = Machine::assemble(config, &assemble::demu_ctors()).expect("assemble");
        if let Some(bytes) = image {
            machine
                .override_boot(device, demu_config::model::BootState::Hex { bytes })
                .expect("boot");
        }
        machine.boot();
        machine
    }

    #[test]
    fn the_bundled_lynx_assembles_to_the_published_family() {
        let machine = bundled_machine("vl65nc02-atari-lynx", "bootrom", None);
        assert_eq!(machine.config.clocks.len(), 1, "the single crystal");
        assert_eq!(machine.config.devices.len(), 7, "the seven device slots");
        // The pad and switch bytes answer from the Mikey die: the pad
        // reads clear and the switch byte carries the cart slot lines
        // high.
        assert_eq!(machine.peek("cpu", demu_core::Address(0xFCB0)), 0x00);
        assert_eq!(machine.peek("cpu", demu_core::Address(0xFCB1)), 0x06);
        // The map control keeps the boot gates: every space maps to
        // the hardware or the ROM.
        assert_eq!(machine.peek("cpu", demu_core::Address(0xFFF9)), 0x00);
    }

    #[test]
    fn the_bundled_lynx_injected_pads_reach_the_die() {
        let mut machine = bundled_machine("vl65nc02-atari-lynx", "bootrom", None);
        {
            let Some(mikey) = machine.device_by_name_mut("mikey") else {
                panic!("mikey exists");
            };
            assert!(mikey.input_mut().expect("sink").inject("press up"));
        }
        // The injected press shows through the pad register on the
        // bus, and the die owns the window over the Suzy page.
        assert_eq!(machine.peek("cpu", demu_core::Address(0xFCB0)), 0x80);
    }

    #[test]
    fn the_bundled_lynx_boot_copies_and_parks_in_the_ram() {
        // The boot ROM image carries the copy flow of the check
        // screen: the ROM page copies its tail into the RAM underneath
        // and the map write then parks the core in the copied RAM.
        let loader: [u8; 16] = [
            0x78, // SEI
            0xD8, // CLD
            0xA2, 0x00, // LDX #$00
            0xBD, 0x10, 0xFE, // LDA $FE10,X
            0x9D, 0x10, 0xFE, // STA $FE10,X
            0xE8, // INX
            0xE0, 0x08, // CPX #$08
            0xD0, 0xF5, // BNE back
            0xEA, // NOP
        ];
        let mut image = image_of(&loader);
        image[16..24].copy_from_slice(&[
            0xA9, 0x0C, // LDA #$0C: the ROM and the vector spaces off
            0x8D, 0xF9, 0xFF, // STA $FFF9
            0x80, 0xFE, // BRA back to its own byte
            0xEA,
        ]);
        let mut machine = bundled_machine("vl65nc02-atari-lynx", "bootrom", Some(image));
        machine.run_until(Tick(4 * 300));
        assert_eq!(
            machine.peek("cpu", demu_core::Address(0xFFF9)),
            0x0C,
            "the boot ROM wrote the map control"
        );
        assert_eq!(
            machine.peek("cpu", demu_core::Address(0xFE10)),
            0xA9,
            "the copied tail shows through the gated ROM slice"
        );
        assert_eq!(
            cpu_pc_of(&machine),
            0xFE15,
            "the core parked in the copied RAM"
        );
    }
}
#[cfg(test)]
pub mod pce_machine_tests {
    use crate::assemble;
    use demu_core::Tick;
    use demu_core::machine::Machine;

    /// Assemble the mini HuC6280 machine: the core, the work RAM, and
    /// the HuCard, with the published reset flow.
    fn mini_machine(card_image: &[u8]) -> Machine {
        let mut hex = String::new();
        for b in card_image {
            hex.push_str(&format!("{:02x}", b));
        }
        let raw = format!(
            r#"
            [system]
            name = "huc-mini"
            triple = "test"
            description = "the mini HuC6280 machine of the framework tests"
            [clock.main]
            frequency = "21477270"
            [[device]]
            name = "cpu0"
            kind = "huc6280.cpu"
            domain = "main"
            [device.dividers]
            cpu = 3
            [[device]]
            name = "ram0"
            kind = "pce.ram"
            domain = "main"
            [device.params]
            size = 8192
            [[device]]
            name = "cart"
            kind = "pce.cart"
            domain = "main"
            [[bus]]
            name = "cpu"
            address_bits = 21
            owner = "cpu0"
            [[window]]
            name = "w_card"
            bus = "cpu"
            device = "cart"
            base = "0x000000"
            size = "0x1EE000"
            [[window]]
            name = "w_ram"
            bus = "cpu"
            device = "ram0"
            base = "0x1F0000"
            size = "0x002000"
            [boot.ram0]
            pattern = "zero"
            [boot.cart]
            hex = "{hex}"
            "#
        );
        let parsed = demu_config::RawConfig::parse(&raw).expect("mini config parses");
        let (config, warnings) = demu_config::validate(&parsed).expect("mini config validates");
        for w in warnings {
            eprintln!("warning: {} - {}", w.code, w.message);
        }
        Machine::assemble(config, &assemble::demu_ctors()).expect("mini machine assembles")
    }

    /// The card image of a program and the vector tail.
    fn card_of(code: &[u8], vectors: &[(usize, u8)], size: usize) -> Vec<u8> {
        let mut image = vec![0u8; size];
        for (i, b) in code.iter().enumerate() {
            image[i] = *b;
        }
        for (at, b) in vectors {
            if *at < size + 1 && *at > 0 {
                let at = *at;
                image[at] = *b;
            }
        }
        image
    }

    fn cpu_reg(machine: &Machine, name: &str) -> u64 {
        machine
            .device_by_name("cpu0")
            .and_then(|d| d.cpu())
            .expect("cpu")
            .registers()
            .get(name)
            .copied()
            .expect("register")
    }

    fn cpu_pc_of(machine: &Machine) -> u16 {
        u16::try_from(cpu_reg(machine, "PC")).expect("pc fits")
    }

    #[test]
    fn pce_runs_the_reset_vector_through_bank_zero() {
        // The reset code parks; the vectors live in card bank 0.
        let image = card_of(
            &[0xEA, 0x18, 0x80, 0xFE],
            &[(0x1FFE, 0x00), (0x1FFF, 0xE0)],
            0x2000,
        );
        let mut machine = mini_machine(&image);
        machine.boot();
        machine.run_until(Tick(3 * 100));
        // The MPR 7 default bank 0 carried the reset vector; the pc
        // landed in the park branch.
        assert_eq!(cpu_pc_of(&machine), 0xE002, "the reset program ran");
    }

    #[test]
    fn tam_maps_a_bank_into_zero_page() {
        // MPR 0 set to bank 0 makes CPU $0000 read the card.
        let code = [
            0xA9, 0x00, // LDA #$00
            0x53, 0x01, // TAM #01: MPR0 = A
            0xAD, 0x00, 0x00, // LDA $0000
            0x80, 0xFE, // park
        ];
        let image = card_of(&code, &[(0x1FFE, 0x00), (0x1FFF, 0xE0)], 0x2000);
        let mut machine = mini_machine(&image);
        machine.boot();
        machine.run_until(Tick(3 * 100));
        assert_eq!(cpu_pc_of(&machine), 0xE007, "the program parked");
        assert_eq!(
            cpu_reg(&machine, "A"),
            0xA9,
            "the mapped bank read through MPR0"
        );
    }

    #[test]
    fn the_work_ram_sits_at_bank_f8() {
        // A store through MPR1's default $F8 reaches the work RAM.
        let code = [
            0xA9, 0x42, // LDA #$42
            0x8D, 0x00, 0x20, // STA $2000
            0x80, 0xFE, // park
        ];
        let image = card_of(&code, &[(0x1FFE, 0x00), (0x1FFF, 0xE0)], 0x2000);
        let mut machine = mini_machine(&image);
        machine.boot();
        machine.run_until(Tick(3 * 100));
        assert_eq!(
            machine.peek("cpu", demu_core::Address(0x1F_0000)),
            0x42,
            "the zero page view stored into the work RAM"
        );
    }

    #[test]
    fn the_timer_serves_through_the_timer_vector() {
        // The timer runs at the fastest rate; the IRQ enable turns the
        // timer source on; the service lands in the timer park.
        let code = [
            0xA9, 0x0A, // LDA #$0A
            0x8D, 0x00, 0x0C, // STA $0C00 (the preset)
            0xA9, 0x01, 0x8D, 0x01, 0x0C, // STA $0C01 (start and enable)
            0xA9, 0x04, 0x8D, 0x01, 0x14, // STA $1401 (the legacy timer enable form)
            0x58, // CLI
            0x80, 0xFE, // park in a self branch
        ];
        let mut image = card_of(
            &code,
            &[
                (0x1FFA, 0x40),
                (0x1FFB, 0xE0),
                (0x1FFE, 0x00),
                (0x1FFF, 0xE0),
            ],
            0x2100,
        );
        // The timer service parks at $E040.
        image[0x40] = 0x80;
        image[0x41] = 0xFE;
        let mut machine = mini_machine(&image);
        machine.boot();
        machine.run_until(Tick(3 * 20_000));
        // The timer underflowed, the entry ran, and the program parked
        // in the service park.
        assert_eq!(cpu_pc_of(&machine), 0xE040, "the timer service ran");
        let p = cpu_reg(&machine, "P");
        assert_eq!(p & 0x04, 0x04, "the entry left the disable set");
        assert_ne!(
            cpu_reg(&machine, "S") & 0x03,
            0,
            "the service pushed the three bytes"
        );
    }

    #[test]
    fn pce_runs_deterministically() {
        let code = [0xEA, 0xE8, 0xCA, 0x18, 0x80, 0xFE];
        let run = || -> (Tick, Vec<u8>) {
            let image = card_of(&code, &[(0x1FFE, 0x00), (0x1FFF, 0xE0)], 0x2100);
            let mut machine = mini_machine(&image);
            machine.boot();
            machine.run_until(Tick(6000));
            let mut out = Vec::new();
            machine.save(&mut out).expect("save");
            (machine.now(), out)
        };
        let (t1, s1) = run();
        let (t2, s2) = run();
        assert_eq!(t1, t2);
        assert_eq!(s1, s2, "identical runs produce identical state");
    }

    #[test]
    fn pce_save_and_restore_continues_identically() {
        let code = [0xEA, 0xE8, 0xCA, 0x18, 0x80, 0xFE];
        let image = card_of(&code, &[(0x1FFE, 0x00), (0x1FFF, 0xE0)], 0x2100);
        let mut machine = mini_machine(&image);
        machine.boot();
        machine.run_until(Tick(3000));
        let mut buf = Vec::new();
        machine.save(&mut buf).expect("save");
        // Two machines restored from the same save continue
        // identically.
        let mut twin_a = mini_machine(&image);
        twin_a.load(&buf).expect("load a");
        let mut twin_b = mini_machine(&image);
        twin_b.load(&buf).expect("load b");
        twin_a.run_until(Tick(6000));
        twin_b.run_until(Tick(6000));
        let mut again = Vec::new();
        twin_a.save(&mut again).expect("save");
        let mut check = Vec::new();
        twin_b.save(&mut check).expect("save");
        assert_eq!(again, check, "the two restores continue identically");
    }

    /// Assemble and boot one machine from a bundled triple, with the
    /// card image as the boot-state override of the HuCard.
    fn bundled_machine(triple: &str, image: Option<Vec<u8>>) -> Machine {
        let raw = demu_config::bundled::resolve_source(triple).expect("bundled config");
        let (config, _) = demu_config::validate(&raw).expect("validate");
        let mut machine = Machine::assemble(config, &assemble::demu_ctors()).expect("assemble");
        if let Some(bytes) = image {
            machine
                .override_boot("cart", demu_config::model::BootState::Hex { bytes })
                .expect("boot");
        }
        machine.boot();
        machine
    }

    #[test]
    fn the_bundled_engine_assembles_to_the_published_family() {
        let machine = bundled_machine("mos6502-nec-pcengine", None);
        assert_eq!(machine.config.clocks.len(), 1, "the single crystal");
        assert_eq!(machine.config.devices.len(), 7, "the seven device slots");
        // The VDC status port answers through its window with its
        // cleared flag form, and the VCE control byte reads back.
        assert_eq!(machine.peek("cpu", demu_core::Address(0x1FE000)), 0x00);
        assert_eq!(machine.peek("cpu", demu_core::Address(0x1FE400)), 0x00);
        // The direct color-table face answers the entry bytes of the
        // published window: the reset palette is black at entry zero.
        assert_eq!(machine.peek("cpu", demu_core::Address(0x1FF800)), 0x00);
        assert_eq!(machine.peek("cpu", demu_core::Address(0x1FF9FE)), 0x00);
    }

    #[test]
    fn the_bundled_engine_boots_and_parks_through_the_card() {
        let image = card_of(
            &[0xEA, 0x18, 0x80, 0xFE],
            &[(0x1FFE, 0x00), (0x1FFF, 0xE0)],
            0x4000,
        );
        let mut machine = bundled_machine("mos6502-nec-pcengine", Some(image));
        machine.run_until(Tick(3 * 100));
        assert_eq!(
            cpu_pc_of(&machine),
            0xE002,
            "the reset program ran and parked"
        );
        // The video pair stays in its reset form through the park.
        assert_eq!(machine.peek("cpu", demu_core::Address(0x1FE000)), 0x00);
        // The turbotap pad answers the I/O page with the idle row
        // form: no press at the pad select, the country line clear
        // on the not-Japan form.
        assert_eq!(machine.peek("cpu", demu_core::Address(0x1FF000)), 0x3F);
    }
}

/// The Z80 machine group: the Sega 8-bit consoles, the Sinclair
/// machines, and the TI-85, end to end over the machine assembly.
#[cfg(test)]
pub mod z80_machine_tests {
    use crate::assemble;
    use demu_core::AudioSource as _;
    use demu_core::Tick;
    use demu_core::machine::Machine;

    /// Assemble a machine from a raw TOML document and boot it.
    fn machine_of(raw: String) -> Machine {
        let parsed = demu_config::RawConfig::parse(&raw).expect("mini config parses");
        let (config, warnings) = demu_config::validate(&parsed).expect("mini config validates");
        for w in warnings {
            eprintln!("warning: {} - {}", w.code, w.message);
        }
        let mut machine =
            Machine::assemble(config, &assemble::demu_ctors()).expect("mini machine assembles");
        machine.boot();
        machine
    }

    /// The hex text of a byte image.
    fn hex_of(image: &[u8]) -> String {
        let mut hex = String::new();
        for b in image.iter() {
            hex.push_str(&format!("{b:02x}"));
        }
        hex
    }

    /// The CPU register value of the machine's first CPU.
    fn cpu_reg(machine: &Machine, name: &str) -> u64 {
        machine
            .devices
            .iter()
            .find_map(|d| d.cpu())
            .expect("a cpu")
            .registers()
            .get(name)
            .copied()
            .expect("register")
    }

    fn cpu_pc_of(machine: &Machine) -> u16 {
        u16::try_from(cpu_reg(machine, "PC")).expect("pc fits")
    }

    /// A 48K Spectrum mini machine with `body_hex` planted at the boot
    /// pc and `a` in the accumulator at boot.
    ///
    /// The RAM devices split the 48 KiB space, so the body plants
    /// inside the device whose window covers the pc.
    fn spectrum48_of(name: &str, pc: u64, a: u8, body_hex: &str) -> Machine {
        let index = (pc - 0x4000) as usize;
        let mut hex_a = String::new();
        let mut hex_b = String::new();
        let mut hex_c = String::new();
        let target = match index / 0x4000 {
            0 => &mut hex_a,
            1 => &mut hex_b,
            _ => &mut hex_c,
        };
        for _ in 0..(index % 0x4000) {
            target.push_str("00");
        }
        target.push_str(body_hex);
        let pc_str = format!("0x{pc:04X}");
        let raw = format!(
            r#"
        [system]
        name = "{name}"
        triple = "z80-sinclair-spectrum"
        [clock.main]
        frequency = "3500000"
        [[device]]
        name = "cpu0"
        kind = "z80.cpu"
        domain = "main"
        [device.dividers]
        cpu = 1
        [device.params]
        bus = "cpu"
        io_bus = "io"
        contended = "0x4000-0x8000,0xE000-0x10000"
        io_contend = "0x40-0x80"
        contend_from = 14336
        contend_to = 57344
        contend_base = 14344
        [[device]]
        name = "u0"
        kind = "zx.ula"
        domain = "main"
        [device.params]
        form = "spectrum"
        line_t = 224
        frame_lines = 312
        [[device]]
        name = "rom0"
        kind = "z80.rom"
        domain = "main"
        [device.params]
        size = 16384
        [[device]]
        name = "ram_a"
        kind = "z80.ram"
        domain = "main"
        [device.params]
        size = 16384
        [[device]]
        name = "ram_b"
        kind = "z80.ram"
        domain = "main"
        [device.params]
        size = 16384
        [[device]]
        name = "ram_c"
        kind = "z80.ram"
        domain = "main"
        [device.params]
        size = 16384
        [[bus]]
        name = "cpu"
        address_bits = 16
        owner = "cpu0"
        [[bus]]
        name = "io"
        address_bits = 17
        owner = "cpu0"
        [[window]]
        name = "w_ula"
        bus = "io"
        device = "u0"
        base = "0x10000"
        size = "0x10000"
        [[window]]
        name = "w_rom"
        bus = "cpu"
        device = "rom0"
        base = "0x0000"
        size = "0x4000"
        [[window]]
        name = "w_ram_a"
        bus = "cpu"
        device = "ram_a"
        base = "0x4000"
        size = "0x4000"
        [[window]]
        name = "w_ram_b"
        bus = "cpu"
        device = "ram_b"
        base = "0x8000"
        size = "0x4000"
        [[window]]
        name = "w_ram_c"
        bus = "cpu"
        device = "ram_c"
        base = "0xC000"
        size = "0x4000"
        [[connection]]
        from = "u0.int"
        to = "cpu0.int"
        semantics = "irq"
        [[connection]]
        from = "cpu0.halt"
        to = "u0.halt"
        semantics = "display-sync"
        [boot.cpu0]
        registers = {{ pc = "{pc_str}", a = "{a:#04x}" }}
        [boot.rom0]
        hex = "f376"
        [boot.ram_a]
        hex = "{hex_a}"
        [boot.ram_b]
        hex = "{hex_b}"
        [boot.ram_c]
        hex = "{hex_c}"
        "#,
            name = name,
            pc_str = pc_str,
            a = a,
            hex_a = hex_a,
            hex_b = hex_b,
            hex_c = hex_c,
        );
        machine_of(raw)
    }

    /// The opcode fetches of the loop at `loop_pc` inside one frame.
    ///
    /// The refresh register wraps every 128 fetches and cannot rank a
    /// frame of thousands of fetches. The core emits one Fetch event
    /// per opcode fetch instead, so the event totals measure the run
    /// pace.
    fn frame_fetches(machine: &mut Machine, loop_pc: u16) -> u64 {
        use std::cell::Cell;
        let count = Cell::new(0u64);
        machine.run_hook(
            Tick(69888 + 500),
            &mut |event| {
                if let demu_core::triggers::TriggerEvent::Fetch(address) = event
                    && address.0 as u16 == loop_pc
                {
                    count.set(count.get() + 1);
                }
                false
            },
            true,
        );
        count.get()
    }

    /// One pixel of a video device's published frame.
    fn pixel_of(machine: &Machine, device: &str, x: u32, y: u32) -> [u8; 3] {
        let f = machine
            .device_by_name(device)
            .and_then(|d| d.video())
            .expect("video device")
            .frame()
            .expect("published frame")
            .clone();
        let i = ((y * f.width + x) * 3) as usize;
        [f.pixels[i], f.pixels[i + 1], f.pixels[i + 2]]
    }

    /// Inject one input event at one device of the machine.
    fn inject_event(machine: &mut Machine, device: &str, event: &str) {
        let ok = machine
            .device_by_name_mut(device)
            .and_then(|d| d.input_mut())
            .map(|s| s.inject(event))
            .unwrap_or(false);
        assert!(ok, "the device {device} took the event {event}");
    }

    /// The frames published by one video device.
    fn frames_of(machine: &Machine, device: &str) -> u64 {
        machine
            .device_by_name(device)
            .and_then(|d| d.video())
            .map(|v| v.frames())
            .expect("frames")
    }

    /// The Sega mini machine: the core, the PSG, the pad with the
    /// named form, the work RAM, and the named-form VDP, over the
    /// two-bus shape with the mirrored port windows.
    fn sega_mini(vdp_kind: &str, pad_form: &str, extra: &str, image: &[u8]) -> Machine {
        let hex = hex_of(image);
        let raw = format!(
            r#"
        [system]
        name = "z80-sega-mini"
        triple = "z80-sega-sg1000"
        [clock.main]
        frequency = "10738635"
        [[device]]
        name = "cpu0"
        kind = "z80.cpu"
        domain = "main"
        [device.dividers]
        cpu = 3
        [device.params]
        bus = "cpu"
        io_bus = "io"
        [[device]]
        name = "vdp"
        kind = "{vdp_kind}"
        domain = "main"
        [device.dividers]
        dot = 2
        [[device]]
        name = "psg"
        kind = "sn76489.psg"
        domain = "main"
        [device.dividers]
        psg = 48
        [[device]]
        name = "pad"
        kind = "sega.io"
        domain = "main"
        [device.params]
        form = "{pad_form}"
        [[device]]
        name = "ram0"
        kind = "z80.ram"
        domain = "main"
        [device.params]
        size = 8192
        [[device]]
        name = "cart"
        kind = "sega.cart"
        domain = "main"
        [device.params]
        form = "sg1000"
        [[bus]]
        name = "cpu"
        address_bits = 16
        owner = "cpu0"
        [[bus]]
        name = "io"
        address_bits = 17
        owner = "cpu0"
        [[window]]
        name = "w_cart"
        bus = "cpu"
        device = "cart"
        base = "0x0000"
        size = "0x8000"
        [[window]]
        name = "w_ram"
        bus = "cpu"
        device = "ram0"
        base = "0xC000"
        size = "0x2000"
        mirror_stride = "0x2000"
        mirror_count = 1
        [[window]]
        name = "w_psg"
        bus = "io"
        device = "psg"
        base = "0x1007E"
        size = "0x0002"
        mirror_stride = "0x0100"
        mirror_count = 255
        [[window]]
        name = "w_vdp"
        bus = "io"
        device = "vdp"
        base = "0x100BE"
        size = "0x0002"
        mirror_stride = "0x0100"
        mirror_count = 255
        [[window]]
        name = "w_pad"
        bus = "io"
        device = "pad"
        base = "0x100DC"
        size = "0x0002"
        mirror_stride = "0x0100"
        mirror_count = 255
        [[window]]
        name = "w_start"
        bus = "io"
        device = "pad"
        base = "0x10000"
        size = "0x0001"
        [[connection]]
        from = "vdp.irq"
        to = "cpu0.int"
        semantics = "irq"
        [[connection]]
        from = "pad.nmi"
        to = "cpu0.nmi"
        semantics = "irq"
        {extra}
        [boot.cpu0]
        registers = {{ pc = "0x0000" }}
        [boot.cart]
        hex = "{hex}"
        [boot.ram0]
        pattern = "zero"
        "#
        );
        machine_of(raw)
    }

    /// Assemble a bundled machine and boot it.
    fn bundled(triple: &str) -> Machine {
        let raw = demu_config::bundled::resolve_source(triple).expect("bundled config");
        let (config, _) = demu_config::validate(&raw).expect("validate");
        let mut machine = Machine::assemble(config, &assemble::demu_ctors()).expect("assemble");
        machine.boot();
        machine
    }

    /// The Sinclair DI; HALT park image with the idle loop at 0x0066.
    const ZX_PARK: &str = "f3760000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000018fe";

    #[test]
    fn sg1000_paints_a_tms_tile() {
        // The program: the registers, the name entry, the pattern of
        // char 1, the cell color, and the park.
        let mut image = vec![
            0x3E, 0x80, 0xD3, 0xBF, // R3 = 0x80: the color table at 0x2000
            0x3E, 0x03, 0xD3, 0xBF, 0x3E, 0x0E, 0xD3, 0xBF, // R2 = 0x0E: the name at 0x3800
            0x3E, 0x02, 0xD3, 0xBF, 0x3E, 0x40, 0xD3, 0xBF, // R1 = 0x40: the display on
            0x3E, 0x01, 0xD3, 0xBF, 0x3E, 0x00, 0xD3, 0xBF, // the VRAM pointer set to 0x3800
            0x3E, 0xF8, 0xD3, 0xBF, 0x3E, 0x01, 0xD3, 0xBE, // the name entry 0 = char 1
            0x3E, 0x08, 0xD3, 0xBF, // the VRAM pointer at 0x0008
            0x3E, 0xC0, 0xD3, 0xBF,
        ];
        for v in [0x18u8, 0x3C, 0x7E, 0xFF, 0x7E, 0x3C, 0x18, 0x00] {
            image.extend_from_slice(&[0x3E, v, 0xD3, 0xBE]);
        }
        image.extend_from_slice(&[
            0x3E, 0x00, 0xD3, 0xBF, // the VRAM pointer at 0x2000 (the color cell 0)
            0x3E, 0xE0, 0xD3, 0xBF, 0x3E, 0x21, 0xD3, 0xBE, // fg 2, bg 1
            0x18, 0xFE, // park
        ]);
        let mut machine = sega_mini("tms9918.vdp", "sg1000", "", &image);
        machine.run_until(Tick(684 * 262 * 2));
        // The pattern row 0 = 0x18: pixels 3 and 4 carry the ink; the
        // pixel 0 stays on paper.
        assert_eq!(pixel_of(&machine, "vdp", 3, 0), [33, 200, 66]);
        assert_eq!(pixel_of(&machine, "vdp", 0, 0), [0, 0, 0]);
        assert_eq!(frames_of(&machine, "vdp"), 2);
        // The program parked at the JR byte itself.
        assert_eq!(cpu_pc_of(&machine), 0x0058, "the program parked");
    }

    #[test]
    fn sg1000_frame_irq_reaches_the_core() {
        // R1 = 0x60: the display on with the frame interrupt enabled;
        // EI; HALT; the handler at 0x0038 parks.
        let mut image = vec![
            0x3E, 0x60, 0xD3, 0xBF, 0x3E, 0x01, 0xD3, 0xBF, 0xFB, 0x76, 0x00, 0x00,
        ];
        let mut padded = vec![0u8; 0x40];
        padded[..image.len()].copy_from_slice(&image);
        image = padded;
        image[0x38] = 0x18;
        image[0x39] = 0xFE;
        let mut machine = sega_mini("tms9918.vdp", "sg1000", "", &image);
        machine.run_until(Tick(684 * 262 * 3));
        // The mode-0 service supplied RST 38h from the open port and
        // the handler parked.
        assert_eq!(cpu_pc_of(&machine), 0x0038, "the interrupt handler parked");
    }

    /// The Master System image: mode 4 with two CRAM entries, the
    /// name entry, a uniform tile, and the park.
    fn sms_image() -> Vec<u8> {
        let p = vec![
            0x3E, 0x04, 0xD3, 0xBF, // R0 = 0x04: mode 4
            0x3E, 0x00, 0xD3, 0xBF, 0x3E, 0x42, 0xD3,
            0xBF, // R1 = 0x42: display on, bit 1 set
            0x3E, 0x01, 0xD3, 0xBF, 0x3E, 0x0F, 0xD3, 0xBF, // R2 = 0x0F: the name base 0x3C00
            0x3E, 0x02, 0xD3, 0xBF, 0x3E, 0x04, 0xD3,
            0xBF, // R4 = 0x04: the tile pool at 0x2000
            0x3E, 0x04, 0xD3, 0xBF, 0x3E, 0x00, 0xD3, 0xBF, // the pointer at 0x0000: the CRAM
            0x3E, 0xC0, 0xD3, 0xBF, 0x3E, 0x15, 0xD3,
            0xBE, // the CRAM 0: the dim gray backdrop
            0x3E, 0x24, 0xD3, 0xBE, // the CRAM 1: teal
            0x3E, 0x0F, 0xD3, 0xBF, // the pointer at 0x000F: the CRAM 15
            0x3E, 0xC0, 0xD3, 0xBF, 0x3E, 0x36, 0xD3, 0xBE, // the CRAM 15: green-yellow
            0x3E, 0x00, 0xD3, 0xBF, // the name entry 0 = tile 1 (0x3C00)
            0x3E, 0xFC, 0xD3, 0xBF, 0x3E, 0x01, 0xD3, 0xBE, 0x3E, 0x20, 0xD3,
            0xBF, // the tile 1 pattern at 0x2020
            0x3E, 0xE0, 0xD3, 0xBF, 0x0E, 0x20, // C = 32
            0x3E, 0x88, // A = 0x88: bit 7 set in all four planes
            0xD3, 0xBE, 0x0D, 0x20, 0xFB, // the fill loop
            0x18, 0xFE, // park
        ];
        p
    }

    #[test]
    fn sms_paints_a_mode4_tile() {
        let mut machine = sega_mini("sms.vdp", "sms", "", &sms_image());
        machine.run_until(Tick(684 * 262 * 2));
        // Pixel 0 of the tile sits on CRAM 15; the transparent pixel
        // 1 shows the CRAM 0 backdrop.
        assert_eq!(pixel_of(&machine, "vdp", 0, 0), [170, 255, 85]);
        assert_eq!(pixel_of(&machine, "vdp", 1, 0), [85, 85, 85]);
    }

    fn gg_image() -> Vec<u8> {
        // The Game Gear keeps two byte entries: the CRAM 15 sits at
        // 0x001E and 0x001F.
        let mut image = sms_image();
        // Replace the sms one-byte CRAM 15 write with the two-byte
        // GG entry write: find the six bytes of the pointer set and
        // the data write, then splice the replacement before the
        // name-entry block.
        let needle = [
            0x3Eu8, 0x0F, 0xD3, 0xBF, 0x3E, 0xC0, 0xD3, 0xBF, 0x3E, 0x36, 0xD3, 0xBE,
        ];
        let at = image
            .windows(needle.len())
            .position(|w| w == needle)
            .expect("the CRAM 15 write in the SMS image");
        let replacement = [
            0x3Eu8, 0x1E, 0xD3, 0xBF, 0x3E, 0xC0, 0xD3, 0xBF, 0x3E, 0xF5, 0xD3, 0xBE, 0x3E, 0x0A,
            0xD3, 0xBE,
        ];
        image.splice(at..at + needle.len(), replacement);
        image
    }

    #[test]
    fn gg_publishes_the_160_by_144_window() {
        // The GG image plus the name fill: the window shows the
        // raster rows 24-167 and the columns 48-207, so the walk maps
        // them to the name rows 3-20 and the columns 6-25 of the
        // 32-wide table, the entries 96 through 671. The fill loop
        // stores tile 1 in every entry of that band, and the pattern
        // loop sets every plane bit of tile 1, so the walk gives the
        // color index 15 on every window column.
        let mut image = gg_image();
        let at = image.len() - 2; // before the park
        let extra = [
            0x3E, 0x60, 0xD3, 0xBF, // the pointer low byte 0x60
            0x3E, 0xFC, 0xD3, 0xBF, // the write code raises the pointer to 0x3C60
            0x01, 0x40, 0x02, // LD BC, 576, the 18 rows of 32 entries
            0x3E, 0x01, 0xD3, 0xBE, // LD A, 1 stores tile 1 through the data port
            0x0B, 0x78, 0xB1, // DEC BC, LD A,B, OR C
            0x20, 0xF7, // JR NZ, the store byte
            0x3E, 0x20, 0xD3, 0xBF, // the pointer low byte 0x20
            0x3E, 0xE0, 0xD3, 0xBF, // the write code raises the pointer to 0x2020
            0x01, 0x20, 0x00, // LD BC, 32, the 32 pattern bytes of the tile
            0x3E, 0xFF, 0xD3, 0xBE, // LD A, 0xFF sets every plane bit
            0x0B, 0x78, 0xB1, // DEC BC, LD A,B, OR C
            0x20, 0xF7, // JR NZ, the store byte
        ];
        image.splice(at..at, extra);
        let mut machine = sega_mini("gg.vdp", "gg", "", &image);
        machine.run_until(Tick(684 * 262 * 2));
        let f = machine
            .device_by_name("vdp")
            .and_then(|d| d.video())
            .and_then(|v| v.frame())
            .expect("frame")
            .clone();
        assert_eq!((f.width, f.height), (160, 144), "the Game Gear window");
        assert_eq!(pixel_of(&machine, "vdp", 0, 0), [170, 255, 85]);
        assert_eq!(pixel_of(&machine, "vdp", 159, 143), [170, 255, 85]);
    }

    #[test]
    fn sms_pause_pulls_the_nmi_handler() {
        let mut padded = vec![0u8; 0x68];
        padded[0] = 0xF3;
        padded[1] = 0x76;
        padded[0x66] = 0x18; // JR -2: the idle loop
        padded[0x67] = 0xFE;
        let mut machine = sega_mini("sms.vdp", "sms", "", &padded);
        machine.boot();
        // The pulse latches at the machine's current tick, so the
        // press lands before the run starts.
        inject_event(&mut machine, "pad", "press pause");
        machine.run_until(Tick(4000));
        inject_event(&mut machine, "pad", "release pause");
        assert_eq!(cpu_pc_of(&machine), 0x0066, "the NMI handler ran");
    }

    #[test]
    fn gg_start_reads_through_port_zero() {
        let mut machine = sega_mini("gg.vdp", "gg", "", &[0xF3u8, 0x76]);
        machine.boot();
        machine.run_until(Tick(1000));
        inject_event(&mut machine, "pad", "press start");
        assert_eq!(
            machine.peek("io", demu_core::Address(0x10000)),
            0x00,
            "START low"
        );
        inject_event(&mut machine, "pad", "release start");
        assert_eq!(
            machine.peek("io", demu_core::Address(0x10000)),
            0x80,
            "START idle"
        );
    }

    #[test]
    fn sn76489_streams_samples_after_a_tone_setup() {
        // Tone A at full volume: register 1 (the latch 0x82) takes
        // attenuation 0, and register 0 (0x81) takes the period data.
        let image = [
            0x3Eu8, 0x82, 0xD3, 0x7E, 0x3E, 0x00, 0xD3, 0x7E, 0x3E, 0x80, 0xD3, 0x7E, 0x3E, 0x01,
            0xD3, 0x7E, 0x18, 0xFE,
        ];
        let mut padded = vec![0u8; 0x40];
        padded[..image.len()].copy_from_slice(&image);
        let mut machine = sega_mini("sms.vdp", "sms", "", &padded);
        machine.run_until(Tick(40000));
        let drained = machine
            .device_by_name_mut("psg")
            .and_then(|d| {
                d.as_any_mut()
                    .downcast_mut::<demu_dev_audio::Sn76489>()
                    .map(|psg| psg.take_samples().len())
            })
            .unwrap_or(0);
        assert!(
            drained > 0,
            "the PSG streamed samples while the core parked"
        );
    }

    /// The Sinclair mini machine with the named form, the T-states
    /// per scanline, the ROM size, and the boot image replacing the
    /// ROM.
    fn sinclair_mini(form: &str, line_t: u32, rom_size: u32, image_hex: &str) -> Machine {
        let quirks = if form == "zx81" {
            "\n[quirks]
\"zx.zx81_slow\" = true
"
        } else {
            ""
        };
        let raw = format!(
            r#"
        [system]
        name = "zx-mini"
        triple = "z80-sinclair-zx81"
        [clock.main]
        frequency = "3250000"
        [[device]]
        name = "cpu0"
        kind = "z80.cpu"
        domain = "main"
        [device.dividers]
        cpu = 1
        [device.params]
        bus = "cpu"
        io_bus = "io"
        [[device]]
        name = "u0"
        kind = "zx.ula"
        domain = "main"
        [device.params]
        form = "{form}"
        line_t = {line_t}
        frame_lines = 310
        [[device]]
        name = "rom0"
        kind = "z80.rom"
        domain = "main"
        [device.params]
        size = {rom_size}
        [[device]]
        name = "ram0"
        kind = "z80.ram"
        domain = "main"
        [device.params]
        size = 4096
        [[bus]]
        name = "cpu"
        address_bits = 16
        owner = "cpu0"
        [[bus]]
        name = "io"
        address_bits = 17
        owner = "cpu0"
        [[window]]
        name = "w_ula"
        bus = "io"
        device = "u0"
        base = "0x10000"
        size = "0x10000"
        [[window]]
        name = "w_rom"
        bus = "cpu"
        device = "rom0"
        base = "0x0000"
        size = "{rom_size_hex:04X}"
        [[window]]
        name = "w_ram"
        bus = "cpu"
        device = "ram0"
        base = "0x4000"
        size = "0x1000"
        [[connection]]
        from = "u0.int"
        to = "cpu0.int"
        semantics = "irq"
        [[connection]]
        from = "u0.nmi"
        to = "cpu0.nmi"
        semantics = "irq"
        [[connection]]
        from = "cpu0.halt"
        to = "u0.halt"
        semantics = "display-sync"
        {quirks}
        [boot.cpu0]
        registers = {{ pc = "0x0000" }}
        [boot.rom0]
        hex = "{image_hex}"
        [boot.ram0]
        pattern = "zero"
        "#,
            rom_size_hex = rom_size,
        );
        machine_of(raw)
    }

    #[test]
    fn zx80_renders_the_dfile_and_parks() {
        let mut machine = sinclair_mini("zx80", 208, 4096, ZX_PARK);
        machine.boot();
        machine.run_until(Tick(310 * 208 * 2));
        // The frame publishes with the paper display; the idle loop
        // at 0x0066 parks the core at its JR address.
        assert_eq!(cpu_pc_of(&machine), 0x0066, "the idle loop parks the core");
        assert_eq!(
            pixel_of(&machine, "u0", 32, 24),
            [0, 0, 0],
            "a blank DFILE paints paper"
        );
    }

    #[test]
    fn zx81_block_graphics_paint_through_the_dfile() {
        let mut machine = sinclair_mini("zx81", 208, 8192, ZX_PARK);
        machine.boot();
        machine.run_until(Tick(2000));
        // The block char of bit 1 paints its top-right quarters of
        // the first char cell per the module's published mapping.
        assert!(machine.debug_write("cpu", demu_core::Address(0x4000), 0x42));
        machine.run_until(Tick(310 * 208));
        assert_eq!(pixel_of(&machine, "u0", 32 + 2, 24), [255, 255, 255]);
        assert_eq!(
            pixel_of(&machine, "u0", 32, 24),
            [0, 0, 0],
            "paper where the bit is clear"
        );
    }

    #[test]
    fn spectrum_border_changes_through_port_fe() {
        // A program in RAM: the border red through port FE, then the
        // park; the boot pc points at the RAM.
        let image = vec![0x3Eu8, 0x04, 0xD3, 0xFE, 0x18, 0xFE];
        let mut ram_hex = String::new();
        for b in &image {
            ram_hex.push_str(&format!("{b:02x}"));
        }
        // The 48 KiB RAM hex dump: 0x1000 zeros then the program at
        // 0x5000 = the RAM offset 0x1000; the boot pc = 0x5000.
        let pad = "0".repeat(0x1000 * 2);
        let ram_hex_padded = format!("{pad}{}", hex_of(&image));
        let raw = format!(
            r#"
        [system]
        name = "zx-spectrum-mini"
        triple = "z80-sinclair-spectrum"
        [clock.main]
        frequency = "3500000"
        [[device]]
        name = "cpu0"
        kind = "z80.cpu"
        domain = "main"
        [device.dividers]
        cpu = 1
        [device.params]
        bus = "cpu"
        io_bus = "io"
        contended = "0x4000-0x8000,0xE000-0x10000"
        io_contend = "0x40-0x80"
        contend_from = 14336
        contend_to = 57344
        contend_base = 14344
        [[device]]
        name = "u0"
        kind = "zx.ula"
        domain = "main"
        [device.params]
        form = "spectrum"
        line_t = 224
        frame_lines = 312
        [[device]]
        name = "rom0"
        kind = "z80.rom"
        domain = "main"
        [device.params]
        size = 16384
        [[device]]
        name = "ram_a"
        kind = "z80.ram"
        domain = "main"
        [device.params]
        size = 16384
        [[device]]
        name = "ram_b"
        kind = "z80.ram"
        domain = "main"
        [device.params]
        size = 16384
        [[device]]
        name = "ram_c"
        kind = "z80.ram"
        domain = "main"
        [device.params]
        size = 16384
        [[bus]]
        name = "cpu"
        address_bits = 16
        owner = "cpu0"
        [[bus]]
        name = "io"
        address_bits = 17
        owner = "cpu0"
        [[window]]
        name = "w_ula"
        bus = "io"
        device = "u0"
        base = "0x10000"
        size = "0x10000"
        [[window]]
        name = "w_rom"
        bus = "cpu"
        device = "rom0"
        base = "0x0000"
        size = "0x4000"
        [[window]]
        name = "w_ram_a"
        bus = "cpu"
        device = "ram_a"
        base = "0x4000"
        size = "0x4000"
        [[window]]
        name = "w_ram_b"
        bus = "cpu"
        device = "ram_b"
        base = "0x8000"
        size = "0x4000"
        [[window]]
        name = "w_ram_c"
        bus = "cpu"
        device = "ram_c"
        base = "0xC000"
        size = "0x4000"
        [[connection]]
        from = "u0.int"
        to = "cpu0.int"
        semantics = "irq"
        [[connection]]
        from = "cpu0.halt"
        to = "u0.halt"
        semantics = "display-sync"
        [boot.cpu0]
        registers = {{ pc = "0xD000" }}
        [boot.rom0]
        hex = "{ZX_PARK}"
        [boot.ram_a]
        pattern = "zero"
        [boot.ram_b]
        pattern = "zero"
        [boot.ram_c]
        hex = "{ram_hex_padded}"
        "#,
            ram_hex_padded = ram_hex_padded
        );
        let mut machine = machine_of(raw);
        machine.boot();
        machine.run_until(Tick(69888 + 1000));
        assert_eq!(
            pixel_of(&machine, "u0", 5, 5),
            [205, 0, 0],
            "the border paints red"
        );
    }

    #[test]
    fn spectrum_contended_run_lags_the_uncontended_run() {
        // The same JR -2 idle runs from the screen RAM (contended)
        // and from the upper RAM (uncontended).
        let mut contended = spectrum48_of("zx-contention-a", 0x4000, 0x00, "18fe");
        let mut clean = spectrum48_of("zx-contention-b", 0x9000, 0x00, "18fe");
        let f_c = frame_fetches(&mut contended, 0x4000);
        let f_u = frame_fetches(&mut clean, 0x9000);
        assert!(
            f_c < f_u,
            "the contended loop ran fewer fetches: {f_c} vs {f_u}"
        );
        // The lag clears the ten percent mark within one frame.
        let lag = 100 * (f_u - f_c) / f_u;
        assert!(lag > 10, "the lag under ten percent: {f_c} vs {f_u}");
    }

    #[test]
    fn spectrum_contended_port_run_lags_the_uncontended_port_run() {
        // Both idle loops sit in the contended screen RAM, so the
        // memory fetch delays cancel between the twins; the two ULA
        // port writes per iteration carry the difference. The port
        // writes are OUT (0xFE),A: the accumulator rides the port high
        // byte, and the port low byte 0xFE contends while 0x00 does
        // not.
        let mut contended = spectrum48_of("zx-io-contention-a", 0x4000, 0xFE, "d3fed3fe18fa");
        let mut clean = spectrum48_of("zx-io-contention-b", 0x4000, 0x00, "d300d30018fa");
        let f_c = frame_fetches(&mut contended, 0x4000);
        let f_u = frame_fetches(&mut clean, 0x4000);
        assert!(
            f_c < f_u,
            "the contended port loop ran fewer fetches: {f_c} vs {f_u}"
        );
        // The lag clears the ten percent mark within one frame.
        let lag = 100 * (f_u - f_c) / f_u;
        assert!(lag > 10, "the lag under ten percent: {f_c} vs {f_u}");
    }

    /// The TI-85 mini machine: the mem die, the LCD, and the link.
    fn ti85_mini(image: &[u8]) -> Machine {
        let hex = hex_of(image);
        let raw = format!(
            r#"
        [system]
        name = "ti85-mini"
        triple = "z80-ti-85"
        [clock.main]
        frequency = "6000000"
        [[device]]
        name = "cpu0"
        kind = "z80.cpu"
        domain = "main"
        [device.dividers]
        cpu = 1
        [device.params]
        bus = "cpu"
        io_bus = "io"
        [[device]]
        name = "mem0"
        kind = "ti85.mem"
        domain = "main"
        [[device]]
        name = "lcd0"
        kind = "ti85.lcd"
        domain = "main"
        [[device]]
        name = "link0"
        kind = "ti85.link"
        domain = "main"
        [[bus]]
        name = "cpu"
        address_bits = 16
        owner = "cpu0"
        [[bus]]
        name = "io"
        address_bits = 17
        owner = "cpu0"
        [[window]]
        name = "w_mem_low"
        bus = "cpu"
        device = "mem0"
        base = "0x0000"
        size = "0x4000"
        [[window]]
        name = "w_mem_mid"
        bus = "cpu"
        device = "mem0"
        base = "0x4000"
        size = "0x4000"
        [[window]]
        name = "w_mem_high"
        bus = "cpu"
        device = "mem0"
        base = "0x8000"
        size = "0x4000"
        [[window]]
        name = "w_mem_top"
        bus = "cpu"
        device = "mem0"
        base = "0xC000"
        size = "0x4000"
        [[window]]
        name = "w_display"
        bus = "cpu"
        device = "lcd0"
        base = "0xFC00"
        size = "0x0400"
        overlay = "lcd"
        overlay_read_priority = 1
        overlay_write_priority = 1
        [[window]]
        name = "w_driver"
        bus = "cpu"
        device = "lcd0"
        base = "0xFEC0"
        size = "0x0040"
        overlay = "lcd"
        overlay_read_priority = 2
        overlay_write_priority = 2
        [[window]]
        name = "w_link"
        bus = "io"
        device = "link0"
        base = "0x10005"
        size = "0x0001"
        [[window]]
        name = "w_memports"
        bus = "io"
        device = "mem0"
        base = "0x10006"
        size = "0x0003"
        [boot.cpu0]
        registers = {{ pc = "0x0000" }}
        [boot.mem0]
        hex = "{hex}"
        "#
        );
        machine_of(raw)
    }

    #[test]
    fn ti85_lcd_publishes_the_buffer_bits() {
        let mut machine = ti85_mini(&[0xF3u8, 0x76]);
        machine.boot();
        machine.run_until(Tick(1000));
        assert!(machine.debug_write("cpu", demu_core::Address(0xFC00), 0xF0));
        machine.run_until(Tick(10));
        // Bit 7 is the leftmost pixel: the columns 0-3 light.
        assert_eq!(pixel_of(&machine, "lcd0", 0, 0), [24, 84, 24]);
        assert_eq!(pixel_of(&machine, "lcd0", 4, 0), [232, 240, 232]);
    }

    #[test]
    fn ti85_mem_routes_the_banked_pages() {
        let mut machine = ti85_mini(&[0xF3u8, 0x76]);
        machine.boot();
        assert!(machine.debug_write("cpu", demu_core::Address(0x8000), 0xAA));
        assert_eq!(machine.peek("cpu", demu_core::Address(0x8000)), 0xAA);
        // The controller pages bank 1 there already; port 6 to bank 0
        // moves that page out.
        assert!(machine.debug_write("io", demu_core::Address(0x10006), 0x00));
        assert_eq!(
            machine.peek("cpu", demu_core::Address(0x8000)),
            0x00,
            "a banked page moved out"
        );
        assert!(machine.debug_write("io", demu_core::Address(0x10006), 0x01));
        assert_eq!(
            machine.peek("cpu", demu_core::Address(0x8000)),
            0xAA,
            "the page moved back in"
        );
        // The ROM page switch through port 8.
        assert!(machine.debug_write("io", demu_core::Address(0x10008), 0x02));
        assert_eq!(
            machine.peek("cpu", demu_core::Address(0x0000)),
            0x00,
            "page 2 is empty"
        );
    }

    #[test]
    fn ti85_link_levels_round_trip() {
        let mut machine = ti85_mini(&[0xF3u8, 0x76]);
        machine.boot();
        // Both lines output-enabled high: the read reports the drive
        // levels in bits 0-1.
        assert!(machine.debug_write("io", demu_core::Address(0x10005), 0x0F));
        assert_eq!(
            machine.peek("io", demu_core::Address(0x10005)),
            0x03,
            "the enabled drives read back"
        );
        // Both lines released: the remote levels ride in.
        assert!(machine.debug_write("io", demu_core::Address(0x10005), 0x00));
        inject_event(&mut machine, "link0", "link 0 1");
        assert_eq!(
            machine.peek("io", demu_core::Address(0x10005)),
            0x02,
            "the remote white low, red high"
        );
    }

    #[test]
    fn the_bundled_z80_configs_boot_and_park() {
        // The park addresses: the ZX80/ZX81 forms take their frame
        // non-maskable edges into the idle loop at 0x0066 and park
        // inside it; the rest park on the HALT at 0x0002.
        let parks = [
            ("z80-sega-sg1000", 0x0002),
            ("z80-sega-mastersystem", 0x0002),
            ("z80-sega-gamegear", 0x0002),
            ("z80-sinclair-zx80", 0x0066),
            ("z80-sinclair-zx81", 0x0066),
            ("z80-sinclair-spectrum", 0x0002),
            ("z80-sinclair-spectrum128", 0x0002),
            ("z80-ti-85", 0x0002),
        ];
        for (triple, park) in parks {
            let mut machine = bundled(triple);
            machine.run_until(Tick(400000));
            assert_eq!(cpu_pc_of(&machine), park, "{triple} parks");
        }
    }

    #[test]
    fn the_bundled_spectrums_show_the_frame_cadence() {
        // One 48K frame = 69,888 T-states; one 128K frame = 70,908.
        // The run includes the boundary at its own end tick: the frame
        // publishes land at t = 0 and at every frame length after.
        let mut s48 = bundled("z80-sinclair-spectrum");
        s48.run_until(Tick(69888 * 3));
        assert_eq!(frames_of(&s48, "u0"), 4);
        let mut s128 = bundled("z80-sinclair-spectrum128");
        s128.run_until(Tick(70908 * 3));
        assert_eq!(frames_of(&s128, "u0"), 4);
    }

    #[test]
    fn the_sinclair_pages_move_through_the_port_plane() {
        // The bundled 128K machine's controller takes the paging port
        // write over the port plane at $17FFD and moves banks.
        let raw = demu_config::bundled::resolve_source("z80-sinclair-spectrum128")
            .expect("variant config");
        let (config, _) = demu_config::validate(&raw).expect("validate");
        let mut machine = Machine::assemble(config, &assemble::demu_ctors()).expect("assemble");
        machine.boot();
        machine.run_until(Tick(1000));
        // Bank 1 pages in over the door.
        assert!(machine.debug_write("io", demu_core::Address(0x17FFD), 0x01));
        assert_eq!(
            machine.peek("cpu", demu_core::Address(0xC000)),
            0x00,
            "bank 1 idle"
        );
        assert!(machine.debug_write("cpu", demu_core::Address(0xC000), 0x42));
        assert_eq!(machine.peek("cpu", demu_core::Address(0xC000)), 0x42);
        // Bank 5 pages in; the door serves its own copy.
        assert!(machine.debug_write("io", demu_core::Address(0x17FFD), 0x05));
        assert_eq!(
            machine.peek("cpu", demu_core::Address(0xC000)),
            0x00,
            "bank 5 idle"
        );
        // The paging port answers $FF (write-only).
        assert_eq!(machine.peek("io", demu_core::Address(0x17FFD)), 0xFF);
    }

    #[test]
    fn spectrum_save_restore_continues_identically() {
        let mut machine = bundled("z80-sinclair-spectrum");
        machine.run_until(Tick(200000));
        let mut buf = Vec::new();
        machine.save(&mut buf).expect("save");
        let mut twin_a = bundled("z80-sinclair-spectrum");
        twin_a.load(&buf).expect("load a");
        let mut twin_b = bundled("z80-sinclair-spectrum");
        twin_b.load(&buf).expect("load b");
        twin_a.run_until(Tick(69888 * 2));
        twin_b.run_until(Tick(69888 * 2));
        let mut again = Vec::new();
        twin_a.save(&mut again).expect("save");
        let mut check = Vec::new();
        twin_b.save(&mut check).expect("save");
        assert_eq!(again, check, "the two restores continue identically");
    }

    #[test]
    fn zz81_save_restore_continues_identically() {
        let mut machine = bundled("z80-sinclair-zx81");
        machine.run_until(Tick(200000));
        let mut buf = Vec::new();
        machine.save(&mut buf).expect("save");
        // Both twins take the same save and run the same tail; the
        // byte comparison needs the same ticks on both sides.
        let mut twin_a = bundled("z80-sinclair-zx81");
        twin_a.load(&buf).expect("load a");
        let mut twin_b = bundled("z80-sinclair-zx81");
        twin_b.load(&buf).expect("load b");
        twin_a.run_until(Tick(310 * 208));
        twin_b.run_until(Tick(310 * 208));
        let mut again = Vec::new();
        twin_a.save(&mut again).expect("save");
        let mut check = Vec::new();
        twin_b.save(&mut check).expect("save");
        assert_eq!(again, check, "the restored ZX81 continues identically");
    }

    #[test]
    fn sms_save_restore_continues_identically() {
        let mut machine = bundled("z80-sega-mastersystem");
        machine.run_until(Tick(200000));
        let mut buf = Vec::new();
        machine.save(&mut buf).expect("save");
        let mut twin_a = bundled("z80-sega-mastersystem");
        twin_a.load(&buf).expect("load a");
        let mut twin_b = bundled("z80-sega-mastersystem");
        twin_b.load(&buf).expect("load b");
        twin_a.run_until(Tick(69888));
        twin_b.run_until(Tick(69888));
        let mut again = Vec::new();
        twin_a.save(&mut again).expect("save");
        let mut check = Vec::new();
        twin_b.save(&mut check).expect("save");
        assert_eq!(again, check, "the two restores continue identically");
    }
}

/// The 68000 machine group: the Mega Drive views, end to end over the
/// machine assembly.
#[cfg(test)]
pub mod genesis_machine_tests {
    use crate::assemble;
    use demu_core::Device;
    use demu_core::Tick;
    use demu_core::machine::Machine;

    /// Assemble and boot one machine from a bundled triple, with the
    /// test image as the positional cart form.
    fn machine_for(triple: &str, cart: Option<Vec<u8>>) -> Machine {
        let raw = demu_config::bundled::resolve_source(triple).expect("bundled config");
        let (config, warnings) = demu_config::validate(&raw).expect("config is valid");
        for w in warnings {
            eprintln!("warning: {} - {}", w.code, w.message);
        }
        let mut machine =
            Machine::assemble(config, &assemble::demu_ctors()).expect("machine assembles");
        if let Some(bytes) = cart {
            machine
                .override_boot("cart", demu_config::model::BootState::Hex { bytes })
                .expect("boot cart");
        }
        machine.boot();
        machine
    }

    /// A Mega Drive cart: the reset vectors name the program at $200
    /// and the program parks in a two-byte self branch.
    fn genesis_cart() -> Vec<u8> {
        let mut rom = vec![0xFFu8; 0x40000];
        // The initial supervisor stack pointer and program counter.
        rom[0..8].copy_from_slice(&[0x01, 0x02, 0x02, 0x00, 0x00, 0x00, 0x02, 0x00]);
        // The SEGA MEGA DRIVE header pad, then the park at $200.
        rom[0x200] = 0x60; // BRA.S over itself
        rom[0x201] = 0xFE;
        // The sound program region: the mapper's boot ROM form answers
        // the first 2K of the cart image at the sound bus; the stub
        // parks too (the machine's own boot hex handles it).
        rom
    }

    fn m68k_registers(machine: &Machine) -> std::collections::BTreeMap<String, u64> {
        machine
            .device_by_name("cpu0")
            .and_then(|d| d.cpu())
            .expect("cpu")
            .registers()
    }

    #[test]
    fn both_views_declare_the_published_clocks() {
        let machine = machine_for("m68000-sega-genesis", None);
        assert_eq!(machine.scheduler.primary_name(), "m68k");
        let m = machine.config.clocks["m68k"].frequency.num();
        assert_eq!(m, 53693175);
        assert_eq!(machine.config.clocks["m68k"].frequency.den(), 1);
        // The sound division reduces to the integer Z80 crystal.
        assert_eq!(machine.config.clocks["z80"].frequency.num(), 3579545);
        assert_eq!(machine.config.clocks["z80"].frequency.den(), 1);
        let machine = machine_for("z80-sega-genesis", None);
        assert_eq!(
            machine.scheduler.primary_name(),
            "sound",
            "the sound view paces on the Z80"
        );
    }

    #[test]
    fn the_68000_parks_at_the_cart_program() {
        let mut machine = machine_for("m68000-sega-genesis", Some(genesis_cart()));
        machine.run_until(Tick(2_000_000));
        let regs = m68k_registers(&machine);
        assert!(
            (0x200..=0x202).contains(&(regs["PC"] as u16 as u32)),
            "the core parks at the cart program, pc={:#06x}",
            regs["PC"]
        );
    }

    #[test]
    fn the_sound_core_boots_the_mapper_stub_and_parks() {
        let mut machine = machine_for("m68000-sega-genesis", Some(genesis_cart()));
        machine.run_until(Tick(2_000_000));
        let zpc = machine
            .device_by_name("soundcpu")
            .and_then(|d| d.cpu())
            .expect("sound cpu")
            .registers();
        assert_eq!(zpc["PC"], 0x3, "the stub's park lands on the JR");
    }

    /// The arbiter handoff: the 68000's bus request stops the sound
    /// core and the grant bit reads active low; the release wakes it.
    #[test]
    fn the_arbiter_handoff_holds_and_releases_the_sound_core() {
        let mut machine = machine_for("m68000-sega-genesis", Some(genesis_cart()));
        machine.run_until(Tick(1_000_000));
        // The Z80 sound core is parked in its boot stub.
        let running = machine
            .device_by_name_mut("soundcpu")
            .and_then(|d| d.as_any_mut().downcast_mut::<demu_cpu_z80::CpuDevice>())
            .map(|d| match d {
                demu_cpu_z80::CpuDevice::Z80(z) => !z.stalled && z.next_event().is_some(),
            })
            .expect("sound cpu");
        assert!(running, "the sound core runs its stub before the handoff");

        // The published request: $0100 = the bus request.
        assert!(machine.debug_write("cpu", demu_core::Address(0xA1_1100), 0x01));
        machine.run_until(Tick(1_100_000));
        let held = machine
            .device_by_name_mut("soundcpu")
            .and_then(|d| d.as_any_mut().downcast_mut::<demu_cpu_z80::CpuDevice>())
            .map(|d| match d {
                demu_cpu_z80::CpuDevice::Z80(z) => z.stalled,
            })
            .expect("sound cpu");
        assert!(held, "the request stalls the sound core");
        // The grant bit reads active low.
        assert_eq!(machine.peek("cpu", demu_core::Address(0xA1_1100)), 0x00);

        // The release write (bit 0 clear) wakes the parked core.
        assert!(machine.debug_write("cpu", demu_core::Address(0xA1_1100), 0x00));
        machine.run_until(Tick(1_200_000));
        let released = machine
            .device_by_name_mut("soundcpu")
            .and_then(|d| d.as_any_mut().downcast_mut::<demu_cpu_z80::CpuDevice>())
            .map(|d| match d {
                demu_cpu_z80::CpuDevice::Z80(z) => !z.stalled && z.next_event().is_some(),
            })
            .expect("sound cpu");
        assert!(released, "the release wakes the sound core");
        // The released grant bit reads high again (the arbiter's
        // active-low grant form).
        assert_eq!(
            machine.peek("cpu", demu_core::Address(0xA1_1100)),
            0x01,
            "the grant bit releases to the inactive-high form"
        );
    }

    #[test]
    fn the_mega_drive_runs_deterministically_and_restores() {
        let cart = genesis_cart();
        let mut machine = machine_for("m68000-sega-genesis", Some(cart.clone()));
        machine.run_until(Tick(1_500_000));
        // The arbiter's request stands mid-run: the save carries it.
        assert!(machine.debug_write("cpu", demu_core::Address(0xA1_1100), 0x01));
        machine.run_until(Tick(2_000_000));
        let mut buf = Vec::new();
        machine.save(&mut buf).expect("save");
        // Two machines restored from the same save continue
        // identically with the hold standing.
        let mut twin_a = machine_for("m68000-sega-genesis", Some(cart.clone()));
        twin_a.load(&buf).expect("load a");
        let mut twin_b = machine_for("m68000-sega-genesis", Some(cart.clone()));
        twin_b.load(&buf).expect("load b");
        twin_a.run_until(Tick(3_000_000));
        twin_b.run_until(Tick(3_000_000));
        let mut again = Vec::new();
        twin_a.save(&mut again).expect("save a");
        let mut check = Vec::new();
        twin_b.save(&mut check).expect("save b");
        assert_eq!(again, check, "the two restores continue identically");
    }
}

/// The Neo Geo AES machine group: the 68000 view and the sound view
/// over the machine assembly.
#[cfg(test)]
pub mod neogeo_machine_tests {
    use crate::assemble;
    use demu_core::Tick;
    use demu_core::machine::Machine;

    /// Assemble and boot one machine from a bundled triple, with the
    /// P ROM image as the positional cart form.
    fn machine_for(triple: &str, prom: Option<Vec<u8>>) -> Machine {
        let raw = demu_config::bundled::resolve_source(triple).expect("bundled config");
        let (config, warnings) = demu_config::validate(&raw).expect("config is valid");
        for w in warnings {
            eprintln!("warning: {} - {}", w.code, w.message);
        }
        let mut machine =
            Machine::assemble(config, &assemble::demu_ctors()).expect("machine assembles");
        if let Some(bytes) = prom {
            machine
                .override_boot("cart", demu_config::model::BootState::Hex { bytes })
                .expect("boot cart");
        }
        machine.boot();
        machine
    }

    /// A two-MiB P ROM: the first MiB carries the park form at its
    /// start (the vectors stay informational in the preset form), the
    /// second MiB carries a marker byte at its start for the banked
    /// window.
    fn neogeo_prom() -> Vec<u8> {
        let mut rom = vec![0xFFu8; 2 * 0x100000];
        rom[0x000] = 0x60; // BRA.S over itself, the park form
        rom[0x001] = 0xFE;
        rom[0x100000] = 0xA5; // The second MiB's marker byte
        rom
    }

    fn m68k_registers(machine: &Machine) -> std::collections::BTreeMap<String, u64> {
        machine
            .device_by_name("cpu0")
            .and_then(|d| d.cpu())
            .expect("cpu")
            .registers()
    }

    #[test]
    fn the_aes_boots_to_the_bios_park() {
        let mut machine = machine_for("m68000-neogeo-aes", Some(neogeo_prom()));
        machine.run_until(Tick(9_000_000));
        let regs = m68k_registers(&machine);
        assert!(
            (0xC00000..=0xC00004).contains(&regs["PC"]),
            "the bios stub parks, pc={:#08x}",
            regs["PC"]
        );
        // The LSPC publishes frames over the parked run: the 264-line
        // NTSC frame over the 24 MHz master's /2 runs about 24 frames
        // in this window.
        let frames = machine
            .device_by_name("lspc")
            .and_then(|d| d.video())
            .map(|v| v.frames())
            .expect("lspc");
        assert!(frames > 10, "frames={frames}");
    }

    #[test]
    fn the_sound_view_paces_on_the_z80_and_reads_the_sound_bus_by_default() {
        let machine = machine_for("z80-neogeo-aes", None);
        assert_eq!(machine.scheduler.primary_name(), "z80");
        assert_eq!(machine.primary_bus(), Some("sound"));
        // The shell's registers command reads the first cpu device,
        // which the sound view declares as the Z80.
        let regs = machine
            .devices
            .iter()
            .find_map(|d| d.cpu())
            .expect("a cpu")
            .registers();
        assert!(regs.contains_key("PC"), "the z80 reports its registers");
    }

    #[test]
    fn controller_injection_reaches_the_published_bits() {
        let mut machine = machine_for("m68000-neogeo-aes", None);
        {
            let Some(io) = machine.device_by_name_mut("io") else {
                panic!("io exists");
            };
            let sink = io.input_mut().expect("input sink");
            assert!(sink.inject("p1 press 8"));
        }
        // A pressed button reads as a cleared bit in the active-low
        // form: bit 3 (Right) reads low, the rest read high.
        let byte = machine.peek("cpu", demu_core::Address(0x300000));
        assert_eq!(
            byte, 0xF7,
            "the pressed bit 3 reads low and the idle bits read high"
        );
    }

    #[test]
    fn the_sound_latch_crosses_and_the_reply_returns() {
        let mut machine = machine_for("m68000-neogeo-aes", Some(neogeo_prom()));
        // The 68k writes a command byte and the Z80 writes a reply
        // through its port form. Both crossings arm one die tick
        // after the write; issuing both before the long run keeps
        // both arms inside the run's real-time window (the sound die
        // sits on the slower Z80 domain, so an arm written after the
        // run began would name a tick past the run's end).
        assert!(machine.debug_write("cpu", demu_core::Address(0x320000), 0x42));
        assert!(machine.debug_write("soundio", demu_core::Address(0x1000C), 0x77));
        machine.run_until(Tick(2_000_000));
        let latch = machine.peek("soundio", demu_core::Address(0x10000));
        assert_eq!(latch, 0x42, "the command crossed to the sound side");
        let reply = machine.peek("cpu", demu_core::Address(0x320000));
        assert_eq!(reply, 0x77, "the reply crossed back to the 68k");
    }

    #[test]
    fn the_machine_runs_deterministically_and_restores() {
        let prom = neogeo_prom();
        let mut machine = machine_for("m68000-neogeo-aes", Some(prom.clone()));
        machine.run_until(Tick(2_000_000));
        let mut buf = Vec::new();
        machine.save(&mut buf).expect("save");
        // Two machines restored from the same save continue
        // identically.
        let mut twin_a = machine_for("m68000-neogeo-aes", Some(prom.clone()));
        twin_a.load(&buf).expect("load a");
        let mut twin_b = machine_for("m68000-neogeo-aes", Some(prom.clone()));
        twin_b.load(&buf).expect("load b");
        twin_a.run_until(Tick(3_000_000));
        twin_b.run_until(Tick(3_000_000));
        let mut again = Vec::new();
        twin_a.save(&mut again).expect("save a");
        let mut check = Vec::new();
        twin_b.save(&mut check).expect("save b");
        assert_eq!(again, check, "the two restores continue identically");
    }
}
