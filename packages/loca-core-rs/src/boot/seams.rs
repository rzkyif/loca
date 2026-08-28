//! The boot-seam suite (D-165).
//!
//! **Why this file exists.** Seven times in this port a subsystem landed
//! complete, tested and unreachable: the module was right, its production call
//! site lived in a file the implementing package did not own, and no test
//! spanned the two. D-134 carried four, D-141 one, N2 one, and the local
//! inference wiring one. Six were found after the fact.
//!
//! Every instance has the same shape, so per-package tests cannot catch it by
//! construction: the package's suite ends at its module boundary, and the line
//! that makes the module reachable is on the other side of it. A source-reading
//! guard cannot catch it either, and that is settled rather than assumed:
//! D-162 established that such a guard is a `grep`, so it states the strings it
//! searches for and never the property it wants, and D-159 has one being
//! satisfied by the file's own doc comments while the code it described had
//! been deleted.
//!
//! What is left is to boot a real core and ask the resulting object graph what
//! it actually got. That is what this file does.
//!
//! **The house rule these follow.** Each test below was watched failing with
//! its own wiring line staged out of `boot.rs`, and passing with it restored.
//! An assertion of "X is wired" is only a test if removing the wiring turns it
//! red, and D-165 exists precisely because the previous state of the world did
//! not: staging the removal of `set_local_provider` left all 2167 feature-on
//! tests green.
//!
//! **These read live state, never source text.** `has_inbound` and
//! `has_local_provider` interrogate the field boot filled, so they cannot be
//! satisfied by a comment describing the code that used to fill it. That is the
//! distinction D-162 draws between an instrument and a restatement.
//!
//! **Adding a seam here.** When `boot.rs` gains a line whose loss would be
//! silent, add a case. The test is: delete the line, and ask whether anything
//! in the crate goes red. If nothing does, it belongs in this file.

use std::sync::Arc;

use crate::config::{Channel, Config};
use crate::paths::Paths;

use super::{Booted, Frontend};

/// Boot a real core into a temp home.
///
/// The `MemoryStore` is not a convenience. A real `Service` boot runs
/// `tls::ensure` and then `vault.set`, which on a developer machine writes an
/// entry into the login keychain, and `LOCA_VAULT_FILE_STORE` cannot help
/// because it is read inside `select()` and `set_var` is unsound in a threaded
/// test binary (`secrets/store.rs:242-244`). `boot_with_store` exists for this.
///
/// The `TempDir` is returned rather than dropped because dropping it deletes
/// the home out from under the core that is still holding it open.
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

