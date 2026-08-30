//! Boot: construct every subsystem in dependency order and hand back a running
//! [`transport::Core`]. The desktop binary calls [`boot`] then
//! [`Booted::serve`]; the mobile frontend calls [`boot_with`] with
//! [`Frontend::Embedded`] and drives the core in process, never binding a
//! socket (on Android `127.0.0.1` is not app-private).

use std::sync::Arc;

use crate::auth::Auth;
use crate::config::Config;
use crate::contract::api::ServerToClientFrame;
use crate::db::Db;
use crate::error::{internal, Result};
use crate::paths::Paths;
use crate::secrets::Vault;
use crate::sessions::SessionsStore;
use crate::sidecars::Manager;
use crate::storage::deps::{RequirementsNotifier, StorageDeps};
use crate::transport::router::Services;
use crate::transport::state::{AppContext, ClientAuth, ClientIdentity, StatusProvider};
use crate::transport::status::{CoreStatusService, SidecarSource};
use crate::transport::tls::{self, TlsMaterial};
use crate::transport::{ConnectionSeed, Core, FrameBus};

use tokio::sync::broadcast;

/// The two boot-only keys (`server.bindHost`, `server.behindProxy`), read raw
/// from `settings.json`. Kept out of the settings service so a paired client
/// cannot widen network exposure or weaken the pairing posture over the API.
struct RawSettings(serde_json::Map<String, serde_json::Value>);

impl RawSettings {
    fn bool(&self, key: &str) -> bool {
        self.0.get(key).and_then(serde_json::Value::as_bool) == Some(true)
    }

    /// Trimmed; empty is absent, so a blank line does not bind to nothing.
    fn string(&self, key: &str) -> Option<String> {
        let value = self.0.get(key)?.as_str()?.trim();
        (!value.is_empty()).then(|| value.to_string())
    }
}

async fn raw_settings(paths: &Paths) -> RawSettings {
    let path = paths.settings_file();
    match crate::files::read_json::<serde_json::Map<String, serde_json::Value>>(path).await {
        Ok(map) => RawSettings(map),
        Err(e) if e.code == crate::error::ErrorCode::NotFound => RawSettings(Default::default()),
        Err(e) => {
            tracing::warn!(
                target: "loca::boot",
                "could not read settings.json for the boot-only keys, using defaults: {e}",
            );
            RawSettings(Default::default())
        }
    }
}

/// Seeds a just-connected client with the current core status. Registered on
/// the bus (not the WS upgrade handler) so the embedded frontend is seeded too.
struct CoreStatusSeed(Arc<CoreStatusService>);

impl ConnectionSeed for CoreStatusSeed {
    fn seed(&self, _client_id: &str) -> Vec<ServerToClientFrame> {
        vec![ServerToClientFrame::CoreStatus {
            snapshot: self.0.snapshot(),
        }]
    }
}

/// Breaks the models <-> requirements construction cycle: filled once
/// `Requirements` exists, and set-once because a second one would own a second
/// recompute worker. Notifications before that are dropped.
#[derive(Default)]
struct DeferredRequirements(std::sync::OnceLock<crate::requirements::Requirements>);

impl crate::storage::deps::RequirementsNotifier for DeferredRequirements {
    fn changed(&self) {
        if let Some(requirements) = self.0.get() {
            requirements.changed();
        }
    }
}

/// Breaks the chat <-> tools construction cycle. Behaves as `NoTools` until
/// filled, which no turn can observe, and set-once because a second
/// `HostedTools` would be a second registry view.
#[derive(Default)]
struct DeferredTools(std::sync::OnceLock<Arc<dyn crate::chat::ToolHost>>);

impl DeferredTools {
    fn inner(&self) -> &dyn crate::chat::ToolHost {
        match self.0.get() {
            Some(h) => h.as_ref(),
            None => &crate::chat::NoTools,
        }
    }
}

#[async_trait::async_trait]
impl crate::chat::ToolHost for DeferredTools {
    fn is_live(&self) -> bool {
        self.0.get().is_some()
    }

    async fn installed_extension_tools(&self) -> Vec<crate::contract::api::Tool> {
        self.inner().installed_extension_tools().await
    }

    async fn get_tool(&self, tool_id: &str) -> Option<crate::contract::api::Tool> {
        self.inner().get_tool(tool_id).await
    }

    async fn list_mcp_tools(&self) -> Vec<crate::contract::api::Tool> {
        self.inner().list_mcp_tools().await
    }

    async fn is_mcp_server(&self, id: &str) -> bool {
        self.inner().is_mcp_server(id).await
    }

    async fn load_tool_embeddings(
        &self,
    ) -> std::collections::HashMap<String, crate::chat::deps::ToolEmbedding> {
        self.inner().load_tool_embeddings().await
    }

    async fn filter_phase1(
        &self,
        query_vector: &[f32],
        options: crate::chat::Phase1Options,
    ) -> crate::chat::Phase1Result {
        self.inner().filter_phase1(query_vector, options).await
    }

