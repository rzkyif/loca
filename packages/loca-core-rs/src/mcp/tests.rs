//! MCP tests: the registry, the manager against a real in-process server, the
//! stdio spawn plan, and the route slice.
//!
//! [`fake`] is a hand-written JSON-RPC server over a `tokio::io::duplex` pair,
//! driven by the real rmcp client over the same framing the stdio transport
//! uses. Enabling rmcp's own server half would test that rmcp agrees with
//! itself; this way a change that broke the wire shape fails here.
//!
// Test support: a broken fixture is a broken test, and panicking says so at the
// line that caused it rather than threading a `Result` through every helper.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use futures_util::FutureExt;

use crate::db::migrate;
use crate::db::Db;
use crate::paths::Paths;
use crate::secrets::Vault;
use crate::transport::FrameBus;

use super::registry::{NewServer, Registry, ServerConfig, ServerPatch, ToggleSet};
use super::session::{Connected, Connector};
use super::Mcp;
use crate::contract::api::{McpRemoteAuth, McpStdioRuntime, McpTransportKind};

// --- fixture -----------------------------------------------------------------

fn db() -> Db {
    let db = Db::open_in_memory().expect("in-memory db");
    db.call_blocking(|conn| migrate::migrate(conn))
        .expect("migrate");
    db
}

fn vault(paths: &Paths) -> Vault {
    Vault::with_store(
        paths.secrets_enc_file(),
        Arc::new(crate::secrets::testing::MemoryStore::new()),
    )
}

/// A vault on its own, for the credential tests that want no `Mcp` around it.
/// The temp directory has to outlive it, so it comes back with one.
fn a_vault() -> (tempfile::TempDir, Vault) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let paths = Paths::rooted(tmp.path());
    paths.ensure_dirs().expect("ensure_dirs");
    let v = vault(&paths);
    (tmp, v)
}

fn stdio_draft(name: &str) -> NewServer {
    NewServer {
        name: name.to_string(),
        kind: McpTransportKind::Stdio,
        ..NewServer::default()
    }
}

fn enabled_stdio(name: &str) -> NewServer {
    NewServer {
        command: Some("fake".to_string()),
        enabled: Some(true),
        ..stdio_draft(name)
    }
}

/// One remote server pointed at a URL, for the transport tests.
fn remote_config(url: &str, auth: McpRemoteAuth) -> ServerConfig {
    ServerConfig {
        id: "remote".to_string(),
        name: "Remote".to_string(),
        kind: McpTransportKind::Remote,
        command: None,
        args: Vec::new(),
        runtime: McpStdioRuntime::Deno,
        deno_allow_all: false,
        deno_permissions: Vec::new(),
        url: Some(url.to_string()),
        remote_auth: auth,
        has_auth: false,
        oauth_authorized: false,
        enabled: true,
        tool_enabled: Vec::new(),
        prompt_enabled: Vec::new(),
        tool_always_available_off: Vec::new(),
        created_at_ms: 0,
        updated_at_ms: 0,
    }
}

// --- the fake MCP server -----------------------------------------------------

mod fake {
    //! A minimal MCP server: newline-delimited JSON-RPC over a duplex stream.

    use std::sync::{Arc, Mutex};