/// A runner that must never be the one the scheduler uses.
///
/// Only ever handed to `attach_automated` to read its return value, which is
/// `true` only for the call that WINS the set-once. Boot is supposed to have
/// won already, so this must lose.
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
    // `boot.rs`'s `ctx.frames.on_inbound(Arc::new(chat.clone()))`.
    //
    // Losing it makes the ENTIRE chat module unreachable, and silently: the hub
    // parses a `chat.start` frame, finds no handler, logs a warning and DROPS
    // it. Every route in the module can be perfect and no client can start a
    // turn. Nothing else in the crate covers this line, because the bus is F6's
    // and chat is D4's and the call joining them is boot's.
    let (booted, _dir) = booted(Frontend::Service).await;
    assert!(
        booted.core.frames().has_inbound(),
        "boot returned a core whose frame bus has no inbound handler, so every \
         client frame will be dropped and no chat turn can ever start",
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_real_boot_gives_the_scheduler_its_runner() {
    // `boot.rs`'s `scheduled.attach_automated(...)`, which closes the
    // Chat <-> Scheduler construction cycle.
    //
    // The probe reads `attach_automated`'s return value, which is `true` only
    // for the call that won the set-once `OnceLock`. If boot wired it, this
    // second attach LOSES and answers false. If boot did not, this one wins and
    // answers true, which is the failure.
    //
    // Without boot's call every due tick logs an error and `runNow` answers
    // `server_unavailable`: scheduled prompts and greetings stop firing, and
    // the only symptom is a schedule that quietly never runs.
    let (booted, _dir) = booted(Frontend::Service).await;
    assert!(
        !booted.scheduled.attach_automated(Arc::new(NeverRun)),
        "the scheduler had no automated runner after a real boot, so this probe \
         was able to install one: every scheduled prompt and greeting would fail",
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_desktop_boot_has_a_warm_worker_pool() {
    // `boot.rs`'s `extensions_builder.pool(pool)`.
    //
    // `.pool(...)` is what makes `refresh_permissions` reach live workers and
    // what lets an extension tool run at all. `None` is CORRECT on mobile,
    // where the spawner does not exist because starting a downloaded
    // interpreter is what W^X and App Store 2.5.2 forbid, so this asserts the
    // desktop arm specifically rather than the field being populated in
    // general.
    //
    // **The first version of this test asserted `booted.extension_pool
    // .is_some()` and was VACUOUS**, caught by staging the removal of
    // `.pool(...)` and watching it stay green. Boot builds the pool, hands it
    // back on `Booted` for shutdown, and passes it to the builder in a separate
    // statement; removing only the builder call leaves the field populated. So
    // the field answers "was a pool built", and the property is "did the
    // extension subsystem GET it". Same shape as D-159: the assertion was fine
    // and its lens was pointed one object to the left.
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
    // `boot.rs`'s `llm.set_local_provider(...)`. THE seam of D-165: staging its
    // removal left all 2167 feature-on tests green, which is what made the
    // whole family a systemic problem rather than seven separate oversights.
    //
    // Without it a local turn resolves `HttpProvider` and talks to a
    // `llama-server` sidecar port that Stage 3 removed, so on-device inference
    // fails with a connection error on a build that statically links the engine
    // it declined to use.
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
    // `boot.rs`'s `engine.attach_status(...)`, which closes Q8 (D-167).
    //
    // Same probe shape as the scheduler's runner: `attach_status` answers
    // `true` only for the call that won the set-once, so if boot wired it this
    // second attach LOSES. Without boot's call nothing ever publishes
    // `SidecarKind::Llama`, an unpublished kind reads as `Disabled`, and
    // `LlmLoading::still_loading` answers false for a model that is mid-load:
    // greetings and scheduled prompts fire into the load and error instead of
    // waiting, and the core bar renders no row for a multi-gigabyte read.
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
    // `boot.rs`'s `LlamaEmbedder::new(..)` reaching `Embedding::new`.
    //
    // The seam whose absence lasted longest: the trait, the model identity and
    // `NoLocalEmbedder` all shipped, `NoLocalEmbedder`'s doc said "implemented
    // by N2", and N2 did not. Every core ran with no local embedder, so memory
    // injection was skipped and tool relevance sent the model an unfiltered
    // catalog, on every platform.
    //
    // It survived because the degradation is BY DESIGN: `Embedding::embed`
    // failing with `server_unavailable` is exactly what a fresh install does
    // before its model is downloaded, and every caller is written to shrug at
    // it. Nothing anywhere distinguished "not downloaded yet" from "never
    // built".
    //
    // Which is why the probe reads `is_local_engine` and NOT `is_available`.
    // `is_available` answers false for a real engine whose GGUF is absent, and
    // the temp home this boots into has no GGUF, so a probe on it would be
    // green either way. Same trap as the vacuous worker-pool probe: the
    // assertion would be fine and its lens pointed at the wrong property.
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
    // The converse, load-bearing for the same reason its inference twin below
    // is: without it the assertion above could be satisfied by an embedder
    // installed unconditionally, which would put a llama.cpp-backed engine on a
    // build that never linked llama.cpp.
    let (booted, _dir) = booted(Frontend::Service).await;
    assert!(
        !booted.embedding.has_local_engine(),
        "a build without `local-inference` installed a local embedder anyway",
    );
}

#[cfg(not(feature = "local-inference"))]
#[tokio::test(flavor = "multi_thread")]
async fn a_default_boot_installs_no_in_process_engine() {
    // The converse, and it is load-bearing rather than symmetry for its own
    // sake. Without it the assertion above could be satisfied by a provider
    // installed unconditionally, which would put a local provider on builds
    // that never linked llama.cpp. This pins that the seam is genuinely
    // feature-gated, and it is the arm the DEFAULT gate runs.
    let (booted, _dir) = booted(Frontend::Service).await;
    assert!(
        !booted.llm.has_local_provider(),
        "a build without `local-inference` installed a local provider anyway",
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_real_boot_gives_chat_the_real_tool_host() {
    // `boot.rs`'s `tools.0.set(HostedTools::new(...))`, which closes the fourth
    // construction cycle (chat needs a tool host; the extension supervisor needs
    // chat as its memory indexer).
    //
    // **This is the regression with the widest blast radius found in the port.**
    // `ChatDeps::unwired` installs `NoTools`, whose every catalog read is empty,
    // and boot never called `with_tools`. So the model was offered NO tools at
    // all, extension and MCP alike, on every platform. It fails silently in the
    // worst way: the turn runs normally, and `/api/v1/tools` still lists the
    // whole catalog because that route reads the subsystems directly instead of
    // through this seam. The client shows tools; none of them is callable.
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
    // `boot.rs`'s `.with_hooks(RevocationHooks { .. })`.
    //
    // A WebSocket authenticates ONCE, at upgrade (`frame_bus.rs:185`), so
    // without `close_sockets` a device whose access was just removed keeps a
    // working, streaming connection until it happens to disconnect. The oracle
    // closes it inline in the revoke route and says exactly that
    // (`http/routes/pairing.ts:104`); the port moved it to a hook and never
    // filled it, so revoking deleted a database row and nothing else.
    //
    // `gc_sessions` rides along: sessions are JSON files with no foreign key to
    // cascade, so a revoked client's transcripts and attachments stayed on disk
    // forever.
    let (booted, _dir) = booted(Frontend::Service).await;
    assert!(
        booted.revocation_wired,
        "the revocation hooks are still the no-op defaults, so revoking a client \
         leaves its live sockets streaming and its sessions on disk",
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_real_boot_can_restart_a_subsystem() {
    // `boot.rs`'s `sidecars.attach_control(...)`, which is what the client's
    // Retry button in the Services field reaches.
    //
    // The routes were absent entirely for a while, which the sidecars module's
    // own header recorded as "the client's Retry button has no route to call".
    // They are served now, re-homed onto the in-process engines, and this is the
    // half that stops them answering a validation error forever.
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
    // `boot.rs`'s `frames.add_seed(CoreStatusSeed(..))`.
    //
    // The client's bar is fed by `core.status` frames and by nothing else
    // (`core-status.svelte.ts:28`; `/api/v1/health` is read for capabilities
    // only), and it is constructed holding `starting_up`. Emission is
    // edge-only, so without the seed a client joining a steady core is never
    // told anything and the bar reads "Starting Up" for the whole session.
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
    // `boot.rs`'s `status.subscribe()` loop.
    //
    // The seed alone leaves the bar frozen at whatever was true at connect: no
    // Busy while a turn streams, no Downloading while a model is fetched, no
    // Updating, no Error. `CoreStatusService::subscribe` is documented "every
    // edge, for the hub to broadcast" and had no subscriber at all, which is
    // why the aggregate every other package feeds moved an object nobody read.
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
    // `boot.rs`'s four `StorageDeps::with_*` calls. The mount landed with only
    // `models` and `requirements` filled, and `Unwired` answers three of the
    // other four with an EMPTY list rather than an error, so the storage screen
    // reported no sessions, no extensions and no binaries whatever was on disk
    // and nothing anywhere went red.
    //
    // Each probe below is chosen because `Unwired` cannot produce it: the two
    // deletes fail there with "not wired into this core", the reset fails, and
    // the lock set is empty. None of them depends on the temp home containing
    // any data.
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

    // Binaries: `deno` is required wherever a tool worker can be spawned, and
    // the lock is what stops the screen offering to delete it.
    assert_eq!(
        storage.binaries.locked_kinds().await.unwrap(),
        crate::requirements::required_binary_kinds()
            .iter()
            .map(std::string::ToString::to_string)
            .collect::<std::collections::HashSet<_>>(),
        "the binaries seam is unwired, so every installed binary reads as safe \
         to delete and the binaries category is empty",
    );

    // Settings: the one seam that FAILS rather than degrading, because a
    // factory reset that silently does nothing is worse than one that says it
    // could not run.
    storage.settings.factory_reset().await.expect(
        "the settings seam is unwired, so Clear on the settings category \
                 answers an error instead of resetting anything",
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_real_boot_reconciles_the_memories_on_disk() {
    // `boot.rs`'s `memories.rescan()` pass.
    //
    // The oracle reconciles at boot (`main.ts:217-226`) and the port did not,
    // so a memory added or edited while the core was down stayed invisible or
    // stale until the user happened to find the Rescan button in settings.
    // Nothing else in the crate covers this: the store's rescan is D5's and is
    // tested there, and the line that runs it at boot is boot's.
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
    // `boot.rs`'s `binaries.attach_requirements(..)`.
    //
    // A binaries row reaches `Completed` when its ARCHIVE lands, before the
    // extract and before the version is recorded, so `requirements::edges`
    // refuses those rows and this notification is the ONLY thing that can take
    // an installed binary out of `missing`. Without it the queue shows the
    // download finished and the client's gate stays on "Downloading..." with no
    // further edge to correct it.
    let (booted, _dir) = booted(Frontend::Service).await;
    assert!(
        booted.binaries.has_requirements(),
        "the installer has no requirements notifier, so finishing a binary \
         install never clears it from the pending-downloads gate",
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_real_boot_recomputes_requirements_on_a_download_edge() {
    // `boot.rs`'s `if edges.note(&entries) { requirements.changed(); }` inside
    // the existing queue subscription.
    //
    // The whole join, end to end: a model row reaching a terminal state has to
    // reach the coalescing recompute worker and come back out as a broadcast,
    // or a file that finished downloading stays in `missing` and the client
    // gates the app on it forever. Every piece is unit-tested on its own and
    // the line that connects them is boot's.
    let (booted, dir) = booted(Frontend::Service).await;
    let mut conn = booted.core.frames().register("seam-probe");
    // Drain the core-status seed, so what follows is not it.
    tokio::time::timeout(std::time::Duration::from_secs(5), conn.next())
        .await
        .expect("no seed arrived");

    // The queue's fast path: a file already on disk is recorded `Completed` and
    // broadcast with no network and no worker.
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
