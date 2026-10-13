//! The JSON-lines renderer.
//!
//! One request object per line, one response object per line. The request
//! `id` is copied to the response. Unknown commands produce a typed error
//! object; every command of the text REPL is available.

use std::io::{BufRead, Write};

use crate::protocol::{self, ErrorCode, Request, Response};
use crate::service::MachineService;
use crate::text::dispatch;

/// The argument text for the dispatcher: a JSON string is passed as-is, a
/// scalar is rendered bare, and an array joins its stringifiable items
/// with spaces, matching the text REPL's whitespace-separated form.
fn args_text(args: &serde_json::Value) -> String {
    match args {
        serde_json::Value::Null => String::new(),
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(items) => {
            items.iter().map(scalar_text).collect::<Vec<_>>().join(" ")
        }
        other => other.to_string(),
    }
}

/// The text of one JSON scalar: strings bare, numbers and booleans
/// rendered.
fn scalar_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Execute one request object.
pub fn handle(service: &mut MachineService, req: &Request) -> Response {
    let id = req.id.clone();
    let result = dispatch(service, &req.cmd, &args_text(&req.args));
    match result {
        Ok(out) => match out {
            crate::service::CommandOutput::Text { json, .. } => protocol::ok(id, json),
        },
        Err(e) => protocol::err(id, e.error_code(), e.to_string()),
    }
}

/// Run the JSON-lines loop over a reader and writer until end of input.
pub fn run(
    service: &mut MachineService,
    input: &mut dyn BufRead,
    output: &mut dyn Write,
) -> std::io::Result<()> {
    loop {
        let mut line = String::new();
        if input.read_line(&mut line)? == 0 {
            return Ok(());
        }
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Request>(&line) {
            Ok(req) => handle(service, &req),
            Err(e) => protocol::err(
                serde_json::Value::Null,
                ErrorCode::BadArgs,
                format!("malformed request: {e}"),
            ),
        };
        let line = serde_json::to_string(&response).expect("a protocol response serializes");
        writeln!(output, "{line}")?;
        output.flush()?;
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
    fn handle_reports_ok_with_data() {
        let mut s = service();
        let req = Request {
            id: serde_json::json!(1),
            cmd: "info".into(),
            args: serde_json::json!(null),
        };
        let resp = handle(&mut s, &req);
        assert!(resp.ok);
        assert_eq!(resp.id, serde_json::json!(1));
        assert!(resp.data.is_some());
    }

    #[test]
    fn handle_reports_typed_errors() {
        let mut s = service();
        let req = Request {
            id: serde_json::json!(2),
            cmd: "frobnicate".into(),
            args: serde_json::json!(null),
        };
        let resp = handle(&mut s, &req);
        assert!(!resp.ok);
        assert_eq!(resp.error.unwrap().code, ErrorCode::BadArgs);
    }

    #[test]
    fn read_command_returns_bytes() {
        let mut s = service();
        let req = Request {
            id: serde_json::json!(3),
            cmd: "read".into(),
            args: serde_json::json!("0x8000"),
        };
        let resp = handle(&mut s, &req);
        assert!(resp.ok);
        assert!(resp.data.unwrap()["bytes"].as_str().unwrap().contains("00"));
    }

    #[test]
    fn lines_loop_framing() {
        let mut s = service();
        let mut input =
            "{\"id\":1,\"cmd\":\"run\",\"args\":50}\n{\"id\":2,\"cmd\":\"regs\"}\n".as_bytes();
        let mut out = Vec::new();
        run(&mut s, &mut input, &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        // Two response lines, one object each.
        assert_eq!(text.lines().count(), 2);
        assert!(text.lines().all(|l| l.starts_with('{')));
        assert!(text.contains("\"ok\":true"));
    }

    #[test]
    fn malformed_line_produces_error_response() {
        let mut s = service();
        let mut input = "this is not json\n".as_bytes();
        let mut out = Vec::new();
        run(&mut s, &mut input, &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("malformed request"));
    }
}
