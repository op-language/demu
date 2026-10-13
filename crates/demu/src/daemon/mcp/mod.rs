//! The MCP Streamable HTTP surface of the daemon.
//!
//! Clients connect to `http://<bind>/mcp`. One MCP connection owns one
//! transport session from the local session manager, while the
//! emulator sessions live daemon-wide in the registry and survive
//! client disconnects. The session tools and the command passthrough
//! live in [`tools`], and the named debug tools live in
//! [`self::debug_tools`]; every session-carrying tool bridges to its
//! session worker through a blocking job, so machine work never joins
//! the HTTP runtime.

mod debug_tools;
mod tools;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use rmcp::{
    handler::server::{ServerHandler, router::tool::ToolRouter},
    model::{Implementation, ServerCapabilities, ServerConfig},
    tool_handler,
    transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    },
};

use crate::daemon::{log, registry::Registry};

/// The session tool server of the daemon.
///
/// One instance serves one MCP connection; the registry and the data
/// root stay shared across connections.
struct DemuMcp {
    /// The registry of live emulator sessions.
    registry: Arc<Registry>,
    /// The root that defaults every session output directory.
    data_root: PathBuf,
    /// The router the tool macro builds.
    tool_router: ToolRouter<Self>,
}

/// The maximum body size of one MCP request: the 64 MiB decoded ROM
/// cap plus base64 growth and protocol framing.
const MAX_REQUEST_BODY_BYTES: usize = 128 * 1024 * 1024;

/// The HTTP server configuration of the daemon.
///
/// The default loopback host allowlist of the transport stays active:
/// requests whose `Host` header names no loopback host fail, which
/// stops browser-side DNS rebinding without any extra setup.
fn http_server_config() -> StreamableHttpServerConfig {
    StreamableHttpServerConfig::default().with_max_request_body_bytes(MAX_REQUEST_BODY_BYTES)
}

/// Build the streamable HTTP service of the daemon.
fn build_service(
    registry: Arc<Registry>,
    data_root: PathBuf,
) -> StreamableHttpService<DemuMcp, LocalSessionManager> {
    StreamableHttpService::new(
        move || Ok(DemuMcp::new(Arc::clone(&registry), data_root.clone())),
        Arc::new(LocalSessionManager::default()),
        http_server_config(),
    )
}

/// Serve the daemon MCP surface until a shutdown signal arrives.
pub(crate) async fn serve(
    registry: Arc<Registry>,
    http: SocketAddr,
    data_root: PathBuf,
) -> Result<(), super::DaemonError> {
    let listener = tokio::net::TcpListener::bind(http).await?;
    let router = Router::new().nest_service("/mcp", build_service(registry, data_root));
    log::emit("listening", &format!("http://{http}/mcp"));
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

/// The future that completes when the daemon should stop: SIGINT or
/// SIGTERM.
#[cfg(unix)]
async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let sigterm = async {
        match signal(SignalKind::terminate()) {
            Ok(mut sigterm) => {
                sigterm.recv().await;
            }
            Err(error) => {
                log::emit("signal service unavailable", &error.to_string());
                std::future::pending::<()>().await;
            }
        }
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = sigterm => {}
    }
}

