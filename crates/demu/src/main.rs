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
        // The core stopped on the copied STP byte: the stopped run
        // returned, with the park state holding the stop byte's
        // position.
        assert!(machine.now() < Tick(4 * 200), "the stopped run ended early");
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
