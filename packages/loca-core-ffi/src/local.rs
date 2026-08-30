//! The booted core the Tauri commands talk to, and the cell that starts it once.

use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;
use tokio::sync::Mutex;

use loca_core::boot::{Booted, Frontend};
use loca_core::config::{Channel, Config};
use loca_core::error::{internal, Result};
use loca_core::paths::Paths;
use loca_core::transport::{ClientIdentity, Core};

use crate::http::{self, CoreHttpReply, CoreHttpRequest};
use crate::ws::{Connections, WsSink};

/// What `core_start` reports back, so the client can show which core it is
/// talking to without a round trip through the HTTP surface.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CoreInfo {
    pub version: String,
    pub channel: String,
    pub root: PathBuf,
}

/// A core running in this process, with no listener. Holds the whole [`Booted`]
/// rather than just its [`Core`]: dropping the rest would take the MCP
/// connections, the sidecar registry and the download queue with it.
pub struct LocalCore {
    booted: Booted,
    /// The identity every in-process request is made as. Resolved once, at
    /// start, because it is a database row rather than a constant.
    client: ClientIdentity,
    ws: Connections,
}

impl LocalCore {
    /// Boot a core in process, under `root`, on `channel`.
    ///
    /// `root` is the app-private directory the frontend supplies, because
    /// Android has no `$HOME` to derive one from. `channel` is the client's own
    /// resolved channel (baked in at build time on mobile, where there is no
    /// usable environment), so the embedded core lands in the same channel as
    /// the app that hosts it rather than defaulting to stable.
    ///
    /// The port is set from the channel even though nothing binds it: the local
    /// inference server's channel-adjusted port derives from it.
    /// Boot a core in process with the vault's secure store supplied by the
    /// frontend.
    ///
    /// **There is deliberately no no-store variant.** Dropping back to one at
    /// the Client's single call site would put the embedded Core's master key
    /// in plaintext with every test green; having no such constructor makes
    /// that a compile error instead.
    ///
    /// **This is the only way the Android sealing is reachable.** Android has no
    /// OS credential store, so `secrets::store::select` falls back to a
    /// plaintext dotfile, and the sealed store needs an AndroidKeyStore key only
    /// the JVM frontend can build. `None` is correct everywhere else.
    pub async fn start_with_store(
        root: impl Into<PathBuf>,
        channel: &str,
        store: Option<Arc<dyn loca_core::secrets::SecureStore>>,
    ) -> Result<Self> {
        let channel = Channel::parse(channel)?;
        let config = Config {
            channel,
            port: channel.core_port(),
            ..Config::from_env()?
        };
        let booted = loca_core::boot::boot_with_store(
            config,
            Paths::rooted(root),
            Frontend::Embedded,
            store,
        )
        .await?;
        // Cannot happen: `Frontend::Embedded` fills this. Stated rather than
        // unwrapped, so the app never panics without context.
        let client = booted
            .local_client
            .clone()
            .ok_or_else(|| internal("an embedded boot returned no local client identity"))?;
        Ok(Self::new(booted, client))
    }

    /// Wrap an already-booted core, so a caller that booted for itself reaches
    /// the same object.
    pub fn new(booted: Booted, client: ClientIdentity) -> Self {
        Self {
            booted,
            client,
            ws: Connections::new(),
        }
    }

    pub fn core(&self) -> &Core {
        &self.booted.core
    }

    pub fn booted(&self) -> &Booted {
        &self.booted
    }

    pub fn client(&self) -> &ClientIdentity {
        &self.client
    }

    pub fn info(&self) -> CoreInfo {
        let ctx = self.booted.core.context();
        CoreInfo {
            version: ctx.config.version.to_string(),
            channel: ctx.config.channel.to_string(),
            root: ctx.paths.root().to_path_buf(),
        }
    }

    pub async fn http(&self, req: CoreHttpRequest) -> Result<CoreHttpReply> {
        http::call(&self.booted.core, &self.client, req).await
    }

    pub fn ws_open(&self, ws_id: &str, sink: Arc<dyn WsSink>) {
        self.ws
            .open(&self.booted.core, &self.client.id, ws_id, sink);
    }

    pub fn ws_send(&self, ws_id: &str, data: String) {
        self.ws.send(ws_id, data);
    }

    pub fn ws_close(&self, ws_id: &str) {
        self.ws.close(ws_id);
    }
}

/// A value built at most once, on first use, by a fallible async initializer.
/// Generic over `T` so the cell is testable without booting a core.
///
/// - **At most one.** A second core means a second database handle, download
///   queue and settings cache over one file.
/// - **Concurrent callers get the same one**, because the lock is held across
///   the initializer rather than only around the read.
/// - **A failure is NOT cached**, so a transient bad boot does not brick the app
///   until the user kills it. This is why it is not a `OnceCell`.
pub struct StartOnce<T>(Mutex<Option<Arc<T>>>);

impl<T> Default for StartOnce<T> {
    fn default() -> Self {
        Self(Mutex::new(None))
    }
}

