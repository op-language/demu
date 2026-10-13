//! The text REPL renderer.
//!
//! One command per line. `help` lists the command families. The renderer
//! reads lines from `input` and writes results to `output`; `quit` and
//! end-of-input end the session.

use std::io::{BufRead, Write};

use crate::service::{CommandError, MachineService};
use crate::split_command;

/// Run the text REPL until end of input or a `quit` command.
pub fn run(
    service: &mut MachineService,
    input: &mut dyn BufRead,
    output: &mut dyn Write,
) -> std::io::Result<()> {
    loop {
        write!(output, "demu> ")?;
        output.flush()?;
        let mut line = String::new();
        if input.read_line(&mut line)? == 0 {
            writeln!(output, "bye")?;
            output.flush()?;
            return Ok(());
        }
        let (cmd, args) = split_command(&line);
        if cmd == "quit" || cmd == "exit" {
            writeln!(output, "bye")?;
            output.flush()?;
            return Ok(());
        }
        match execute(service, &cmd, &args) {
            Ok(out) => writeln!(output, "{}", out.text())?,
            Err(e) => writeln!(output, "error: {e}")?,
        }
        output.flush()?;
    }
}

impl CommandOutputText for crate::service::CommandOutput {
    fn text(&self) -> String {
        match self {
            crate::service::CommandOutput::Text { text, .. } => text.clone(),
        }
    }
}

/// The text form of a command output.
pub trait CommandOutputText {
    /// The human-readable text.
    fn text(&self) -> String;
}

/// Execute one text command line.
///
/// # Errors
///
/// Returns the command error for the caller to report.
pub fn execute(
    service: &mut MachineService,
    cmd: &str,
    args: &str,
) -> Result<crate::service::CommandOutput, CommandError> {
    dispatch(service, cmd, args)
}