    async fn filter_phase2(
        &self,
        query: &str,
        candidates: Vec<crate::contract::api::ToolDescriptor>,
        endpoint: &crate::llm::LlmEndpointConfig,
        reasoning_budget: u32,
    ) -> Vec<crate::contract::api::ToolDescriptor> {
        self.inner()
            .filter_phase2(query, candidates, endpoint, reasoning_budget)
            .await
    }

    async fn verify_tool_fresh(&self, extension_id: &str) -> Result<()> {
        self.inner().verify_tool_fresh(extension_id).await
    }

    fn validate_tool_args(
        &self,
        tool: &crate::contract::api::Tool,
        arguments_json: &str,
    ) -> Result<String> {
        self.inner().validate_tool_args(tool, arguments_json)
    }

    async fn start_tool_call(
        &self,
        spec: crate::chat::deps::ToolCallSpec,
        events: tokio::sync::mpsc::UnboundedSender<crate::chat::deps::ToolCallEvent>,
    ) -> Result<Box<dyn crate::chat::deps::ToolCallController>> {
        self.inner().start_tool_call(spec, events).await
    }

    async fn call_mcp_tool(
        &self,
        server_id: &str,
        name: &str,
        args: serde_json::Map<String, serde_json::Value>,
    ) -> Result<serde_json::Value> {
        self.inner().call_mcp_tool(server_id, name, args).await
    }

    async fn resolve_mcp_tokens(&self, text: &str) -> crate::chat::McpTokens {
        self.inner().resolve_mcp_tokens(text).await
    }
}

/// A booted core, plus the handles the frontend needs that the router does not.
/// Several fields are read ports for the boot-seam suite (`boot/seams.rs`):
/// clones of handles boot already holds, never second instances.
pub struct Booted {
    pub core: Core,
    pub status: Arc<CoreStatusService>,
    pub sidecars: Arc<Manager>,
    /// So the frontend can close MCP connections before the listener does.
    pub mcp: crate::mcp::Mcp,
    /// The warm-worker pool. PTY-mode workers are not in the manager's
    /// `children`, so `sidecars.shutdown()` alone leaves them running. `None`
    /// on mobile, where starting a downloaded interpreter is forbidden.
    pub extension_pool: Option<crate::extensions::worker::pool::WorkerPool>,
    /// So the frontend can stop the arm loop before the listener closes.
    pub scheduled: crate::scheduled::Scheduler,
    /// Whether chat's tool seam holds the real `HostedTools`.
    pub tools_wired: bool,
    /// Whether revocation actually revokes: see [`crate::auth::Auth::hooks_wired`].
    pub revocation_wired: bool,
    pub extensions: crate::extensions::Extensions,
    #[cfg(feature = "local-inference")]
    pub local_engine: Arc<crate::inference::engine::LocalEngine>,
    pub llm: Arc<crate::llm::Llm>,
    pub embedding: crate::memories::Embedding,
    /// `None` when `behindProxy` is set: the proxy owns TLS.
    pub tls: Option<TlsMaterial>,
    pub downloads: crate::downloads::queue::DownloadQueue,
    pub binaries: crate::downloads::binaries::installer::BinariesInstaller,
    pub memories: crate::memories::MemoriesStore,
    pub storage: StorageDeps,
    /// The identity the embedded frontend makes every in-process request as;
    /// `None` for [`Frontend::Service`]. Minted here because `Booted` exposes
    /// no `Auth`, and the ffi crate building a second one would be unsafe.
    pub local_client: Option<ClientIdentity>,
}

/// Which frontend is booting this core. A value rather than a `cfg`, so a
/// desktop test can boot `Embedded` and assert what it skipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Frontend {
    /// The desktop service binary: binds a TLS listener and runs everything.
    Service,
    /// Linked into the mobile app and driven in process, with no listener.
    Embedded,
}

impl Frontend {
    fn is_embedded(self) -> bool {
        self == Self::Embedded
    }
}

/// Construct every subsystem for the desktop service.
pub async fn boot(config: Config, paths: Paths) -> Result<Booted> {
    boot_with(config, paths, Frontend::Service).await
}

/// Construct every subsystem, for `frontend`. [`Frontend::Embedded`] differs
/// in exactly three places, each marked `Embedded difference N of 3`.
pub async fn boot_with(config: Config, paths: Paths, frontend: Frontend) -> Result<Booted> {
    boot_with_store(config, paths, frontend, None).await
}

