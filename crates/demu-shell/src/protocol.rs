//! The JSON-lines protocol types.
//!
//! One request object per line, one response object per line. The `id`
//! value is copied from request to response. Errors carry a stable
//! machine-readable code from a closed set plus a human-readable message.

use serde::{Deserialize, Serialize};

/// One protocol request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    /// The request id; copied to the response.
    #[serde(default)]
    pub id: serde_json::Value,
    /// The command name.
    pub cmd: String,
    /// The command arguments.
    #[serde(default)]
    pub args: serde_json::Value,
}

/// One protocol response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    /// The request id.
    pub id: serde_json::Value,
    /// True on success.
    pub ok: bool,
    /// The result data on success.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    /// The error object on failure.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ProtocolError>,
}

/// One protocol error object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolError {
    /// The stable machine-readable code.
    pub code: ErrorCode,
    /// The human-readable message.
    pub message: String,
}

/// The closed set of stable error codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// The command name is unknown.
    UnknownCommand,
    /// The arguments do not match the command schema.
    BadArgs,
    /// The address or symbol does not resolve.
    BadAddress,
    /// The trigger text does not parse.
    BadTrigger,
    /// The device name is unknown.
    UnknownDevice,
    /// The register name is unknown.
    UnknownRegister,
    /// No machine is loaded.
    NoMachine,
    /// The file operation failed.
    Io,
    /// The command failed for a run-time reason.
    Failed,
}

impl ErrorCode {
    /// The error code as a string.
    pub fn as_str(&self) -> &'static str {
        match self {
            ErrorCode::UnknownCommand => "unknown_command",
            ErrorCode::BadArgs => "bad_args",
            ErrorCode::BadAddress => "bad_address",
            ErrorCode::BadTrigger => "bad_trigger",
            ErrorCode::UnknownDevice => "unknown_device",
            ErrorCode::UnknownRegister => "unknown_register",
            ErrorCode::NoMachine => "no_machine",
            ErrorCode::Io => "io",
            ErrorCode::Failed => "failed",
        }
    }
}

/// Build a success response.
pub fn ok(id: serde_json::Value, data: serde_json::Value) -> Response {
    Response {
        id,
        ok: true,
        data: Some(data),
        error: None,
    }
}

/// Build an error response.
pub fn err(id: serde_json::Value, code: ErrorCode, message: impl Into<String>) -> Response {
    Response {
        id,
        ok: false,
        data: None,
        error: Some(ProtocolError {
            code,
            message: message.into(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_parses() {
        let text = r#"{"id": 1, "cmd": "read", "args": {"addr": "0x8000", "n": 16}}"#;
        let req: Request = serde_json::from_str(text).unwrap();
        assert_eq!(req.cmd, "read");
        assert_eq!(req.args["n"], 16);
    }

    #[test]
    fn response_round_trips() {
        let text = r#"{"id": 1, "ok": true, "data": {"bytes": "00 11 22"}}"#;
        let resp: Response = serde_json::from_str(text).unwrap();
        let back: Response = serde_json::from_str(&serde_json::to_string(&resp).unwrap()).unwrap();
        assert_eq!(back, resp);
    }

    #[test]
    fn error_response_shape() {
        let resp = err(
            serde_json::json!(2),
            ErrorCode::BadAddress,
            "address 0x9999 does not resolve",
        );
        let text = serde_json::to_string(&resp).unwrap();
        assert!(text.contains("\"ok\":false"));
        assert!(text.contains("bad_address"));
    }
}