    use serde_json::{json, Value};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};

    use super::lk;

    /// What a fake server offers.
    #[derive(Clone, Debug, Default)]
    pub struct Spec {
        pub tools: Vec<(String, Option<String>)>,
        pub prompts: Vec<(String, String)>,
        /// name, uri, text
        pub resources: Vec<(String, String, String)>,
    }

    impl Spec {
        pub fn with_tool(mut self, name: &str) -> Self {
            self.tools.push((name.to_string(), None));
            self
        }

        pub fn with_described_tool(mut self, name: &str, description: &str) -> Self {
            self.tools
                .push((name.to_string(), Some(description.to_string())));
            self
        }

        pub fn with_prompt(mut self, name: &str, text: &str) -> Self {
            self.prompts.push((name.to_string(), text.to_string()));
            self
        }

        pub fn with_resource(mut self, name: &str, uri: &str, text: &str) -> Self {
            self.resources
                .push((name.to_string(), uri.to_string(), text.to_string()));
            self
        }
    }

    /// Control over a running fake.
    #[derive(Clone)]
    pub struct Handle {
        spec: Arc<Mutex<Spec>>,
        outbound: tokio::sync::mpsc::UnboundedSender<Value>,
        stop: Arc<Mutex<Option<tokio::sync::oneshot::Sender<()>>>>,
    }

    impl Handle {
        /// Add a tool and push the notification a real server would.
        pub fn add_tool_and_notify(&self, name: &str) {
            lk(&self.spec).tools.push((name.to_string(), None));
            let _ = self.outbound.send(json!({
                "jsonrpc": "2.0",
                "method": "notifications/tools/list_changed",
            }));
        }

        /// Vanish, the way a crashed server does: the stream ends and the
        /// client sees EOF.
        pub fn drop_connection(&self) {
            if let Some(stop) = lk(&self.stop).take() {
                let _ = stop.send(());
            }
        }
    }

    /// Start a fake server, returning the client's end of the pipe.
    pub fn start(spec: Spec) -> (DuplexStream, Handle) {
        let (client, server) = tokio::io::duplex(64 * 1024);
        let spec = Arc::new(Mutex::new(spec));
        let (outbound, mut outbound_rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
        let (stop, mut stop_rx) = tokio::sync::oneshot::channel();
        let handle = Handle {
            spec: Arc::clone(&spec),
            outbound,
            stop: Arc::new(Mutex::new(Some(stop))),
        };

        tokio::spawn(async move {
            let (read, mut write) = tokio::io::split(server);
            let mut lines = BufReader::new(read).lines();
            loop {
                tokio::select! {
                    _ = &mut stop_rx => return,
                    pushed = outbound_rx.recv() => {
                        let Some(frame) = pushed else { return };
                        if write_line(&mut write, &frame).await.is_err() {
                            return;
                        }
                    }
                    line = lines.next_line() => {
                        let Ok(Some(line)) = line else { return };
                        let Ok(request) = serde_json::from_str::<Value>(&line) else { continue };
                        let Some(reply) = respond(&spec, &request) else { continue };
                        if write_line(&mut write, &reply).await.is_err() {
                            return;
                        }
                    }
                }
            }
        });
        (client, handle)
    }

    async fn write_line<W: tokio::io::AsyncWrite + Unpin>(
        write: &mut W,
        value: &Value,
    ) -> std::io::Result<()> {
        let mut bytes = serde_json::to_vec(value)?;
        bytes.push(b'\n');
        write.write_all(&bytes).await
    }

    fn respond(spec: &Arc<Mutex<Spec>>, request: &Value) -> Option<Value> {
        // A notification carries no id and wants no answer.
        let id = request.get("id")?.clone();
        let method = request.get("method")?.as_str()?;
        let spec = lk(spec).clone();
        let result = match method {
            "initialize" => json!({
                // Echo the client's version back: inventing one risks a version
                // the client refuses, which would look like a transport bug.
                "protocolVersion": request
                    .get("params")
                    .and_then(|p| p.get("protocolVersion"))
                    .cloned()
                    .unwrap_or(json!("2025-06-18")),
                "capabilities": {
                    "tools": { "listChanged": true },
                    "prompts": { "listChanged": true },
                    "resources": { "listChanged": true },
                },
                "serverInfo": { "name": "fake", "version": "1" },
            }),
            "tools/list" => json!({
                "tools": spec.tools.iter().map(|(name, description)| json!({
                    "name": name,
                    "description": description,
                    "inputSchema": { "type": "object" },
                })).collect::<Vec<_>>(),
            }),
            "prompts/list" => json!({
                "prompts": spec.prompts.iter().map(|(name, _)| json!({
                    "name": name,
                    "description": "a prompt",
                    "arguments": [],
                })).collect::<Vec<_>>(),
            }),
            "resources/list" => json!({
                "resources": spec.resources.iter().map(|(name, uri, _)| json!({
                    "uri": uri,
                    "name": name,
                })).collect::<Vec<_>>(),
            }),
            "tools/call" => {
                let name = request
                    .get("params")
                    .and_then(|p| p.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                json!({
                    "content": [{ "type": "text", "text": format!("{name} ok") }],
                    "isError": false,
                })
            }
            "prompts/get" => {
                let name = request
                    .get("params")
                    .and_then(|p| p.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let text = spec
                    .prompts
                    .iter()
                    .find(|(n, _)| n == name)
                    .map(|(_, text)| text.clone())
                    .unwrap_or_default();
                json!({ "messages": [{ "role": "user", "content": { "type": "text", "text": text } }] })
            }
            "resources/read" => {
                let uri = request
                    .get("params")
                    .and_then(|p| p.get("uri"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let text = spec
                    .resources
                    .iter()
                    .find(|(_, u, _)| u == uri)
                    .map(|(_, _, text)| text.clone())
                    .unwrap_or_default();
                json!({ "contents": [{ "uri": uri, "text": text }] })
            }
            _ => json!({}),
        };
        Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
    }
}

/// A [`Connector`] that links the client to an in-process fake instead of
/// spawning a process or reaching a URL.
#[derive(Default)]
struct FakeConnector {
    installed: std::sync::Mutex<std::collections::HashMap<String, fake::Spec>>,
    handles: std::sync::Mutex<std::collections::HashMap<String, fake::Handle>>,
    stdio: bool,
    /// When each connect was attempted, per server id, so a test can prove a
    /// reconnect happened, prove one did not, and prove the gaps between them.
    attempts: std::sync::Mutex<std::collections::HashMap<String, Vec<tokio::time::Instant>>>,
    /// Servers whose connects fail from now on, which is the shape of a command
    /// that exits immediately.
    failing: std::sync::Mutex<std::collections::HashSet<String>>,
    /// Servers whose connects never resolve, which is the shape of an endpoint
    /// that accepts the connection and then says nothing.
    hanging: std::sync::Mutex<std::collections::HashSet<String>>,
}

impl FakeConnector {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            stdio: true,
            ..Self::default()
        })
    }

    fn install(&self, id: &str, spec: fake::Spec) {
        lk(&self.installed).insert(id.to_string(), spec);
    }

    fn handle(&self, id: &str) -> fake::Handle {
        lk(&self.handles)
            .get(id)
            .cloned()
            .expect("the server was never connected")
    }

    fn attempts(&self, id: &str) -> usize {
        self.attempt_times(id).len()
    }

    fn attempt_times(&self, id: &str) -> Vec<tokio::time::Instant> {
        lk(&self.attempts).get(id).cloned().unwrap_or_default()
    }

    /// Whole seconds between consecutive connect attempts, which is what the
    /// backoff schedule is measured in.
    fn attempt_gaps(&self, id: &str) -> Vec<u64> {
        self.attempt_times(id)
            .windows(2)
            .map(|pair| pair[1].duration_since(pair[0]).as_secs())
            .collect()
    }

    /// Fail every connect for this server from now on.
    fn fail_connects(&self, id: &str) {
        lk(&self.failing).insert(id.to_string());
    }

    fn allow_connects(&self, id: &str) {
        lk(&self.failing).remove(id);
    }

    /// Black-hole every connect for this server: the attempt is recorded and
    /// then the future never resolves.
    fn hang_connects(&self, id: &str) {
        lk(&self.hanging).insert(id.to_string());
    }
}

#[async_trait]
impl Connector for FakeConnector {
    async fn connect(&self, server: &ServerConfig) -> crate::error::Result<Connected> {
        lk(&self.attempts)
            .entry(server.id.clone())
            .or_default()
            .push(tokio::time::Instant::now());
        if lk(&self.hanging).contains(&server.id) {
            std::future::pending::<()>().await;
        }
        if lk(&self.failing).contains(&server.id) {
            return Err(crate::error::internal("the server exited immediately"));
        }
        let spec = lk(&self.installed)
            .get(&server.id)
            .cloned()
            .ok_or_else(|| crate::error::internal("no fake server installed"))?;
        let (client, handle) = fake::start(spec);
        lk(&self.handles).insert(server.id.clone(), handle);

        let (events_handler, events) = super::session::event_channel();
        let tx = super::session::sender(&events_handler);
        let service = rmcp::ServiceExt::serve(events_handler, client)
            .await
            .map_err(|err| crate::error::internal(format!("fake connect: {err}")))?;
        let session = super::session::adopt(service, tx, None);
        Ok(Connected { session, events })
    }

    fn stdio_supported(&self) -> bool {
        self.stdio
    }
}

struct Fixture {
    _tmp: tempfile::TempDir,
    mcp: Mcp,
    connector: Arc<FakeConnector>,
    frames: Arc<FrameBus>,
    paths: Arc<Paths>,
    vault: Vault,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().expect("tempdir");
    let paths = Paths::rooted(tmp.path());
    paths.ensure_dirs().expect("ensure_dirs");
    let paths = Arc::new(paths);
    let vault = vault(&paths);
    let frames = Arc::new(FrameBus::new());
    let connector = FakeConnector::new();
    let mcp = Mcp::with_connector(
        db(),
        vault.clone(),
        Arc::clone(&frames),
        Arc::clone(&connector) as Arc<dyn Connector>,
    );
    Fixture {
        _tmp: tmp,
        mcp,
        connector,
        frames,
        paths,
        vault,
    }
}

/// A lock guard that ignores poisoning, since a panicking test poisons the
/// fixture's mutexes and the follow-on failure hides the real one.
fn lk<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Let a spawned watcher task observe what just happened. Every wait here is a
/// task hand-off rather than a timeout on real work.
async fn settle() {
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
}

// --- registry ----------------------------------------------------------------

mod registry_tests {
    use super::*;

    fn registry() -> Registry {
        Registry::new(db())
    }

    #[tokio::test]
    async fn a_disabled_draft_may_omit_its_command() {
        let registry = registry();
        let server = registry.create(stdio_draft("Draft")).await.unwrap();
        assert!(!server.enabled);
        assert_eq!(server.command, None);
        // The defaults the client relies on: allow-all on, everything else off.
        assert!(server.deno_allow_all);
        assert_eq!(server.runtime, McpStdioRuntime::Custom);
        assert_eq!(server.remote_auth, McpRemoteAuth::None);
        assert!(server.tool_always_available_off.is_empty());
    }

    #[tokio::test]
    async fn creating_an_enabled_server_without_a_target_is_refused() {
        let registry = registry();
        let stdio = registry
            .create(NewServer {
                enabled: Some(true),
                ..stdio_draft("Bad")
            })
            .await
            .unwrap_err();
        assert!(stdio.message.contains("command"), "{}", stdio.message);

        let remote = registry
            .create(NewServer {
                name: "Bad".to_string(),
                kind: McpTransportKind::Remote,
                enabled: Some(true),
                ..NewServer::default()
            })
            .await
            .unwrap_err();
        assert!(remote.message.contains("url"), "{}", remote.message);
    }

    #[tokio::test]
    async fn a_blank_name_is_refused() {
        let registry = registry();
        let err = registry
            .create(NewServer {
                name: "   ".to_string(),
                ..stdio_draft("ignored")
            })
            .await
            .unwrap_err();
        assert!(err.message.contains("name"), "{}", err.message);
    }

    #[tokio::test]
    async fn enabling_an_incomplete_draft_is_refused() {
        let registry = registry();
        let draft = registry.create(stdio_draft("Draft")).await.unwrap();
        let err = registry
            .update(
                &draft.id,
                ServerPatch {
                    enabled: Some(true),
                    ..ServerPatch::default()
                },
            )
            .await
            .unwrap_err();
        assert!(err.message.contains("command"), "{}", err.message);
    }

    #[tokio::test]
    async fn a_rename_leaves_the_connection_config_alone() {
        let registry = registry();
        let draft = registry
            .create(NewServer {
                command: Some("npx".to_string()),
                args: Some(vec!["-y".to_string(), "server".to_string()]),
                ..stdio_draft("Draft")
            })
            .await
            .unwrap();
        let renamed = registry
            .update(
                &draft.id,
                ServerPatch {
                    name: Some("Renamed".to_string()),
                    ..ServerPatch::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(renamed.name, "Renamed");
        assert_eq!(renamed.command.as_deref(), Some("npx"));
        assert_eq!(renamed.args, vec!["-y".to_string(), "server".to_string()]);
        assert!(!renamed.enabled);
    }

    #[tokio::test]
    async fn an_all_whitespace_name_keeps_the_stored_one() {
        let registry = registry();
        let server = registry.create(stdio_draft("Original")).await.unwrap();
        let patched = registry
            .update(
                &server.id,
                ServerPatch {
                    name: Some("   ".to_string()),
                    ..ServerPatch::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(patched.name, "Original");
    }

    #[tokio::test]
    async fn the_json_sets_toggle_idempotently() {
        let registry = registry();
        let server = registry.create(stdio_draft("S")).await.unwrap();

        let on = registry
            .toggle(&server.id, ToggleSet::Tool, "search", true)
            .await
            .unwrap();
        assert_eq!(on.tool_enabled, vec!["search".to_string()]);
        // A set, so enabling twice is still one entry.
        let again = registry
            .toggle(&server.id, ToggleSet::Tool, "search", true)
            .await
            .unwrap();
        assert_eq!(again.tool_enabled, vec!["search".to_string()]);
        let off = registry
            .toggle(&server.id, ToggleSet::Tool, "search", false)
            .await
            .unwrap();
        assert!(off.tool_enabled.is_empty());

        let prompt = registry
            .toggle(&server.id, ToggleSet::Prompt, "commit", true)
            .await
            .unwrap();
        assert_eq!(prompt.prompt_enabled, vec!["commit".to_string()]);
    }

    #[tokio::test]
    async fn toggling_an_unknown_server_is_not_found() {
        let registry = registry();
        let err = registry
            .toggle("nope", ToggleSet::Tool, "search", true)
            .await
            .unwrap_err();
        assert_eq!(err.code, crate::error::ErrorCode::NotFound);
    }

    #[tokio::test]
    async fn the_list_is_ordered_by_name_case_insensitively() {
        let registry = registry();
        for name in ["zeta", "Alpha", "beta"] {
            registry.create(stdio_draft(name)).await.unwrap();
        }
        let names: Vec<String> = registry
            .list()
            .await
            .unwrap()
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(names, vec!["Alpha", "beta", "zeta"]);
    }

    #[tokio::test]
    async fn deleting_an_unknown_id_is_a_no_op() {
        let registry = registry();
        registry.delete("never-existed").await.unwrap();
    }
}

// --- manager -----------------------------------------------------------------

mod manager_tests {
    use super::*;
    use crate::contract::api::McpConnectionStatus;

    async fn connected(f: &Fixture, spec: fake::Spec) -> String {
        let server = f.mcp.create(enabled_stdio("Fake")).await.unwrap();
        f.connector.install(&server.id, spec);
        f.mcp.resync().await.unwrap();
        server.id
    }

    #[tokio::test]
    async fn connecting_discovers_capabilities_and_round_trips_a_tool_call() {
        let f = fixture();
        let id = connected(
            &f,
            fake::Spec::default()
                .with_tool("ping")
                .with_resource("doc", "file:///d", "DATA"),
        )
        .await;

        let server = f.mcp.get(&id).await.unwrap();
        assert_eq!(server.status, McpConnectionStatus::Connected);
        assert_eq!(server.tool_count, 1);
        assert_eq!(server.resource_count, 1);

        let result = f
            .mcp
            .call_tool(&id, "ping", serde_json::Map::new())
            .await
            .unwrap();
        assert!(
            result.to_string().contains("ping ok"),
            "unexpected result: {result}"
        );

        let read = f.mcp.list_resources().await.unwrap();
        assert_eq!(read[0].uri, "file:///d");
    }

    #[tokio::test]
    async fn a_tools_list_changed_notification_refreshes_the_cached_list() {
        let f = fixture();
        let id = connected(&f, fake::Spec::default().with_tool("a")).await;
        assert_eq!(f.mcp.get(&id).await.unwrap().tool_count, 1);

        f.connector.handle(&id).add_tool_and_notify("b");
        settle().await;

        let mut names: Vec<String> = f
            .mcp
            .list_all_tools()
            .await
            .unwrap()
            .into_iter()
            .map(|t| t.name)
            .collect();
        names.sort();
        assert_eq!(names, vec!["a".to_string(), "b".to_string()]);
    }

    #[tokio::test]
    async fn an_unexpected_drop_flips_the_status_to_error() {
        let f = fixture();
        let id = connected(&f, fake::Spec::default().with_tool("a")).await;
        assert_eq!(
            f.mcp.get(&id).await.unwrap().status,
            McpConnectionStatus::Connected
        );

        f.connector.handle(&id).drop_connection();
        settle().await;

        let server = f.mcp.get(&id).await.unwrap();
        assert_eq!(server.status, McpConnectionStatus::Error);
        assert_eq!(server.status_error.as_deref(), Some("Connection closed"));
        // The capability cache goes with the connection, so a dropped server
        // stops offering tools rather than offering stale ones.
        assert_eq!(server.tool_count, 0);
        assert!(f.mcp.list_all_tools().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_drop_repaints_every_client() {
        let f = fixture();
        let mut connection = f.frames.register("watcher");
        let id = connected(&f, fake::Spec::default().with_tool("a")).await;
        // Drain whatever the resync broadcast.
        while connection.next().now_or_never().is_some() {}

        f.connector.handle(&id).drop_connection();
        settle().await;

        let frame = connection.next().now_or_never().flatten();
        assert!(frame.is_some(), "a dropped connection broadcast nothing");
    }

    #[tokio::test]
    async fn an_intentional_disconnect_does_not_schedule_a_reconnect() {
        let f = fixture();
        let id = connected(&f, fake::Spec::default().with_tool("a")).await;
        assert_eq!(f.connector.attempts(&id), 1);

        f.mcp.disconnect(&id).await;
        settle().await;
        // Long enough for the first backoff step to have fired if one had been
        // armed, which is what the delete-before-close ordering prevents.
        tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
        assert_eq!(
            f.connector.attempts(&id),
            1,
            "an intentional disconnect armed a reconnect",
        );
        assert_eq!(
            f.mcp.get(&id).await.unwrap().status,
            McpConnectionStatus::Disconnected,
        );
    }

    /// The positive reconnect path: a dropped connection comes back on its own.
    #[tokio::test(start_paused = true)]
    async fn a_dropped_connection_reconnects_on_its_own() {
        let f = fixture();
        let id = connected(&f, fake::Spec::default().with_tool("a")).await;

        f.connector.handle(&id).drop_connection();
        settle().await;
        assert_eq!(
            f.mcp.get(&id).await.unwrap().status,
            McpConnectionStatus::Error,
        );

        // The first backoff step is one second.
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        settle().await;
        assert_eq!(f.connector.attempts(&id), 2, "no reconnect was attempted");
        assert_eq!(
            f.mcp.get(&id).await.unwrap().status,
            McpConnectionStatus::Connected,
        );
    }

    /// The backoff has to ADVANCE, and then STOP: an attempt count that resets
    /// makes every step the first one, so a server whose command exits
    /// immediately is retried once a second forever.
    #[tokio::test(start_paused = true)]
    async fn a_server_that_keeps_dropping_backs_off_and_then_gives_up() {
        let f = fixture();
        let id = connected(&f, fake::Spec::default().with_tool("a")).await;
        f.connector.fail_connects(&id);

        f.connector.handle(&id).drop_connection();
        settle().await;
        // Far past the whole schedule (1 + 2 + 5 + 15 + 30 = 53s).
        tokio::time::sleep(std::time::Duration::from_secs(600)).await;

        assert_eq!(
            f.connector.attempt_gaps(&id),
            vec![1, 2, 5, 15, 30],
            "the backoff did not follow 1s/2s/5s/15s/30s",
        );
        assert_eq!(
            f.connector.attempts(&id),
            6,
            "the manager never gave up: one connect plus five backoff steps is the whole schedule",
        );
        assert_eq!(
            f.mcp.get(&id).await.unwrap().status,
            McpConnectionStatus::Error,
        );
    }

    /// The Reconnect button after an exhausted backoff starts the schedule
    /// over, which is what makes the attempt count per-incident rather than
    /// per-server.
    #[tokio::test(start_paused = true)]
    async fn an_intentional_reconnect_resets_the_backoff() {
        let f = fixture();
        let id = connected(&f, fake::Spec::default().with_tool("a")).await;
        f.connector.fail_connects(&id);
        f.connector.handle(&id).drop_connection();
        settle().await;
        tokio::time::sleep(std::time::Duration::from_secs(600)).await;
        assert_eq!(f.connector.attempts(&id), 6, "the whole schedule ran");

        // The Reconnect button: disconnect, then resync.
        f.connector.allow_connects(&id);
        f.mcp.disconnect(&id).await;
        f.mcp.resync().await.unwrap();
        settle().await;
        assert_eq!(
            f.mcp.get(&id).await.unwrap().status,
            McpConnectionStatus::Connected,
        );

        // The next drop gets the whole schedule again, not the exhausted end
        // of the previous one.
        f.connector.fail_connects(&id);
        let before = f.connector.attempts(&id);
        f.connector.handle(&id).drop_connection();
        settle().await;
        tokio::time::sleep(std::time::Duration::from_secs(600)).await;
        assert_eq!(
            f.connector.attempt_gaps(&id)[before - 1..],
            [1, 2, 5, 15, 30],
            "the backoff did not restart after an intentional reconnect",
        );
    }

    /// The reason the sync lock is released before the connects: every MCP CRUD
    /// route resyncs, so a server at an endpoint that accepts TCP and never
    /// answers would take the lock at boot and keep it for the whole connect
    /// timeout, leaving the user unable to delete the server wedging them. The
    /// bound is real time on purpose: this is the test that must not hang.
    #[tokio::test]
    async fn a_black_holed_connect_does_not_wedge_the_crud_routes() {
        let f = fixture();
        let wedger = f.mcp.create(enabled_stdio("Wedger")).await.unwrap().id;
        f.connector.hang_connects(&wedger);

        let syncing = tokio::spawn({
            let mcp = f.mcp.clone();
            async move { mcp.resync().await }
        });
        settle().await;
        assert_eq!(
            f.connector.attempts(&wedger),
            1,
            "the wedging connect never started, so the test proves nothing",
        );
        assert!(!syncing.is_finished(), "the connect did not black-hole");

        // Delete, then the resync every mutating route ends with.
        let removed = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            f.mcp.delete(&wedger).await.unwrap();
            f.mcp.resync().await.unwrap();
        })
        .await;
        assert!(
            removed.is_ok(),
            "a black-holed connect held the sync lock, so the server could not be removed",
        );
        syncing.abort();
    }

    /// And the connect itself is bounded, so the wedged server lands in `error`
    /// rather than staying `connecting` forever. Guarded at ten minutes, so with
    /// no bound in the code the failure is an assertion rather than a hang.
    #[tokio::test(start_paused = true)]
    async fn a_connect_that_never_answers_is_given_up_on() {
        let f = fixture();
        let id = f.mcp.create(enabled_stdio("Silent")).await.unwrap().id;
        f.connector.hang_connects(&id);

        let started = tokio::time::Instant::now();
        let synced =
            tokio::time::timeout(std::time::Duration::from_secs(600), f.mcp.resync()).await;
        assert!(synced.is_ok(), "the connect was never bounded");
        assert_eq!(
            started.elapsed().as_secs(),
            90,
            "the connect bound is not the 90s CONNECT_TIMEOUT",
        );

        let server = f.mcp.get(&id).await.unwrap();
        assert_eq!(server.status, McpConnectionStatus::Error);
        assert!(
            server
                .status_error
                .unwrap_or_default()
                .contains("timed out"),
            "the user is not told the connect timed out",
        );
    }

    #[tokio::test]
    async fn a_notification_from_a_replaced_connection_is_ignored() {
        let f = fixture();
        let id = connected(&f, fake::Spec::default().with_tool("a")).await;
        let stale = f.connector.handle(&id);

        // Replace the connection: the manager now holds a newer generation.
        f.mcp.disconnect(&id).await;
        f.mcp.resync().await.unwrap();
        settle().await;
        assert_eq!(f.connector.attempts(&id), 2);

        // The old server pushes a change. It must not touch the live cache.
        stale.add_tool_and_notify("ghost");
        settle().await;

        let names: Vec<String> = f
            .mcp
            .list_all_tools()
            .await
            .unwrap()
            .into_iter()
            .map(|t| t.name)
            .collect();
        assert_eq!(names, vec!["a".to_string()]);
    }

    /// The generation check that matters is the one AFTER the fetch await: a
    /// connection can be replaced while a refresh is in flight, and writing that
    /// stale list back would replace a live server's capabilities with a dead
    /// one's. The window is inside the manager, so the session is the double.
    #[tokio::test]
    async fn a_refresh_that_finishes_after_its_connection_was_replaced_is_dropped() {
        let f = fixture();
        let gate = Arc::new(tokio::sync::Notify::new());
        let connector = Arc::new(GatedConnector {
            gate: Arc::clone(&gate),
            connects: std::sync::Mutex::new(0),
        });
        let mcp = Mcp::with_connector(
            db(),
            f.vault.clone(),
            Arc::clone(&f.frames),
            Arc::clone(&connector) as Arc<dyn Connector>,
        );
        let server = mcp.create(enabled_stdio("Gated")).await.unwrap();
        mcp.resync().await.unwrap();
        assert_eq!(
            mcp.list_all_tools()
                .await
                .unwrap()
                .into_iter()
                .map(|t| t.name)
                .collect::<Vec<_>>(),
            vec!["stale".to_string()],
        );

        // The first connection reports a list change; its re-fetch blocks.
        connector.notify_list_changed();
        settle().await;

        // Replace the connection while that fetch is still in flight. The
        // second one offers a different tool.
        mcp.disconnect(&server.id).await;
        mcp.resync().await.unwrap();
        assert_eq!(
            mcp.list_all_tools()
                .await
                .unwrap()
                .into_iter()
                .map(|t| t.name)
                .collect::<Vec<_>>(),
            vec!["fresh".to_string()],
        );

        // Now let the stale fetch finish. It must not touch the live cache.
        gate.notify_waiters();
        settle().await;
        assert_eq!(
            mcp.list_all_tools()
                .await
                .unwrap()
                .into_iter()
                .map(|t| t.name)
                .collect::<Vec<_>>(),
            vec!["fresh".to_string()],
            "a refresh from a replaced connection overwrote the live capabilities",
        );
    }

    /// Hands out a session whose first `list_tools` after connect blocks on a
    /// gate, so a test can hold a refresh open across a reconnect.
    struct GatedConnector {
        gate: Arc<tokio::sync::Notify>,
        connects: std::sync::Mutex<usize>,
    }

    impl GatedConnector {
        fn notify_list_changed(&self) {
            if let Some(tx) = lk(self.senders()).first() {
                let _ = tx.send(crate::mcp::session::SessionEvent::ListChanged(
                    crate::mcp::session::Capability::Tools,
                ));
            }
        }

        fn senders(
            &self,
        ) -> &'static std::sync::Mutex<
            Vec<tokio::sync::mpsc::UnboundedSender<crate::mcp::session::SessionEvent>>,
        > {
            static SENDERS: std::sync::OnceLock<
                std::sync::Mutex<
                    Vec<tokio::sync::mpsc::UnboundedSender<crate::mcp::session::SessionEvent>>,
                >,
            > = std::sync::OnceLock::new();
            SENDERS.get_or_init(|| std::sync::Mutex::new(Vec::new()))
        }
    }

    #[async_trait]
    impl Connector for GatedConnector {
        async fn connect(&self, _server: &ServerConfig) -> crate::error::Result<Connected> {
            let mut connects = lk(&self.connects);
            *connects += 1;
            let first = *connects == 1;
            drop(connects);

            let (tx, events) = tokio::sync::mpsc::unbounded_channel();
            if first {
                lk(self.senders()).push(tx);
            }
            Ok(Connected {
                session: Arc::new(GatedSession {
                    name: if first { "stale" } else { "fresh" },
                    gate: Arc::clone(&self.gate),
                    calls: std::sync::Mutex::new(0),
                }),
                events,
            })
        }

        fn stdio_supported(&self) -> bool {
            true
        }
    }

    struct GatedSession {
        name: &'static str,
        gate: Arc<tokio::sync::Notify>,
        calls: std::sync::Mutex<usize>,
    }

    #[async_trait]
    impl crate::mcp::session::Session for GatedSession {
        async fn list_tools(&self) -> crate::error::Result<Vec<crate::mcp::session::ToolDef>> {
            let call = {
                let mut calls = lk(&self.calls);
                *calls += 1;
                *calls
            };
            // The connect-time fetch answers immediately; the refresh waits.
            if call > 1 {
                self.gate.notified().await;
            }
            Ok(vec![crate::mcp::session::ToolDef {
                name: self.name.to_string(),
                description: None,
                input_schema: serde_json::Map::new(),
            }])
        }

        async fn list_prompts(&self) -> crate::error::Result<Vec<crate::mcp::session::PromptDef>> {
            Ok(Vec::new())
        }

        async fn list_resources(
            &self,
        ) -> crate::error::Result<Vec<crate::mcp::session::ResourceDef>> {
            Ok(Vec::new())
        }

        async fn call_tool(
            &self,
            _name: &str,
            _args: serde_json::Map<String, serde_json::Value>,
        ) -> crate::error::Result<serde_json::Value> {
            Ok(serde_json::Value::Null)
        }

        async fn get_prompt(
            &self,
            _name: &str,
            _args: BTreeMap<String, String>,
        ) -> crate::error::Result<Vec<crate::mcp::session::PromptMessage>> {
            Ok(Vec::new())
        }

        async fn read_resource(
            &self,
            _uri: &str,
        ) -> crate::error::Result<Vec<crate::mcp::session::ResourceChunk>> {
            Ok(Vec::new())
        }

        fn close(&self) {}
    }

    #[tokio::test]
    async fn disabling_a_server_disconnects_it() {
        let f = fixture();
        let id = connected(&f, fake::Spec::default().with_tool("a")).await;

        f.mcp
            .update(
                &id,
                ServerPatch {
                    enabled: Some(false),
                    ..ServerPatch::default()
                },
            )
            .await
            .unwrap();
        f.mcp.resync().await.unwrap();

        assert_eq!(
            f.mcp.get(&id).await.unwrap().status,
            McpConnectionStatus::Disconnected,
        );
    }

    #[tokio::test]
    async fn a_failed_connect_is_remembered_as_an_error_and_does_not_block_the_others() {
        let f = fixture();
        // Two enabled servers; only the second has a fake installed, so the
        // first fails to connect.
        let broken = f.mcp.create(enabled_stdio("Broken")).await.unwrap();
        let working = f.mcp.create(enabled_stdio("Working")).await.unwrap();
        f.connector
            .install(&working.id, fake::Spec::default().with_tool("a"));

        f.mcp.resync().await.unwrap();

        assert_eq!(
            f.mcp.get(&broken.id).await.unwrap().status,
            McpConnectionStatus::Error,
        );
        assert_eq!(
            f.mcp.get(&working.id).await.unwrap().status,
            McpConnectionStatus::Connected,
        );
    }

    #[tokio::test]
    async fn a_prompt_resolves_to_one_flattened_instruction() {
        let f = fixture();
        let id = connected(
            &f,
            fake::Spec::default().with_prompt("commit", "Write a commit message"),
        )
        .await;

        let text = f
            .mcp
            .resolve_prompt(&id, "commit", BTreeMap::new())
            .await
            .unwrap();
        assert_eq!(text, "Write a commit message");
    }

    /// See `Manager::session_or_err` for why this status and not another.
    #[tokio::test]
    async fn a_request_against_a_disconnected_server_is_an_internal_error() {
        let f = fixture();
        let server = f.mcp.create(stdio_draft("Never connected")).await.unwrap();
        let err = f
            .mcp
            .resolve_prompt(&server.id, "commit", BTreeMap::new())
            .await
            .unwrap_err();
        assert_eq!(err.code, crate::error::ErrorCode::InternalError);
        assert!(err.message.contains("is not connected"), "{}", err.message);
    }

    #[tokio::test]
    async fn shutdown_closes_every_connection() {
        let f = fixture();
        let id = connected(&f, fake::Spec::default().with_tool("a")).await;
        f.mcp.shutdown().await;
        settle().await;
        assert_eq!(
            f.mcp.get(&id).await.unwrap().status,
            McpConnectionStatus::Disconnected,
        );
    }

    #[tokio::test]
    async fn only_enabled_tools_are_enabled_in_the_shared_tool_shape() {
        let f = fixture();
        let id = connected(
            &f,
            fake::Spec::default()
                .with_described_tool("search", "Find things")
                .with_tool("write"),
        )
        .await;
        f.mcp.set_tool_enabled(&id, "search", true).await.unwrap();

        let tools = f.mcp.list_all_tools().await.unwrap();
        assert_eq!(tools.len(), 2);
        let search = tools.iter().find(|t| t.name == "search").unwrap();
        assert_eq!(
            search.provider_kind,
            crate::contract::api::ToolProviderKind::Mcp
        );
        assert_eq!(search.provider_name.as_deref(), Some("Fake"));
        assert_eq!(search.description, "Find things");
        assert_eq!(search.id, format!("{id}::search"));
        assert!(search.enabled);
        // Always-available is the default for an MCP tool.
        assert!(search.always_available);
        assert!(!tools.iter().find(|t| t.name == "write").unwrap().enabled);

        f.mcp
            .set_tool_always_available(&id, "search", false)
            .await
            .unwrap();
        let tools = f.mcp.list_all_tools().await.unwrap();
        assert!(
            !tools
                .iter()
                .find(|t| t.name == "search")
                .unwrap()
                .always_available
        );
    }
}

// --- token resolution --------------------------------------------------------

mod token_tests {
    use super::*;
    use crate::mcp::tokens::{self, ResolvableResource, ResourceReader};

    struct Reader;

    #[async_trait]
    impl ResourceReader for Reader {
        async fn read(&self, _server_id: &str, uri: &str) -> crate::error::Result<String> {
            match uri {
                "mem://notes" => Ok("secret plan".to_string()),
                "file:///big" => Ok("x".repeat(100_000)),
                other => Ok(format!("body of {other}")),
            }
        }
    }

    fn resource(name: &str, uri: &str) -> ResolvableResource {
        ResolvableResource {
            server_id: "srv".to_string(),
            server_name: "Fake".to_string(),
            uri: uri.to_string(),
            name: name.to_string(),
        }
    }

    #[tokio::test]
    async fn a_resource_reference_becomes_fenced_reference_data() {
        let result = tokens::resolve(
            "see @my-notes please",
            vec![resource("My Notes", "mem://notes")],
            &Reader,
        )
        .await;
        let block = result.block.expect("a block");
        assert!(block.contains("reference DATA only"));
        assert!(block.contains("secret plan"));
        assert!(result.claimed.contains("my-notes"));
    }

    #[tokio::test]
    async fn a_prompt_token_is_left_alone() {
        let result = tokens::resolve(
            "run /commit",
            vec![resource("commit", "mem://commit")],
            &Reader,
        )
        .await;
        assert!(result.block.is_none());
    }

    #[tokio::test]
    async fn a_repeated_token_resolves_once_and_a_hash_is_never_a_resource() {
        let result = tokens::resolve(
            "@readme then @readme again, not #readme",
            vec![resource("readme", "file:///r")],
            &Reader,
        )
        .await;
        let block = result.block.expect("a block");
        assert_eq!(block.matches("BEGIN RESOURCE").count(), 1);
    }

    #[tokio::test]
    async fn a_large_body_is_capped() {
        let result = tokens::resolve("@big", vec![resource("big", "file:///big")], &Reader).await;
        let block = result.block.expect("a block");
        assert!(block.chars().count() < 65_000, "block too long");
    }

    #[tokio::test]
    async fn an_ambiguous_slug_resolves_to_neither_server() {
        let mut second = resource("read me", "file:///second");
        second.server_id = "other".to_string();
        let result = tokens::resolve(
            "@read-me",
            vec![resource("Read Me", "file:///first"), second],
            &Reader,
        )
        .await;
        assert!(result.block.is_none());
        assert!(result.claimed.is_empty());
    }

    /// The mention boundary is ASCII: an accented letter does NOT suppress the
    /// mention after it, so a Unicode-aware `is_alphanumeric` would refuse
    /// `@notes` in `"café@notes"` and send a different prompt.
    #[tokio::test]
    async fn the_mention_boundary_is_ascii() {
        let resolved = |text: &'static str| async move {
            tokens::resolve(text, vec![resource("notes", "file:///n")], &Reader)
                .await
                .claimed
                .contains("notes")
        };

        assert!(
            resolved("café@notes").await,
            "an accented letter suppressed the mention",
        );
        assert!(resolved("Ω@notes").await);
        assert!(resolved("日本@notes").await);
        // The ASCII half of the rule is unchanged: an email address, a path and
        // a doubled sigil are still not mentions.
        assert!(!resolved("me@notes").await);
        assert!(!resolved("dir/@notes").await);
        assert!(!resolved("@@notes").await);
        assert!(!resolved("x_@notes").await);
        assert!(resolved("(@notes)").await);
    }

    #[test]
    fn slugging_matches_the_reference_syntax() {
        assert_eq!(tokens::slug("My Notes"), "my-notes");
        assert_eq!(tokens::slug("  Spaced  Out  "), "spaced-out");
        assert_eq!(tokens::slug("READ.ME"), "read-me");
    }

    #[test]
    fn flattening_joins_and_caps() {
        use crate::mcp::session::PromptMessage;
        let flat = tokens::flatten_prompt_messages(&[
            PromptMessage {
                text: "line one".to_string(),
            },
            PromptMessage {
                text: "line two".to_string(),
            },
        ]);
        assert_eq!(flat, "line one\nline two");

        let big = tokens::flatten_prompt_messages(&[PromptMessage {
            text: "x".repeat(100_000),
        }]);
        assert!(big.chars().count() <= 64_000);
    }
}

// --- stdio -------------------------------------------------------------------

mod stdio_tests {
    use super::*;
    use crate::mcp::stdio::{plan, StdioSupport};

    fn config(runtime: McpStdioRuntime, command: &str) -> ServerConfig {
        ServerConfig {
            id: "id".to_string(),
            name: "Fake".to_string(),
            kind: McpTransportKind::Stdio,
            command: Some(command.to_string()),
            args: vec!["--flag".to_string()],
            runtime,
            deno_allow_all: true,
            deno_permissions: Vec::new(),
            url: None,
            remote_auth: McpRemoteAuth::None,
            has_auth: false,
            oauth_authorized: false,
            enabled: true,
            tool_enabled: Vec::new(),
            prompt_enabled: Vec::new(),
            tool_always_available_off: Vec::new(),
            created_at_ms: 0,
            updated_at_ms: 0,
        }
    }

    /// A `Paths` with a `deno` binary in place, since the deno runtime resolves
    /// it before anything else.
    fn paths_with_deno() -> (tempfile::TempDir, Paths) {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::rooted(tmp.path());
        paths.ensure_dirs().unwrap();
        let deno = paths.bin_path(crate::sidecars::deno::binary_name());
        std::fs::write(&deno, b"#!/bin/sh\n").unwrap();
        (tmp, paths)
    }

    #[test]
    fn a_device_without_subprocesses_refuses_before_planning_anything() {
        let (_tmp, paths) = paths_with_deno();
        let err = plan(
            &config(McpStdioRuntime::Custom, "npx"),
            &paths,
            StdioSupport::Unsupported,
        )
        .unwrap_err();
        assert_eq!(err.code, crate::error::ErrorCode::ServerUnavailable);
        assert!(err.message.contains("cannot run a local MCP server"));
    }

    #[test]
    fn the_deno_runtime_denies_the_core_secrets_even_under_allow_all() {
        let (_tmp, paths) = paths_with_deno();
        let plan = plan(
            &config(McpStdioRuntime::Deno, "npm:@scope/server"),
            &paths,
            StdioSupport::Available,
        )
        .unwrap();

        assert_eq!(
            plan.program,
            paths.bin_path(crate::sidecars::deno::binary_name())
        );
        assert_eq!(plan.args[0], "run");
        assert_eq!(plan.args[1], "--allow-all");
        let deny_read = &plan.args[2];
        let deny_write = &plan.args[3];
        assert!(deny_read.starts_with("--deny-read="));
        assert!(deny_write.starts_with("--deny-write="));
        for required in [
            paths.secrets_enc_file(),
            paths.admin_token_file(),
            paths.db_file(),
            paths.memories_dir(),
            crate::secrets::master_key_file(paths.root()),
            // The transient the atomic master-key write creates.
            crate::secrets::master_key_tmp_file(paths.root()),
        ] {
            let needle = required.to_string_lossy().to_string();
            assert!(deny_read.contains(&needle), "deny-read is missing {needle}");
            assert!(
                deny_write.contains(&needle),
                "deny-write is missing {needle}",
            );
        }
        // The run target and the user's args come last, in that order.
        assert_eq!(plan.args[4], "npm:@scope/server");
        assert_eq!(plan.args[5], "--flag");
        assert_eq!(
            plan.env.get(&std::ffi::OsString::from("DENO_DIR")),
            Some(&std::ffi::OsString::from(paths.deno_cache_dir())),
        );
    }

    #[test]
    fn a_manual_permission_list_replaces_allow_all() {
        let (_tmp, paths) = paths_with_deno();
        let mut server = config(McpStdioRuntime::Deno, "npm:@scope/server");
        server.deno_allow_all = false;
        server.deno_permissions = vec!["--allow-net=example.com".to_string()];
        let plan = plan(&server, &paths, StdioSupport::Available).unwrap();
        assert_eq!(
            plan.args,
            vec![
                "run".to_string(),
                "--allow-net=example.com".to_string(),
                "npm:@scope/server".to_string(),
                "--flag".to_string(),
            ],
        );
    }

    #[test]
    fn the_environment_carries_nothing_outside_the_allowlist() {
        let (_tmp, paths) = paths_with_deno();
        let plan = plan(
            &config(McpStdioRuntime::Deno, "npm:@scope/server"),
            &paths,
            StdioSupport::Available,
        )
        .unwrap();
        for key in plan.env.keys() {
            let key = key.to_string_lossy().to_string();
            assert!(
                key == "DENO_DIR"
                    || crate::sidecars::process::OPERATIONAL_ENV_KEYS.contains(&key.as_str()),
                "{key} is not on the allowlist",
            );
        }
    }

    #[test]
    fn a_missing_launcher_points_at_the_thing_to_install() {
        let (_tmp, paths) = paths_with_deno();
        let err = plan(
            &config(McpStdioRuntime::Custom, "npx"),
            &paths,
            StdioSupport::Available,
        );
        // `npx` may genuinely exist on a developer machine, so this asserts the
        // message only when the lookup failed.
        if let Err(err) = err {
            assert!(err.message.contains("Install Node.js"), "{}", err.message);
        }
    }

    #[test]
    fn an_absolute_command_that_does_not_exist_is_reported() {
        let (_tmp, paths) = paths_with_deno();
        let err = plan(
            &config(McpStdioRuntime::Custom, "/definitely/not/here/uvx"),
            &paths,
            StdioSupport::Available,
        )
        .unwrap_err();
        assert!(err.message.contains("uvx"), "{}", err.message);
        assert!(err.message.contains("Install it"), "{}", err.message);
    }
}

// --- the OAuth credential store ----------------------------------------------

mod oauth_store_tests {
    use super::*;
    use crate::mcp::oauth::store::VaultCredentials;
    use crate::mcp::secret_key::{auth_secret_name, oauth_secret_name};
    use rmcp::transport::auth::{
        CredentialStore, StateStore, StoredAuthorizationState, StoredCredentials,
    };

    fn store(v: &Vault, server_id: &str) -> VaultCredentials {
        VaultCredentials::new(v.clone(), server_id.to_string())
    }

    /// An in-flight sign-in, as the SDK saves it: the PKCE verifier keyed by
    /// the CSRF token it will come back with. Built through serde rather than
    /// the constructor, which wants `oauth2`'s newtypes.
    fn pending(verifier: &str, csrf: &str) -> StoredAuthorizationState {
        serde_json::from_value(serde_json::json!({
            "pkce_verifier": verifier,
            "csrf_token": csrf,
            "created_at": 0,
        }))
        .unwrap()
    }

    /// Both rmcp traits are one bag under one key, and each half survives the
    /// other being written: the flow saves the verifier and the callback saves
    /// the tokens, so a `save` that did not read first would drop one half.
    #[tokio::test]
    async fn the_two_halves_of_the_bag_do_not_overwrite_each_other() {
        let (_tmp, v) = a_vault();
        let store = store(&v, "srv");

        StateStore::save(&store, "csrf-1", pending("verifier-1", "csrf-1"))
            .await
            .unwrap();
        CredentialStore::save(
            &store,
            StoredCredentials::new("client-1".to_string(), None, Vec::new(), None),
        )
        .await
        .unwrap();

        // A second handle over the same key: what the connect path builds,
        // separately from the flow that wrote it.
        let reopened = store_reopened(&v);
        assert_eq!(
            CredentialStore::load(&reopened)
                .await
                .unwrap()
                .map(|c| c.client_id),
            Some("client-1".to_string()),
            "saving the in-flight state dropped the registration",
        );
        assert_eq!(
            StateStore::load(&reopened, "csrf-1")
                .await
                .unwrap()
                .map(|s| s.pkce_verifier),
            Some("verifier-1".to_string()),
            "saving the registration dropped the in-flight state",
        );

        // Clearing the tokens is not clearing a sign-in in progress.
        CredentialStore::clear(&store).await.unwrap();
        assert!(CredentialStore::load(&store).await.unwrap().is_none());
        assert!(StateStore::load(&store, "csrf-1").await.unwrap().is_some());

        StateStore::delete(&store, "csrf-1").await.unwrap();
        assert!(StateStore::load(&store, "csrf-1").await.unwrap().is_none());
    }

    fn store_reopened(v: &Vault) -> VaultCredentials {
        store(v, "srv")
    }

    /// Everything is under `mcp.<id>.oauth`, and nothing leaks into the bearer
    /// key or another server's bag.
    #[tokio::test]
    async fn the_bag_lives_under_one_key_per_server() {
        let (_tmp, v) = a_vault();
        CredentialStore::save(
            &store(&v, "srv"),
            StoredCredentials::new("client-1".to_string(), None, Vec::new(), None),
        )
        .await
        .unwrap();

        assert!(v.get(&oauth_secret_name("srv")).await.unwrap().is_some());
        assert!(v.get(&auth_secret_name("srv")).await.unwrap().is_none());
        assert!(CredentialStore::load(&store(&v, "other"))
            .await
            .unwrap()
            .is_none());
    }

    /// A bag written in a shape this build cannot read costs one
    /// re-authorization, not a server that can never connect again.
    #[tokio::test]
    async fn an_unreadable_bag_reads_as_absent_rather_than_failing() {
        let (_tmp, v) = a_vault();
        v.set(&oauth_secret_name("srv"), "{\"credentials\":42}")
            .await
            .unwrap();

        assert!(CredentialStore::load(&store(&v, "srv"))
            .await
            .unwrap()
            .is_none());
        // And it is recoverable: a fresh save replaces it.
        CredentialStore::save(
            &store(&v, "srv"),
            StoredCredentials::new("client-1".to_string(), None, Vec::new(), None),
        )
        .await
        .unwrap();
        assert!(CredentialStore::load(&store(&v, "srv"))
            .await
            .unwrap()
            .is_some());
    }
}

// --- the OAuth sign-in flow --------------------------------------------------

mod oauth_flow_tests {
    use super::*;
    use crate::mcp::oauth::flow::Flows;
    use crate::mcp::oauth::store::VaultCredentials;
    use rmcp::transport::auth::CredentialStore;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// An authorization server that publishes no metadata, registers any
    /// client, and issues a token for any code.
    ///
    /// Publishing nothing is deliberate: every discovery well-known 404s, which
    /// is what drives rmcp to its legacy endpoint fallback, so `/authorize`,
    /// `/token` and `/register` are derived from the base URL and one listener
    /// serves the MCP endpoint and the authorization server alike.
    struct FakeAuthServer {
        base: String,
        exchanges: Arc<AtomicUsize>,
        serving: tokio::task::JoinHandle<()>,
    }

    impl FakeAuthServer {
        /// How many codes were presented at the token endpoint. Zero is the
        /// assertion that matters for a rejected callback.
        fn exchanges(&self) -> usize {
            self.exchanges.load(Ordering::Relaxed)
        }
    }

    async fn fake_authorization_server() -> FakeAuthServer {
        let exchanges = Arc::new(AtomicUsize::new(0));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let counted = Arc::clone(&exchanges);
        let app = axum::Router::new()
            .route(
                "/register",
                axum::routing::post(|| async {
                    axum::Json(serde_json::json!({
                        "client_id": "test-client",
                        "client_secret": serde_json::Value::Null,
                        "redirect_uris": [],
                    }))
                }),
            )
            .route(
                "/token",
                axum::routing::post(move || {
                    let counted = Arc::clone(&counted);
                    async move {
                        counted.fetch_add(1, Ordering::Relaxed);
                        axum::Json(serde_json::json!({
                            "access_token": "tok",
                            "token_type": "bearer",
                        }))
                    }
                }),
            )
            .fallback(|| async { (axum::http::StatusCode::NOT_FOUND, "not found") });
        let serving = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        FakeAuthServer {
            base: format!("http://{addr}/mcp"),
            exchanges,
            serving,
        }
    }

    /// A sign-in in progress: the URL the client would open, split into the
    /// loopback redirect and the CSRF `state` the server will echo back.
    struct Started {
        redirect: String,
        csrf: String,
        outcome: Arc<std::sync::Mutex<Option<bool>>>,
    }

    async fn start(flows: &Flows, base: &str, v: &Vault) -> Started {
        let outcome: Arc<std::sync::Mutex<Option<bool>>> = Arc::default();
        let recorded = Arc::clone(&outcome);
        let url = flows
            .start(
                "srv",
                base,
                v,
                Box::new(move |ok| {
                    *lk(&recorded) = Some(ok);
                }),
            )
            .await
            .expect("the sign-in could not be started")
            .expect("a sign-in with no stored tokens has to produce a URL to open");

        let parsed = reqwest::Url::parse(&url).unwrap();
        let mut redirect = None;
        let mut csrf = None;
        for (key, value) in parsed.query_pairs() {
            match key.as_ref() {
                "redirect_uri" => redirect = Some(value.into_owned()),
                "state" => csrf = Some(value.into_owned()),
                _ => {}
            }
        }
        Started {
            redirect: redirect.expect("no redirect_uri in the authorization URL"),
            csrf: csrf.expect("no state in the authorization URL: there is no CSRF token"),
            outcome: Arc::clone(&outcome),
        }
    }

    impl Started {
        /// What the browser does when the authorization server redirects it.
        async fn land(&self, query: &str) -> String {
            reqwest::get(format!("{}?{query}", self.redirect))
                .await
                .expect("the loopback listener was not accepting")
                .text()
                .await
                .unwrap()
        }

        fn outcome(&self) -> Option<bool> {
            *lk(&self.outcome)
        }
    }

    async fn tokens(v: &Vault) -> Option<String> {
        CredentialStore::load(&VaultCredentials::new(v.clone(), "srv".to_string()))
            .await
            .unwrap()
            .and_then(|stored| stored.token_response)
            .map(|_| "present".to_string())
    }

    /// The whole flow, end to end: discovery, dynamic registration, the
    /// authorization URL, the browser landing on the loopback listener, and the
    /// code exchanged for tokens that reach the vault.
    #[tokio::test]
    async fn a_completed_sign_in_exchanges_the_code_and_stores_the_tokens() {
        let auth_server = fake_authorization_server().await;
        let (_tmp, v) = a_vault();
        let flows = Flows::new();

        let started = start(&flows, &auth_server.base, &v).await;
        let page = started
            .land(&format!("code=the-code&state={}", started.csrf))
            .await;

        assert!(page.contains("Signed in"), "the browser saw: {page}");
        assert_eq!(started.outcome(), Some(true), "the caller was not told");
        assert_eq!(auth_server.exchanges(), 1);
        assert!(
            tokens(&v).await.is_some(),
            "the issued tokens never reached the vault",
        );
        auth_server.serving.abort();
    }

    /// The CSRF property, stated as what it protects: a code the flow did not
    /// ask for is never presented at the token endpoint.
    ///
    /// rmcp keys the PKCE verifier by the `state` it issued and looks it up
    /// BEFORE exchanging, so a mismatch costs no request at all. That ordering
    /// is the whole protection, and it is what this asserts; the `state`
    /// requirement in `callback` is a second, local check over the same thing.
    #[tokio::test]
    async fn a_callback_whose_state_does_not_match_never_exchanges_the_code() {
        let auth_server = fake_authorization_server().await;
        let (_tmp, v) = a_vault();
        let flows = Flows::new();

        let started = start(&flows, &auth_server.base, &v).await;
        let page = started
            .land(&format!("code=stolen&state=not-{}", started.csrf))
            .await;

        assert!(page.contains("Sign-in failed"), "the browser saw: {page}");
        assert_eq!(started.outcome(), Some(false));
        assert_eq!(
            auth_server.exchanges(),
            0,
            "a code arriving with the wrong state was exchanged anyway",
        );
        assert!(tokens(&v).await.is_none(), "tokens were stored anyway");
        auth_server.serving.abort();
    }

    /// A callback carrying no code at all (the user pressed Deny) ends the
    /// flow rather than leaving a listener bound for five minutes.
    #[tokio::test]
    async fn a_refused_sign_in_ends_the_flow() {
        let auth_server = fake_authorization_server().await;
        let (_tmp, v) = a_vault();
        let flows = Flows::new();

        let started = start(&flows, &auth_server.base, &v).await;
        let page = started.land("error=access_denied").await;

        assert!(page.contains("Sign-in failed"), "the browser saw: {page}");
        assert_eq!(started.outcome(), Some(false));
        assert_eq!(auth_server.exchanges(), 0);
        // Resolving a flow tears its listener down, so a later callback with a
        // valid-looking code cannot reach it at all.
        settle().await;
        let second = reqwest::get(format!(
            "{}?code=the-code&state={}",
            started.redirect, started.csrf
        ))
        .await;
        assert!(
            second.is_err(),
            "a resolved flow left its loopback listener bound",
        );
        auth_server.serving.abort();
    }

    /// A stray hit on the loopback port must not abort a sign-in in progress:
    /// only `/callback` resolves a flow. The port is real for as long as the
    /// flow is open, so a favicon probe or a port scan can reach it.
    #[tokio::test]
    async fn a_stray_request_on_the_loopback_port_does_not_abort_the_sign_in() {
        let auth_server = fake_authorization_server().await;
        let (_tmp, v) = a_vault();
        let flows = Flows::new();

        let started = start(&flows, &auth_server.base, &v).await;
        let root = started.redirect.replace("/callback", "/favicon.ico");
        let probe = reqwest::get(&root).await.unwrap();
        assert_eq!(probe.status(), reqwest::StatusCode::NOT_FOUND);
        assert_eq!(started.outcome(), None, "a stray hit resolved the flow");

        let page = started
            .land(&format!("code=the-code&state={}", started.csrf))
            .await;
        assert!(page.contains("Signed in"), "the browser saw: {page}");
        assert_eq!(started.outcome(), Some(true));
        auth_server.serving.abort();
    }

    /// How many in-flight sign-in states the bag is holding. Read from the raw
    /// vault entry, because the count is the whole point and `StateStore` can
    /// only be asked about a key it already knows.
    async fn stored_states(v: &Vault) -> usize {
        let raw = v
            .get(&crate::mcp::secret_key::oauth_secret_name("srv"))
            .await
            .unwrap();
        let Some(raw) = raw else {
            return 0;
        };
        serde_json::from_str::<serde_json::Value>(&raw)
            .unwrap()
            .get("states")
            .and_then(|states| states.as_object())
            .map(|states| states.len())
            .unwrap_or(0)
    }

    /// Abandoned sign-ins do not accumulate secret material in the vault: rmcp
    /// removes the PKCE entry only on a successful exchange, so without a reap
    /// every closed browser tab leaves one behind permanently.
    #[tokio::test]
    async fn an_abandoned_sign_in_does_not_pile_up_in_the_vault() {
        let auth_server = fake_authorization_server().await;
        let (_tmp, v) = a_vault();
        let flows = Flows::new();

        let first = start(&flows, &auth_server.base, &v).await;
        assert_eq!(
            stored_states(&v).await,
            1,
            "the sign-in stored no verifier, so this test proves nothing",
        );

        // The browser was closed. Nothing tears the entry down at this point,
        // by design: doing it here would race the next `start`.
        flows.cancel_all();

        for _ in 0..3 {
            let next = start(&flows, &auth_server.base, &v).await;
            assert_ne!(
                next.csrf, first.csrf,
                "the second sign-in reused the first one's state",
            );
            assert_eq!(
                stored_states(&v).await,
                1,
                "abandoned sign-ins are accumulating in the vault",
            );
            flows.cancel_all();
        }
        auth_server.serving.abort();
    }

    /// Reaping the abandoned states must not take the tokens with it: the reap
    /// runs before `initialize_from_store`, so a reap that wrote a fresh bag
    /// would discard the registration and the tokens. The second `start`
    /// returning `None` is what catches it.
    #[tokio::test]
    async fn reaping_an_abandoned_sign_in_leaves_the_tokens_alone() {
        let auth_server = fake_authorization_server().await;
        let (_tmp, v) = a_vault();
        let flows = Flows::new();

        let started = start(&flows, &auth_server.base, &v).await;
        started
            .land(&format!("code=the-code&state={}", started.csrf))
            .await;
        assert!(tokens(&v).await.is_some(), "the first sign-in did not land");

        let again = flows
            .start("srv", &auth_server.base, &v, Box::new(|_| {}))
            .await
            .expect("the sign-in could not be started");
        assert!(
            again.is_none(),
            "a server whose stored tokens still work was sent to the browser again",
        );
        assert!(
            tokens(&v).await.is_some(),
            "starting a new sign-in discarded the tokens the last one earned",
        );
        auth_server.serving.abort();
    }

    /// `cancel` (and so `cancel_all`, which shutdown calls) actually stops the
    /// listener. A listener left bound holds a loopback port open past the flow
    /// it belongs to, and past the process on the way down.
    #[tokio::test]
    async fn cancelling_a_sign_in_releases_the_loopback_port() {
        let auth_server = fake_authorization_server().await;
        let (_tmp, v) = a_vault();
        let flows = Flows::new();

        let started = start(&flows, &auth_server.base, &v).await;
        flows.cancel_all();
        settle().await;

        let landed = reqwest::get(format!("{}?code=x&state=y", started.redirect)).await;
        assert!(
            landed.is_err(),
            "the loopback listener was still accepting after the flow was cancelled",
        );
        assert_eq!(
            started.outcome(),
            None,
            "cancelling reported an outcome; it abandons the flow rather than failing it",
        );
        auth_server.serving.abort();
    }
}

// --- the remote transport ----------------------------------------------------

mod remote_tests {
    use super::*;
    use crate::mcp::remote;
    use std::time::Duration;

    /// A listener that completes the TCP handshake and then says nothing: a
    /// black-holed endpoint. The accepted streams are held rather than dropped,
    /// because dropping one would show the client a connection reset instead.
    async fn black_hole() -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((stream, _)) = listener.accept().await {
                held.push(stream);
            }
        });
        (format!("http://{addr}/mcp"), task)
    }

    /// An endpoint that records the `Authorization` header of every request and
    /// then refuses. What Core SENT is under test, so the handshake failing
    /// afterwards is expected.
    async fn recording_endpoint() -> (String, Seen, tokio::task::JoinHandle<()>) {
        let seen: Seen = Arc::default();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = axum::Router::new().fallback({
            let seen = Arc::clone(&seen);
            move |headers: axum::http::HeaderMap| {
                let seen = Arc::clone(&seen);
                async move {
                    let sent = headers
                        .get(axum::http::header::AUTHORIZATION)
                        .and_then(|value| value.to_str().ok())
                        .map(str::to_string);
                    lk(&seen).push(sent);
                    (axum::http::StatusCode::NOT_FOUND, "not found")
                }
            }
        });
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{addr}/mcp"), seen, task)
    }

    /// One entry per request the endpoint saw, holding its `Authorization`
    /// header if it had one.
    type Seen = Arc<std::sync::Mutex<Vec<Option<String>>>>;

    fn requests(seen: &Seen) -> Vec<Option<String>> {
        lk(seen).clone()
    }

    /// The stored bearer token has to actually reach the wire. Nothing else in
    /// the suite crosses the seam between the vault and an outbound request.
    #[tokio::test]
    async fn a_bearer_token_reaches_the_wire_as_an_authorization_header() {
        let (url, seen, serving) = recording_endpoint().await;
        let f = fixture();
        let mut server = remote_config(&url, McpRemoteAuth::Bearer);
        server.has_auth = true;
        f.vault
            .set(
                &crate::mcp::secret_key::auth_secret_name(&server.id),
                "s3cret",
            )
            .await
            .unwrap();

        let _ = remote::connect_bounded(&server, &f.vault, Duration::from_secs(10)).await;
        serving.abort();

        let sent = requests(&seen);
        assert!(!sent.is_empty(), "the transport made no request at all");
        assert_eq!(
            sent[0].as_deref(),
            Some("Bearer s3cret"),
            "the stored token never reached the wire",
        );
    }

    /// Switching a server back to `none` stops sending a token left over from a
    /// previous bearer setup. The mode gates the read, not `has_auth`.
    #[tokio::test]
    async fn an_open_endpoint_is_sent_no_token_even_when_one_is_stored() {
        let (url, seen, serving) = recording_endpoint().await;
        let f = fixture();
        let mut server = remote_config(&url, McpRemoteAuth::None);
        server.has_auth = true;
        f.vault
            .set(
                &crate::mcp::secret_key::auth_secret_name(&server.id),
                "s3cret",
            )
            .await
            .unwrap();

        let _ = remote::connect_bounded(&server, &f.vault, Duration::from_secs(10)).await;
        serving.abort();

        let sent = requests(&seen);
        assert!(!sent.is_empty(), "the transport made no request at all");
        assert!(
            sent.iter().all(|header| header.is_none()),
            "a leftover bearer token was sent to an open endpoint: {sent:?}",
        );
    }

    /// An `oauth` server that has never been signed into is refused, and reaches
    /// nothing. The network assertion is the one that matters: the error alone
    /// would pass with the refusal moved somewhere that has already reached out.
    #[tokio::test]
    async fn an_unsigned_in_oauth_server_makes_no_request_at_all() {
        let (url, seen, serving) = recording_endpoint().await;
        let f = fixture();
        let server = remote_config(&url, McpRemoteAuth::Oauth);
        assert!(!server.oauth_authorized, "the fixture signed itself in");

        let connected = remote::connect_bounded(&server, &f.vault, Duration::from_secs(10)).await;
        serving.abort();

        let Err(err) = connected else {
            panic!("a server with no OAuth tokens was treated as connected");
        };
        assert_eq!(err.code, crate::error::ErrorCode::ProviderUnauthorized);
        assert_eq!(err.message, remote::SIGN_IN_REQUIRED);
        assert!(
            requests(&seen).is_empty(),
            "an unsigned-in OAuth server reached the network: {:?}",
            requests(&seen),
        );
    }

    /// The `oauth_authorized` gate, in the one state that distinguishes it:
    /// tokens present while the ROW says not authorized. That is reachable,
    /// because `Mcp::start_oauth`'s callback stores the tokens through the SDK
    /// and only then updates the row. Without the gate that server would connect
    /// on every sync using a grant the row does not admit to holding, and the
    /// Sign in button would never be offered.
    #[tokio::test]
    async fn a_server_the_row_says_is_not_signed_in_is_refused_even_with_tokens_stored() {
        use rmcp::transport::auth::{CredentialStore, OAuthTokenResponse, StoredCredentials};

        let (url, seen, serving) = recording_endpoint().await;
        let f = fixture();
        let server = remote_config(&url, McpRemoteAuth::Oauth);

        let token: OAuthTokenResponse = serde_json::from_value(serde_json::json!({
            "access_token": "tok",
            "token_type": "bearer",
        }))
        .unwrap();
        let store =
            crate::mcp::oauth::store::VaultCredentials::new(f.vault.clone(), server.id.clone());
        CredentialStore::save(
            &store,
            StoredCredentials::new("client-1".to_string(), Some(token), Vec::new(), None),
        )
        .await
        .unwrap();
        // Pin the seeding rather than trusting it: the bag is read back through
        // `unwrap_or_default`, so a shape mismatch would read as "no tokens"
        // and this test would pass for the wrong reason.
        assert!(
            CredentialStore::load(&store)
                .await
                .unwrap()
                .is_some_and(|stored| stored.token_response.is_some()),
            "the seeded tokens did not survive a round trip through the bag",
        );

        let connected = remote::connect_bounded(&server, &f.vault, Duration::from_secs(10)).await;
        serving.abort();

        let Err(err) = connected else {
            panic!("a server the row says is not signed in was connected anyway");
        };
        assert_eq!(err.message, remote::SIGN_IN_REQUIRED);
        assert!(
            requests(&seen).is_empty(),
            "a server the row says is not signed in reached the network: {:?}",
            requests(&seen),
        );
    }

    /// The handshake is bounded, and the bound is what returns rather than the
    /// connect hanging forever. Real time, real socket and a real `reqwest`
    /// client, because a stubbed transport cannot see that rmcp's
    /// `expect_response` has no timeout of its own. The 300ms bound is passed in
    /// rather than the 60s [`HANDSHAKE_TIMEOUT`](crate::mcp::session::HANDSHAKE_TIMEOUT).
    #[tokio::test]
    async fn a_remote_server_that_never_answers_is_not_waited_for_forever() {
        let (url, listening) = black_hole().await;
        let f = fixture();
        let server = remote_config(&url, McpRemoteAuth::None);

        let connected = tokio::time::timeout(
            Duration::from_secs(20),
            remote::connect_bounded(&server, &f.vault, Duration::from_millis(300)),
        )
        .await;
        listening.abort();

        let Err(err) = connected.expect("the handshake was never bounded: it did not return")
        else {
            panic!("a server that never answered was treated as connected");
        };
        assert!(
            err.message.contains("did not finish the MCP handshake"),
            "the user is not told what went wrong: {}",
            err.message,
        );
    }
}

// --- routes ------------------------------------------------------------------

mod routes_tests {
    use super::*;

    use axum::body::{to_bytes, Body};
    use axum::http::{Request, StatusCode};
    use serde_json::{json, Value};
    use tower::ServiceExt;

    use crate::mcp::routes;
    use crate::transport::testing::{context_with, StubAuth};

    const TOKEN: &str = "alice-token";

    struct Harness {
        _tmp: tempfile::TempDir,
        router: axum::Router,
        mcp: Mcp,
        connector: Arc<FakeConnector>,
    }

    fn harness() -> Harness {
        let f = fixture();
        let auth = Arc::new(StubAuth::new().with_client(TOKEN, "alice"));
        let mut ctx = context_with(auth);
        ctx.paths = Arc::clone(&f.paths);
        ctx.vault = f.vault.clone();
        let router = routes::routes(&ctx, f.mcp.clone());
        Harness {
            _tmp: f._tmp,
            router,
            mcp: f.mcp,
            connector: f.connector,
        }
    }

    impl Harness {
        async fn call(&self, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
            self.call_with_auth(Some(TOKEN), method, path, body).await
        }

        async fn call_with_auth(
            &self,
            token: Option<&str>,
            method: &str,
            path: &str,
            body: Option<Value>,
        ) -> (StatusCode, Value) {
            let mut builder = Request::builder()
                .method(method)
                .uri(path)
                .header("content-type", "application/json");
            if let Some(token) = token {
                builder = builder.header("authorization", format!("Bearer {token}"));
            }
            let request = match body {
                Some(value) => builder.body(Body::from(value.to_string())).unwrap(),
                None => builder.body(Body::empty()).unwrap(),
            };
            let response = self.router.clone().oneshot(request).await.unwrap();
            let status = response.status();
            let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            let value = if bytes.is_empty() {
                Value::Null
            } else {
                serde_json::from_slice(&bytes).unwrap_or(Value::Null)
            };
            (status, value)
        }

        async fn get(&self, path: &str) -> (StatusCode, Value) {
            self.call("GET", path, None).await
        }

        async fn del(&self, path: &str) -> (StatusCode, Value) {
            self.call("DELETE", path, None).await
        }

        async fn post(&self, path: &str, body: Value) -> (StatusCode, Value) {
            self.call("POST", path, Some(body)).await
        }

        async fn post_none(&self, path: &str) -> (StatusCode, Value) {
            self.call("POST", path, None).await
        }

        async fn patch(&self, path: &str, body: Value) -> (StatusCode, Value) {
            self.call("PATCH", path, Some(body)).await
        }

        async fn create(&self, body: Value) -> String {
            let (status, created) = self.post("/api/v1/mcp", body).await;
            assert_eq!(status, StatusCode::CREATED, "{created}");
            created["id"].as_str().unwrap().to_string()
        }
    }

    #[tokio::test]
    async fn the_slice_requires_a_paired_client() {
        let h = harness();
        let (status, body) = h.call_with_auth(None, "GET", "/api/v1/mcp", None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"]["code"], "missing_token");
    }

    #[tokio::test]
    async fn the_lifecycle_round_trips_over_http() {
        let h = harness();
        let (status, empty) = h.get("/api/v1/mcp").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(empty["servers"], json!([]));

        let id = h
            .create(json!({ "name": "Weather", "kind": "stdio", "command": "npx" }))
            .await;

        let (_, listed) = h.get("/api/v1/mcp").await;
        assert_eq!(listed["servers"].as_array().unwrap().len(), 1);

        let (status, got) = h.get(&format!("/api/v1/mcp/{id}")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(got["name"], "Weather");
        assert_eq!(got["status"], "disconnected");
        assert_eq!(got["toolCount"], 0);
        assert_eq!(got["denoAllowAll"], true);
        assert_eq!(got["hasAuth"], false);

        let (status, patched) = h
            .patch(&format!("/api/v1/mcp/{id}"), json!({ "name": "Renamed" }))
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(patched["name"], "Renamed");
        assert_eq!(patched["command"], "npx");

        let (status, deleted) = h.del(&format!("/api/v1/mcp/{id}")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(deleted["ok"], true);
        let (status, _) = h.get(&format!("/api/v1/mcp/{id}")).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn an_unknown_field_is_rejected() {
        let h = harness();
        let (status, body) = h
            .post(
                "/api/v1/mcp",
                json!({ "name": "X", "kind": "stdio", "nope": true }),
            )
            .await;
        // The body schema is strict, so an unknown key is a 400.
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "validation_error");
    }

    #[tokio::test]
    async fn a_bearer_token_is_stored_but_never_echoed() {
        let h = harness();
        let id = h
            .create(json!({
                "name": "Remote",
                "kind": "remote",
                "url": "https://example.invalid/mcp",
                "remoteAuth": "bearer",
                "authToken": "super-secret",
            }))
            .await;

        let (_, got) = h.get(&format!("/api/v1/mcp/{id}")).await;
        assert_eq!(got["hasAuth"], true);
        assert!(
            !got.to_string().contains("super-secret"),
            "the projection leaked the token: {got}",
        );

        // An empty string clears it; an omitted field leaves it alone.
        let (_, patched) = h
            .patch(
                &format!("/api/v1/mcp/{id}"),
                json!({ "name": "Still Remote" }),
            )
            .await;
        assert_eq!(patched["hasAuth"], true);
        let (_, cleared) = h
            .patch(&format!("/api/v1/mcp/{id}"), json!({ "authToken": "" }))
            .await;
        assert_eq!(cleared["hasAuth"], false);
    }

    /// The MCP credentials share the vault with the settings secrets and must
    /// stay unreachable through `/api/v1/settings/secrets`.
    #[tokio::test]
    async fn an_mcp_token_is_not_a_settings_secret() {
        let h = harness();
        let id = h
            .create(json!({
                "name": "Remote",
                "kind": "remote",
                "url": "https://example.invalid/mcp",
                "remoteAuth": "bearer",
                "authToken": "super-secret",
            }))
            .await;

        let name = crate::mcp::secret_key::auth_secret_name(&id);
        assert!(
            !crate::settings::schema::schema().is_secret_key(&name),
            "{name} would be reachable through the settings secrets routes",
        );
        assert_eq!(
            crate::mcp::secret_key::oauth_secret_name(&id),
            format!("mcp.{id}.oauth"),
        );
    }

    #[tokio::test]
    async fn deleting_a_server_forgets_its_secrets() {
        let h = harness();
        let id = h
            .create(json!({
                "name": "Remote",
                "kind": "remote",
                "url": "https://example.invalid/mcp",
                "remoteAuth": "bearer",
                "authToken": "super-secret",
            }))
            .await;
        let key = crate::mcp::secret_key::auth_secret_name(&id);
        assert!(h.mcp.vault_for_test().get(&key).await.unwrap().is_some());

        h.del(&format!("/api/v1/mcp/{id}")).await;
        assert!(h.mcp.vault_for_test().get(&key).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn enabling_without_a_target_is_a_validation_error() {
        let h = harness();
        let (status, body) = h
            .post(
                "/api/v1/mcp",
                json!({ "name": "Bad", "kind": "stdio", "enabled": true }),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "validation_error");
    }

    #[tokio::test]
    async fn patching_an_unknown_server_is_not_found() {
        let h = harness();
        let (status, _) = h
            .patch("/api/v1/mcp/nope", json!({ "name": "Renamed" }))
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn the_tool_and_prompt_toggles_round_trip() {
        let h = harness();
        let id = h
            .create(json!({ "name": "S", "kind": "stdio", "command": "npx" }))
            .await;

        let (status, on) = h
            .post_none(&format!("/api/v1/mcp/{id}/tools/search/enable"))
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(on["toolEnabled"], json!(["search"]));

        let (_, off) = h
            .post_none(&format!("/api/v1/mcp/{id}/tools/search/disable"))
            .await;
        assert_eq!(off["toolEnabled"], json!([]));

        let (_, prompt_on) = h
            .post_none(&format!("/api/v1/mcp/{id}/prompts/commit/enable"))
            .await;
        assert_eq!(prompt_on["promptEnabled"], json!(["commit"]));

        // Always-available is an opt-OUT set: turning it off adds the name.
        let (_, opted_out) = h
            .post_none(&format!(
                "/api/v1/mcp/{id}/tools/search/always-available/disable"
            ))
            .await;
        assert_eq!(opted_out["toolAlwaysAvailableOff"], json!(["search"]));
        let (_, opted_in) = h
            .post_none(&format!(
                "/api/v1/mcp/{id}/tools/search/always-available/enable"
            ))
            .await;
        assert_eq!(opted_in["toolAlwaysAvailableOff"], json!([]));
    }

    #[tokio::test]
    async fn a_bad_action_is_a_validation_error() {
        let h = harness();
        let id = h
            .create(json!({ "name": "S", "kind": "stdio", "command": "npx" }))
            .await;
        let (status, body) = h
            .post_none(&format!("/api/v1/mcp/{id}/tools/search/toggle"))
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["message"], "bad action");
    }

    /// The two overlaps where a static segment and a parameter compete: axum
    /// resolves by static-beats-parameter, and this pins the answer.
    #[tokio::test]
    async fn a_static_segment_wins_over_the_id_parameter() {
        let h = harness();
        let (status, body) = h.get("/api/v1/mcp/prompts").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["prompts"], json!([]));
        let (status, body) = h.get("/api/v1/mcp/resources").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["resources"], json!([]));

        // `.../prompts/{p}/resolve` must not be read as an enable/disable
        // action, which would answer 400 "bad action" instead of reaching the
        // server.
        let id = h
            .create(json!({ "name": "S", "kind": "stdio", "command": "npx" }))
            .await;
        let (status, body) = h
            .post(
                &format!("/api/v1/mcp/{id}/prompts/commit/resolve"),
                json!({ "arguments": {} }),
            )
            .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body["error"]["code"], "internal_error");
    }

    #[tokio::test]
    async fn resolving_a_prompt_returns_the_flattened_text() {
        let h = harness();
        let id = h
            .create(json!({
                "name": "S", "kind": "stdio", "command": "npx", "enabled": true
            }))
            .await;
        h.connector.install(
            &id,
            fake::Spec::default().with_prompt("commit", "Write a commit message"),
        );
        h.mcp.resync().await.unwrap();

        let (status, body) = h
            .post(
                &format!("/api/v1/mcp/{id}/prompts/commit/resolve"),
                json!({ "arguments": { "scope": "core" } }),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["text"], "Write a commit message");
    }

    #[tokio::test]
    async fn reconnect_returns_the_projection() {
        let h = harness();
        let id = h
            .create(json!({ "name": "S", "kind": "stdio", "command": "npx" }))
            .await;
        let (status, body) = h.post_none(&format!("/api/v1/mcp/{id}/reconnect")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["status"], "disconnected");
    }

    #[tokio::test]
    async fn oauth_start_is_refused_for_anything_but_a_remote_url() {
        let h = harness();
        let id = h
            .create(json!({ "name": "S", "kind": "stdio", "command": "npx" }))
            .await;
        let (status, body) = h.post_none(&format!("/api/v1/mcp/{id}/oauth/start")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(
            body["error"]["message"],
            "OAuth is only for a remote server with a url"
        );
    }

    #[tokio::test]
    async fn the_prompts_and_resources_listings_follow_the_connected_servers() {
        let h = harness();
        let id = h
            .create(json!({
                "name": "S", "kind": "stdio", "command": "npx", "enabled": true
            }))
            .await;
        h.connector.install(
            &id,
            fake::Spec::default()
                .with_prompt("commit", "Write a commit")
                .with_resource("Readme", "file:///readme", "hi"),
        );
        h.mcp.resync().await.unwrap();

        let (_, prompts) = h.get("/api/v1/mcp/prompts").await;
        assert_eq!(prompts["prompts"][0]["name"], "commit");
        assert_eq!(prompts["prompts"][0]["enabled"], false);
        assert_eq!(prompts["prompts"][0]["serverName"], "S");

        let (_, resources) = h.get("/api/v1/mcp/resources").await;
        assert_eq!(resources["resources"][0]["uri"], "file:///readme");
    }
}
