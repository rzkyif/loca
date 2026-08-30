//! Keeping the resident models equal to what the settings ask for: the decision
//! table, with no process to restart.
//!
//! Three triggers: boot, a settings change, and a model finishing its download.
//! The third is not a subscription of ours, because boot already owns one
//! download-queue subscription and a second subscriber would duplicate the edge
//! detection, so [`Reconciler::note_downloads`] is called BY that loop.
//!
//! Download rows survive restarts, so a row left `Completed` by an earlier run
//! would read as a fresh completion and reload a multi-gigabyte model the
//! moment anything touched the queue. The last-seen map is therefore seeded
//! from the persisted snapshot before subscribing.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::contract::api::{
    DownloadEntry, DownloadStatus, SidecarKind, SidecarSnapshot, SidecarStatus,
};
use crate::settings::{Settings, SettingsRecord};
use crate::sidecars::Manager;

use super::config::{self, Desired, SPEECH_KEYS};

/// The download groups whose completion means a speech model just landed.
const SPEECH_GROUPS: &[&str] = &["stt", "tts"];

/// What a reconciler drives. The engine slots implement this; a test implements
/// it with a recorder, so the whole decision table is exercised without sherpa,
/// without models, and without a filesystem.
pub trait Slots: Send + Sync {
    /// Make the resident models equal `desired`. Returns a message describing
    /// what failed to load, if anything, so the caller can publish `Error`
    /// without this trait knowing what a subsystem row is.
    fn apply(&self, desired: &Desired) -> Result<(), String>;
}

/// Reconciles the local speech engine against the settings.
pub struct Reconciler {
    settings: Settings,
    sidecars: Arc<Manager>,
    slots: Arc<dyn Slots>,
    espeak_dir: PathBuf,
    models_dir: PathBuf,
    seen: Mutex<HashMap<String, DownloadStatus>>,
    /// Serialises the whole read-settings-then-apply sequence: without it the
    /// last writer of the STATUS need not be the last writer of the MODELS.
    /// Three things call `reconcile` and a load takes seconds, so two can
    /// overlap, apply in one order and publish in the other, and leave the row
    /// saying `Running` with nothing resident. Neither state recovers without
    /// another settings change. `tokio`'s mutex because it is held across an
    /// `await`.
    reconciling: tokio::sync::Mutex<()>,
}

impl Reconciler {
    pub fn new(
        settings: Settings,
        sidecars: Arc<Manager>,
        slots: Arc<dyn Slots>,
        espeak_dir: PathBuf,
        models_dir: PathBuf,
    ) -> Self {
        Self {
            settings,
            sidecars,
            slots,
            espeak_dir,
            models_dir,
            seen: Mutex::new(HashMap::new()),
            reconciling: tokio::sync::Mutex::new(()),
        }
    }

    /// Reconcile once against the settings as they are now.
    ///
    /// `load_resolved`, never `load`: the sparse record omits `stt.modelType` /
    /// `stt.modelFiles` while they hold their schema defaults, so a user who
    /// accepted the offered model and patched only `stt.enabled` would derive
    /// `stt: None` and never load a thing. The requirements computation reads
    /// resolved settings and downloads those defaults, so both halves have to
    /// read the same way.
    pub async fn reconcile(&self) {
        // Held for the read AND the apply: see `reconciling`.
        let _serialised = self.reconciling.lock().await;
        let settings = match self.settings.load_resolved().await {
            Ok(settings) => settings,
            Err(err) => {
                tracing::warn!(target: "loca::speech", "could not read settings: {err}");
                return;
            }
        };
        self.apply(&settings).await;
    }

