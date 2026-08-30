//! The store suite: both stores, the deltas they report, and the frames they
//! put on the wire.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use serde_json::json;
use tempfile::TempDir;

use super::*;
use crate::db::migrate;
use crate::transport::Outbound;

struct Env {
    settings: Settings,
    frames: Arc<FrameBus>,
    file: PathBuf,
    _dir: TempDir,
}

fn env() -> Env {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("settings.json");
    let db = Db::open_in_memory().unwrap();
    db.call_blocking(|conn| migrate(conn)).unwrap();
    let frames = Arc::new(FrameBus::new());
    Env {
        settings: Settings::with_file(&file, db.clone(), Arc::clone(&frames)),
        frames,
        file,
        _dir: dir,
    }
}

impl Env {
    /// A second handle over the same file and database, which is what "the next
    /// boot" means here.
    fn reopen(&self) -> Settings {
        Settings::with_file(
            &self.file,
            self.settings.inner.db.clone(),
            Arc::clone(&self.frames),
        )
    }

    fn add_client(&self, id: &str) {
        self.settings
            .inner
            .db
            .call_blocking(|conn| {
                conn.execute(
                    "INSERT INTO clients (id, name, token_hash, created_at_ms, last_seen_ms) \
                     VALUES (?1, 'test', ?2, 0, 0)",
                    [id, &format!("hash-{id}")],
                )?;
                Ok(())
            })
            .unwrap();
    }
}

fn map(value: Value) -> SettingsRecord {
    match value {
        Value::Object(map) => map,
        other => panic!("not an object: {other}"),
    }
}

/// The next frame on a connection, decoded, or `None` if none is queued.
fn next_frame(conn: &mut crate::transport::Connection) -> Option<Value> {
    match conn.next().now_or_never().flatten() {
        Some(Outbound::Frame(json)) => Some(serde_json::from_str(&json).unwrap()),
        Some(other) => panic!("expected a frame, got {other:?}"),
        None => None,
    }
}

use futures_util::FutureExt as _;

// --- the shared store --------------------------------------------------------

#[tokio::test]
async fn an_absent_file_reads_as_an_empty_record() {
    let env = env();
    assert!(env.settings.load().await.unwrap().is_empty());
    // And nothing was created by reading.
    assert!(!env.file.exists());
}

#[tokio::test]
async fn an_existing_file_is_read_as_it_stands() {
    let env = env();
    std::fs::write(
        &env.file,
        r#"{ "llm.host": "0.0.0.0", "server.bindHost": "1.2.3.4" }"#,
    )
    .unwrap();
    let loaded = env.settings.load().await.unwrap();
    assert_eq!(loaded["llm.host"], "0.0.0.0");
    // A non-schema key is part of the record. Sanitization happens at the API
    // boundary, not here: boot reads `server.bindHost` from this same file.
    assert_eq!(loaded["server.bindHost"], "1.2.3.4");
}

#[tokio::test]
async fn a_file_that_is_not_an_object_is_our_bug_not_the_clients() {
    for contents in ["[1,2,3]", "not json at all"] {
        let env = env();
        std::fs::write(&env.file, contents).unwrap();
        let err = env.settings.load().await.unwrap_err();
        // `internal_error`, never `validation_error`: the blanket serde mapping
        // is right for an inbound body and wrong for a file we wrote.
        assert_eq!(err.code, ErrorCode::InternalError, "{contents}");
    }
}

#[tokio::test]
async fn a_patch_merges_and_survives_the_next_boot() {
    let env = env();
    env.settings
        .patch(map(
            json!({ "llm.host": "0.0.0.0", "llm.contextSize": 8192 }),
        ))
        .await
        .unwrap();
    let merged = env
        .settings
        .patch(map(json!({ "llm.contextSize": 4096 })))
        .await
        .unwrap();
    assert_eq!(merged["llm.host"], "0.0.0.0");
    assert_eq!(merged["llm.contextSize"], 4096);
    assert_eq!(env.reopen().load().await.unwrap(), merged);
}

