//! The boot-seam suite: a subsystem can be complete and tested and still be
//! unreachable, because the line that wires it lives in `boot.rs` and no
//! per-module suite spans that boundary. So these boot a REAL core and
//! interrogate the resulting object graph, never source text.
//!
//! Adding a seam: when `boot.rs` gains a line whose loss would be silent, add a
//! case. The test is whether deleting that line turns something red.

use std::sync::Arc;

use crate::config::{Channel, Config};
use crate::paths::Paths;

use super::{Booted, Frontend};

/// Boot a real core into a temp home. The `MemoryStore` keeps a `Service` boot
/// from sealing its TLS key into the developer's login keychain; the `TempDir`
/// is returned because dropping it deletes the home out from under the core.
async fn booted(frontend: Frontend) -> (Booted, tempfile::TempDir) {
    booted_in(frontend, tempfile::tempdir().expect("tempdir")).await
}

/// [`booted`] over a home a test prepared first, for the boot passes that read
/// the disk on their way up.
async fn booted_in(frontend: Frontend, dir: tempfile::TempDir) -> (Booted, tempfile::TempDir) {
    let booted = super::boot_with_store(
        Config {
            channel: Channel::Stable,
            port: Channel::Stable.core_port(),
            ..Config::from_env().expect("config")
        },
        Paths::rooted(dir.path()),
        frontend,
        Some(Arc::new(crate::secrets::testing::MemoryStore::new())),
    )
    .await
    .expect("boot");
    (booted, dir)
}

/// Handed to `attach_automated` only to read its return value, `true` for the
/// call that wins the set-once. Boot should have won, so this must lose.
struct NeverRun;