/// [`boot_with`], with the vault's credential store injectable; `None` is the
/// production path. A `Service` boot seals the TLS key into a real login
/// keychain entry, and `LOCA_VAULT_FILE_STORE` cannot help because `set_var` is
/// unsound in a threaded test binary. Never feature-gated.
pub async fn boot_with_store(
    config: Config,
    paths: Paths,
    frontend: Frontend,
    store: Option<Arc<dyn crate::secrets::SecureStore>>,
) -> Result<Booted> {
    paths.ensure_dirs()?;

    // Deno's `--deny-read` / `--deny-write` are comma-separated with no escape,
    // so a comma anywhere above the core root truncates the whole deny list
    // and every tool worker silently gains the vault, credentials and database.
    if let Some(bad) = crate::extensions::sandbox::unrepresentable_deny_path(&paths) {
        return Err(crate::error::validation(format!(
            "the core root contains a comma, at {}. Deno's sandbox flags are comma-separated \
             with no escape, so the deny list protecting the secrets vault, the admin \
             credentials and the database cannot be expressed and would silently protect \
             nothing. Move the core home (LOCA_CORE_HOME) somewhere without a comma.",
            bad.display(),
        )));
    }

    let paths = Arc::new(paths);
    let db = Db::open_migrated(&paths.db_file())?;
    let vault = match store {
        Some(store) => Vault::with_store(paths.secrets_enc_file(), store),
        None => Vault::new(&paths, config.channel),
    };
    let frames = Arc::new(FrameBus::new());
    let sessions = SessionsStore::new(Arc::clone(&paths));

    // A WebSocket authenticates once, at upgrade, so revocation must close it,
    // and sessions are JSON files with no cascade. `drop_settings_cache` stays
    // default: the rows cascade with `clients` and nothing caches them.
    let auth = Arc::new(Auth::new(db.clone(), Arc::clone(&paths)).with_hooks(
        crate::auth::RevocationHooks {
            gc_sessions: {
                let sessions = sessions.clone();
                Arc::new(move |client_id: String| {
                    let sessions = sessions.clone();
                    Box::pin(async move {
                        let owned = match sessions.list(&client_id).await {
                            Ok(owned) => owned,
                            Err(err) => {
                                tracing::warn!(
                                    target: "loca::auth",
                                    "could not list sessions of revoked client {client_id}: {err}",
                                );
                                return;
                            }
                        };
                        for entry in owned {
                            match sessions.delete(&client_id, &entry.id).await {
                                Ok(paths) => {
                                    for path in paths {
                                        crate::sessions::store::remove_file(path).await.ok();
                                    }
                                }
                                Err(err) => tracing::warn!(
                                    target: "loca::auth",
                                    "could not delete session {} of revoked client: {err}",
                                    entry.id,
                                ),
                            }
                        }
                    })
                })
            },
            drop_settings_cache: crate::auth::RevocationHooks::default().drop_settings_cache,
            close_sockets: {
                let frames = Arc::clone(&frames);
                Arc::new(move |client_id: String| {
                    let frames = Arc::clone(&frames);
                    Box::pin(async move {
                        frames.close_client(&client_id, 4001, "client revoked");
                    })
                })
            },
        },
    ));
    let local_client = match frontend {
        Frontend::Embedded => Some(crate::auth::to_identity(auth.ensure_local_client().await?)),
        Frontend::Service => None,
    };
    // Ordered: the aggregate pulls its subsystem list from the registry, and
    // `attach_status` is what makes a subsystem change recompute it.
    let sidecars = Arc::new(Manager::new());
    let status = Arc::new(CoreStatusService::new(
        Arc::clone(&sidecars) as Arc<dyn SidecarSource>
    ));
    sidecars.attach_status((*status).clone());
    // Emission is edge-only, so a client needs both the seed (or a client
    // joining a steady core never learns its status) and the subscription.
    // Registered before the listener binds so the seed cannot be missed.
    frames.add_seed(Arc::new(CoreStatusSeed(Arc::clone(&status))) as Arc<dyn ConnectionSeed>);
    {
        let mut changes = status.subscribe();
        let current = Arc::clone(&status);
        let frames = Arc::clone(&frames);
        tokio::spawn(async move {
            loop {
                match changes.recv().await {
                    Ok(snapshot) => {
                        frames.broadcast_all(&ServerToClientFrame::CoreStatus { snapshot });
                    }
                    // The aggregate is a whole-value snapshot, so re-reading
                    // the current one is a complete recovery from lag.
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(target: "loca::ws", "core status feed lagged {n}");
                        frames.broadcast_all(&ServerToClientFrame::CoreStatus {
                            snapshot: current.snapshot(),
                        });
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }
    let settings = crate::settings::Settings::new(&paths, db.clone(), Arc::clone(&frames));

    // Env precedence differs by key: LOCA_CORE_HOST wins when NON-EMPTY;
    // LOCA_CORE_BEHIND_PROXY wins whenever DEFINED (`=0` is false and
    // suppresses the file read; accepts "1" or case-insensitive "true").
    let raw = raw_settings(&paths).await;
    let behind_proxy = match std::env::var("LOCA_CORE_BEHIND_PROXY") {
        Ok(v) => v == "1" || v.eq_ignore_ascii_case("true"),
        Err(_) => raw.bool("server.behindProxy"),
    };
    let bind_host = match std::env::var("LOCA_CORE_HOST") {
        Ok(v) if !v.trim().is_empty() => v,
        _ => raw
            .string("server.bindHost")
            .unwrap_or_else(|| config.host.clone()),
    };
    let config = Config {
        host: bind_host,
        ..config
    };

    // Embedded difference 1 of 3: no TLS material. There is no listener to
    // present a certificate on, and generating one would seal a key in the
    // device keychain for nothing. `Booted::serve` refuses without TLS.
    let tls = if behind_proxy || frontend.is_embedded() {
        None
    } else {
        let sealed = vault.get(tls::KEY_SECRET).await?;
        let (material, new_jwk) = tls::ensure(sealed.as_deref(), &config.host)?;
        if let Some(jwk) = new_jwk {
            vault.set(tls::KEY_SECRET, &jwk).await?;
        }
        // The public half beside the core root, so a co-located tool can trust
        // this listener as a CA of one.
        crate::files::write_atomic(
            paths.tls_cert_file(),
            material.cert_pem.clone().into_bytes(),
        )
        .await?;
        Some(material)
    };

    let ctx = AppContext {
        config: Arc::new(config),
        paths,
        db,
        vault,
        auth: Arc::clone(&auth) as Arc<dyn ClientAuth>,
        settings,
        frames,
        status: Arc::clone(&status) as Arc<dyn StatusProvider>,
        behind_proxy,
        // From `frontend`, not `tls_pin.is_none()`: a proxied desktop core
        // would otherwise report itself unable to pair, schedule or update.
        capabilities: crate::transport::health::Capabilities::detect(frontend),
        tls_pin: tls.as_ref().map(|t| t.pin.clone()),
        started_at: std::time::Instant::now(),
    };

    // Before the listener binds, so no request can observe the half state.
    sessions.sweep_orphaned_dirs().await;

    // Exactly one `Net`: it owns the download path's only `reqwest` clients and
    // counts its calls, which the consent tests assert on. Not a total of
    // outbound requests: `catalog::load` fetches through `reqwest` directly.
    let net = Arc::new(crate::downloads::net::Net::new()?);
    let queue = {
        // A second `Models`, and safe: its only mutable state is the hardware
        // cache and it is erased to `CatalogHashes` at once. The real one below
        // takes the download seam, which needs this queue.
        let catalog: Arc<dyn crate::downloads::deps::CatalogHashes> = Arc::new(
            crate::models::Models::new(&ctx, crate::models::deps::ModelsDeps::unwired()),
        );
        let transfer = Arc::new(crate::downloads::transfer::Transfer::new(
            Arc::clone(&net),
            Arc::clone(&catalog),
            &ctx.config.hf_base_url,
        ));
        crate::downloads::queue::DownloadQueue::new(
            crate::downloads::store::DownloadStore::new(ctx.db.clone(), &ctx.config.hf_base_url),
            transfer,
            (*ctx.paths).clone(),
            &ctx.config.hf_base_url,
        )
    };
    // `start`, not `normalize`: normalising alone leaves every recovered row
    // with no worker behind it, a "Downloading" bar that never moves.
    queue.start().await?;

    let deferred_requirements = Arc::new(DeferredRequirements::default());
    let models = crate::models::Models::new(
        &ctx,
        crate::models::deps::ModelsDeps::unwired()
            .with_requirements(Arc::clone(&deferred_requirements)
                as Arc<dyn crate::storage::deps::RequirementsNotifier>)
            .with_downloads(Arc::new(
                crate::downloads::models_seam::ModelDownloads::new(
                    queue.clone(),
                    Arc::clone(&net),
                    (*ctx.paths).clone(),
                    &ctx.config.hf_base_url,
                ),
            )),
    );

    // One `Arc<dyn LocalSpeech>` shared by the route slice and the broker's
    // speech seam; two would be two engines with two queues. Built here so the
    // reconciler is in scope for the download subscription below.
    let (speech, speech_reconciler) = crate::speech::local::build(
        ctx.settings.clone(),
        ctx.paths.root().to_path_buf(),
        ctx.paths.models_dir().to_path_buf(),
        Arc::clone(&sidecars),
        (*status).clone(),
    );

    // Download activity feeds the core status, the `downloads.snapshot` frame
    // and the requirements recompute. The last goes through `DownloadEdges`
    // because a BINARIES row completing is pre-extract.
    {
        let mut changes = queue.subscribe();
        let status = Arc::clone(&status);
        let frames = Arc::clone(&ctx.frames);
        let speech_downloads = speech_reconciler.clone();
        // Not primed from `initial`: one redundant recompute beats reading a
        // real completion as already-seen.
        let edges = crate::requirements::DownloadEdges::new();
        let requirements = Arc::clone(&deferred_requirements);
        let initial = queue.snapshot().await.unwrap_or_default();
        status.note_downloads(&initial);
        // Rows survive restarts: without seeding, the first broadcast reads
        // every persisted `Completed` row as fresh and reloads a model.
        if let Some(reconciler) = &speech_reconciler {
            reconciler.seed_downloads(&initial);
        }
        frames.broadcast_all(&ServerToClientFrame::DownloadsSnapshot { items: initial });
        tokio::spawn(async move {
            loop {
                match changes.recv().await {
                    Ok(entries) => {
                        status.note_downloads(&entries);
                        frames.broadcast_all(&ServerToClientFrame::DownloadsSnapshot {
                            items: (*entries).clone(),
                        });
                        if edges.note(&entries) {
                            requirements.changed();
                        }
                        if let Some(reconciler) = &speech_downloads {
                            // Not awaited: `note_downloads` chains into a
                            // model load, which would freeze this consumer for
                            // its whole duration. Safe to overlap.
                            let reconciler = Arc::clone(reconciler);
                            let entries = Arc::clone(&entries);
                            tokio::spawn(async move {
                                reconciler.note_downloads(&entries).await;
                            });
                        }
                    }
                    // The next send carries the whole snapshot, so a lagged
                    // consumer just carries on. Breaking would stop every
                    // future update for the life of the process.
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(target: "loca::downloads", "status feed lagged {n}");
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    let downloads_port = queue.clone();
    let binaries = crate::downloads::binaries::installer::BinariesInstaller::new(
        Arc::clone(&net),
        queue,
        (*ctx.paths).clone(),
        ctx.config.channel,
        &ctx.config.binary_manifest_url(),
    );
    // The only thing that takes an installed binary out of `missing`: the
    // queue's `Completed` for a binaries row is pre-extract.
    binaries
        .attach_requirements(Arc::clone(&deferred_requirements) as Arc<dyn RequirementsNotifier>);
    // `queue.start()` skips `binaries` rows (no URL, not resumable), so this is
    // the only thing that clears one left behind by a killed core.
    binaries.reconcile_interrupted_installs().await;

    // Built here rather than in the router because it needs `start()` after
    // boot and `shutdown()` before the listener closes.
    //
    // Embedded difference 2 of 3: no stdio MCP, which is a spawned child running
    // a downloaded interpreter. The routes still serve and `plan` refuses with
    // a reason the user can act on.
    let mcp = crate::mcp::Mcp::new(
        Arc::clone(&ctx.paths),
        ctx.db.clone(),
        ctx.vault.clone(),
        Arc::clone(&ctx.frames),
        Arc::clone(&sidecars),
        if frontend.is_embedded() {
            crate::mcp::stdio::StdioSupport::Unsupported
        } else {
            crate::mcp::stdio::StdioSupport::Available
        },
    );

    // One `Llm`: two would be two schedulers over one local model, and the
    // semaphore that serialises local turns would be sized against a queue it
    // could not see.
    let llm = Arc::new(crate::llm::Llm::new(ctx.vault.clone(), ctx.config.channel));
    llm.attach_status((*status).clone());

    #[cfg(feature = "local-inference")]
    let local_engine = {
        let engine = Arc::new(crate::inference::engine::LocalEngine::new(
            ctx.settings.clone(),
            (*ctx.paths).clone(),
        ));
        llm.set_local_provider(Arc::new(crate::inference::provider::LocalProvider::new(
            Arc::clone(&engine),
        )));
        // The stall watchdog unloads rather than only logging: a wedged local
        // turn otherwise holds the weights resident.
        llm.set_watchdog_handler(Some(crate::inference::idle::IdleUnload::watchdog_handler(
            Arc::clone(&engine),
        )));
        // Without this nothing publishes `SidecarKind::Llama`, so automated
        // turns fire into a mid-load model instead of waiting for it.
        engine.attach_status(Arc::clone(&sidecars));
        engine
    };

    let memories = crate::memories::MemoriesStore::new(Arc::clone(&ctx.paths), ctx.db.clone());

    // Not gated on `llm.provider`: a user answering from an external provider
    // still wants memories injected. Without `local-inference` there is no
    // engine, so `NoLocalEmbedder` is correct rather than a stub.
    #[cfg(feature = "local-inference")]
    let local_embedder: Arc<dyn crate::memories::LocalEmbedder> = Arc::new(
        crate::inference::embed::LlamaEmbedder::new((*ctx.paths).clone()),
    );
    #[cfg(not(feature = "local-inference"))]
    let local_embedder: Arc<dyn crate::memories::LocalEmbedder> =
        Arc::new(crate::memories::NoLocalEmbedder);

    let embedding =
        crate::memories::Embedding::new(ctx.settings.clone(), ctx.vault.clone(), local_embedder);
    let tool_embedding = embedding.clone();
    let embedding_port = embedding.clone();

    // Voices come from the catalog, not the engine, so the dropdown works on a
    // core with no local speech engine at all.
    let speech_voices: Arc<dyn crate::speech::TtsVoiceSource> =
        Arc::new(crate::models::speech::CatalogVoices::new(models.clone()));

    // Chat <-> scheduler cycle, closed by `attach_automated` below. A scheduler
    // whose runner was never attached refuses rather than no-opping.
    let scheduled = crate::scheduled::Scheduler::new(
        ctx.db.clone(),
        crate::scheduled::deps::SchedulerDeps::unwired().with_loading(Arc::new(
            crate::chat::LlmLoading::new(ctx.settings.clone(), Arc::clone(&sidecars)),
        )),
    );

    // The tool seam, filled after the extension and MCP subsystems exist.
    // Without it the model is offered no tools while `/api/v1/tools` still
    // lists them all, since that route reads the subsystems directly.
    let tools = Arc::new(DeferredTools::default());
    let deps = crate::chat::deps::ChatDeps::unwired()
        .with_schedule(Arc::new(scheduled.clone()))
        .with_tools(Arc::clone(&tools) as Arc<dyn crate::chat::ToolHost>);
    #[cfg(feature = "local-inference")]
    let deps = deps.with_idle(Arc::new(crate::inference::idle::IdleUnload::new(
        Arc::clone(&local_engine),
    )));

    let chat = crate::chat::Chat::new(crate::chat::ChatConfig {
        sessions: sessions.clone(),
        memories: memories.clone(),
        embedding,
        settings: ctx.settings.clone(),
        llm: (*llm).clone(),
        frames: Arc::clone(&ctx.frames),
        deps,
        summaries: Arc::new(crate::chat::background::LlmSummaries::new(
            (*llm).clone(),
            ctx.settings.clone(),
        )),
    });

    // Without this the hub parses a `chat.start` frame, finds no handler, and
    // drops it: the whole chat module is unreachable.
    ctx.frames.on_inbound(Arc::new(chat.clone()));

    scheduled.attach_automated(Arc::new(crate::scheduled::ChatAutomated::new(chat.clone())));

    // The indexer is the same `chat` handle `Services::memory_indexer` carries:
    // a second one would summarise from a queue that does not gate on turn
    // activity.
    let indexer: Arc<dyn crate::memories::MemoryIndexer> = Arc::new(chat.clone());
    let extension_registry =
        crate::extensions::registry::Registry::new(ctx.db.clone(), (*ctx.paths).clone());
    let shipped_memories: Arc<dyn crate::extensions::install::runner::ExtensionMemories> = Arc::new(
        crate::memories::ExtensionMemoriesStore::new(memories.clone(), Arc::clone(&indexer)),
    );
    let installer = crate::extensions::install::runner::Installer::new(
        crate::extensions::install::runner::InstallerConfig {
            paths: (*ctx.paths).clone(),
            config: (*ctx.config).clone(),
            settings: ctx.settings.clone(),
            registry: extension_registry.clone(),
            http: Arc::new(crate::extensions::install::net::HttpClient::new()?),
            sidecars: Arc::clone(&sidecars),
            memories: Arc::clone(&shipped_memories),
            events: Arc::new(
                crate::extensions::install::runner::BroadcastInstallEvents::new(Arc::clone(
                    &ctx.frames,
                )),
            ),
        },
    );

    // A second `SpeechService` over the same `Manager` and `LocalSpeech`; safe
    // because the service holds no state.
    let broker_speech = crate::speech::SpeechService::new(
        &ctx,
        Arc::clone(&sidecars),
        Arc::clone(&speech),
        Arc::clone(&speech_voices),
    );
    let broker = Arc::new(crate::extensions::broker::Broker::new(
        (*ctx.paths).clone(),
        Arc::new(extension_registry.clone()),
        Arc::new(crate::settings::BrokerSettingsStore::new(
            ctx.settings.clone(),
        )),
        Arc::new(crate::memories::ToolMemoriesStore::new(
            memories.clone(),
            Arc::clone(&indexer),
        )),
        Arc::new(crate::llm::ToolCompletions::new(
            (*llm).clone(),
            ctx.settings.clone(),
        )),
        Arc::new(crate::speech::ToolSpeechService::new(broker_speech)),
        // Warms the local model before a tool's `llm` call.
        #[cfg(feature = "local-inference")]
        (Arc::clone(&local_engine) as Arc<dyn crate::extensions::broker::EnsureModelLoaded>),
        #[cfg(not(feature = "local-inference"))]
        Arc::new(crate::llm::NoModelToLoad),
    ));

    // `.pool(...)` is what makes `refresh_permissions` reach a warm `deno`,
    // whose sandbox flags are fixed at spawn. The spawner takes the installer's
    // `Registry` clone, so both share one verification cache. Desktop only.
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    let pool = Some(crate::extensions::worker::pool::WorkerPool::new(
        Arc::new(crate::extensions::worker::spawner::DenoSpawner::new(
            (*ctx.paths).clone(),
            extension_registry.clone(),
            Arc::clone(&sidecars),
        )),
        crate::extensions::worker::pool::DEFAULT_POOL_CONFIG,
    ));
    #[cfg(any(target_os = "android", target_os = "ios"))]
    let pool: Option<crate::extensions::worker::pool::WorkerPool> = None;

    let mut extensions_builder = crate::extensions::Extensions::builder(
        installer,
        Arc::clone(&shipped_memories),
        Arc::clone(&ctx.frames),
    )
    .embedder(Arc::new(crate::memories::ToolEmbedding::new(
        tool_embedding,
    )))
    .mcp_tools(Arc::new(mcp.clone()))
    .broker(broker);
    if let Some(pool) = pool.clone() {
        extensions_builder = extensions_builder.pool(pool);
    }
    let extensions = extensions_builder.build();

    // What the client's Retry button calls on a row stuck in `Error`. Absent,
    // the restart route answers a validation error rather than a 204 that did
    // nothing.
    struct InProcessControl {
        #[cfg(feature = "local-inference")]
        engine: Arc<crate::inference::engine::LocalEngine>,
        speech: Option<Arc<crate::speech::local::reconcile::Reconciler>>,
    }

    #[async_trait::async_trait]
    impl crate::sidecars::SubsystemControl for InProcessControl {
        async fn restart(&self, kind: crate::contract::api::SidecarKind) -> Result<()> {
            match kind {
                crate::contract::api::SidecarKind::Llama => {
                    // Dropping the weights IS the restart: `ensure_loaded`
                    // reloads from current settings at the top of every turn.
                    #[cfg(feature = "local-inference")]
                    {
                        self.engine.unload().await;
                        return Ok(());
                    }
                    #[cfg(not(feature = "local-inference"))]
                    Err(internal(
                        "this build has no in-process language model to restart",
                    ))
                }
                crate::contract::api::SidecarKind::Speech => match &self.speech {
                    Some(reconciler) => {
                        reconciler.reconcile().await;
                        Ok(())
                    }
                    None => Err(internal("this build has no in-process speech engine")),
                },
                other => Err(crate::error::validation(format!(
                    "{other} is not a controllable subsystem"
                ))),
            }
        }
    }

    sidecars.attach_control(Arc::new(InProcessControl {
        #[cfg(feature = "local-inference")]
        engine: Arc::clone(&local_engine),
        speech: speech_reconciler.clone(),
    }));

    // The provider -> base-dir map an extension's shipped memories resolve
    // through is in memory and only written by an install, so it is rebuilt
    // for every installed extension at boot.
    match extensions.registry().list().await {
        Ok(installed) => {
            for extension in installed {
                memories.register_provider(&extension.id, ctx.paths.extensions_dir());
            }
        }
        Err(err) => tracing::warn!(
            target: "loca::memories",
            "extension memory provider registration failed: {}", err.message,
        ),
    }

    // Awaited, so a memory the model can be given on the first turn does not
    // race boot. Failure is a warning: an unreconciled store is still readable.
    match memories.rescan().await {
        Ok(out) => {
            if out.added + out.removed + out.changed > 0 {
                tracing::info!(
                    target: "loca::memories",
                    "memory rescan: {} added, {} removed, {} changed",
                    out.added, out.removed, out.changed,
                );
            }
            indexer.schedule(None);
        }
        Err(err) => tracing::warn!(
            target: "loca::memories", "memory rescan failed: {}", err.message,
        ),
    }

    // Read while `chat` is still in scope, and after the set.
    let tools_wired_check = |chat: &crate::chat::Chat| chat.tools_live();
    if tools
        .0
        .set(Arc::new(crate::tool_host::HostedTools::new(
            extensions.clone(),
            mcp.clone(),
            (*llm).clone(),
        )) as Arc<dyn crate::chat::ToolHost>)
        .is_err()
    {
        return Err(internal("the tool host was already set"));
    }
    let tools_wired = tools_wired_check(&chat);
    let revocation_wired = auth.hooks_wired();

    // Hide an extension's memories from every listing unless it is installed,
    // mirroring how its tools are gated. The base-dir mapping stays for every
    // extension, so content still resolves after a later install.
    memories.set_provider_gate(Some(Arc::new(
        crate::extensions::ExtensionProviderGate::new(extension_registry.clone()),
    )));

    // Awaits only the decision; the install itself is a background job. Never
    // fetches: production installs only from the install-planted artefacts.
    crate::extensions::install::seed::seed_extensions_if_needed(extensions.installer()).await;

    // Content-hash drift, in the background: a large extension takes seconds
    // to walk. A drifted extension has its tools disabled until re-enabled.
    {
        let registry = extensions.registry().clone();
        tokio::spawn(async move {
            match registry.verify_all_on_boot().await {
                Ok(drifted) if !drifted.is_empty() => tracing::warn!(
                    target: "loca::extensions",
                    "extension content drift: {} extension(s) flipped to drift and had their tools disabled: {}",
                    drifted.len(),
                    drifted.join(", "),
                ),
                Ok(_) => {}
                Err(err) => tracing::warn!(
                    target: "loca::extensions",
                    "extension content verification failed on boot: {err}",
                ),
            }
        });
    }

    // Both built once: `Requirements` owns the coalescing recompute worker and
    // `Greetings` the per-client dedup map.
    let requirements = crate::requirements::Requirements::new(
        ctx.settings.clone(),
        models.clone(),
        Arc::new(binaries.clone()) as Arc<dyn crate::requirements::deps::Binaries>,
        Arc::clone(&ctx.frames),
    );
    let _ = deferred_requirements.0.set(requirements.clone());
    // A settings change is the fourth trigger: picking a different model is
    // what puts a file in `missing` in the first place.
    {
        let mut changes = ctx.settings.subscribe();
        let requirements = requirements.clone();
        tokio::spawn(async move {
            // The recompute reads the store fresh, so one is the right answer
            // to a lag as well; only a closed channel ends the task.
            while let Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) = changes.recv().await {
                requirements.changed();
            }
        });
    }
    let greetings = crate::requirements::Greetings::new(
        chat.clone(),
        Arc::new(crate::chat::LlmLoading::new(
            ctx.settings.clone(),
            Arc::clone(&sidecars),
        )),
    );

    // The same `Chat` answers "is an agent mid-turn on this session" off the
    // live stream registry; a second one would say no every time.
    let chat_for_storage = chat.clone();
    let storage = StorageDeps::unwired()
        .with_models(Arc::new(models.clone()))
        .with_requirements(Arc::clone(&deferred_requirements)
            as Arc<dyn crate::storage::deps::RequirementsNotifier>)
        .with_binaries(Arc::new(binaries.clone()))
        .with_sessions(Arc::new(chat_for_storage))
        .with_extensions(Arc::new(extensions.clone()))
        .with_settings(Arc::new(ctx.settings.clone()));
    let memories_port = memories.clone();
    let binaries_port = binaries.clone();
    let services = Services {
        auth,
        memories,
        memory_indexer: Arc::new(chat.clone()),
        sessions,
        chat: Arc::new(chat),
        storage: storage.clone(),
        sidecars: Arc::clone(&sidecars),
        llm: Arc::clone(&llm),
        speech,
        speech_voices,
        models,
        binaries,
        mcp: mcp.clone(),
        extensions: extensions.clone(),
        requirements,
        greetings,
        scheduled: scheduled.clone(),
        status: Arc::clone(&status),
    };

    // After the router is built, so nothing races a route not yet mounted.
    // Reconnecting to a server the user enabled is consented network, and
    // remote MCP works on a phone, so this is not skipped when embedded.
    mcp.start();
    // Embedded difference 3 of 3: the arm loop does not start. Scheduled
    // prompts are out of scope on mobile (they would need the app in the
    // foreground); the routes still serve so existing rows stay visible.
    if !frontend.is_embedded() {
        scheduled.start();
    }

    // After the router: the initial reconcile can load a model. Not skipped
    // on embedded, since local speech is in scope on mobile.
    if let Some(reconciler) = &speech_reconciler {
        reconciler.spawn_settings_watch();
    }

    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    if let Some(pool) = &pool {
        pool.spawn_settings_watch(ctx.settings.clone());
    }

    // An embedded core never calls `serve()`, where desktop marks boot done
    // (there, "done" means the listener is bound), so `booting` would stay
    // true forever and the status bar would never leave "Starting up".
    if frontend.is_embedded() {
        status.note_boot_done();
    }

    Ok(Booted {
        core: Core::new(ctx, services),
        status,
        sidecars,
        mcp,
        extension_pool: pool,
        scheduled,
        tools_wired,
        revocation_wired,
        extensions,
        #[cfg(feature = "local-inference")]
        local_engine,
        llm,
        embedding: embedding_port,
        downloads: downloads_port,
        binaries: binaries_port,
        memories: memories_port,
        storage,
        tls,
        local_client,
    })
}

impl Booted {
    /// Bind and serve until the core is shut down. Desktop only; mobile drives
    /// the router in-process instead.
    pub async fn serve(&self) -> Result<()> {
        let Some(tls) = self.tls.as_ref() else {
            return Err(internal(
                "LOCA_CORE_BEHIND_PROXY is set, but the plaintext listener it needs \
                 lands with the proxy support it exists for",
            ));
        };
        let ctx = self.core.context();
        let listener = self
            .core
            .bind(tls, &ctx.config.host, ctx.config.port)
            .await?;
        let addr = listener.local_addr();
        self.status.note_boot_done();
        // A bound listener implies the database opened and the TLS key
        // unsealed. Without this the marker's `attempts` climbs every boot
        // until a healthy update is rolled back.
        crate::update::rollback::commit_update(&ctx.config, &ctx.paths).await;
        // A prefix: the pin is public, but a full base64 SHA-256 looks like a
        // sealed key to the log scrubber.
        let pin = ctx.tls_pin.as_deref().map_or("none".to_string(), |p| {
            format!("{}...", p.chars().take(12).collect::<String>())
        });
        tracing::info!(target: "loca::boot", "listening on https://{addr} (pin={pin})");
        self.core.serve_bound(listener).await;
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod seams;