#[tokio::test]
async fn a_null_deletes_and_deleting_an_absent_key_changes_nothing() {
    let env = env();
    let mut changes = env.settings.subscribe();
    env.settings
        .patch(map(json!({ "llm.host": "0.0.0.0" })))
        .await
        .unwrap();
    changes.recv().await.unwrap();

    let merged = env
        .settings
        .patch(map(json!({ "llm.host": null })))
        .await
        .unwrap();
    assert!(merged.is_empty());
    let change = changes.recv().await.unwrap();
    assert_eq!(change.changed.iter().collect::<Vec<_>>(), ["llm.host"]);

    // Deleting it again is not a change, so nothing is published.
    env.settings
        .patch(map(json!({ "llm.host": null })))
        .await
        .unwrap();
    assert!(changes.recv().now_or_never().is_none());
}

#[tokio::test]
async fn resolved_settings_layer_the_sparse_record_over_the_schema_defaults() {
    let env = env();
    env.settings
        .patch(map(json!({ "llm.contextSize": 8192 })))
        .await
        .unwrap();
    let resolved = env.settings.load_resolved().await.unwrap();
    assert_eq!(resolved["llm.contextSize"], 8192);
    // Never persisted, and requirement computation depends on seeing it.
    assert_eq!(
        resolved["llm.modelPath"],
        json!("@unsloth/Qwen3.5-2B-GGUF/main/Qwen3.5-2B-Q4_K_M.gguf")
    );
    assert!(!env
        .settings
        .load()
        .await
        .unwrap()
        .contains_key("llm.modelPath"));
}

#[tokio::test]
async fn a_reset_empties_the_file_and_reports_every_previous_key() {
    let env = env();
    env.settings
        .patch(map(
            json!({ "llm.host": "0.0.0.0", "llm.contextSize": 8192 }),
        ))
        .await
        .unwrap();
    let mut conn = env.frames.register("c1");
    while next_frame(&mut conn).is_some() {}

    env.settings.reset().await.unwrap();
    assert!(env.settings.load().await.unwrap().is_empty());
    assert!(env.reopen().load().await.unwrap().is_empty());
    let frame = next_frame(&mut conn).unwrap();
    assert_eq!(frame["kind"], "settings.updated");
    assert_eq!(frame["values"], json!({}));
    assert_eq!(frame["deleted"], json!(["llm.contextSize", "llm.host"]));
}

