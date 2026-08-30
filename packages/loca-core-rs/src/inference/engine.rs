//! The loaded model, the one context, and the rules for when each is replaced.
//! The first file here that names a shim type, so it is gated while everything
//! it decides with ([`super::options`], [`super::backends`]) is not. The
//! conversions into the shim's option structs live here and nowhere else.
//!
//! [`LocalEngine::ensure_loaded`] makes three decisions: gated off (external
//! provider or no model) unloads whatever is resident; a model file not yet on
//! disk logs and waits, since the requirements flow is probably downloading it;
//! otherwise a changed [`LoadSpec`] reloads.
//!
//! Lock ordering: `state` before `context`, always. The two are separate
//! because a turn holds the context for its whole generation and must not block
//! a settings read for that long.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::Mutex;

use crate::contract::api::{SidecarKind, SidecarSnapshot, SidecarStatus};
use crate::error::Result;
use crate::models::manager::resolve_hf_path;
use crate::paths::Paths;
use crate::settings::{Settings, SettingsRecord};
use crate::sidecars::Manager;

use super::options::{self, ContextSpec, LoadSpec};
use super::shim;

#[cfg(not(any(target_os = "android", target_os = "ios")))]
use super::backends;

/// The model, its templates, and the specification they were loaded for.
struct Loaded {
    model: Arc<shim::Model>,
    templates: Arc<shim::ChatTemplates>,
    /// The multimodal projector, when one was configured and loaded. Behind its
    /// own `Mutex` because `mtmd_helper_eval_chunks` is not thread-safe and is
    /// held across a whole turn's prefill.
    mtmd: Option<Arc<Mutex<shim::Mtmd>>>,
    n_ctx_train: i32,
    spec: LoadSpec,
}

/// What a turn needs to run, taken out from under the lock. Everything is an
/// `Arc` so a turn holds the model alive for its own duration even if an idle
/// unload lands mid-generation; the alternative is holding the load lock for
/// the length of a generation.
pub struct Ready {
    pub model: Arc<shim::Model>,
    pub templates: Arc<shim::ChatTemplates>,
    /// The loaded projector, or `None` for a text-only turn.
    pub mtmd: Option<Arc<Mutex<shim::Mtmd>>>,
    /// The context length the model was TRAINED at, not the one it is loaded
    /// with: a user may configure either side of it.
    pub n_ctx_train: i32,
    pub spec: LoadSpec,
}

/// The one inference context, and the prompt it currently holds. One per core:
/// a per-turn context would reallocate the KV cache every turn and make
/// cross-turn prefix reuse impossible.
pub struct ContextState {
    pub context: shim::Context,
    /// The tokens the KV cache holds, so the next turn can decode only the
    /// suffix it does not already have.
    pub tokens: Vec<i32>,
    /// The specification this context belongs to, stored with the tokens and
    /// compared as a unit: reusing a cache across a spec change conditions a
    /// turn on another configuration's state with nothing failing.
    pub spec: LoadSpec,
    /// How many prompt tokens the last turn reused rather than decoded. Not
    /// observable from the delta stream, so a reuse that stopped working would
    /// otherwise only show up as every turn getting slower.
    pub reused: usize,
}

impl ContextState {
    /// Creates the context for a loaded model.
    pub fn new(model: Arc<shim::Model>, spec: &LoadSpec) -> Result<Self> {
        Ok(Self {
            context: shim::Context::new(model, &context_options(&spec.context))?,
            tokens: Vec::new(),
            spec: spec.clone(),
            reused: 0,
        })
    }
}

#[derive(Default)]
struct EngineState {
    loaded: Option<Loaded>,
}

/// The in-process llama.cpp engine.
pub struct LocalEngine {
    settings: Settings,
    paths: Paths,
    install_dir: PathBuf,
    state: Mutex<EngineState>,
    context: Arc<Mutex<Option<ContextState>>>,
    /// Where this engine reports its load state.
    sidecars: std::sync::OnceLock<Arc<Manager>>,
}