/// The command dispatch shared by both renderers.
pub fn dispatch(
    service: &mut MachineService,
    cmd: &str,
    args: &str,
) -> Result<crate::service::CommandOutput, CommandError> {
    let words: Vec<&str> = args.split_whitespace().collect();
    match cmd {
        "info" => service.info().map(info_text),
        "status" => service.info().map(info_text),
        "regs" => service.regs(),
        "set-reg" => {
            let [name, value] = words.as_slice() else {
                return Err(CommandError::BadArgs(
                    "usage: set-reg <name> <value>".into(),
                ));
            };
            let value = demu_config::model::parse_number(value)
                .ok_or_else(|| CommandError::BadArgs(format!("not a number: {value}")))?;
            service.set_reg(name, value)
        }
        "read" => {
            let [addr] = words.as_slice() else {
                return Err(CommandError::BadArgs("usage: read <addr>".to_string()));
            };
            service.read(addr, 16)
        }
        "read-n" => {
            let [addr, n] = words.as_slice() else {
                return Err(CommandError::BadArgs(
                    "usage: read-n <addr> <n>".to_string(),
                ));
            };
            let n = demu_config::model::parse_number(n)
                .ok_or_else(|| CommandError::BadArgs(format!("not a number: {n}")))?;
            service.read(addr, n as u32)
        }
        "write" => {
            let [addr, rest @ ..] = words.as_slice() else {
                return Err(CommandError::BadArgs(
                    "usage: write <addr> <bytes>".to_string(),
                ));
            };
            let bytes = hex_bytes(rest).ok_or_else(|| {
                CommandError::BadArgs("bytes must be pairs of hex digits".to_string())
            })?;
            service.write(addr, &bytes)
        }
        "run" => {
            let units = match words.first() {
                Some(text) => demu_config::model::parse_number(text)
                    .ok_or_else(|| CommandError::BadArgs(format!("not a number: {text}")))?,
                None => u64::MAX,
            };
            if units == u64::MAX {
                service.continue_run()
            } else {
                service.run(units)
            }
        }
        "step" => {
            let units = match words.first() {
                Some(text) => demu_config::model::parse_number(text)
                    .ok_or_else(|| CommandError::BadArgs(format!("not a number: {text}")))?,
                None => 1,
            };
            service.step(units)
        }
        "run-until" => {
            let spec = args.trim();
            if spec.is_empty() {
                return Err(CommandError::BadArgs(
                    "usage: run-until vblank|irq|fetch <addr>|read <addr>|write <addr>".into(),
                ));
            }
            service.run_until(spec)
        }
        "run-frame" => {
            let frames = match words.first() {
                Some(text) => demu_config::model::parse_number(text)
                    .ok_or_else(|| CommandError::BadArgs(format!("not a number: {text}")))?,
                None => 1,
            };
            service.run_frame(frames)
        }
        "break" => {
            let [token] = words.as_slice() else {
                return Err(CommandError::BadArgs(
                    "usage: break <addr|symbol> [cond]".to_string(),
                ));
            };
            let id = service.break_at(token, None)?;
            Ok(crate::service::CommandOutput::plain(format!(
                "trigger {id} set"
            )))
        }
        "dump" => {
            let [addr, n, path] = words.as_slice() else {
                return Err(CommandError::BadArgs(
                    "usage: dump <addr> <n> <file>".to_string(),
                ));
            };
            let n = demu_config::model::parse_number(n)
                .ok_or_else(|| CommandError::BadArgs(format!("not a number: {n}")))?;
            let n = u32::try_from(n).map_err(|_| CommandError::BadArgs("n too large".into()))?;
            service.dump_file(addr, n, path)
        }
        "fill" => {
            let [addr, n, byte] = words.as_slice() else {
                return Err(CommandError::BadArgs(
                    "usage: fill <addr> <n> <byte>".to_string(),
                ));
            };
            let n = demu_config::model::parse_number(n)
                .ok_or_else(|| CommandError::BadArgs(format!("not a number: {n}")))?;
            let n = u32::try_from(n).map_err(|_| CommandError::BadArgs("n too large".into()))?;
            let byte = demu_config::model::parse_number(byte)
                .ok_or_else(|| CommandError::BadArgs(format!("not a number: {byte}")))?;
            let byte =
                u8::try_from(byte).map_err(|_| CommandError::BadArgs("byte too large".into()))?;
            service.fill(addr, n, byte)
        }
        "screenshot" => {
            let mut path: Option<String> = None;
            let mut png = false;
            for word in words {
                match word {
                    "--png" => png = true,
                    "--ppm" => {}
                    other => {
                        if path.is_some() {
                            return Err(CommandError::BadArgs(
                                "usage: screenshot [--ppm|--png] <path>".into(),
                            ));
                        }
                        path = Some(other.to_string());
                    }
                }
            }
            let Some(path) = path else {
                return Err(CommandError::BadArgs(
                    "usage: screenshot [--ppm|--png] <path>".into(),
                ));
            };
            let format = if png {
                Some(crate::service::ShotFormat::Png)
            } else {
                None
            };
            service.screenshot(&path, format)
        }
        "dump-vram" | "dump-nametable" | "dump-oam" | "dump-palette" | "dump-framebuffer" => {
            let [path] = words.as_slice() else {
                return Err(CommandError::BadArgs(format!("usage: {cmd} <path>")));
            };
            let layer = match cmd {
                "dump-vram" => demu_core::VideoLayer::Vram,
                "dump-nametable" => demu_core::VideoLayer::NameTable,
                "dump-oam" => demu_core::VideoLayer::Oam,
                "dump-palette" => demu_core::VideoLayer::Palette,
                _ => demu_core::VideoLayer::Framebuffer,
            };
            service.dump_layer(layer, path)
        }
        "inject" => {
            let [device, event @ ..] = words.as_slice() else {
                return Err(CommandError::BadArgs(
                    "usage: inject <device> <event>".to_string(),
                ));
            };
            service.inject(device, &event.join(" "))
        }
        "script" => {
            let [path] = words.as_slice() else {
                return Err(CommandError::BadArgs("usage: script <file>".to_string()));
            };
            let text =
                std::fs::read_to_string(path).map_err(|e| CommandError::Io(e.to_string()))?;
            service.load_script(&text)
        }
        "mkv-input" => {
            let (positional, out) =
                positional_and_output(&words, "usage: mkv-input <script> -o <file>")?;
            let [script_path] = positional.as_slice() else {
                return Err(CommandError::BadArgs(
                    "usage: mkv-input <script> -o <file>".to_string(),
                ));
            };
            let Some(out_path) = out else {
                return Err(CommandError::BadArgs(
                    "usage: mkv-input <script> -o <file>".to_string(),
                ));
            };
            service.mkv_input(script_path, out_path)
        }
        "mkv-render" => {
            let (positional, out) = positional_and_output(
                &words,
                "usage: mkv-render <input.mkv> <frames> -o <output.mkv>",
            )?;
            let [input_path, frames] = positional.as_slice() else {
                return Err(CommandError::BadArgs(
                    "usage: mkv-render <input.mkv> <frames> -o <output.mkv>".to_string(),
                ));
            };
            let frame_count = demu_config::model::parse_number(frames)
                .ok_or_else(|| CommandError::BadArgs(format!("not a number: {frames}")))?;
            let Some(out_path) = out else {
                return Err(CommandError::BadArgs(
                    "usage: mkv-render <input.mkv> <frames> -o <output.mkv>".to_string(),
                ));
            };
            service.mkv_render(input_path, frame_count, out_path)
        }
        "render-session" => {
            let (positional, out) =
                positional_and_output(&words, "usage: render-session <frames> -o <file>")?;
            let frames = match positional.as_slice() {
                [] => 0,
                [frames] => demu_config::model::parse_number(frames)
                    .ok_or_else(|| CommandError::BadArgs(format!("not a number: {frames}")))?,
                _ => {
                    return Err(CommandError::BadArgs(
                        "usage: render-session <frames> -o <file>".to_string(),
                    ));
                }
            };
            let Some(out_path) = out else {
                return Err(CommandError::BadArgs(
                    "usage: render-session <frames> -o <file>".to_string(),
                ));
            };
            service.render_session(frames, out_path)
        }
        "watch" => {
            let [token] = words.as_slice() else {
                return Err(CommandError::BadArgs(
                    "usage: watch <addr> [cond]".to_string(),
                ));
            };
            let id = service.watch(token, None)?;
            Ok(crate::service::CommandOutput::plain(format!(
                "trigger {id} set"
            )))
        }
        "triggers" => {
            let list = service
                .triggers()
                .all()
                .iter()
                .map(|t| format!("{}: {:?}", t.id, t.kind))
                .collect::<Vec<_>>();
            Ok(crate::service::CommandOutput::plain(list.join("\n")))
        }
        "clear" => {
            let [id_text] = words.as_slice() else {
                return Err(CommandError::BadArgs("usage: clear <id>".to_string()));
            };
            let id = demu_config::model::parse_number(id_text)
                .ok_or_else(|| CommandError::BadArgs(format!("not a number: {id_text}")))?;
            let removed = service.triggers_mut().clear(id);
            Ok(crate::service::CommandOutput::plain(
                if removed {
                    "cleared"
                } else {
                    "no such trigger"
                }
                .to_string(),
            ))
        }
        "save-state" => {
            let [path] = words.as_slice() else {
                return Err(CommandError::BadArgs(
                    "usage: save-state <file>".to_string(),
                ));
            };
            let machine = service.machine_mut().ok_or(CommandError::NoMachine)?;
            let mut file =
                std::fs::File::create(path).map_err(|e| CommandError::Io(e.to_string()))?;
            machine
                .save(&mut file)
                .map_err(|e| CommandError::Io(e.to_string()))?;
            Ok(crate::service::CommandOutput::plain(format!(
                "saved to {path}"
            )))
        }
        "load-state" => {
            let [path] = words.as_slice() else {
                return Err(CommandError::BadArgs(
                    "usage: load-state <file>".to_string(),
                ));
            };
            let buf = std::fs::read(path).map_err(|e| CommandError::Io(e.to_string()))?;
            let machine = service.machine_mut().ok_or(CommandError::NoMachine)?;
            machine
                .load(&buf)
                .map_err(|e| CommandError::Failed(e.to_string()))?;
            Ok(crate::service::CommandOutput::plain(format!(
                "loaded from {path}"
            )))
        }
        "symbols" => {
            let [path] = words.as_slice() else {
                return Err(CommandError::BadArgs(
                    "usage: symbols <file.opl>".to_string(),
                ));
            };
            let text =
                std::fs::read_to_string(path).map_err(|e| CommandError::Io(e.to_string()))?;
            let table = demu_symbols::SymbolTable::parse(&text)
                .map_err(|e| CommandError::Failed(e.to_string()))?;
            service.load_symbols(&table);
            Ok(crate::service::CommandOutput::plain(format!(
                "loaded {} symbols",
                table.entries().count()
            )))
        }
        "sym" => {
            let [name] = words.as_slice() else {
                return Err(CommandError::BadArgs("usage: sym <name>".to_string()));
            };
            let symbols = service.symbols().ok_or(CommandError::Failed(
                "no symbol table is loaded".to_string(),
            ))?;
            match symbols.by_name(name) {
                Some(entry) => Ok(crate::service::CommandOutput::plain(format!(
                    "{} = {:#x} ({} {})",
                    entry.name, entry.address, entry.kind, entry.section
                ))),
                None => Err(CommandError::BadAddress(name.to_string())),
            }
        }
        "quit" | "exit" => Ok(crate::service::CommandOutput::plain("bye")),
        other => Err(CommandError::BadArgs(format!("unknown command: {other}"))),
    }
}

