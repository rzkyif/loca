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

/// A core running in this process, with no listener.
///
/// Holds the whole [`Booted`] rather than just its [`Core`], because the rest of
/// it is what keeps the subsystems alive: dropping `Booted` would take the MCP
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
    /// The port is set from the channel even though nothing binds it: it is
    /// what the local inference server's channel-adjusted port derives from
    /// (D-078), so leaving it at whatever the empty environment produced would
    /// be a silent mismatch rather than an unused field.
    /// Boot a core in process with the vault's secure store supplied by the
    /// frontend.
    ///
    /// **There is deliberately no no-store variant.** There was one, and the
    /// adversarial review's F-E showed why it is a hazard: swapping
    /// `start_with_store(root, ch, vault_store(&root))` for `start(root, ch)` at
    /// the Client's single call site put the embedded Core's master key back in
    /// plaintext with every test and clippy green, defeated only by a
    /// source-reading guard that a doc comment satisfied. Deleting the
    /// convenience constructor turns that whole class of regression into a
    /// compile error, which is the one guard that cannot be talked around.
    ///
    /// **This is the only way the Android sealing is reachable.** On Android
    /// there is no OS credential store, so `secrets::store::select` falls back
    /// to a plaintext dotfile; the sealed store needs an AndroidKeyStore key,
    /// which is reachable only through the JVM, so only the frontend can build
    /// one. Passing `None` is correct everywhere else: iOS has
    /// `apple-native-keyring-store` and desktop has a real keychain, and in both
    /// cases `select` picks better than a caller could.
    ///
    /// Kept separate from `start` rather than added to it so no existing caller
    /// moves, matching how `boot_with_store` sits beside `boot_with`.
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
        // Cannot happen: `Frontend::Embedded` is what fills this. Stated rather
        // than unwrapped, because the alternative to a clear message here is a
        // panic inside the app with no context.
        let client = booted
            .local_client
            .clone()
            .ok_or_else(|| internal("an embedded boot returned no local client identity"))?;
        Ok(Self::new(booted, client))
    }

    /// Wrap an already-booted core.
    ///
    /// Separate from [`LocalCore::start_with_store`] so a caller that booted for
    /// itself, such as a test, reaches the same object.
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
///
/// Generic over `T` for one reason: it makes the cell testable without booting a
/// core. The three properties below are the cell's own, not the core's, and a
/// version welded to `LocalCore` could only be exercised by a full boot, which
/// is how this kind of logic ends up untested.
///
/// - **At most one.** A second core would mean a second database handle, a
///   second download queue and a second settings cache over one file, which is
///   the lost update the settings write lock exists to prevent.
/// - **Concurrent callers get the same one**, because the lock is held across
///   the initializer rather than only around the read.
/// - **A failure is NOT cached.** A boot that failed on a transient condition
///   must be retryable, or a single bad start bricks the app until the user
///   kills it. This is why it is not a `OnceCell`.
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
    // These are the only tests that reach `LocalCore`'s own methods, because
    // building one needs a real `Booted`. Without them the four delegations
    // below `start` are complete, unit-tested nowhere, and unreachable, which is
    // the D-100 shape: `ws_open` passes `&self.client.id`, and a wrong argument
    // there would be invisible.

    async fn started(dir: &tempfile::TempDir) -> LocalCore {
        LocalCore::start_with_store(dir.path(), "stable", None)
            .await
            .expect("embedded boot")
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_service_boot_creates_no_reserved_client_row() {
        // The converse of every embedded test here, and the one structural
        // property standing between a network-reachable core and a permanently
        // pre-authenticated client row. The match in `boot_with_store` is the
        // only thing enforcing it, and collapsing that match left the entire
        // repo green (review.md C1).
        //
        // What a `Service` core would gain from that row: it appears in
        // `GET /api/v1/pairing/clients` as "This device" to every paired client,
        // it can be neither revoked nor rotated, it accumulates
        // `client_settings` nobody can clear, and it is one lost `token_hash`
        // invariant away from being a working pre-authenticated identity on a
        // listener bound to 0.0.0.0.
        //
        // The `MemoryStore` is not a convenience: a real `Service` boot runs
        // `tls::ensure` then `vault.set`, which on a developer machine writes an
        // entry into the login keychain. That is why `boot_with_store` exists.
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

        // **This is the assertion that makes the test worth having.** The line
        // above only checks what boot REPORTED; this checks what it DID. A
        // future edit that created the row and simply did not report it would
        // pass the first and fail here, and the row is what carries every
        // consequence listed above.
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

    /// The reserved id, spelled here rather than imported: `auth::LOCAL_CLIENT_ID`
    /// is the value under test, so a test that read it would follow the constant
    /// if someone changed it and assert nothing (the D-084 lens problem).
    const RESERVED_ID: &str = "local";

    #[tokio::test(flavor = "multi_thread")]
    async fn an_embedded_boot_writes_no_certificate() {
        // **This test is the entire argument for `Frontend` being a value
        // rather than a `cfg`.** A cfg-gated skip could not be exercised on any
        // machine anyone develops on, which is precisely how D-117 happened: the
        // mobile half of this project had no host-side coverage at all.
        //
        // An embedded core has no listener to present a certificate on and no
        // pairing handshake to bind its pin, so generating one would seal a key
        // in the vault (a real keychain write on the device) and leave a
        // certificate on disk that only misleads whoever finds it.
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
        // Skip 2 of boot's three embedded differences, and it was unguarded:
        // forcing `StdioSupport::Available` survived the review's mutation pass
        // against the whole repo.
        //
        // A stdio MCP server is a spawned child running a downloaded
        // interpreter. On Android that is a W^X violation and on iOS it is App
        // Store rule 2.5.2, so the store-review symptom of getting this wrong
        // is a rejected build and the user-facing one is a spawn that fails
        // mysteriously instead of `plan` refusing with a reason.
        let dir = tempfile::tempdir().unwrap();
        let core = started(&dir).await;
        assert!(!core.booted().mcp.stdio_supported());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_embedded_boot_does_not_arm_the_scheduler() {
        // Skip 3 of boot's three embedded differences, and the last one that
        // was unguarded: making `scheduled.start()` unconditional survived the
        // review's mutation pass against the whole repo.
        //
        // The arm loop is the only subsystem that STARTS work rather than
        // serving it. On a phone it would open sessions and begin LLM turns in
        // the background of an app the OS suspends and resumes at will.
        //
        // The routes still serve, so a prompt already in the table stays
        // readable and deletable rather than becoming invisible.
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
        // The frontend supplies both because a phone has neither a usable `$HOME`
        // nor a usable environment. If either were read from the environment
        // instead, a dev build would quietly share a stable install's state.
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
        // Reaches `LocalCore::http`, which is the delegation no other test
        // touches. A per-client settings write is the sharpest probe available:
        // it needs the identity AND needs its `clients` row to exist, so a
        // wrong id here is a foreign-key 500 rather than a silent pass.
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
        // The other unreached delegation, and the one where a wrong argument
        // hides: `ws_open` passes `&self.client.id`, so a connection registered
        // under any other id would receive no per-client broadcast and nothing
        // else would notice.
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
                    // Yield inside the initializer, so a cell that only locked
                    // around the read would interleave here and build twice.
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
        // A boot that failed on a transient condition has to be retryable. A
        // `OnceCell` would store the failure and brick the app until restart.
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