/// The future that completes when the daemon should stop: SIGINT only.
#[cfg(not(unix))]
async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for DemuMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions(
                "The session tools manage emulator sessions; `command` runs the demu-shell \
             command set on one session by name. Create a session first, then run \
             commands. File paths of shell commands confine to the session output \
             directory, and input files arrive by placing them into that directory \
             out of band.",
            )
            .with_server_info(Implementation::new("demu", env!("CARGO_PKG_VERSION")))
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use axum::Router;
    use rmcp::{
        RoleClient, ServiceError, ServiceExt,
        model::{CallToolRequestParams, CallToolResult, ClientConfig, JsonObject, ProtocolVersion},
        transport::streamable_http_client::{
            StreamableHttpClientTransport, StreamableHttpClientTransportConfig,
        },
    };
    use serde_json::{Value, json};

    use super::build_service;
    use crate::daemon::registry::Registry;

    /// The running test server: the URL, the shutdown sender, and the
    /// acceptor task.
    struct TestServer {
        url: String,
        shutdown: Option<tokio::sync::oneshot::Sender<()>>,
        acceptor: tokio::task::JoinHandle<()>,
    }

    impl Drop for TestServer {
        fn drop(&mut self) {
            if let Some(shutdown) = self.shutdown.take() {
                let _ = shutdown.send(());
            }
            self.acceptor.abort();
        }
    }

    /// Spawn the daemon MCP service on an ephemeral loopback port.
    async fn spawn_server(registry: Arc<Registry>, data_root: PathBuf) -> TestServer {
        let service = build_service(registry, data_root);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("the test listener binds");
        let addr = listener
            .local_addr()
            .expect("the listener reports its addr");
        let (shutdown, shutdown_rx) = tokio::sync::oneshot::channel();
        let acceptor = tokio::spawn(async move {
            let _ = axum::serve(listener, Router::new().nest_service("/mcp", service))
                .with_graceful_shutdown(async {
                    let _ = shutdown_rx.await;
                })
                .await;
        });
        TestServer {
            url: format!("http://{addr}/mcp"),
            shutdown: Some(shutdown),
            acceptor,
        }
    }

    /// Connect one full rmcp client over the streamable HTTP transport.
    async fn connect(url: &str) -> rmcp::service::RunningService<RoleClient, ClientConfig> {
        let transport = StreamableHttpClientTransport::from_config(
            StreamableHttpClientTransportConfig::with_uri(url.to_owned()),
        );
        ClientConfig::default()
            .with_protocol_version(ProtocolVersion::LATEST_WITH_INITIALIZE)
            .serve(transport)
            .await
            .expect("the client performs the MCP handshake")
    }

    /// Call one tool and return its result.
    async fn call(
        client: &rmcp::service::RunningService<RoleClient, ClientConfig>,
        name: &str,
        args: Value,
    ) -> Result<CallToolResult, ServiceError> {
        let arguments: JsonObject = match args {
            Value::Null => JsonObject::new(),
            other => other.as_object().expect("arguments are an object").clone(),
        };
        client
            .call_tool(CallToolRequestParams::new(name.to_owned()).with_arguments(arguments))
            .await
    }

    /// The pretty JSON payload of a successful tool result.
    fn result_json(result: &CallToolResult) -> Value {
        let text = result
            .content
            .first()
            .and_then(|content| content.as_text())
            .expect("the result carries text content")
            .text
            .clone();
        serde_json::from_str(&text).expect("the result payload is JSON")
    }

    /// The temp directory of one test, cleared of prior artifacts.
    fn temp_root(case: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("demu-daemon-mcp-{case}"));
        std::fs::remove_dir_all(&path).ok();
        path
    }

    /// A minimal Game Boy cartridge image: a jump frame and a parked tail.
    ///
    /// The boot ROM gates the boot twice: it compares the logo bytes
    /// at 0x104 through 0x133 against its own table and locks on a
    /// mismatch, then verifies the header checksum at 0x14D. The test
    /// ROM embeds the bundled logo bytes and the checksum byte of the
    /// zero-filled header, so the machine boots through the bootloader
    /// and reaches the jump frame.
    fn gb_rom() -> Vec<u8> {
        let mut rom = vec![0u8; 0x8000];
        rom[0x100..0x108].copy_from_slice(&[0x00, 0xC3, 0x50, 0x01, 0x00, 0x00, 0x00, 0x00]);
        rom[0x104..0x134].copy_from_slice(&[
            0xCE, 0xED, 0x66, 0x66, 0xCC, 0x0D, 0x00, 0x0B, //
            0x03, 0x73, 0x00, 0x83, 0x00, 0x0C, 0x00, 0x0D, //
            0x00, 0x08, 0x11, 0x1F, 0x88, 0x89, 0x00, 0x0E, //
            0xDC, 0xCC, 0x6E, 0xE6, 0xDD, 0xDD, 0xD9, 0x99, //
            0xBB, 0xBB, 0x67, 0x63, 0x6E, 0x0E, 0xEC, 0xCC, //
            0xDD, 0xDC, 0x99, 0x9F, 0xBB, 0xB9, 0x33, 0x3E, //
        ]);
        rom[0x150..0x152].copy_from_slice(&[0x76, 0x76]);
        rom[0x14D] = 0xE7;
        rom
    }

    #[tokio::test]
    async fn the_server_lists_the_session_tools() {
        let registry = Arc::new(Registry::new(8));
        let server = spawn_server(registry, temp_root("tool-list")).await;
        let client = connect(&server.url).await;
        let tools = client.list_tools(None).await.expect("the tools list").tools;
        let names: Vec<_> = tools.iter().map(|tool| tool.name.clone()).collect();
        for expected in [
            "session_create",
            "session_list",
            "session_info",
            "session_destroy",
            "command",
            "run",
            "run_frame",
            "run_until",
            "step",
            "read_memory",
            "write_memory",
            "regs",
            "set_reg",
            "break",
            "watch",
            "screenshot",
        ] {
            assert!(
                names.iter().any(|name| name == expected),
                "{expected} is missing from the tool list: {names:?}"
            );
        }
        let _ = client.cancel().await;
    }

    #[tokio::test]
    async fn a_session_lifecycle_round_trip() {
        let registry = Arc::new(Registry::new(8));
        let data_root = temp_root("lifecycle");
        let server = spawn_server(Arc::clone(&registry), data_root.clone()).await;
        let client = connect(&server.url).await;

        use base64::Engine as _;
        let created = call(
            &client,
            "session_create",
            json!({
                "name": "roundtrip",
                "config_triple": "sm83-nintendo-gameboy",
                "rom_base64": base64::engine::general_purpose::STANDARD.encode(gb_rom()),
            }),
        )
        .await
        .expect("the session creates");
        let created = result_json(&created);
        assert_eq!(created["name"], "roundtrip");
        assert!(
            created["output_dir"]
                .as_str()
                .expect("the output dir is text")
                .starts_with(data_root.to_str().expect("utf-8 temp root")),
            "the default output directory lives under the data root: {created:?}"
        );
        assert_eq!(created["warnings"], json!([]));

        let info = call(&client, "session_info", json!({"name": "roundtrip"}))
            .await
            .expect("the info runs");
        let info = result_json(&info);
        assert_eq!(info["name"], "Nintendo Game Boy");
        assert_eq!(info["triple"], "sm83-nintendo-gameboy");

        let listed = call(&client, "session_list", Value::Null)
            .await
            .expect("the list runs");
        let listed = result_json(&listed);
        assert_eq!(
            listed,
            json!([{"name": "roundtrip", "output_dir": created["output_dir"]}])
        );

        let destroyed = call(&client, "session_destroy", json!({"name": "roundtrip"}))
            .await
            .expect("the destroy runs");
        assert_eq!(result_json(&destroyed), json!({"name": "roundtrip"}));

        let destroyed_again = call(&client, "session_destroy", json!({"name": "roundtrip"})).await;
        assert!(
            destroyed_again
                .expect_err("the double destroy fails")
                .to_string()
                .contains("no session named roundtrip"),
            "the double destroy reports the session name"
        );

        // The empty registry lists no sessions; the name is reusable.
        let listed = call(&client, "session_list", Value::Null)
            .await
            .expect("the list runs");
        assert_eq!(result_json(&listed), json!([]));
        let _ = client.cancel().await;
    }

    #[tokio::test]
    async fn the_passthrough_reads_memory_and_lists_reachable_shell_errors() {
        let registry = Arc::new(Registry::new(8));
        let data_root = temp_root("passthrough");
        let server = spawn_server(registry, data_root).await;
        let client = connect(&server.url).await;

        use base64::Engine as _;
        let rom_b64 = base64::engine::general_purpose::STANDARD.encode(gb_rom());
        call(
            &client,
            "session_create",
            json!({
                "name": "shellsession",
                "config_triple": "sm83-nintendo-gameboy",
                "rom_base64": rom_b64,
            }),
        )
        .await
        .expect("the session creates");

        let read = call(
            &client,
            "command",
            json!({"name": "shellsession", "cmd": "read", "args": ["0x8000"]}),
        )
        .await
        .expect("the passthrough runs");
        let read = result_json(&read);
        assert_eq!(read["ok"], true, "the read succeeds: {read:?}");
        assert!(
            read["data"]["bytes"]
                .as_str()
                .expect("byte text")
                .contains("00"),
            "the read returns bytes: {read:?}"
        );

        let unknown_command = call(
            &client,
            "command",
            json!({"name": "shellsession", "cmd": "frobnicate", "args": []}),
        )
        .await
        .expect("the passthrough reports shell errors as results");
        let report = result_json(&unknown_command);
        assert_eq!(report["ok"], false);
        assert_eq!(report["error"]["code"], "bad_args");
        assert!(
            report["error"]["message"]
                .as_str()
                .expect("the error message is text")
                .contains("unknown command"),
            "the unknown command message names the command kind"
        );

        let unknown_session = call(
            &client,
            "command",
            json!({"name": "missing", "cmd": "regs", "args": []}),
        )
        .await;
        assert!(
            unknown_session
                .expect_err("the unknown session fails")
                .to_string()
                .contains("no session named missing"),
            "the unknown session reports as a tool error"
        );

        call(&client, "session_destroy", json!({"name": "shellsession"}))
            .await
            .expect("the destroy runs");
        let _ = client.cancel().await;
    }

    #[tokio::test]
    async fn the_debug_tools_drive_a_game_boy_session_and_log_the_calls() {
        let registry = Arc::new(Registry::new(8));
        let data_root = temp_root("debug-tools");
        let server = spawn_server(Arc::clone(&registry), data_root.clone()).await;
        let client = connect(&server.url).await;

        use base64::Engine as _;
        let rom_b64 = base64::engine::general_purpose::STANDARD.encode(gb_rom());
        let created = call(
            &client,
            "session_create",
            json!({
                "name": "debugged",
                "config_triple": "sm83-nintendo-gameboy",
                "rom_base64": rom_b64,
            }),
        )
        .await
        .expect("the session creates");
        let output_dir: String = result_json(&created)["output_dir"]
            .as_str()
            .expect("the output dir is text")
            .to_owned();

        // The breakpoint arms first, so the free run parks on it.
        let broken = call(
            &client,
            "break",
            json!({"name": "debugged", "addr": "0x150"}),
        )
        .await
        .expect("the break runs");
        assert_eq!(
            result_json(&broken),
            json!("trigger 1 set"),
            "the break reports its trigger id"
        );

        let free = call(&client, "run", json!({"name": "debugged"}))
            .await
            .expect("the free run stops on the breakpoint");
        let free = result_json(&free);
        assert_eq!(
            free["trigger_hit"], true,
            "the breakpoint parks the free run: {free:?}"
        );
        assert!(
            free["now"].as_u64().expect("the run reports now")
                > free["start"].as_u64().expect("the run reports start"),
            "the free run advanced to the breakpoint: {free:?}"
        );

        let stepped = call(&client, "step", json!({"name": "debugged", "units": 8}))
            .await
            .expect("the step runs");
        let stepped = result_json(&stepped);
        assert!(
            stepped["now"].as_u64().expect("the step reports now")
                >= stepped["start"].as_u64().expect("the step reports start") + 8,
            "a bounded step advances at least its units: {stepped:?}"
        );

        call(
            &client,
            "set_reg",
            json!({"name": "debugged", "reg": "A", "value": 66}),
        )
        .await
        .expect("the register set runs");
        let regs = call(&client, "regs", json!({"name": "debugged"}))
            .await
            .expect("the registers read runs");
        let regs = result_json(&regs);
        assert_eq!(regs["A"], 66, "the register round trip lands: {regs:?}");

        let readed = call(
            &client,
            "read_memory",
            json!({"name": "debugged", "addr": "0xFF44", "count": 4}),
        )
        .await
        .expect("the read runs");
        let readed = result_json(&readed);
        assert_eq!(
            readed["addr"],
            json!("0xff44"),
            "the read echoes the resolved address: {readed:?}"
        );
        assert_eq!(
            readed["bytes"]
                .as_str()
                .expect("the read carries a hex text")
                .split(' ')
                .count(),
            4,
            "the count applies: {readed:?}"
        );

        let written = call(
            &client,
            "write_memory",
            json!({"name": "debugged", "addr": "0xC000", "bytes_hex": "AB CD"}),
        )
        .await
        .expect("the write runs");
        let written = result_json(&written);
        assert_eq!(written["written"], 2, "both bytes land: {written:?}");

        let read_back = call(
            &client,
            "read_memory",
            json!({"name": "debugged", "addr": "0xC000", "count": 2}),
        )
        .await
        .expect("the read-back runs");
        assert_eq!(
            result_json(&read_back)["bytes"],
            json!("AB CD"),
            "the write landed in memory"
        );

        let watched = call(
            &client,
            "watch",
            json!({"name": "debugged", "addr": "0xC000"}),
        )
        .await
        .expect("the watch runs");
        assert_eq!(result_json(&watched), json!("trigger 2 set"));

        let triggers = call(
            &client,
            "command",
            json!({"name": "debugged", "cmd": "triggers", "args": []}),
        )
        .await
        .expect("the trigger list runs");
        let triggers = result_json(&triggers);
        assert_eq!(
            triggers["ok"], true,
            "the trigger list succeeds: {triggers:?}"
        );
        let rendered = triggers["data"].to_string();
        assert!(
            rendered.contains("Breakpoint") && rendered.contains("WriteWatch"),
            "the trigger list shows the breakpoint and the watchpoint: {rendered}"
        );

        let bounded = call(&client, "run", json!({"name": "debugged", "units": 60}))
            .await
            .expect("the bounded run runs");
        let bounded = result_json(&bounded);
        assert!(
            bounded["now"].as_u64().expect("the run reports now")
                >= bounded["start"].as_u64().expect("the run reports start") + 60,
            "the bounded run reaches its units: {bounded:?}"
        );

        let frames = call(
            &client,
            "run_frame",
            json!({"name": "debugged", "frames": 2}),
        )
        .await
        .expect("the frame run completes");
        let frames = result_json(&frames);
        let frame_pair = frames["frames"].as_array().expect("the frame pair");
        assert!(
            frame_pair[1].as_u64().expect("the end frames")
                > frame_pair[0].as_u64().expect("the start frames"),
            "two display frames complete: {frames:?}"
        );

        let vblank = call(
            &client,
            "run_until",
            json!({"name": "debugged", "event": "vblank"}),
        )
        .await
        .expect("the run until the next vblank completes");
        let vblank = result_json(&vblank);
        assert_eq!(
            vblank["event"], "Vblank",
            "the run until reports the fired vblank: {vblank:?}"
        );

        // Two captures on one parked tick: the second gains -1.
        let first = call(&client, "screenshot", json!({"name": "debugged"}))
            .await
            .expect("the screenshot captures");
        let second = call(&client, "screenshot", json!({"name": "debugged"}))
            .await
            .expect("the second screenshot captures");
        let blocks: Vec<&CallToolResult> = vec![&first, &second];
        let mut paths = Vec::new();
        for (attempt, block) in blocks.into_iter().enumerate() {
            let image = block
                .content
                .first()
                .and_then(|content| content.as_image())
                .expect("the screenshot carries an image block");
            assert_eq!(image.mime_type, "image/png");
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(&image.data)
                .expect("the image decodes");
            let text = block
                .content
                .get(1)
                .and_then(|content| content.as_text())
                .expect("the screenshot carries a text block")
                .text
                .clone();
            let payload: Value = serde_json::from_str(&text).expect("the text payload is JSON");
            assert_eq!(
                payload["width"], 160,
                "the game boy frame width: {payload:?}"
            );
            assert_eq!(
                payload["height"], 144,
                "the game boy frame height: {payload:?}"
            );
            assert_eq!(
                payload["size"],
                decoded.len() as u64,
                "the size matches the image bytes: {payload:?}"
            );
            let path = payload["path"].as_str().expect("the path is text");
            assert!(
                path.starts_with(&output_dir) && path.ends_with(".png"),
                "the screenshot lands in the session directory: {path}"
            );
            if attempt == 1 {
                assert!(
                    path.ends_with("-1.png"),
                    "the second capture on one tick gains the -1 suffix: {path}"
                );
            }
            assert!(
                std::path::Path::new(path).exists(),
                "the capture wrote its file: {path}"
            );
            paths.push(path.to_owned());
        }
        assert_ne!(paths[0], paths[1], "the two captures name different files");

        let failed = call(
            &client,
            "command",
            json!({"name": "debugged", "cmd": "frobnicate", "args": []}),
        )
        .await
        .expect("the failing passthrough reports the shell error as a result");
        let failed = result_json(&failed);
        assert_eq!(failed["ok"], false);
        assert_eq!(failed["error"]["code"], "bad_args");

        call(&client, "session_destroy", json!({"name": "debugged"}))
            .await
            .expect("the destroy runs");

        // The session command log: one request-response pair per logged
        // tool call, in call order.
        let log_text =
            std::fs::read_to_string(std::path::Path::new(&output_dir).join("session.jsonl"))
                .expect("the session command log reads back");
        let entries: Vec<Value> = log_text
            .lines()
            .map(|line| serde_json::from_str(line).expect("each log line is JSON"))
            .collect();
        assert_eq!(
            entries.len(),
            32,
            "16 logged calls pair into request and response entries: {log_text}"
        );
        let expected_tools = [
            "break",
            "run",
            "step",
            "set_reg",
            "regs",
            "read_memory",
            "write_memory",
            "read_memory",
            "watch",
            "command",
            "run",
            "run_frame",
            "run_until",
            "screenshot",
            "screenshot",
            "command",
        ];
        for (index, expected) in expected_tools.iter().enumerate() {
            let request = &entries[index * 2];
            let response = &entries[index * 2 + 1];
            assert_eq!(request["direction"], "request", "call {expected}");
            assert_eq!(request["tool"], *expected, "the request names its tool");
            assert!(
                request["wall_ms"].as_u64().is_some(),
                "the request carries the wall clock"
            );
            assert_eq!(response["direction"], "response", "call {expected}");
            assert_eq!(response["tool"], *expected, "the response names its tool");
            assert!(
                response["tick"].as_u64().is_some(),
                "the response carries the machine tick"
            );
            assert!(
                response["wall_ms"].as_u64().is_some(),
                "the response carries the wall clock"
            );
        }
        assert_eq!(
            entries[0]["args"],
            json!({"addr": "0x150"}),
            "the break records its address"
        );
        assert_eq!(
            entries[2]["args"],
            json!({}),
            "the free run records no units"
        );
        assert_eq!(
            entries[4]["args"],
            json!({"units": 8}),
            "the step records its units"
        );
        assert_eq!(entries[6]["args"], json!({"reg": "A", "value": 66}));
        assert_eq!(entries[10]["args"], json!({"addr": "0xFF44", "count": 4}));
        assert_eq!(
            entries[12]["args"],
            json!({"addr": "0xC000", "bytes_hex": "AB CD"})
        );
        assert_eq!(
            entries[14]["args"],
            json!({"addr": "0xC000", "count": 2}),
            "the read-back records its address and count"
        );
        assert_eq!(
            entries[16]["args"],
            json!({"addr": "0xC000"}),
            "the watch records its address"
        );
        assert_eq!(
            entries[18]["args"],
            json!({"cmd": "triggers", "args": []}),
            "the passthrough records its command as received"
        );
        assert_eq!(entries[20]["args"], json!({"units": 60}));
        assert_eq!(entries[22]["args"], json!({"frames": 2}));
        assert_eq!(entries[24]["args"], json!({"event": "vblank"}));
        assert_eq!(
            entries[26]["args"],
            json!({}),
            "the screenshots record no args"
        );
        assert_eq!(entries[28]["args"], json!({}));
        assert_eq!(
            entries[30]["args"],
            json!({"cmd": "frobnicate", "args": []})
        );
        for (index, entry) in entries.iter().enumerate() {
            if index == 31 {
                assert_eq!(
                    entry["ok"], false,
                    "the failing passthrough records its failure"
                );
            } else if index % 2 == 1 {
                assert_eq!(entry["ok"], true, "response {index}: {entry}");
                assert!(
                    entry.get("error").is_none(),
                    "the successful response carries no error key"
                );
            }
        }
        assert_eq!(
            entries[31]["error"], "bad_args",
            "the failing response records the protocol error code"
        );
        assert!(
            entries.iter().all(|entry| {
                let tool = entry["tool"].as_str().expect("each entry names its tool");
                !matches!(
                    tool,
                    "session_create" | "session_list" | "session_info" | "session_destroy"
                )
            }),
            "the session management tools log nothing"
        );
        let _ = client.cancel().await;
    }

    #[tokio::test]
    async fn the_debug_tools_reject_bad_arguments_cleanly() {
        let registry = Arc::new(Registry::new(8));
        let server = spawn_server(registry, temp_root("debug-args")).await;
        let client = connect(&server.url).await;

        use base64::Engine as _;
        let rom_b64 = base64::engine::general_purpose::STANDARD.encode(gb_rom());
        call(
            &client,
            "session_create",
            json!({
                "name": "argcheck",
                "config_triple": "sm83-nintendo-gameboy",
                "rom_base64": rom_b64,
            }),
        )
        .await
        .expect("the session creates");

        // The capture on the fresh boot reports the machine's cached
        // frame at tick 0.
        let boot_shot = call(&client, "screenshot", json!({"name": "argcheck"}))
            .await
            .expect("the capture on the fresh boot reports the cached frame");
        let boot_text = boot_shot
            .content
            .get(1)
            .and_then(|content| content.as_text())
            .expect("the screenshot carries a text block")
            .text
            .clone();
        let boot_payload: Value =
            serde_json::from_str(&boot_text).expect("the text payload is JSON");
        assert_eq!(
            boot_payload["width"], 160,
            "the cached frame width: {boot_payload:?}"
        );
        assert!(
            boot_payload["path"]
                .as_str()
                .expect("the path is text")
                .ends_with("screenshot-0.png"),
            "the fresh boot capture lands at tick 0: {boot_payload:?}"
        );

        let over_cap = call(
            &client,
            "read_memory",
            json!({"name": "argcheck", "addr": "0x8000", "count": 65537}),
        )
        .await;
        assert!(
            over_cap
                .expect_err("the over-cap read fails")
                .to_string()
                .contains("byte cap"),
            "the over-cap read names the cap"
        );

        let odd_digits = call(
            &client,
            "write_memory",
            json!({"name": "argcheck", "addr": "0xC000", "bytes_hex": "XYZ"}),
        )
        .await;
        assert!(
            odd_digits
                .expect_err("the odd digit text fails")
                .to_string()
                .contains("hex digit pairs"),
            "the odd digit text names the hex form"
        );

        let empty_bytes = call(
            &client,
            "write_memory",
            json!({"name": "argcheck", "addr": "0xC000", "bytes_hex": ""}),
        )
        .await;
        assert!(
            empty_bytes
                .expect_err("the empty text fails")
                .to_string()
                .contains("at least one byte"),
            "the empty text names the byte rule"
        );

        let unknown_event = call(
            &client,
            "run_until",
            json!({"name": "argcheck", "event": "banana"}),
        )
        .await;
        assert!(
            unknown_event
                .expect_err("the unknown event fails")
                .to_string()
                .contains("unknown run_until event"),
            "the unknown event names the shell event forms"
        );

        let unknown_register = call(
            &client,
            "set_reg",
            json!({"name": "argcheck", "reg": "ZZ", "value": 1}),
        )
        .await;
        assert!(
            unknown_register
                .expect_err("the unknown register fails")
                .to_string()
                .contains("unknown register"),
            "the unknown register names the register"
        );

        let missing = call(&client, "run", json!({"name": "missing", "units": 1})).await;
        assert!(
            missing
                .expect_err("the missing session fails")
                .to_string()
                .contains("no session named missing"),
            "the missing session names the session"
        );

        call(&client, "session_destroy", json!({"name": "argcheck"}))
            .await
            .expect("the destroy runs");
        let _ = client.cancel().await;
    }

    #[tokio::test]
    async fn a_screenshot_on_a_machine_without_video_fails_cleanly() {
        let registry = Arc::new(Registry::new(8));
        let server = spawn_server(registry, temp_root("no-video")).await;
        let client = connect(&server.url).await;

        // The ZX80 machine with its ULA removed: no video device.
        let config_toml = {
            let mut text = String::from(
                "# A Z80 machine with no video device: the ULA of the ZX80\
                 triple is gone.\n",
            );
            text.push_str("[system]\nname = \"Z80 No Video\"\ntriple = \"z80-no-video\"\n\n");
            text.push_str("[clock.main]\nfrequency = \"3250000\"\n\n");
            text.push_str(
                "[[device]]\nname = \"cpu0\"\nkind = \"z80.cpu\"\ndomain = \"main\"\n\
                 [device.dividers]\ncpu = 1\n[device.params]\nbus = \"cpu\"\n\n",
            );
            text.push_str(
                "[[device]]\nname = \"rom0\"\nkind = \"z80.rom\"\ndomain = \"main\"\n\
                 [device.params]\nsize = 4096\n\n",
            );
            text.push_str(
                "[[device]]\nname = \"ram0\"\nkind = \"z80.ram\"\ndomain = \"main\"\n\
                 [device.params]\nsize = 1024\n\n",
            );
            text.push_str("[[bus]]\nname = \"cpu\"\naddress_bits = 16\nowner = \"cpu0\"\n\n");
            text.push_str(
                "[[window]]\nname = \"w_rom\"\nbus = \"cpu\"\ndevice = \"rom0\"\n\
                 base = \"0x0000\"\nsize = \"0x1000\"\n\n",
            );
            text.push_str(
                "[[window]]\nname = \"w_ram\"\nbus = \"cpu\"\ndevice = \"ram0\"\n\
                 base = \"0x4000\"\nsize = \"0x0400\"\n\n",
            );
            text.push_str("[boot.cpu0]\nregisters = { pc = \"0x0000\" }\n\n");
            text.push_str("[boot.rom0]\nhex = \"\"\"f3760000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000018fe\"\"\"\n");
            text
        };
        call(
            &client,
            "session_create",
            json!({"name": "novideo", "config_toml": config_toml}),
        )
        .await
        .expect("the no-video session creates");

        let capture = call(&client, "screenshot", json!({"name": "novideo"})).await;
        assert!(
            capture
                .expect_err("the capture without video fails")
                .to_string()
                .contains("no video device"),
            "the capture names the missing video device"
        );

        call(&client, "session_destroy", json!({"name": "novideo"}))
            .await
            .expect("the destroy runs");
        let _ = client.cancel().await;
    }

    #[tokio::test]
    async fn file_commands_confine_under_the_session_directory() {
        let registry = Arc::new(Registry::new(8));
        let data_root = temp_root("confinement");
        let server = spawn_server(registry, data_root).await;
        let client = connect(&server.url).await;

        use base64::Engine as _;
        let rom_b64 = base64::engine::general_purpose::STANDARD.encode(gb_rom());
        let created = call(
            &client,
            "session_create",
            json!({
                "name": "files",
                "config_triple": "sm83-nintendo-gameboy",
                "rom_base64": rom_b64,
            }),
        )
        .await
        .expect("the session creates");
        let output_dir: String = result_json(&created)["output_dir"]
            .as_str()
            .expect("the output dir is text")
            .to_owned();

        let dumped = call(
            &client,
            "command",
            json!({
                "name": "files",
                "cmd": "dump",
                "args": ["0x8000", "16", "out.bin"],
            }),
        )
        .await
        .expect("the dump runs");
        assert_eq!(result_json(&dumped)["ok"], true, "the dump succeeds");
        let dumped_path = std::path::Path::new(&output_dir).join("out.bin");
        assert!(
            dumped_path.exists(),
            "the dump lands under the session directory"
        );

        let escape = call(
            &client,
            "command",
            json!({
                "name": "files",
                "cmd": "dump",
                "args": ["0x8000", "16", "../escape.bin"],
            }),
        )
        .await;
        assert!(
            escape
                .expect_err("the escape fails")
                .to_string()
                .contains("escapes the session directory"),
            "the escape reports a bad-args error"
        );

        let whitespace = call(
            &client,
            "command",
            json!({
                "name": "files",
                "cmd": "dump",
                "args": ["0x8000", "16", "has space"],
            }),
        )
        .await;
        assert!(
            whitespace
                .expect_err("the whitespace token fails")
                .to_string()
                .contains("must not contain whitespace"),
            "the whitespace token reports a bad-args error"
        );

        let out_file = call(
            &client,
            "command",
            json!({
                "name": "files",
                "cmd": "render-session",
                "args": ["1", "-o", "render.mkv"],
            }),
        )
        .await
        .expect("the confined -o value renders into the session directory");
        assert_eq!(
            result_json(&out_file)["ok"],
            true,
            "the render-session runs on the game boy video device"
        );

        call(&client, "session_destroy", json!({"name": "files"}))
            .await
            .expect("the destroy runs");
        let _ = client.cancel().await;
    }

    #[tokio::test]
    async fn create_args_reject_xor_name_and_cap_violations() {
        let registry = Arc::new(Registry::new(8));
        let server = spawn_server(registry, temp_root("arg-errors")).await;
        let client = connect(&server.url).await;

        let bad_name = call(
            &client,
            "session_create",
            json!({"name": "bad name", "config_triple": "sm83-nintendo-gameboy"}),
        )
        .await;
        assert!(
            bad_name.is_err(),
            "an invalid session name fails the create"
        );

        let both_configs = call(
            &client,
            "session_create",
            json!({
                "name": "both",
                "config_toml": "[system]\nname = \"x\"\ntriple = \"sm83-nintendo-gameboy\"",
                "config_triple": "sm83-nintendo-gameboy",
            }),
        )
        .await;
        assert!(
            both_configs
                .expect_err("both configs fail")
                .to_string()
                .contains("exactly one"),
            "both configs report the xor rule"
        );

        let neither_config = call(&client, "session_create", json!({"name": "neither"})).await;
        assert!(
            neither_config
                .expect_err("neither config fails")
                .to_string()
                .contains("exactly one"),
            "neither config reports the xor rule"
        );

        // 65 MiB of zero bytes decodes over the 64 MiB cap.
        use base64::Engine as _;
        let oversized =
            base64::engine::general_purpose::STANDARD.encode(vec![0u8; 65 * 1024 * 1024]);
        let over_cap = call(
            &client,
            "session_create",
            json!({
                "name": "over-cap",
                "config_triple": "sm83-nintendo-gameboy",
                "rom_base64": oversized,
            }),
        )
        .await;
        assert!(
            over_cap
                .expect_err("the oversize ROM fails")
                .to_string()
                .contains("byte cap"),
            "the oversize ROM names the cap"
        );

        let unknown_info = call(&client, "session_info", json!({"name": "missing"})).await;
        assert!(unknown_info.is_err(), "info on an unknown session fails");
        let _ = client.cancel().await;
    }

    #[tokio::test]
    async fn a_session_over_the_cap_rejects_until_a_slot_frees() {
        let registry = Arc::new(Registry::new(2));
        let server = spawn_server(registry, temp_root("cap")).await;
        let client = connect(&server.url).await;
        for name in ["cap-a", "cap-b"] {
            call(
                &client,
                "session_create",
                json!({"name": name, "config_triple": "sm83-nintendo-gameboy"}),
            )
            .await
            .expect("the session creates");
        }
        let over_cap = call(
            &client,
            "session_create",
            json!({"name": "cap-c", "config_triple": "sm83-nintendo-gameboy"}),
        )
        .await;
        assert!(
            over_cap
                .expect_err("the cap rejects")
                .to_string()
                .to_lowercase()
                .contains("limit"),
            "a session over the cap reports the limit"
        );
        call(&client, "session_destroy", json!({"name": "cap-a"}))
            .await
            .expect("the destroy frees the slot");
        call(
            &client,
            "session_create",
            json!({"name": "cap-c", "config_triple": "sm83-nintendo-gameboy"}),
        )
        .await
        .expect("a freed slot serves a new session");
        for name in ["cap-b", "cap-c"] {
            call(&client, "session_destroy", json!({"name": name}))
                .await
                .expect("the destroy runs");
        }
        let _ = client.cancel().await;
    }

    #[test]
    fn a_bind_failure_exits_two() {
        // Hold the port, then ask the daemon for the same address: the
        // bind fails and the exit code is 2.
        let holder = std::net::TcpListener::bind("127.0.0.1:0").expect("the holder binds");
        let port = holder.local_addr().expect("the holder reports").port();
        let code = crate::daemon::run(&["--http".to_owned(), format!("127.0.0.1:{port}")]);
        assert_eq!(code, std::process::ExitCode::from(2));
        drop(holder);
    }
}
