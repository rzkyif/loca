//! The loaded model, the one context, and the rules for when each is replaced.
//!
//! This is the first file in the package that names a shim type, which is why it
//! is gated on `local-inference` while everything it decides with
//! ([`super::options`], [`super::backends`]) is not. The conversions from this
//! module's own specs into the shim's option structs live here and nowhere else:
//! one per type, field by field, so the shim's surface can move without any
//! decision moving with it.
//!
//! # What replaces the sidecar supervisor
//!
//! Desktop today spawns `llama-server` and `sidecar-boot.ts` decides when to
//! restart it. There is no process now, so the same three decisions are made
//! here, and `applyLlama` (`services/sidecar-boot.ts:204-226`) is the
//! specification for all three:
//!
//! 1. **Gated off** (an external provider, or no model configured): stop. Here,
//!    that is unloading whatever is resident. Holding a multi-GB model for a
//!    user who is answering from OpenAI is the whole cost of getting this wrong.
//! 2. **A prerequisite is missing** (the model file is not on disk): stop, log,
//!    and wait. Not an error: the requirements flow downloads the file and the
//!    next turn picks it up. Failing here would flip the corebar to Error while
//!    a download the user consented to is still running.
//! 3. **Otherwise**: restart with the current settings. Here, that is comparing
//!    [`LoadSpec`] and reloading on any difference.
//!
//! The one place this is not a transcription is that the TypeScript splits the
//! work between `applyLlama` (on a settings change, and it stops the server) and
//! `ensureLoaded` (at the top of a turn, and it never stops anything). There is
//! no supervisor here to hold the other half, so [`LocalEngine::ensure_loaded`]
//! does both jobs and the observable end state is the one `applyLlama` leaves.
//!
//! # Lock ordering
//!
//! `state` before `context`, always, and nothing takes them in the other order.
//! The two are separate because a turn holds the context for its whole
//! generation and must not block a settings read for that long.

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
    /// The multimodal projector, when the configuration asked for one and it
    /// loaded. `None` is the ordinary case: a text model, or a vision model with
    /// images turned off.
    ///
    /// Behind its own `Mutex` because `mtmd_helper_eval_chunks` is documented
    /// NOT thread-safe, so it needs the same one-at-a-time discipline the
    /// context has, and it is held across a whole turn's prefill.
    mtmd: Option<Arc<Mutex<shim::Mtmd>>>,
    n_ctx_train: i32,
    spec: LoadSpec,
}

/// What a turn needs to run, taken out from under the lock.
///
/// Everything here is an `Arc`, so a turn holds the model and its templates
/// alive for its own duration even if an idle unload lands mid-generation. That
/// is deliberate: the alternative is a turn holding the load lock, which would
/// block every settings read for the length of a generation.
pub struct Ready {
    pub model: Arc<shim::Model>,
    pub templates: Arc<shim::ChatTemplates>,
    /// The loaded projector, or `None` for a text-only turn. Carried on `Ready`
    /// for the same reason everything else here is: the turn holds it alive for
    /// its own duration, so an idle unload mid-generation cannot pull it out.
    pub mtmd: Option<Arc<Mutex<shim::Mtmd>>>,
    /// The context length the model was TRAINED at, which is not the context it
    /// is loaded with: a user may configure either side of it.
    pub n_ctx_train: i32,
    pub spec: LoadSpec,
}

/// The one inference context, and the prompt it currently holds.
///
/// One per core rather than one per turn. A per-turn context would reallocate
/// the KV cache every turn (hundreds of MB for a 2B model at 4096) and would
/// make cross-turn prefix reuse impossible, which is the cost the llama-server
/// slot was quietly paying for us.
pub struct ContextState {
    pub context: shim::Context,
    /// The tokens the KV cache holds, so the next turn can decode only the
    /// suffix it does not already have.
    pub tokens: Vec<i32>,
    /// The specification this context belongs to. Stored with the tokens and
    /// compared as a unit: reusing a cache across a spec change conditions a
    /// turn on another configuration's state, which produces fluent, confident,
    /// wrong output with nothing failing.
    pub spec: LoadSpec,
    /// How many prompt tokens the last turn reused rather than decoded.
    ///
    /// Production bookkeeping that a test can read, rather than a test driving
    /// its own copy of the prefill loop (D-084): "the second turn decoded fewer
    /// tokens" is not observable from the delta stream at all, and a reuse that
    /// silently stopped working would only show up as every turn getting slower.
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
    /// Where this engine reports its load state, so the rest of the core can
    /// see a model coming up. See [`LocalEngine::attach_status`].
    sidecars: std::sync::OnceLock<Arc<Manager>>,
}