impl<T> StartOnce<T> {
    pub fn new() -> Self {
        Self::default()
    }

    /// The existing value, or `init`'s, stored for next time.
    pub async fn get_or_init<F, Fut>(&self, init: F) -> Result<Arc<T>>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        let mut slot = self.0.lock().await;
        if let Some(existing) = slot.as_ref() {
            return Ok(Arc::clone(existing));
        }
        let built = Arc::new(init().await?);
        *slot = Some(Arc::clone(&built));
        Ok(built)
    }

    /// The value if it has been built, without building it. Backs
    /// `core_running`.
    pub async fn get(&self) -> Option<Arc<T>> {
        self.0.lock().await.as_ref().map(Arc::clone)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // --- the embedded boot ---------------------------------------------------
    //
    // The only tests that reach `LocalCore`'s own methods, because building one
    // needs a real `Booted`. Without them the four delegations below `start`
    // are unit-tested nowhere: `ws_open` passes `&self.client.id`, and a wrong
    // argument there would be invisible.

    async fn started(dir: &tempfile::TempDir) -> LocalCore {
        LocalCore::start_with_store(dir.path(), "stable", None)
            .await
            .expect("embedded boot")
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_service_boot_creates_no_reserved_client_row() {
        // The one structural property between a network-reachable core and a
        // permanently pre-authenticated client row, enforced only by the match
        // in `boot_with_store`. A `Service` core with that row would show it in
        // `GET /api/v1/pairing/clients`, could neither revoke nor rotate it, and
        // would be one lost `token_hash` invariant away from a working
        // pre-authenticated identity on a listener bound to 0.0.0.0.
        //
        // The `MemoryStore` keeps a real `Service` boot from sealing its TLS key
        // into the developer's login keychain.
        let dir = tempfile::tempdir().unwrap();
        let booted = loca_core::boot::boot_with_store(
            Config {
                channel: Channel::Stable,
                port: Channel::Stable.core_port(),
                ..Config::from_env().expect("config")
            },
            Paths::rooted(dir.path()),
            Frontend::Service,
            Some(Arc::new(loca_core::secrets::testing::MemoryStore::new())),
        )
        .await
        .expect("service boot");

        assert!(booted.local_client.is_none());

        // The line above checks what boot REPORTED; this checks what it DID. An
        // edit that created the row and did not report it passes the first.
        let row = booted
            .core
            .context()
            .db
            .call(|conn| {
                Ok(conn
                    .query_row(
                        "SELECT COUNT(*) FROM clients WHERE id = ?",
                        (crate::local::tests::RESERVED_ID,),
                        |r| r.get::<_, i64>(0),
                    )
                    .unwrap_or(0))
            })
            .await
            .expect("count the reserved row");
        assert_eq!(row, 0, "a Service boot created the reserved client row");
    }

    /// The reserved id, spelled here rather than imported: it is the value
    /// under test, so reading the constant would assert nothing.
    const RESERVED_ID: &str = "local";

    #[tokio::test(flavor = "multi_thread")]
    async fn an_embedded_boot_writes_no_certificate() {
        // **The argument for `Frontend` being a value rather than a `cfg`:** a
        // cfg-gated skip could not be exercised on any development machine.
        //
        // An embedded core has no listener to present a certificate on, so
        // generating one would seal a key in the vault for nothing.
        let dir = tempfile::tempdir().unwrap();
        let core = started(&dir).await;

        let paths = Paths::rooted(dir.path());
        assert!(
            !paths.tls_cert_file().exists(),
            "an embedded boot wrote {}",
            paths.tls_cert_file().display(),
        );
        assert!(core.booted().tls.is_none());
        assert!(core.core().context().tls_pin.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_embedded_boot_does_not_support_stdio_mcp() {
        // Skip 2 of boot's three embedded differences. A stdio MCP server is a
        // spawned child running a downloaded interpreter, which W^X and App
        // Store rule 2.5.2 forbid: the symptom of getting this wrong is a
        // rejected build, and a spawn that fails mysteriously rather than a
        // `plan` that refuses with a reason.
        let dir = tempfile::tempdir().unwrap();
        let core = started(&dir).await;
        assert!(!core.booted().mcp.stdio_supported());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_embedded_boot_does_not_arm_the_scheduler() {
        // Skip 3 of boot's three embedded differences. The arm loop is the only
        // subsystem that STARTS work rather than serving it, and on a phone it
        // would begin LLM turns in the background of an app the OS suspends at
        // will. The routes still serve, so an existing prompt stays readable.
        let dir = tempfile::tempdir().unwrap();
        let core = started(&dir).await;
        assert!(!core.booted().scheduled.is_armed());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_embedded_boot_carries_the_reserved_identity() {
        let dir = tempfile::tempdir().unwrap();
        let core = started(&dir).await;
        assert_eq!(core.client().id, "local");
        assert_eq!(core.client().name, "This device");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_root_and_channel_the_frontend_supplied_are_the_ones_used() {
        // The frontend supplies both because a phone has neither a usable
        // `$HOME` nor a usable environment; reading either from the environment
        // would make a dev build share a stable install's state.
        let dir = tempfile::tempdir().unwrap();
        let core = LocalCore::start_with_store(dir.path(), "dev", None)
            .await
            .expect("boot");
        let info = core.info();
        assert_eq!(info.channel, "dev");
        assert_eq!(info.root, dir.path());
        assert!(dir.path().join("core.sqlite").exists());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_request_through_local_core_is_authenticated_as_the_reserved_client() {
        // Reaches `LocalCore::http`, the delegation no other test touches. A
        // per-client settings write needs the identity AND its `clients` row,
        // so a wrong id is a foreign-key 500 rather than a silent pass.
        let dir = tempfile::tempdir().unwrap();
        let core = started(&dir).await;

        let reply = core
            .http(CoreHttpRequest {
                url: "/api/v1/settings".to_string(),
                method: "PATCH".to_string(),
                headers: std::collections::HashMap::from([(
                    "content-type".to_string(),
                    "application/json".to_string(),
                )]),
                body_b64: Some(
                    base64::Engine::encode(
                        &base64::engine::general_purpose::STANDARD,
                        br#"{"llm.temperature":0.5}"#,
                    )
                    .to_string(),
                ),
            })
            .await
            .expect("the call");
        assert_eq!(reply.status, 200);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn ws_open_registers_under_the_reserved_client_id() {
        // The other unreached delegation: `ws_open` passes `&self.client.id`,
        // so a connection registered under any other id silently receives no
        // per-client broadcast.
        let dir = tempfile::tempdir().unwrap();
        let core = started(&dir).await;

        let sink = Arc::new(RecordingSink::default());
        core.ws_open("w1", sink.clone());

        core.core().frames().broadcast_to_client(
            &core.client().id,
            &loca_core::contract::api::ServerToClientFrame::UpdateStaged {
                version: "7.7.7".to_string(),
            },
        );

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if sink.saw_message_containing("7.7.7") {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the broadcast never reached the connection; \
                 it was registered under the wrong client id",
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        core.ws_close("w1");
    }

    #[derive(Default)]
    struct RecordingSink(std::sync::Mutex<Vec<crate::ws::WsEvent>>);

    impl RecordingSink {
        fn saw_message_containing(&self, needle: &str) -> bool {
            self.0
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .any(|e| matches!(e, crate::ws::WsEvent::Message(m) if m.contains(needle)))
        }
    }

    impl WsSink for RecordingSink {
        fn emit(&self, _ws_id: &str, event: crate::ws::WsEvent) {
            self.0.lock().unwrap_or_else(|e| e.into_inner()).push(event);
        }
    }

    // --- the start-once cell -------------------------------------------------

    #[tokio::test]
    async fn the_initializer_runs_once_however_many_callers_there_are() {
        let cell: StartOnce<usize> = StartOnce::new();
        let calls = AtomicUsize::new(0);
        let init = || async { Ok(calls.fetch_add(1, Ordering::SeqCst)) };

        let first = cell.get_or_init(init).await.unwrap();
        let second = cell.get_or_init(init).await.unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(*first, 0);
        assert_eq!(*second, 0);
        assert!(Arc::ptr_eq(&first, &second));
    }

    #[tokio::test]
    async fn concurrent_callers_share_one_value() {
        let cell: Arc<StartOnce<usize>> = Arc::new(StartOnce::new());
        let calls = Arc::new(AtomicUsize::new(0));

        let mut tasks = Vec::new();
        for _ in 0..8 {
            let cell = Arc::clone(&cell);
            let calls = Arc::clone(&calls);
            tasks.push(tokio::spawn(async move {
                cell.get_or_init(|| async {
                    // A cell that only locked around the read interleaves here
                    // and builds twice.
                    tokio::task::yield_now().await;
                    Ok(calls.fetch_add(1, Ordering::SeqCst))
                })
                .await
                .unwrap()
            }));
        }
        let mut built = Vec::new();
        for task in tasks {
            built.push(task.await.unwrap());
        }

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        for value in &built {
            assert!(Arc::ptr_eq(value, &built[0]));
        }
    }

    #[tokio::test]
    async fn a_failure_is_not_cached_and_the_next_caller_retries() {
        // A `OnceCell` would store the failure and brick the app.
        let cell: StartOnce<usize> = StartOnce::new();
        let attempts = AtomicUsize::new(0);
        let init = || async {
            if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                return Err(internal("first attempt fails"));
            }
            Ok(7usize)
        };

        assert!(cell.get_or_init(init).await.is_err());
        assert_eq!(*cell.get_or_init(init).await.unwrap(), 7);
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn get_does_not_build() {
        let cell: StartOnce<usize> = StartOnce::new();
        assert!(cell.get().await.is_none());
        cell.get_or_init(|| async { Ok(3usize) }).await.unwrap();
        assert_eq!(cell.get().await.map(|v| *v), Some(3));
    }
}