impl LocalEngine {
    /// Backend modules ship beside the running binary, where the relative rpath
    /// already resolves the llama.cpp libraries from.
    pub fn new(settings: Settings, paths: Paths) -> Self {
        let install_dir = default_install_dir(&paths);
        Self::with_install_dir(settings, paths, install_dir)
    }

    /// The same, with the backend directory named explicitly.
    pub fn with_install_dir(settings: Settings, paths: Paths, install_dir: PathBuf) -> Self {
        Self {
            settings,
            paths,
            install_dir,
            state: Mutex::new(EngineState::default()),
            context: Arc::new(Mutex::new(None)),
            sidecars: std::sync::OnceLock::new(),
        }
    }

    /// Report this engine's load state into the subsystem registry, which
    /// [`crate::chat::LlmLoading::still_loading`] and the core bar both read.
    ///
    /// Set-once: a second engine would be a second model resident at once.
    /// Returns whether this call is the one that set it, so a double-attach and
    /// a missing attach are both assertable.
    pub fn attach_status(&self, sidecars: Arc<Manager>) -> bool {
        self.sidecars.set(sidecars).is_ok()
    }

    /// Publish a load state, if boot attached a registry. Absent is not an
    /// error: a unit test may build an engine without one.
    fn publish(&self, status: SidecarStatus, message: Option<String>) {
        let Some(sidecars) = self.sidecars.get() else {
            return;
        };
        sidecars.publish(SidecarSnapshot {
            kind: SidecarKind::Llama,
            status,
            message,
            progress: None,
            // The engine is in process: there is nothing to measure.
            pid: None,
            cpu_pct: None,
            rss_mb: None,
        });
    }

    /// Drop the subsystem row entirely, rather than publishing `Disabled`: a
    /// kind nobody has published occupies no row at all.
    fn clear_status(&self) {
        if let Some(sidecars) = self.sidecars.get() {
            sidecars.clear(SidecarKind::Llama);
        }
    }

    /// What a turn needs, or `None` when no model is resident.
    pub async fn ready(&self) -> Option<Ready> {
        let state = self.state.lock().await;
        state.loaded.as_ref().map(|loaded| Ready {
            model: Arc::clone(&loaded.model),
            templates: Arc::clone(&loaded.templates),
            mtmd: loaded.mtmd.clone(),
            n_ctx_train: loaded.n_ctx_train,
            spec: loaded.spec.clone(),
        })
    }

    /// The settings store this engine reads when nobody hands it a record. The
    /// idle timer needs it: `on_turn_end` is called with no settings.
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// The context slot, for the token loop. An `Arc` because the turn takes an
    /// owned guard onto a blocking thread for the whole generation.
    pub fn context(&self) -> Arc<Mutex<Option<ContextState>>> {
        Arc::clone(&self.context)
    }

    /// Bring the resident model into line with a RESOLVED settings record.
    /// Called at the top of every turn, so the common case does nothing and
    /// returns without taking the context lock.
    pub async fn ensure_loaded(&self, resolved: &SettingsRecord) -> Result<()> {
        let requested = self.plan_load(resolved);
        let mut state = self.state.lock().await;

        match (&requested, &state.loaded) {
            // Warm and matching: the overwhelmingly common case.
            (Some(want), Some(have)) if have.spec == *want => return Ok(()),
            (None, None) => return Ok(()),
            _ => {}
        }

        // The old model goes FIRST: loading the new one alongside it would hold
        // two sets of weights at once.
        self.release(&mut state).await;

        let Some(spec) = requested else {
            tracing::info!(
                target: "loca::inference",
                "local inference gated off by settings (external provider or no model path)",
            );
            self.clear_status();
            return Ok(());
        };

        let path = resolve_hf_path(self.paths.models_dir(), &spec.model_spec);
        if !path.is_file() {
            // Not an error: the requirements flow is probably downloading this
            // file right now.
            tracing::info!(
                target: "loca::inference",
                "local model not loaded: not on disk: {}",
                path.display(),
            );
            self.clear_status();
            return Ok(());
        }

        // `Loading` BEFORE the work: `still_loading` is read from another task
        // while this is in flight, so publishing after would defer nothing.
        self.publish(SidecarStatus::Loading, None);
        match self.load(&spec, &path, resolved) {
            Ok(loaded) => {
                state.loaded = Some(loaded);
                self.publish(SidecarStatus::Running, None);
                Ok(())
            }
            Err(err) => {
                // Also reported: the next automated turn must not sit behind a
                // row still reading `Loading` for a load that already failed.
                self.publish(SidecarStatus::Error, Some(err.to_string()));
                Err(err)
            }
        }
    }