/// Parse the hex digits of the word tokens into bytes.
///
/// The tokens concatenate in order and internal whitespace drops. An
/// odd digit count, or any digit outside `0`-`9` `a`-`f` `A`-`F`,
/// rejects, and the result carries one byte per digit pair.
pub fn hex_bytes(words: &[&str]) -> Option<Vec<u8>> {
    let text: String = words.concat();
    let text: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    if !text.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(text.len() / 2);
    for i in (0..text.len()).step_by(2) {
        let byte = u8::from_str_radix(&text[i..i + 2], 16).ok()?;
        out.push(byte);
    }
    Some(out)
}

/// Split the `-o <path>` output flag out of the words; the rest are
/// positional.
fn positional_and_output<'a>(
    words: &[&'a str],
    usage: &str,
) -> Result<(Vec<&'a str>, Option<&'a str>), CommandError> {
    let mut positional = Vec::new();
    let mut out = None;
    let mut i = 0usize;
    while i < words.len() {
        match words[i] {
            "-o" => {
                if out.is_some() {
                    return Err(CommandError::BadArgs(format!("only one -o: {usage}")));
                }
                let path = words
                    .get(i + 1)
                    .ok_or_else(|| CommandError::BadArgs(usage.to_string()))?;
                out = Some(*path);
                i += 2;
            }
            other => {
                positional.push(other);
                i += 1;
            }
        }
    }
    Ok((positional, out))
}