#[async_trait::async_trait]
impl crate::scheduled::deps::AutomatedRunner for NeverRun {
    async fn run(
        &self,
        _input: crate::scheduled::deps::ScheduledRun,
    ) -> crate::error::Result<String> {
        panic!("the seam probe's runner was invoked, so boot's runner is not the live one")
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_real_boot_registers_the_inbound_frame_handler() {
    // `ctx.frames.on_inbound(..)`. Without it the hub parses a `chat.start`
    // frame, finds no handler and drops it: no client can start a turn.
    let (booted, _dir) = booted(Frontend::Service).await;
    assert!(
        booted.core.frames().has_inbound(),
        "boot returned a core whose frame bus has no inbound handler, so every \
         client frame will be dropped and no chat turn can ever start",
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_real_boot_gives_the_scheduler_its_runner() {
    // `scheduled.attach_automated(..)`, which closes the chat <-> scheduler
    // cycle. Without it every due tick errors and `runNow` answers
    // `server_unavailable`, so a schedule quietly never runs.
    let (booted, _dir) = booted(Frontend::Service).await;
    assert!(
        !booted.scheduled.attach_automated(Arc::new(NeverRun)),
        "the scheduler had no automated runner after a real boot, so this probe \
         was able to install one: every scheduled prompt and greeting would fail",
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_desktop_boot_has_a_warm_worker_pool() {
    // `extensions_builder.pool(pool)`, which is what lets an extension tool run
    // at all. Reads the subsystem, NOT `booted.extension_pool`: boot builds the
    // pool and passes it on in a separate statement, so the field stays
    // populated even when the builder call is gone. `None` is correct on
    // mobile, so this asserts the desktop arm specifically.
    let (booted, _dir) = booted(Frontend::Service).await;
    assert!(
        booted.extensions.has_worker_pool(),
        "a desktop boot left the extension subsystem with no worker pool, so \
         `start_tool_call` refuses every call with ServerUnavailable while the \
         frontend holds warm workers nothing can reach",
    );
}

#[cfg(feature = "local-inference")]
#[tokio::test(flavor = "multi_thread")]
async fn a_local_inference_boot_installs_the_in_process_engine() {
    // `llm.set_local_provider(..)`. Without it a local turn resolves
    // `HttpProvider` and dials a sidecar port that does not exist, on a build
    // that statically links the engine it declined to use.
    let (booted, _dir) = booted(Frontend::Service).await;
    assert!(
        booted.llm.has_local_provider(),
        "the `local-inference` build booted without installing the in-process \
         engine, so every local turn resolves HttpProvider against a sidecar \
         port that no longer exists",
    );
}

#[cfg(feature = "local-inference")]
#[tokio::test(flavor = "multi_thread")]
async fn a_local_inference_boot_lets_the_engine_report_its_load_state() {
    // `engine.attach_status(..)`, probed like the scheduler's runner. Without
    // it nothing publishes `SidecarKind::Llama`, an unpublished kind reads as
    // `Disabled`, and automated turns fire into a mid-load model.
    let (booted, _dir) = booted(Frontend::Service).await;
    assert!(
        !booted
            .local_engine
            .attach_status(Arc::clone(&booted.sidecars)),
        "the in-process engine had no status registry after a real boot, so no \
         model load is ever published: automated turns stop deferring and race \
         the load instead",
    );
}

#[cfg(feature = "local-inference")]
#[tokio::test(flavor = "multi_thread")]
async fn a_local_inference_boot_gives_the_embedding_service_a_real_engine() {
    // `LlamaEmbedder::new(..)` reaching `Embedding::new`. Reads
    // `has_local_engine`, not `is_available`: the latter answers false for a
    // real engine whose GGUF is absent, which is every temp home, so a probe on
    // it would be green either way. Without the seam, memory injection and tool
    // relevance phase 1 degrade exactly like a not-yet-downloaded model.
    let (booted, _dir) = booted(Frontend::Service).await;
    assert!(
        booted.embedding.has_local_engine(),
        "the embedding service booted with `NoLocalEmbedder`, so memory relevance and tool \
         relevance phase 1 silently degrade on a build that links llama.cpp",
    );
}

#[cfg(not(feature = "local-inference"))]
#[tokio::test(flavor = "multi_thread")]
async fn a_default_boot_has_no_local_embedder() {
    // The converse: without it the assertion above could be satisfied by an
    // embedder installed unconditionally on a build that has no llama.cpp.
    let (booted, _dir) = booted(Frontend::Service).await;
    assert!(
        !booted.embedding.has_local_engine(),
        "a build without `local-inference` installed a local embedder anyway",
    );
}

#[cfg(not(feature = "local-inference"))]
#[tokio::test(flavor = "multi_thread")]
async fn a_default_boot_installs_no_in_process_engine() {
    // The converse: pins that the seam is genuinely feature-gated. This is the
    // arm the default gate runs.
    let (booted, _dir) = booted(Frontend::Service).await;
    assert!(
        !booted.llm.has_local_provider(),
        "a build without `local-inference` installed a local provider anyway",
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_real_boot_gives_chat_the_real_tool_host() {
    // `tools.0.set(HostedTools::new(..))`, closing the chat <-> tools cycle.
    // Without it `ChatDeps::unwired`'s `NoTools` offers the model nothing while
    // `/api/v1/tools` still lists the whole catalog, because that route reads
    // the subsystems directly: the client shows tools, none of them callable.
    let (booted, _dir) = booted(Frontend::Service).await;
    assert!(
        booted.tools_wired,
        "chat's tool host is still the empty `NoTools`, so the model will be \
         offered no extension or MCP tools while the tools route keeps listing \
         them",
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_real_boot_makes_revocation_actually_revoke() {
    // `.with_hooks(RevocationHooks { .. })`. A WebSocket authenticates once, at
    // upgrade, so without `close_sockets` a revoked device keeps streaming until
    // it disconnects; without `gc_sessions` its transcripts stay on disk, since
    // sessions are JSON files with no foreign key to cascade.
    let (booted, _dir) = booted(Frontend::Service).await;
    assert!(
        booted.revocation_wired,
        "the revocation hooks are still the no-op defaults, so revoking a client \
         leaves its live sockets streaming and its sessions on disk",
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_real_boot_can_restart_a_subsystem() {
    // `sidecars.attach_control(..)`, what the client's Retry button reaches.
    let (booted, _dir) = booted(Frontend::Service).await;
    assert!(
        booted.sidecars.has_control(),
        "no subsystem control was wired, so Retry on a stuck engine answers a \
         validation error and there is no recovery path short of restarting the \
         whole core",
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_real_boot_seeds_a_new_connection_with_the_core_status() {
    // `frames.add_seed(CoreStatusSeed(..))`. The client's bar is fed by
    // `core.status` frames alone and starts on `starting_up`; emission is
    // edge-only, so without the seed a client joining a steady core is never
    // told anything.
    let (booted, _dir) = booted(Frontend::Service).await;
    let mut conn = booted.core.frames().register("seam-probe");

    let Some(crate::transport::Outbound::Frame(first)) =
        tokio::time::timeout(std::time::Duration::from_secs(5), conn.next())
            .await
            .expect("the connection was seeded with nothing within 5s")
    else {
        panic!("expected a frame");
    };
    assert!(
        first.contains("core.status"),
        "a new connection was not seeded with the core status, so the client bar \
         stays on the value it was constructed with: {first}",
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_real_boot_broadcasts_every_core_status_edge() {
    // `boot.rs`'s `status.subscribe()` loop. The seed alone leaves the bar
    // frozen at whatever was true at connect: no Busy, Downloading or Updating.
    let (booted, _dir) = booted(Frontend::Service).await;
    let mut conn = booted.core.frames().register("seam-probe");
    // Drain the seed, so what follows can only be an edge.
    tokio::time::timeout(std::time::Duration::from_secs(5), conn.next())
        .await
        .expect("no seed arrived");

    // An edge no other signal produces at rest, so nothing else can satisfy it.
    booted.status.note_update_staged();

    let Some(crate::transport::Outbound::Frame(next)) =
        tokio::time::timeout(std::time::Duration::from_secs(5), conn.next())
            .await
            .expect("the status edge was never broadcast within 5s")
    else {
        panic!("expected a frame");
    };
    assert!(
        next.contains("core.status") && next.contains("updating"),
        "a core status edge did not reach the connection, so the bar is frozen \
         at whatever was true when the client connected: {next}",
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_real_boot_fills_every_storage_seam() {
    // `boot.rs`'s four `StorageDeps::with_*` calls. `Unwired` answers three of
    // them with an EMPTY list rather than an error, so an unwired storage
    // screen just reports nothing. Each probe below is chosen because `Unwired`
    // cannot produce it, and none needs the temp home to contain data.
    let (booted, _dir) = booted(Frontend::Service).await;
    let storage = &booted.storage;

    // Sessions: an unknown id is a no-op for the real store, an error for the stub.
    storage
        .sessions
        .delete("no-such-session")
        .await
        .expect("the sessions seam is unwired, so the storage screen lists none");

    // Extensions: the real registry answers extension_not_found; the stub
    // answers an internal "not wired".
    let err = storage
        .extensions
        .delete("no-such-extension")
        .await
        .expect_err("deleting an extension that does not exist should fail");
    assert_eq!(
        err.code,
        crate::error::ErrorCode::ExtensionNotFound,
        "the extensions seam is unwired, so the storage screen lists none: {}",
        err.message,
    );

    // Binaries: the lock is what stops the screen offering to delete `deno`.
    assert_eq!(
        storage.binaries.locked_kinds().await.unwrap(),
        crate::requirements::required_binary_kinds()
            .iter()
            .map(std::string::ToString::to_string)
            .collect::<std::collections::HashSet<_>>(),
        "the binaries seam is unwired, so every installed binary reads as safe \
         to delete and the binaries category is empty",
    );

    // Settings: the one seam that fails rather than degrading.
    storage.settings.factory_reset().await.expect(
        "the settings seam is unwired, so Clear on the settings category \
                 answers an error instead of resetting anything",
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_real_boot_reconciles_the_memories_on_disk() {
    // `boot.rs`'s `memories.rescan()` pass. Without it a memory added while the
    // core was down stays invisible until someone presses Rescan.
    let dir = tempfile::tempdir().expect("tempdir");
    let memories = dir.path().join("memories");
    std::fs::create_dir_all(&memories).expect("memories dir");
    std::fs::write(
        memories.join("placed-by-hand.md"),
        "# Placed by hand\n\nDropped in while the core was down.\n",
    )
    .expect("write");

    let (booted, _dir) = booted_in(Frontend::Service, dir).await;

    let listed = booted.memories.list().await.expect("list");
    assert!(
        listed.iter().any(|m| m.filename == "placed-by-hand.md"),
        "a memory file present before boot was never adopted, so it stays \
         invisible until someone presses Rescan: {listed:?}",
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_real_boot_gives_the_binaries_installer_its_requirements_notifier() {
    // `binaries.attach_requirements(..)`. A binaries row reaches `Completed`
    // pre-extract, so `requirements::edges` refuses it and this notification is
    // the only thing that takes an installed binary out of `missing`.
    let (booted, _dir) = booted(Frontend::Service).await;
    assert!(
        booted.binaries.has_requirements(),
        "the installer has no requirements notifier, so finishing a binary \
         install never clears it from the pending-downloads gate",
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_real_boot_recomputes_requirements_on_a_download_edge() {
    // `if edges.note(&entries) { requirements.changed(); }` in the queue
    // subscription: a model row reaching a terminal state has to reach the
    // recompute worker and come back out as a broadcast, or a finished download
    // stays in `missing` and the client gates the app on it forever.
    let (booted, dir) = booted(Frontend::Service).await;
    let mut conn = booted.core.frames().register("seam-probe");
    // Drain the core-status seed, so what follows is not it.
    tokio::time::timeout(std::time::Duration::from_secs(5), conn.next())
        .await
        .expect("no seed arrived");

    // The queue's fast path: a file already on disk is recorded `Completed`
    // with no network and no worker.
    let rel = "models--seam--probe/blobs/probe.gguf";
    let path = dir.path().join("models").join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).expect("model dir");
    std::fs::write(&path, b"weights").expect("write");
    booted
        .downloads
        .enqueue(&crate::downloads::EnqueueSpec {
            source: "@seam/probe/main/probe.gguf".into(),
            destination: crate::contract::api::DownloadEntryDestination::Models,
            group_id: "seam-probe".into(),
            size_hint: None,
            sha256: None,
            url: None,
            rel_path: Some(rel.into()),
            filename: Some("probe.gguf".into()),
        })
        .await
        .expect("enqueue");

    let deadline = std::time::Duration::from_secs(10);
    let started = tokio::time::Instant::now();
    loop {
        assert!(
            started.elapsed() < deadline,
            "a completed download never produced a requirements.snapshot, so a \
             file that finished downloading stays in `missing` and the client's \
             download gate never clears",
        );
        let Some(crate::transport::Outbound::Frame(json)) =
            tokio::time::timeout(deadline, conn.next())
                .await
                .expect("the connection went quiet")
        else {
            continue;
        };
        if json.contains("requirements.snapshot") {
            break;
        }
    }
}
