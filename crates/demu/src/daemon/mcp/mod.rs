//! The MCP Streamable HTTP surface of the daemon.
//!
//! Clients connect to `http://<bind>/mcp`. One MCP connection owns one
//! transport session from the local session manager, while the
//! emulator sessions live daemon-wide in the registry and survive
//! client disconnects. The tools live in [`tools`]; every tool bridges
//! to its session worker through a blocking job, so machine work never
//! joins the HTTP runtime.

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
    fn gb_rom() -> Vec<u8> {
        let mut rom = vec![0u8; 0x8000];
        rom[0x100..0x108].copy_from_slice(&[0x00, 0xC3, 0x50, 0x01, 0x00, 0x00, 0x00, 0x00]);
        rom[0x150..0x152].copy_from_slice(&[0x76, 0x76]);
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