#[tokio::test]
async fn concurrent_patches_do_not_lose_each_others_writes() {
    // The tripwire for the write lock: without it each task reads the same base
    // record and the last write wins, so most of these keys vanish.
    let env = env();
    let mut tasks = Vec::new();
    for i in 0..50 {
        let settings = env.settings.clone();
        tasks.push(tokio::spawn(async move {
            settings
                .patch(map(json!({ format!("llm.host{i}"): i })))
                .await
                .unwrap();
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }
    let on_disk = env.reopen().load().await.unwrap();
    assert_eq!(on_disk.len(), 50, "lost writes: {on_disk:?}");
}

#[tokio::test]
async fn re_sending_the_same_value_is_not_a_change() {
    let env = env();
    let mut changes = env.settings.subscribe();
    env.settings
        .patch(map(json!({ "llm.contextSize": 4096 })))
        .await
        .unwrap();
    changes.recv().await.unwrap();

    // `1` and `1.0` are the same number on the wire and in the schema, and
    // serde_json says they differ. Left unfolded, this reports a change, and
    // `changed` is what drives the requirements recompute and the sidecar
    // restarts: a no-op PATCH would restart the model server.
    env.settings
        .patch(map(json!({ "llm.contextSize": 4096.0 })))
        .await
        .unwrap();
    assert!(changes.recv().now_or_never().is_none());
}

// --- the per-client overlay --------------------------------------------------

#[tokio::test]
async fn an_overlay_is_layered_over_the_shared_store_for_its_owner_only() {
    let env = env();
    env.add_client("c1");
    env.add_client("c2");
    env.settings
        .patch(map(json!({ "llm.contextSize": 8192 })))
        .await
        .unwrap();
    env.settings
        .patch_client("c1", map(json!({ "llm.temperature": 0.2 })))
        .await
        .unwrap();

    let mine = env.settings.load_effective("c1").await.unwrap();
    assert_eq!(mine["llm.temperature"], 0.2);
    assert_eq!(mine["llm.contextSize"], 8192);

    let theirs = env.settings.load_effective("c2").await.unwrap();
    assert!(!theirs.contains_key("llm.temperature"));
    // The shared half is shared.
    assert_eq!(theirs["llm.contextSize"], 8192);
}

#[tokio::test]
async fn the_resolved_effective_read_carries_defaults_under_the_overlay() {
    // Neither half alone is correct for a chat turn: `load_resolved` drops the
    // overlay and `load_effective` stays sparse. Reading sparse resolves an
    // unset thinking budget to 0, because `num_setting` falls back to its
    // CALLER's value rather than the schema's.
    let env = env();
    env.add_client("c1");
    env.settings
        .patch_client("c1", map(json!({ "llm.temperature": 0.2 })))
        .await
        .unwrap();

    let sparse = env.settings.load_effective("c1").await.unwrap();
    assert!(!sparse.contains_key("prompts.titleGenerationThinkingBudget"));

    let resolved = env.settings.load_effective_resolved("c1").await.unwrap();
    assert_eq!(resolved["prompts.titleGenerationThinkingBudget"], 128);
    assert_eq!(resolved["prompts.memorySummaryThinkingBudget"], 256);
    // The overlay still wins over the schema default, which is the half
    // `load_resolved` loses.
    assert_eq!(resolved["llm.temperature"], 0.2);

    // An explicit 0 is a real value, not an absence: it must survive resolution
    // or turning a budget OFF would silently re-enable it at the schema default.
    env.settings
        .patch_client(
            "c1",
            map(json!({ "prompts.titleGenerationThinkingBudget": 0 })),
        )
        .await
        .unwrap();
    assert_eq!(
        env.settings.load_effective_resolved("c1").await.unwrap()
            ["prompts.titleGenerationThinkingBudget"],
        0
    );
}

#[tokio::test]
async fn an_overlay_delta_reports_sets_and_deletions_and_survives_the_next_boot() {
    let env = env();
    env.add_client("c1");
    let delta = env
        .settings
        .patch_client("c1", map(json!({ "llm.temperature": 0.2, "llm.topK": 40 })))
        .await
        .unwrap();
    assert_eq!(delta.values.len(), 2);
    assert!(delta.deleted.is_empty());

    let delta = env
        .settings
        .patch_client(
            "c1",
            map(json!({ "llm.temperature": null, "llm.topK": 40, "llm.minP": null })),
        )
        .await
        .unwrap();
    assert_eq!(delta.deleted, ["llm.temperature"]);
    // Unchanged and never-set keys are not part of the delta.
    assert!(delta.values.is_empty());

    assert_eq!(
        env.reopen().load_effective("c1").await.unwrap()["llm.topK"],
        40
    );
}

#[tokio::test]
async fn a_corrupt_overlay_row_is_dropped_rather_than_failing_the_read() {
    let env = env();
    env.add_client("c1");
    env.settings
        .inner
        .db
        .call_blocking(|conn| {
            conn.execute(
                "INSERT INTO client_settings (client_id, key, value_json) VALUES ('c1', 'llm.topK', 'not json')",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    let overlay = env.settings.load_overlay("c1").await.unwrap();
    assert!(overlay.is_empty());
}

#[tokio::test]
async fn resetting_every_overlay_tells_each_client_what_it_lost() {
    let env = env();
    env.add_client("c1");
    env.add_client("c2");
    env.settings
        .patch_client("c1", map(json!({ "llm.temperature": 0.2 })))
        .await
        .unwrap();
    env.settings
        .patch_client("c2", map(json!({ "llm.topK": 40 })))
        .await
        .unwrap();
    let mut one = env.frames.register("c1");
    let mut two = env.frames.register("c2");

    env.settings.reset_all_clients().await.unwrap();
    assert!(env.settings.load_overlay("c1").await.unwrap().is_empty());
    assert!(env.settings.load_overlay("c2").await.unwrap().is_empty());
    assert_eq!(
        next_frame(&mut one).unwrap()["deleted"],
        json!(["llm.temperature"])
    );
    assert_eq!(
        next_frame(&mut two).unwrap()["deleted"],
        json!(["llm.topK"])
    );
}

// --- broadcasts --------------------------------------------------------------

#[tokio::test]
async fn a_shared_change_reaches_every_client_and_an_overlay_change_reaches_one() {
    let env = env();
    env.add_client("c1");
    let mut one = env.frames.register("c1");
    let mut two = env.frames.register("c2");

    env.settings
        .patch(map(json!({ "llm.contextSize": 8192 })))
        .await
        .unwrap();
    for conn in [&mut one, &mut two] {
        let frame = next_frame(conn).unwrap();
        assert_eq!(frame["kind"], "settings.updated");
        assert_eq!(frame["values"], json!({ "llm.contextSize": 8192 }));
        assert_eq!(frame["deleted"], json!([]));
    }

    env.settings
        .patch_client("c1", map(json!({ "llm.temperature": 0.2 })))
        .await
        .unwrap();
    assert_eq!(
        next_frame(&mut one).unwrap()["values"],
        json!({ "llm.temperature": 0.2 })
    );
    // Another client must never see, or converge onto, these.
    assert!(next_frame(&mut two).is_none());
}

#[tokio::test]
async fn a_non_schema_or_secret_key_never_crosses_the_wire() {
    let env = env();
    let mut conn = env.frames.register("c1");
    env.settings
        .patch(map(json!({
            "internal.junk": 1,
            "server.bindHost": "0.0.0.0",
            "llm.external.apiKey": "sk-must-not-broadcast",
            "llm.contextSize": 8192,
        })))
        .await
        .unwrap();
    let frame = next_frame(&mut conn).unwrap();
    assert_eq!(frame["values"], json!({ "llm.contextSize": 8192 }));
    assert!(!frame.to_string().contains("sk-must-not-broadcast"));
    // Nothing else went out.
    assert!(next_frame(&mut conn).is_none());
}

#[tokio::test]
async fn a_vault_change_rides_the_same_frame_as_a_name_list() {
    let env = env();
    let vault = crate::secrets::Vault::with_store(
        env._dir.path().join("secrets.enc"),
        Arc::new(crate::secrets::testing::MemoryStore::new()),
    );
    env.settings.spawn_secret_name_relay(&vault);
    // Spawning twice is a no-op, so a second router build does not double every
    // frame.
    env.settings.spawn_secret_name_relay(&vault);
    let mut conn = env.frames.register("c1");

    vault.set("llm.external.apiKey", "sk-value").await.unwrap();
    // The relay is its own task, so give it a turn.
    tokio::task::yield_now().await;
    let frame = next_frame(&mut conn).unwrap();
    assert_eq!(frame["kind"], "settings.updated");
    assert_eq!(frame["secretNames"], json!(["llm.external.apiKey"]));
    assert!(!frame.to_string().contains("sk-value"));
    assert!(next_frame(&mut conn).is_none());
}

/// Deleting the last secret must put an EMPTY list on the wire, not omit the
/// field: the client clears its "saved" placeholders from `[]`, and an absent
/// field leaves them showing a secret that is gone until it refetches.
#[tokio::test]
async fn clearing_the_last_secret_broadcasts_an_empty_name_list() {
    let env = env();
    let vault = crate::secrets::Vault::with_store(
        env._dir.path().join("secrets.enc"),
        Arc::new(crate::secrets::testing::MemoryStore::new()),
    );
    env.settings.spawn_secret_name_relay(&vault);
    let mut conn = env.frames.register("c1");

    vault.set("llm.external.apiKey", "sk-value").await.unwrap();
    tokio::task::yield_now().await;
    next_frame(&mut conn).unwrap();

    vault.delete("llm.external.apiKey").await.unwrap();
    tokio::task::yield_now().await;
    let frame = next_frame(&mut conn).unwrap();
    assert_eq!(frame["kind"], "settings.updated");
    assert_eq!(
        frame["secretNames"],
        json!([]),
        "an empty name list must be present, not skipped"
    );
}