    /// Every call into [`Slots`] goes through `spawn_blocking`: a load is a
    /// multi-gigabyte read plus an ONNX session build, and run inline it would
    /// occupy a tokio worker for minutes, stalling whatever else was scheduled
    /// there with no diagnosis. Decodes use the dedicated worker thread
    /// instead ([`super::worker`]); loads use the blocking pool because they
    /// are driven by settings rather than by a client.
    async fn apply(&self, settings: &SettingsRecord) {
        let models_dir = self.models_dir.clone();
        let desired = config::desired(
            settings,
            &self.espeak_dir,
            &|spec| crate::models::manager::resolve_hf_path(&models_dir, spec),
            &|path| path.is_file(),
        );

        if desired.is_off() {
            // Cleared rather than published as Disabled: a kind nobody has
            // published occupies no subsystem row at all.
            if let Err(err) = self.apply_off_runtime(&desired).await {
                tracing::warn!(target: "loca::speech", "unloading the speech models failed: {err}");
            }
            self.sidecars.clear(SidecarKind::Speech);
            tracing::info!(
                target: "loca::speech",
                "local speech is off (both modules disabled or external, or no model on disk)",
            );
            return;
        }

        // Loading BEFORE the work, so `/tts/status` answers {loaded:false,
        // loading:true} while a multi-gigabyte model is read off disk. That
        // state is what the client's speech toggle renders as a spinner rather
        // than as a broken switch.
        self.publish(SidecarStatus::Loading, None);
        match self.apply_off_runtime(&desired).await {
            Ok(()) => {
                self.publish(SidecarStatus::Running, None);
                tracing::info!(
                    target: "loca::speech",
                    "local speech ready (stt {}, tts {})",
                    desired.stt.is_some(),
                    desired.tts.is_some(),
                );
            }
            Err(message) => {
                // Reported rather than swallowed: nothing else would tell the
                // client a model failed to load.
                tracing::error!(target: "loca::speech", "speech model load failed: {message}");
                self.publish(SidecarStatus::Error, Some(message));
            }
        }
    }

    /// [`Slots::apply`] on the blocking pool. A pool thread that panics or is
    /// shut down mid-load is reported as a load failure rather than swallowed,
    /// so the subsystem row says something went wrong instead of sitting on
    /// `Loading` forever.
    async fn apply_off_runtime(&self, desired: &Desired) -> Result<(), String> {
        let slots = Arc::clone(&self.slots);
        let desired = desired.clone();
        match tokio::task::spawn_blocking(move || slots.apply(&desired)).await {
            Ok(result) => result,
            Err(err) => Err(format!("the speech model loader stopped: {err}")),
        }
    }

    fn publish(&self, status: SidecarStatus, message: Option<String>) {
        self.sidecars.publish(SidecarSnapshot {
            kind: SidecarKind::Speech,
            status,
            message,
            progress: None,
            // No pid, no cpu, no rss: there is no process.
            pid: None,
            cpu_pct: None,
            rss_mb: None,
        });
    }

    /// Seed the last-seen download states without acting on any of them.
    ///
    /// Called with the snapshot boot takes before it subscribes. Without this a
    /// `Completed` row from an earlier run reads as a fresh completion.
    pub fn seed_downloads(&self, entries: &[DownloadEntry]) {
        let mut seen = lock(&self.seen);
        for entry in entries {
            seen.insert(entry.id.clone(), entry.status);
        }
    }

    /// Reconcile if a speech model just finished downloading. Answers whether it
    /// acted, so "a redundant snapshot does nothing" is assertable.
    pub async fn note_downloads(&self, entries: &[DownloadEntry]) -> bool {
        let completed = {
            let mut seen = lock(&self.seen);
            let mut completed = false;
            for entry in entries {
                let was = seen.insert(entry.id.clone(), entry.status);
                // The EDGE, not the state: a row that was already Completed is
                // the persisted result of an earlier run.
                let just_completed = entry.status == DownloadStatus::Completed
                    && was != Some(DownloadStatus::Completed);
                if just_completed && SPEECH_GROUPS.contains(&entry.group_id.as_str()) {
                    completed = true;
                }
            }
            completed
        };
        if !completed {
            return false;
        }
        tracing::info!(target: "loca::speech", "a speech model finished downloading; reloading");
        self.reconcile().await;
        true
    }

    /// True when a settings change touches anything the engine reads.
    pub fn watches(changed: &std::collections::BTreeSet<String>) -> bool {
        changed
            .iter()
            .any(|key| SPEECH_KEYS.contains(&key.as_str()))
    }