fn info_text(json: serde_json::Value) -> crate::service::CommandOutput {
    let name = json["name"].as_str().unwrap_or_default();
    let triple = json["triple"].as_str().unwrap_or_default();
    let now = json["now"].as_u64().unwrap_or(0);
    crate::service::CommandOutput::Text {
        text: format!("{name} ({triple}) at tick {now}"),
        json,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use demu_core::machine::Machine;

    fn service() -> MachineService {
        let mut s = MachineService::new();
        s.load_machine(Machine::new(demu_config::smoke_config().unwrap()).unwrap());
        s.machine_mut().unwrap().boot();
        s
    }

    #[test]
    fn split_command_forms() {
        assert_eq!(
            split_command("  read 0x8000 16 "),
            ("read".to_string(), "0x8000 16".to_string())
        );
        assert_eq!(
            split_command("status"),
            ("status".to_string(), String::new())
        );
    }

    #[test]
    fn info_reports_machine() {
        let mut s = service();
        let out = dispatch(&mut s, "info", "").unwrap();
        assert!(out.text().contains("smoke"));
    }

    #[test]
    fn read_write_round_trip() {
        let mut s = service();
        dispatch(&mut s, "write", "0x8000 DEADBEEF").unwrap();
        let out = dispatch(&mut s, "read-n", "0x8000 4").unwrap();
        assert!(out.text().contains("DE AD BE EF"));
    }

    #[test]
    fn run_and_regs() {
        let mut s = service();
        dispatch(&mut s, "run", "100").unwrap();
        let out = dispatch(&mut s, "regs", "").unwrap();
        assert!(out.text().contains("ip="));
    }

    #[test]
    fn unknown_command_is_reported() {
        let mut s = service();
        match dispatch(&mut s, "frobnicate", "") {
            Err(CommandError::BadArgs(msg)) => assert!(msg.contains("unknown command")),
            other => panic!("expected BadArgs, got {other:?}"),
        }
    }

    #[test]
    fn no_machine_error() {
        let mut s = MachineService::new();
        match dispatch(&mut s, "regs", "") {
            Err(CommandError::NoMachine) => {}
            other => panic!("expected NoMachine, got {other:?}"),
        }
    }

    #[test]
    fn repl_pipe_session() {
        let mut s = service();
        let mut input = "run 50\nregs\nquit\n".as_bytes();
        let mut out = Vec::new();
        run(&mut s, &mut input, &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("ran to"));
        assert!(text.contains("ip="));
        assert!(text.ends_with("bye\n"));
    }
}