    /// Drop the model and its context. The idle timer's expiry, and the
    /// watchdog's recovery.
    pub async fn unload(&self) {
        let mut state = self.state.lock().await;
        if state.loaded.is_none() {
            return;
        }
        self.release(&mut state).await;
        self.clear_status();
        tracing::info!(target: "loca::inference", "local model unloaded");
    }

    /// Free the context, then the model, in that order: [`shim::Context`] holds
    /// an `Arc<Model>`, so dropping [`Loaded`] first frees no weights at all.
    async fn release(&self, state: &mut EngineState) {
        *self.context.lock().await = None;
        state.loaded = None;
    }

    fn load(&self, spec: &LoadSpec, path: &Path, resolved: &SettingsRecord) -> Result<Loaded> {
        self.prepare_devices(resolved)?;

        let model = Arc::new(shim::Model::load(
            &path.to_string_lossy(),
            &model_options(spec),
        )?);
        let meta = model.meta()?;
        if meta.chat_template.is_empty() {
            // llama.cpp falls back to ChatML, so this is not a failure. It is
            // the diagnostic for the turn that follows: a model on a fallback
            // template will not produce tool calls the parser looks for.
            tracing::debug!(
                target: "loca::inference",
                "{} carries no chat template; llama.cpp's ChatML fallback applies",
                path.display(),
            );
        }
        let templates = Arc::new(shim::ChatTemplates::from_model(Arc::clone(&model))?);
        let mtmd = self.load_mtmd(spec, &model)?;

        tracing::info!(
            target: "loca::inference",
            "loaded {} (n_ctx {} of {} trained, {} gpu layers)",
            path.display(),
            spec.context.n_ctx,
            meta.n_ctx_train,
            spec.n_gpu_layers,
        );
        Ok(Loaded {
            model,
            templates,
            mtmd,
            n_ctx_train: meta.n_ctx_train,
            spec: spec.clone(),
        })
    }

    /// Load the multimodal projector, if this spec asks for one. Three of the
    /// four outcomes are `Ok(None)` and none is a failure: none configured, not
    /// on disk yet (the requirements flow downloads it, and failing would break
    /// text chat over a picture the user has not sent), or loaded but carrying
    /// no vision tower. A projector that is present and refuses to load is the
    /// one hard error: answering an image prompt as if the image were not there
    /// is worse than saying so.
    fn load_mtmd(
        &self,
        spec: &LoadSpec,
        model: &Arc<shim::Model>,
    ) -> Result<Option<Arc<Mutex<shim::Mtmd>>>> {
        if spec.mmproj_spec.trim().is_empty() {
            return Ok(None);
        }
        let path = resolve_hf_path(self.paths.models_dir(), &spec.mmproj_spec);
        if !path.is_file() {
            tracing::info!(
                target: "loca::inference",
                "the multimodal projector {} is not on disk yet; this model answers text only                  until it finishes downloading",
                path.display(),
            );
            return Ok(None);
        }

        let mtmd = shim::Mtmd::load(
            Arc::clone(model),
            &path.to_string_lossy(),
            &shim::MtmdOptions {
                n_threads: spec.context.n_threads,
                // Follows the language model: a machine offloading no layers
                // would move the encoder's tensors across a bus for nothing.
                use_gpu: spec.n_gpu_layers > 0,
            },
        )?;

        if !mtmd.supports_vision() {
            tracing::warn!(
                target: "loca::inference",
                "{} loaded but carries no vision tower, so images cannot be used with this model",
                path.display(),
            );
            return Ok(None);
        }

        tracing::info!(
            target: "loca::inference",
            "loaded the multimodal projector {}",
            path.display(),
        );
        Ok(Some(Arc::new(Mutex::new(mtmd))))
    }