    /// Reconcile now, then on every settings change that touches
    /// [`SPEECH_KEYS`]. Owns its own subscription so boot carries no key list.
    /// A lagged receiver resyncs rather than breaking the loop, which would
    /// stop every future reload for the life of the process.
    pub fn spawn_settings_watch(self: &Arc<Self>) {
        let this = Arc::clone(self);
        let mut changes = self.settings.subscribe();
        tokio::spawn(async move {
            this.reconcile().await;
            loop {
                match changes.recv().await {
                    Ok(change) => {
                        if Self::watches(&change.changed) {
                            this.reconcile().await;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(target: "loca::speech", "settings feed lagged {n}");
                        this.reconcile().await;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Where the espeak data lives for a given channel root.
pub fn espeak_dir(root: &Path) -> PathBuf {
    super::espeak::data_dir(root)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use serde_json::json;

    use std::collections::BTreeSet;

    use crate::contract::api::DownloadEntryDestination;
    use crate::transport::status::SidecarSource;
    use crate::transport::testing;

    use super::*;

    /// Records what it was asked to hold, and can be told to fail.
    #[derive(Default)]
    struct Recorder {
        applied: Mutex<Vec<Desired>>,
        fail_with: Mutex<Option<String>>,
        /// What the subsystem row said at the moment `apply` was called, which
        /// is how "Loading is published before the work" becomes assertable.
        seen_status: Mutex<Vec<SidecarStatus>>,
        sidecars: Mutex<Option<Arc<Manager>>>,
        /// Blocks inside `apply`, standing in for a multi-gigabyte model load.
        gate: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
    }

    impl Slots for Recorder {
        fn apply(&self, desired: &Desired) -> Result<(), String> {
            if let Some(sidecars) = lock(&self.sidecars).as_ref() {
                if !desired.is_off() {
                    lock(&self.seen_status).push(sidecars.status(SidecarKind::Speech).status);
                }
            }
            lock(&self.applied).push(desired.clone());
            if let Some(gate) = lock(&self.gate).take() {
                let _ = gate.recv();
            }
            match lock(&self.fail_with).clone() {
                Some(message) => Err(message),
                None => Ok(()),
            }
        }
    }

    struct Harness {
        reconciler: Arc<Reconciler>,
        slots: Arc<Recorder>,
        sidecars: Arc<Manager>,
        settings: Settings,
        models_dir: PathBuf,
        _home: tempfile::TempDir,
    }

    async fn harness() -> Harness {
        let ctx = testing::context();
        // The testing context names a root but does not create it, and
        // `Settings::patch` writes settings.json into it.
        std::fs::create_dir_all(ctx.paths.root()).unwrap();
        let home = tempfile::tempdir().unwrap();
        let slots = Arc::new(Recorder::default());
        let sidecars = Arc::new(Manager::new());
        *lock(&slots.sidecars) = Some(Arc::clone(&sidecars));
        let models_dir = home.path().join("models");
        let reconciler = Arc::new(Reconciler::new(
            ctx.settings.clone(),
            Arc::clone(&sidecars),
            Arc::clone(&slots) as Arc<dyn Slots>,
            home.path().join("espeak-ng-data"),
            models_dir.clone(),
        ));
        Harness {
            reconciler,
            slots,
            sidecars,
            settings: ctx.settings.clone(),
            models_dir,
            _home: home,
        }
    }

    fn entry(id: &str, group: &str, status: DownloadStatus) -> DownloadEntry {
        DownloadEntry {
            abs_path: format!("/models/{id}"),
            added_at_ms: 0,
            destination: DownloadEntryDestination::Models,
            downloaded_bytes: 0,
            error: None,
            filename: format!("{id}.onnx"),
            group_id: group.to_string(),
            id: id.to_string(),
            rel_path: format!("{id}.onnx"),
            size_bytes: None,
            source: "https://example.invalid".to_string(),
            status,
        }
    }

    #[tokio::test]
    async fn speech_off_clears_the_subsystem_row_rather_than_disabling_it() {
        let h = harness().await;
        h.reconciler.reconcile().await;

        // Nothing published: a kind nobody publishes does not render a row.
        assert!(
            h.sidecars
                .statuses()
                .iter()
                .all(|s| s.kind != SidecarKind::Speech),
            "an off engine should occupy no subsystem row",
        );
        // The slots were still told, so a previously resident model is dropped
        // and its memory freed.
        assert_eq!(lock(&h.slots.applied).len(), 1);
        assert!(lock(&h.slots.applied)[0].is_off());
    }

    /// Put a whisper bundle on disk where the resolver will find it and turn
    /// local STT on, so the desired state is genuinely "loaded".
    async fn enable_stt(h: &Harness) {
        let specs = [
            ("encoder", "@a/b/main/enc.onnx"),
            ("decoder", "@a/b/main/dec.onnx"),
            ("tokens", "@a/b/main/tok.txt"),
        ];
        for (_, spec) in specs {
            let path = crate::models::manager::resolve_hf_path(&h.models_dir, spec);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&path, b"weights").unwrap();
        }
        let bundle = format!(
            r#"{{"encoder":"{}","decoder":"{}","tokens":"{}"}}"#,
            specs[0].1, specs[1].1, specs[2].1,
        );
        h.settings
            .patch(
                [
                    ("stt.enabled".to_string(), json!(true)),
                    ("stt.modelType".to_string(), json!("whisper")),
                    ("stt.modelFiles".to_string(), json!(bundle)),
                ]
                .into_iter()
                .collect(),
            )
            .await
            .unwrap();
    }

    /// The path a real user takes: accepting the offered model, so
    /// `stt.modelType` and `stt.modelFiles` keep their schema defaults and are
    /// absent from the sparse record, with `stt.enabled` the only key patched.
    /// Reading `load()` there derives `stt: None` and transcription silently
    /// never works on a machine where the weights are on disk.
    #[tokio::test]
    async fn the_default_model_selection_loads_when_only_the_toggle_is_set() {
        let h = harness().await;

        // The schema default bundle, on disk, exactly as a completed download
        // would leave it.
        for spec in [
            "@csukuangfj/sherpa-onnx-whisper-small/main/small-encoder.int8.onnx",
            "@csukuangfj/sherpa-onnx-whisper-small/main/small-decoder.int8.onnx",
            "@csukuangfj/sherpa-onnx-whisper-small/main/small-tokens.txt",
        ] {
            let path = crate::models::manager::resolve_hf_path(&h.models_dir, spec);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&path, b"weights").unwrap();
        }

        // The ONLY key the user's action writes.
        h.settings
            .patch(
                [("stt.enabled".to_string(), json!(true))]
                    .into_iter()
                    .collect(),
            )
            .await
            .unwrap();

        h.reconciler.reconcile().await;

        let applied = lock(&h.slots.applied);
        let desired = applied.first().expect("the reconciler ran");
        assert!(
            desired.stt.is_some(),
            "the default STT model was not loaded: {desired:?}",
        );
        assert_eq!(
            h.sidecars.status(SidecarKind::Speech).status,
            SidecarStatus::Running,
        );
    }

    #[tokio::test]
    async fn a_loaded_engine_publishes_loading_then_running() {
        let h = harness().await;
        enable_stt(&h).await;
        h.reconciler.reconcile().await;

        let applied = lock(&h.slots.applied);
        assert_eq!(applied.len(), 1);
        assert!(applied[0].stt.is_some(), "the model was found on disk");
        assert_eq!(
            h.sidecars.status(SidecarKind::Speech).status,
            SidecarStatus::Running,
        );
        // Loading is published BEFORE the load. Observed through the recorder
        // rather than by racing the status.
        assert_eq!(
            lock(&h.slots.seen_status).as_slice(),
            &[SidecarStatus::Loading]
        );
    }

    #[tokio::test]
    async fn a_load_failure_is_published_as_error_with_its_reason() {
        let h = harness().await;
        enable_stt(&h).await;
        *lock(&h.slots.fail_with) = Some("failed to load stt model".to_string());

        h.reconciler.reconcile().await;

        let snapshot = h.sidecars.status(SidecarKind::Speech);
        assert_eq!(snapshot.status, SidecarStatus::Error);
        assert_eq!(
            snapshot.message.as_deref(),
            Some("failed to load stt model")
        );
    }

    #[tokio::test]
    async fn turning_speech_off_again_clears_the_row_a_loaded_engine_had_published() {
        let h = harness().await;
        enable_stt(&h).await;
        h.reconciler.reconcile().await;
        assert_eq!(
            h.sidecars.status(SidecarKind::Speech).status,
            SidecarStatus::Running,
        );

        h.settings
            .patch(
                [("stt.enabled".to_string(), json!(false))]
                    .into_iter()
                    .collect(),
            )
            .await
            .unwrap();
        h.reconciler.reconcile().await;

        assert!(
            h.sidecars
                .statuses()
                .iter()
                .all(|s| s.kind != SidecarKind::Speech),
            "the subsystem row survived the engine being turned off",
        );
    }

    #[tokio::test]
    async fn a_redundant_download_snapshot_does_not_reload_the_models() {
        let h = harness().await;
        // A row left Completed by an earlier run, seeded before subscribing.
        let rows = vec![entry("stt-1", "stt", DownloadStatus::Completed)];
        h.reconciler.seed_downloads(&rows);

        assert!(
            !h.reconciler.note_downloads(&rows).await,
            "a persisted Completed row must not read as a fresh completion",
        );
        assert!(lock(&h.slots.applied).is_empty(), "nothing was reloaded");
    }

    #[tokio::test]
    async fn a_speech_model_finishing_its_download_reloads_and_an_unrelated_one_does_not() {
        let h = harness().await;
        h.reconciler
            .seed_downloads(&[entry("stt-1", "stt", DownloadStatus::Downloading)]);

        // An LLM download completing is not ours.
        assert!(
            !h.reconciler
                .note_downloads(&[entry("llm-1", "llm", DownloadStatus::Completed)])
                .await,
        );
        assert!(lock(&h.slots.applied).is_empty());

        // The STT one is.
        assert!(
            h.reconciler
                .note_downloads(&[entry("stt-1", "stt", DownloadStatus::Completed)])
                .await,
        );
        assert_eq!(lock(&h.slots.applied).len(), 1);

        // And it only fires on the edge: the same snapshot again does nothing.
        assert!(
            !h.reconciler
                .note_downloads(&[entry("stt-1", "stt", DownloadStatus::Completed)])
                .await,
        );
        assert_eq!(lock(&h.slots.applied).len(), 1);
    }

    /// A model load must not occupy a tokio worker thread.
    ///
    /// Driven from OUTSIDE the runtime: under `#[tokio::test(worker_threads =
    /// 1)]` the test body shares the single worker with the blocked load and
    /// detects the defect by hanging. Here the runtime is built by hand, both
    /// tasks are spawned onto its single worker, and the main thread waits on a
    /// channel with a deadline.
    #[test]
    fn a_model_load_does_not_occupy_the_async_runtime() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();

        let h = rt.block_on(async {
            let h = harness().await;
            enable_stt(&h).await;
            h
        });
        let (release, gate) = std::sync::mpsc::channel();
        *lock(&h.slots.gate) = Some(gate);

        let reconciler = Arc::clone(&h.reconciler);
        let loading = rt.spawn(async move { reconciler.reconcile().await });

        // Wait, from off the runtime, until the "load" is in progress.
        let mut started = false;
        for _ in 0..400 {
            if !lock(&h.slots.applied).is_empty() {
                started = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(started, "the load never started");

        // The single worker must still be able to run other work.
        let (tx, rx) = std::sync::mpsc::channel();
        rt.spawn(async move {
            let _ = tx.send(42);
        });
        let answer = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the model load occupied the runtime worker: no other task could run");
        assert_eq!(answer, 42);

        let _ = release.send(());
        rt.block_on(loading).unwrap();
        assert_eq!(
            h.sidecars.status(SidecarKind::Speech).status,
            SidecarStatus::Running,
        );
    }

    /// Two reconciles racing must not leave the row and the models disagreeing:
    /// without serialisation an off/on flip applies in one order and publishes
    /// in the other. Whatever the final settings say, the last applied state
    /// and the published row agree.
    #[tokio::test]
    async fn concurrent_reconciles_leave_the_row_agreeing_with_the_models() {
        let h = harness().await;
        enable_stt(&h).await;

        // Two reconciles in flight, then the settings flip off underneath them.
        let a = Arc::clone(&h.reconciler);
        let first = tokio::spawn(async move { a.reconcile().await });
        h.settings
            .patch(
                [("stt.enabled".to_string(), json!(false))]
                    .into_iter()
                    .collect(),
            )
            .await
            .unwrap();
        let b = Arc::clone(&h.reconciler);
        let second = tokio::spawn(async move { b.reconcile().await });

        first.await.unwrap();
        second.await.unwrap();

        let applied = lock(&h.slots.applied);
        let last = applied.last().expect("at least one reconcile ran");
        let row = h.sidecars.status(SidecarKind::Speech).status;
        if last.is_off() {
            assert_eq!(
                row,
                SidecarStatus::Disabled,
                "the models are unloaded but the row still reads {row:?}",
            );
        } else {
            assert_eq!(
                row,
                SidecarStatus::Running,
                "a model is resident but the row reads {row:?}",
            );
        }
    }

    #[test]
    fn the_watch_filter_covers_the_model_selection_keys_and_ignores_unrelated_ones() {
        // Watching only the TTS model pair would leave picking a transcription
        // model doing nothing until something else reconciled.
        let set = |key: &str| BTreeSet::from([key.to_string()]);
        assert!(Reconciler::watches(&set("stt.modelFiles")));
        assert!(Reconciler::watches(&set("stt.modelType")));
        assert!(Reconciler::watches(&set("tts.provider")));
        assert!(Reconciler::watches(&set("stt.threads")));
        assert!(!Reconciler::watches(&set("llm.modelPath")));
        assert!(!Reconciler::watches(&set("ui.theme")));
        assert!(!Reconciler::watches(&BTreeSet::new()));
    }
}
