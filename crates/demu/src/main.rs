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