impl LocalEngine {
    /// The production constructor: backend modules ship beside the running
    /// binary, where D-116's relative rpath already resolves the llama.cpp
    /// libraries from.
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

    /// Report this engine's load state into the subsystem registry.
    ///
    /// **This closes Q8** (D-163 item 4, D-167). `Manager::publish`'s own doc
    /// says "called by whoever owns the subsystem: N2 for the language model",
    /// and nothing did: `SidecarKind::Llama` was published by tests only. Two
    /// things read it, and both got the wrong answer from an unpublished kind,
    /// which reads as `Disabled`:
    ///
    /// - [`crate::chat::LlmLoading::still_loading`], which is what makes
    ///   greetings and scheduled prompts WAIT for a model that is mid-load. An
    ///   automated turn fired during a load races it and errors, and the symptom
    ///   is a schedule that produces a failed session rather than a late one.
    /// - The core bar's subsystem row, which renders nothing for a kind nobody
    ///   publishes, so a multi-gigabyte load looked like an idle core.
    ///
    /// Set-once, following `Manager::attach_status` and
    /// `Scheduler::attach_automated`; a second engine would be a second model
    /// resident at once. Returns whether this call is the one that set it, which
    /// is what makes both a double-attach and a missing attach assertable
    /// instead of invisible.
    pub fn attach_status(&self, sidecars: Arc<Manager>) -> bool {
        self.sidecars.set(sidecars).is_ok()
    }

    /// Publish a load state, if boot attached a registry.
    ///
    /// Absent is not an error: a unit test builds an engine with no registry and
    /// must not be forced to construct one to load a model.
    fn publish(&self, status: SidecarStatus, message: Option<String>) {
        let Some(sidecars) = self.sidecars.get() else {
            return;
        };
        sidecars.publish(SidecarSnapshot {
            kind: SidecarKind::Llama,
            status,
            message,
            progress: None,
            // No pid, no cpu, no rss: the engine is in process, and there is no
            // `llama-server` to measure. The client already renders these as
            // absent for a subsystem that has none.
            pid: None,
            cpu_pct: None,
            rss_mb: None,
        });
    }

    /// Drop the subsystem row entirely.
    ///
    /// Cleared rather than published as `Disabled`, matching N3's speech
    /// reconciler: a kind nobody has published occupies no subsystem row at all,
    /// which is what the Deno core did for a sidecar it never started.
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

    /// The settings store this engine reads when nobody hands it a record.
    ///
    /// The idle timer needs it: `on_turn_end` is called with a stream count and
    /// no settings, and the alternative is a cached copy that goes stale exactly
    /// when the user changes the setting.
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// The context slot, for the token loop.
    ///
    /// An `Arc` rather than a borrow because the turn takes an OWNED guard and
    /// carries it onto a blocking thread for the whole generation, which a
    /// borrowed guard cannot outlive.
    pub fn context(&self) -> Arc<Mutex<Option<ContextState>>> {
        Arc::clone(&self.context)
    }

    /// Bring the resident model into line with a RESOLVED settings record.
    ///
    /// Called at the top of every turn (`chat/turn.rs:84`), so the common case
    /// is the one that does nothing: the same settings that were loaded last
    /// turn compare equal and this returns without taking the context lock.
    pub async fn ensure_loaded(&self, resolved: &SettingsRecord) -> Result<()> {
        let requested = self.plan_load(resolved);
        let mut state = self.state.lock().await;

        match (&requested, &state.loaded) {
            // Warm and matching: the overwhelmingly common case.
            (Some(want), Some(have)) if have.spec == *want => return Ok(()),
            (None, None) => return Ok(()),
            _ => {}
        }

        // Everything else means what is resident is not what is wanted, and the
        // old model goes FIRST. Loading the new one alongside it would hold two
        // sets of weights at once, which on the machines this runs on is the
        // difference between a reload and an out-of-memory kill.
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
            // Not an error, and this is the shape the TypeScript comment is
            // emphatic about: the requirements flow is probably downloading this
            // file right now, and failing the turn here would surface as an
            // error while a download the user consented to is still in flight.
            tracing::info!(
                target: "loca::inference",
                "local model not loaded: not on disk: {}",
                path.display(),
            );
            self.clear_status();
            return Ok(());
        }