    /// What this record asks to be loaded, acceleration preference included.
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    fn plan_load(&self, resolved: &SettingsRecord) -> Option<LoadSpec> {
        let accel = backends::acceleration(resolved);
        options::requested_load(resolved, backends::gpu_layers_override(&accel))
    }

    /// Mobile has no acceleration preference to apply: llama.cpp is linked in,
    /// CPU-only, and there is no backend module to prefer or to override.
    #[cfg(any(target_os = "android", target_os = "ios"))]
    fn plan_load(&self, resolved: &SettingsRecord) -> Option<LoadSpec> {
        options::requested_load(resolved, None)
    }

    /// Load the backend modules this preference calls for and judge what
    /// registered. Lazily, on first load: boot must not touch the model.
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    fn prepare_devices(&self, resolved: &SettingsRecord) -> Result<()> {
        // Re-run on every model load, untracked: a preference change from
        // `cpu` to `auto` adds a directory a once-per-process guard would never
        // revisit, and repeating a load is harmless because ggml refuses to
        // register a registry it already holds.
        let accel = backends::acceleration(resolved);
        let dirs = backends::directories(&accel, &self.install_dir, self.paths.root());

        shim::backend_init();
        for dir in &dirs {
            // A missing directory is normal (no GPU download was accepted).
            // The shim treats it as an error, so the distinction is drawn here
            // rather than by swallowing a result that also hides real failures.
            if !dir.is_dir() {
                continue;
            }
            let n = shim::backend_load_dir(&dir.to_string_lossy())?;
            tracing::debug!(
                target: "loca::inference",
                "loaded {n} backend module(s) from {}",
                dir.display(),
            );
        }

        let devices = shim::devices()?;
        let found = backends::Registered {
            total: devices.len(),
            accelerated: devices
                .iter()
                .filter(|d| d.kind != shim::DeviceKind::Cpu)
                .count(),
        };
        if let Some(warning) = backends::review(&accel, found, &dirs)? {
            tracing::warn!(target: "loca::inference", "{warning}");
        }
        Ok(())
    }

    /// Mobile links every backend in and loads nothing, which is what W^X and
    /// App Store 2.5.2 require. `backend_init` still runs: it initialises ggml
    /// rather than loading anything.
    #[cfg(any(target_os = "android", target_os = "ios"))]
    fn prepare_devices(&self, _resolved: &SettingsRecord) -> Result<()> {
        shim::backend_init();
        Ok(())
    }
}

#[async_trait::async_trait]
impl crate::extensions::broker::EnsureModelLoaded for LocalEngine {
    /// An extension's tool is about to ask for a completion. The resolved
    /// shared record: there is no client on a tool call.
    async fn ensure_loaded(&self) -> Result<()> {
        let resolved = self.settings.load_resolved().await?;
        LocalEngine::ensure_loaded(self, &resolved).await
    }
}

/// Where backend modules ship: beside the running executable, which is what the
/// relative rpath resolves through. `bin/` under the channel root is the
/// fallback for the one case `current_exe` cannot answer.
fn default_install_dir(paths: &Paths) -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| paths.bin_dir())
}

/// [`LoadSpec`] to the shim's model options. Every field is named:
/// `..Default::default()` would answer `n_gpu_layers: 0`, the CPU-only
/// regression for a blank setting, and nothing would fail.
fn model_options(spec: &LoadSpec) -> shim::ModelOptions {
    shim::ModelOptions {
        n_gpu_layers: spec.n_gpu_layers,
        use_mmap: spec.use_mmap,
        // Deliberately not a setting: `mlock` pins the weights into physical
        // memory, the opposite of what survives the iOS jetsam limit.
        use_mlock: false,
    }
}