        // `Loading` BEFORE the work, which is the whole point of the seam: the
        // read that matters (`still_loading`) happens WHILE this is in flight,
        // from another task, so publishing after the load would report a state
        // that is already over and defer nothing.
        self.publish(SidecarStatus::Loading, None);
        match self.load(&spec, &path, resolved) {
            Ok(loaded) => {
                state.loaded = Some(loaded);
                self.publish(SidecarStatus::Running, None);
                Ok(())
            }
            Err(err) => {
                // Reported, not just returned. The turn that triggered the load
                // sees this error, but a load failure is a SUBSYSTEM state: the
                // next automated turn must not sit behind a row still reading
                // `Loading` for a load that already failed.
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

    /// Free the context, then the model, in that order.
    ///
    /// The order is the whole content of this function. [`shim::Context`] holds
    /// an `Arc<Model>`, so dropping [`Loaded`] while a context is alive frees no
    /// weights at all: the unload would be a log line and the memory would stay
    /// exactly where it was, which is the one outcome an idle unload exists to
    /// prevent.
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
            // llama.cpp falls back to ChatML for a model that carries no
            // template of its own (`common/chat.cpp:732-737`), so this is not a
            // failure. It is the diagnostic for the turn that follows: a model
            // being driven by a fallback template will not produce tool calls in
            // any syntax the parser is looking for.
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

    /// Load the multimodal projector, if this spec asks for one.
    ///
    /// Three of the four outcomes are `Ok(None)` and none of them is a failure,
    /// which is the whole shape of this function:
    ///
    /// 1. **No projector configured.** A text model, or a vision model with
    ///    `llm.supportImages` off. The overwhelmingly common case.
    /// 2. **Configured but not on disk yet.** Same treatment as a missing model
    ///    file: the requirements flow downloads it on consent and the next load
    ///    picks it up. Failing here would break TEXT chat, which works perfectly
    ///    well, over a picture the user has not sent.
    /// 3. **Loaded, but audio-only.** A projector can carry no vision tower. It
    ///    is released rather than kept, because holding it would cost the memory
    ///    of a second model to serve nothing.
    /// 4. Loaded and does vision, which is the one `Some`.
    ///
    /// A projector that is present and REFUSES to load is the one hard error: it
    /// means a corrupt or mismatched file, and answering an image prompt as if
    /// the image were not there is worse than saying so.
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
                // Follows the language model: a machine offloading layers has a
                // GPU worth using for the vision encoder too, and one that is
                // not is a machine where the encoder would be moving tensors
                // across a bus for nothing.
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
    /// registered.
    ///
    /// Lazily, on the first load, rather than at boot: boot must not touch the
    /// model, and `backend_init` plus two `dlopen`s is cheap but not free.
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    fn prepare_devices(&self, resolved: &SettingsRecord) -> Result<()> {
        // Re-run on every model load rather than once per process, and nothing
        // tracks what was already loaded. Two reasons: a preference change from
        // `cpu` to `auto` adds a directory that a once-per-process guard would
        // never look at again, and repeating a load is harmless because ggml
        // refuses to register a registry it already holds and closes the second
        // handle (`ggml/src/ggml-backend-reg.cpp:191-195`). A model load is rare
        // enough that a few `dlopen`s cost nothing beside reading the weights.
        let accel = backends::acceleration(resolved);
        let dirs = backends::directories(&accel, &self.install_dir, self.paths.root());

        shim::backend_init();
        for dir in &dirs {
            // A directory that is not there is not a failure: the
            // consent-download destination is absent on every machine that never
            // accepted a GPU download, which is the normal case. The shim treats
            // a missing directory as an error (`shim.cpp:352-355`), so the
            // distinction is drawn here rather than by swallowing its result,
            // which would hide a real enumeration failure too.
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
    /// An extension's tool is about to ask for a completion.
    ///
    /// The RESOLVED shared record, per D-072: there is no client on a tool call,
    /// and acting on a setting means reading the form that carries the schema's
    /// defaults.
    async fn ensure_loaded(&self) -> Result<()> {
        let resolved = self.settings.load_resolved().await?;
        LocalEngine::ensure_loaded(self, &resolved).await
    }
}

/// Where backend modules ship: beside the running executable.
///
/// The installed layout puts the core binary, the llama.cpp libraries and the
/// backend modules in one directory, which is what D-116's relative rpath
/// resolves through. `bin/` under the channel root is the fallback for the one
/// case `current_exe` cannot answer, and it is the same directory in an ordinary
/// install.
fn default_install_dir(paths: &Paths) -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| paths.bin_dir())
}

/// [`LoadSpec`] to the shim's model options.
///
/// Every field is named. Spreading `..Default::default()` over this is the
/// D-071 trap in its most expensive form: `ModelOptions::default()` sets
/// `n_gpu_layers: 0`, which is the right default for a struct and the CPU-only
/// regression for a blank setting, and nothing would fail.
fn model_options(spec: &LoadSpec) -> shim::ModelOptions {
    shim::ModelOptions {
        n_gpu_layers: spec.n_gpu_layers,
        use_mmap: spec.use_mmap,
        // Not a setting, and deliberately not one. `mlock` pins the weights into
        // physical memory, which is the opposite of what keeps a large model
        // alive under the iOS jetsam limit: mmap'd pages are file-backed and
        // evictable, and locking them makes the whole model resident.
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
        // A generation context, so embedding mode is off and pooling means
        // nothing. The embedder builds its own context with the opposite
        // answers; see `inference::embed`.
        embeddings: false,
        pooling_type: String::new(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use serde_json::{json, Value};

    use super::*;

    /// Where the backend modules are for a test binary, which is not where they
    /// are for an installed core. `build.rs` records the build's own staging
    /// directory, and the shim's suite uses the same env var for the same
    /// reason.
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

    /// The same, with a live subsystem registry attached, plus a receiver of
    /// every status EDGE.
    ///
    /// The receiver is what makes the ordering assertable rather than sampled:
    /// `Loading` is transient and a test that polled `status()` would race the
    /// load and pass for the wrong reason on a fast machine. Edges are
    /// buffered, so draining the channel afterwards sees states that have
    /// already been superseded.
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

    /// The tiny model N1 established, kept out of git.
    fn test_gguf() -> Option<PathBuf> {
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("models/test.gguf");
        p.is_file().then_some(p)
    }

    const NO_GGUF: &str = "SKIP: packages/loca-core-rs/models/test.gguf is absent, so the \
        engine's load tests did not run. See docs/rust-port/packages/N1/status.md for the \
        one command that fetches it.";

    fn loading(path: &Path) -> SettingsRecord {
        record(json!({
            "llm.provider": "local",
            "llm.modelPath": path.to_string_lossy(),
            // Small enough that the KV cache costs nothing on a test runner, and
            // the tiny model was trained at 128.
            "llm.contextSize": 512,
        }))
    }

    #[test]
    fn every_field_of_the_spec_reaches_the_shim() {
        // The one conversion per type this file owns. It looks trivial and it is
        // the D-071 trap's home: `..Default::default()` here compiles, reads as
        // tidier, and answers `n_gpu_layers: 0` for a blank setting that means
        // every layer. Nothing else in the crate would notice, and no test that
        // drives a model can see it either, because a CPU-only load succeeds.
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
        // The expensive one. A user answering from OpenAI who still has a model
        // path in their settings (which is the ordinary state: the schema ships
        // one and switching provider does not clear it) must not have those
        // weights read off disk and held.
        let Some(model) = test_gguf() else {
            eprintln!("{NO_GGUF}");
            return;
        };
        let (engine, _ctx) = engine();
        let mut settings = loading(&model);
        settings.insert("llm.provider".into(), json!("external"));

        engine.ensure_loaded(&settings).await.unwrap();
        assert!(engine.ready().await.is_none());

        // And the same record on `local` does load, or the assertion above would
        // hold for an engine that never loads anything at all.
        engine.ensure_loaded(&loading(&model)).await.unwrap();
        assert!(engine.ready().await.is_some());
    }

    #[tokio::test]
    async fn a_model_that_is_not_on_disk_yet_is_not_a_failure() {
        // The requirements flow is probably downloading it. Failing here would
        // surface an error while a consented download is still in flight, which
        // is the exact symptom `llm-idle.ts:44-47` exists to avoid.
        let (engine, _ctx) = engine();
        let missing = PathBuf::from("/nonexistent/models/not-downloaded-yet.gguf");
        engine.ensure_loaded(&loading(&missing)).await.unwrap();
        assert!(engine.ready().await.is_none());
    }

    #[tokio::test]
    async fn an_unchanged_record_keeps_the_same_model_rather_than_reloading_it() {
        // `ensure_loaded` runs at the top of EVERY turn, so a comparison that
        // failed to match would re-read a multi-GB file per turn. Pointer
        // equality is the only assertion that can tell "still loaded" from
        // "loaded again", which is exactly the difference that matters here.
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
        // What replaces restarting the sidecar. Without this a user who raises
        // the context size sees the setting saved, the client show the new
        // value, and every turn keep running against the old context.
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
        // The memory half of decision 1. Leaving the weights resident would be
        // invisible: every turn would still answer, from the external endpoint,
        // with a few gigabytes held for nothing.
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
        // The ordering `release` exists for. With a context alive the model's
        // `Arc` has two holders, so clearing only `loaded` frees nothing and the
        // idle unload becomes a log line with the memory still resident.
        let Some(model) = test_gguf() else {
            eprintln!("{NO_GGUF}");
            return;
        };
        let (engine, _ctx) = engine();
        engine.ensure_loaded(&loading(&model)).await.unwrap();
        let ready = engine.ready().await.expect("the model loaded");

        *engine.context().lock().await =
            Some(ContextState::new(Arc::clone(&ready.model), &ready.spec).unwrap());

        // Three holders now: `Loaded`, the context, and `ready`. Dropping the
        // last leaves the two the engine owns.
        let weights = Arc::downgrade(&ready.model);
        drop(ready);
        engine.unload().await;

        // The weights first, because it is the assertion with the consequence:
        // an empty context slot proves the slot was cleared, not that anything
        // was freed, and the model is also held by `Loaded` and by the templates
        // inside it. This one fails if ANY holder is left behind.
        assert!(
            weights.upgrade().is_none(),
            "the weights are still held after an unload, so nothing was freed",
        );
        assert!(engine.context().lock().await.is_none());
    }

    #[tokio::test]
    async fn the_context_is_built_with_the_configured_size() {
        // The conversion this file owns, checked against llama.cpp's own answer
        // rather than against the struct that was passed in.
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

    // --- Q8: the engine publishes its load state (D-167) -------------------
    //
    // Nothing published `SidecarKind::Llama` before these. `Manager::publish`'s
    // doc named N2 as the publisher for the language model and N2 never called
    // it, so an unpublished kind read as `Disabled` and
    // `LlmLoading::still_loading` answered false for a model that was mid-load.
    // Greetings and scheduled prompts then fired INTO the load and errored,
    // which presents as a flaky model rather than as a wiring gap.

    #[tokio::test]
    async fn a_load_publishes_loading_before_the_work_and_running_after_it() {
        // The ordering is the property, not the end state. `still_loading` is
        // read from ANOTHER task while this one is inside `load`, so a
        // `Running` published only at the end would defer nothing: by the time
        // anything could observe it the load is already over.
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
        // Cleared, not `Disabled`: a kind nobody has published occupies no row,
        // which is what the Deno core did for a sidecar it never started. A
        // stale `Running` row here would be worse than no row, because the core
        // bar would show a model resident that had been dropped.
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
        // The gated-off arm. A user answering from OpenAI must not see a
        // language-model subsystem row, and more importantly `still_loading`
        // must not be able to read a stale `Loading` from a previous local
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
        // The requirements flow is probably downloading this file right now.
        // Publishing `Error` here would flip the core bar to a failure while a
        // download the user consented to is still in flight, and publishing
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