/// [`ContextSpec`] to the shim's context options. Every field named, same
/// reason.
fn context_options(spec: &ContextSpec) -> shim::ContextOptions {
    shim::ContextOptions {
        n_ctx: spec.n_ctx,
        n_batch: spec.n_batch,
        n_threads: spec.n_threads,
        flash_attn: spec.flash_attn,
        // A generation context: the embedder builds its own with the opposite
        // answers.
        embeddings: false,
        pooling_type: String::new(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use serde_json::{json, Value};

    use super::*;

    /// Where the backend modules are for a test binary: `build.rs` records the
    /// build's own staging directory.
    fn install_dir() -> PathBuf {
        PathBuf::from(env!("LOCA_LLAMA_BACKEND_DIR"))
    }

    fn engine() -> (LocalEngine, crate::transport::state::AppContext) {
        let ctx = crate::transport::testing::context();
        let engine = LocalEngine::with_install_dir(
            ctx.settings.clone(),
            (*ctx.paths).clone(),
            install_dir(),
        );
        (engine, ctx)
    }

    /// The same, with a live subsystem registry and a receiver of every status
    /// EDGE. Edges rather than polls: `Loading` is transient, so a test that
    /// sampled `status()` would race the load on a fast machine.
    fn engine_with_status() -> (
        LocalEngine,
        crate::transport::state::AppContext,
        Arc<Manager>,
        tokio::sync::broadcast::Receiver<crate::contract::api::CoreStatusSnapshot>,
    ) {
        let (engine, ctx) = engine();
        let sidecars = Arc::new(Manager::new());
        let status = crate::transport::status::CoreStatusService::new(
            Arc::clone(&sidecars) as Arc<dyn crate::transport::status::SidecarSource>
        );
        sidecars.attach_status(status.clone());
        let rx = status.subscribe();
        engine.attach_status(Arc::clone(&sidecars));
        (engine, ctx, sidecars, rx)
    }

    /// Every `Llama` state that crossed the wire, oldest first.
    fn llama_edges(
        rx: &mut tokio::sync::broadcast::Receiver<crate::contract::api::CoreStatusSnapshot>,
    ) -> Vec<SidecarStatus> {
        let mut seen = Vec::new();
        while let Ok(snapshot) = rx.try_recv() {
            for subsystem in &snapshot.subsystems {
                if subsystem.kind == SidecarKind::Llama {
                    seen.push(subsystem.status);
                }
            }
        }
        seen
    }

    fn record(pairs: Value) -> SettingsRecord {
        match pairs {
            Value::Object(map) => map,
            _ => panic!("test records are objects"),
        }
    }

    /// The tiny test model, kept out of git.
    fn test_gguf() -> Option<PathBuf> {
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("models/test.gguf");
        p.is_file().then_some(p)
    }

    const NO_GGUF: &str = "SKIP: packages/loca-core-rs/models/test.gguf is absent, so the \
        engine's load tests did not run.";

    fn loading(path: &Path) -> SettingsRecord {
        record(json!({
            "llm.provider": "local",
            "llm.modelPath": path.to_string_lossy(),
            // Small enough that the KV cache costs nothing on a test runner.
            "llm.contextSize": 512,
        }))
    }

    #[test]
    fn every_field_of_the_spec_reaches_the_shim() {
        // `..Default::default()` here would compile and answer `n_gpu_layers:
        // 0` for a blank setting that means every layer, and a CPU-only load
        // still succeeds, so no model-driving test could see it.
        let spec = LoadSpec {
            model_spec: "@u/r/main/m.gguf".to_string(),
            mmproj_spec: "@u/r/main/mmproj.gguf".to_string(),
            n_gpu_layers: -1,
            use_mmap: false,
            context: ContextSpec {
                n_ctx: 8192,
                n_batch: 256,
                n_threads: 7,
                flash_attn: true,
            },
        };

        let model = model_options(&spec);
        assert_eq!(model.n_gpu_layers, -1);
        assert!(!model.use_mmap);
        assert!(!model.use_mlock, "mlock is not a setting and stays off");

        let context = context_options(&spec.context);
        assert_eq!(context.n_ctx, 8192);
        assert_eq!(context.n_batch, 256);
        assert_eq!(context.n_threads, 7);
        assert!(context.flash_attn);
    }

    #[tokio::test]
    async fn an_external_provider_loads_nothing_even_with_a_model_configured() {
        // Switching provider does not clear the configured model path, so this
        // is the ordinary state for a user answering from OpenAI.
        let Some(model) = test_gguf() else {
            eprintln!("{NO_GGUF}");
            return;
        };
        let (engine, _ctx) = engine();
        let mut settings = loading(&model);
        settings.insert("llm.provider".into(), json!("external"));

        engine.ensure_loaded(&settings).await.unwrap();
        assert!(engine.ready().await.is_none());

        // The same record on `local` does load, or the assertion above would
        // hold for an engine that never loads anything.
        engine.ensure_loaded(&loading(&model)).await.unwrap();
        assert!(engine.ready().await.is_some());
    }

    #[tokio::test]
    async fn a_model_that_is_not_on_disk_yet_is_not_a_failure() {
        // The requirements flow is probably downloading it.
        let (engine, _ctx) = engine();
        let missing = PathBuf::from("/nonexistent/models/not-downloaded-yet.gguf");
        engine.ensure_loaded(&loading(&missing)).await.unwrap();
        assert!(engine.ready().await.is_none());
    }

    #[tokio::test]
    async fn an_unchanged_record_keeps_the_same_model_rather_than_reloading_it() {
        // Pointer equality is the only assertion that tells "still loaded" from
        // "loaded again".
        let Some(model) = test_gguf() else {
            eprintln!("{NO_GGUF}");
            return;
        };
        let (engine, _ctx) = engine();
        let settings = loading(&model);

        engine.ensure_loaded(&settings).await.unwrap();
        let first = engine.ready().await.expect("the model loaded").model;
        engine.ensure_loaded(&settings).await.unwrap();
        let second = engine
            .ready()
            .await
            .expect("the model is still loaded")
            .model;

        assert!(
            Arc::ptr_eq(&first, &second),
            "an unchanged record must not reload the model",
        );
    }

    #[tokio::test]
    async fn a_changed_setting_reloads_because_there_is_no_process_to_restart() {
        // Without this a user who raises the context size sees it saved and
        // every turn keep running against the old context.
        let Some(model) = test_gguf() else {
            eprintln!("{NO_GGUF}");
            return;
        };
        let (engine, _ctx) = engine();

        engine.ensure_loaded(&loading(&model)).await.unwrap();
        let first = engine.ready().await.expect("the model loaded").model;

        let mut changed = loading(&model);
        changed.insert("llm.contextSize".into(), json!(256));
        engine.ensure_loaded(&changed).await.unwrap();
        let second = engine.ready().await.expect("the model reloaded");

        assert!(
            !Arc::ptr_eq(&first, &second.model),
            "a changed setting must reload",
        );
        assert_eq!(second.spec.context.n_ctx, 256);
    }

    #[tokio::test]
    async fn switching_to_an_external_provider_unloads_what_was_resident() {
        // Leaving the weights resident would be invisible: every turn still
        // answers, with a few gigabytes held for nothing.
        let Some(model) = test_gguf() else {
            eprintln!("{NO_GGUF}");
            return;
        };
        let (engine, _ctx) = engine();
        engine.ensure_loaded(&loading(&model)).await.unwrap();
        assert!(engine.ready().await.is_some());

        let mut external = loading(&model);
        external.insert("llm.provider".into(), json!("external"));
        engine.ensure_loaded(&external).await.unwrap();
        assert!(engine.ready().await.is_none());
    }

    #[tokio::test]
    async fn unloading_drops_the_context_before_the_model() {
        // With a context alive the model's `Arc` has two holders, so clearing
        // only `loaded` frees nothing.
        let Some(model) = test_gguf() else {
            eprintln!("{NO_GGUF}");
            return;
        };
        let (engine, _ctx) = engine();
        engine.ensure_loaded(&loading(&model)).await.unwrap();
        let ready = engine.ready().await.expect("the model loaded");

        *engine.context().lock().await =
            Some(ContextState::new(Arc::clone(&ready.model), &ready.spec).unwrap());

        // Three holders: `Loaded`, the context, and `ready`.
        let weights = Arc::downgrade(&ready.model);
        drop(ready);
        engine.unload().await;

        // The weights first: an empty context slot proves only that the slot
        // was cleared. This one fails if ANY holder is left behind.
        assert!(
            weights.upgrade().is_none(),
            "the weights are still held after an unload, so nothing was freed",
        );
        assert!(engine.context().lock().await.is_none());
    }

    #[tokio::test]
    async fn the_context_is_built_with_the_configured_size() {
        // Checked against llama.cpp's own answer, not the struct passed in.
        let Some(model) = test_gguf() else {
            eprintln!("{NO_GGUF}");
            return;
        };
        let (engine, _ctx) = engine();
        engine.ensure_loaded(&loading(&model)).await.unwrap();
        let ready = engine.ready().await.expect("the model loaded");

        let state = ContextState::new(Arc::clone(&ready.model), &ready.spec).unwrap();
        assert_eq!(state.context.n_ctx(), 512);
    }

    // --- the engine publishes its load state ------------------------------

    #[tokio::test]
    async fn a_load_publishes_loading_before_the_work_and_running_after_it() {
        // The ordering is the property, not the end state: `still_loading` is
        // read from another task while this one is inside `load`.
        let Some(model) = test_gguf() else {
            eprintln!("{NO_GGUF}");
            return;
        };
        let (engine, _ctx, sidecars, mut rx) = engine_with_status();

        engine.ensure_loaded(&loading(&model)).await.unwrap();

        assert_eq!(
            llama_edges(&mut rx),
            vec![SidecarStatus::Loading, SidecarStatus::Running],
            "the engine must announce Loading BEFORE reading the weights and \
             Running only once they are resident",
        );
        assert_eq!(
            sidecars.status(SidecarKind::Llama).status,
            SidecarStatus::Running,
        );
    }

    #[tokio::test]
    async fn an_unloaded_model_clears_its_subsystem_row() {
        // Cleared, not `Disabled`: a stale `Running` row would show the core
        // bar a model resident that had been dropped.
        let Some(model) = test_gguf() else {
            eprintln!("{NO_GGUF}");
            return;
        };
        let (engine, _ctx, sidecars, _rx) = engine_with_status();
        engine.ensure_loaded(&loading(&model)).await.unwrap();
        assert_eq!(
            sidecars.status(SidecarKind::Llama).status,
            SidecarStatus::Running,
        );

        engine.unload().await;

        assert_eq!(
            sidecars.status(SidecarKind::Llama).status,
            SidecarStatus::Disabled,
            "an unloaded engine must stop occupying a subsystem row",
        );
    }

    #[tokio::test]
    async fn an_external_provider_publishes_no_llama_row_at_all() {
        // `still_loading` must not read a stale `Loading` from a previous local
        // session and hold every automated turn forever.
        let (engine, _ctx, sidecars, mut rx) = engine_with_status();
        let mut settings = record(json!({ "llm.provider": "external" }));
        settings.insert("llm.modelPath".into(), json!("/some/model.gguf"));

        engine.ensure_loaded(&settings).await.unwrap();

        assert!(
            llama_edges(&mut rx).is_empty(),
            "a core with an external provider announced a local model state",
        );
        assert_eq!(
            sidecars.status(SidecarKind::Llama).status,
            SidecarStatus::Disabled,
        );
    }

    #[tokio::test]
    async fn a_model_that_is_not_on_disk_yet_publishes_no_row() {
        // `Error` would flip the core bar to a failure mid-download, and
        // `Loading` would hold every automated turn until it finished.
        let (engine, _ctx, sidecars, mut rx) = engine_with_status();
        let missing = PathBuf::from("/nonexistent/models/not-downloaded-yet.gguf");

        engine.ensure_loaded(&loading(&missing)).await.unwrap();

        assert!(
            llama_edges(&mut rx).is_empty(),
            "a model that has not finished downloading must not announce a \
             load state in either direction",
        );
        assert_eq!(
            sidecars.status(SidecarKind::Llama).status,
            SidecarStatus::Disabled,
        );
    }
}
