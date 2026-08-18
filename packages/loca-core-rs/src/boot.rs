//! Boot: construct every subsystem, in dependency order, and hand back a
//! running [`transport::Core`].
//!
//! **Orchestrator-owned**, like the router it feeds. This is the one place that
//! knows how every package's handle is built and in what order, which is
//! exactly the cross-package knowledge no single package should carry.
//!
//! The desktop binary calls [`boot`] then [`Booted::serve`]. The mobile
//! frontend calls [`boot_with`] with [`Frontend::Embedded`] and drives the core
//! in process without ever binding a socket: on Android `127.0.0.1` is not
//! app-private, so a listener would expose the core to every installed app.
//!
//! [`Frontend`] is a VALUE rather than a `cfg`, and the three places the
//! embedded arm differs are marked `Embedded difference N of 3` below. Each is
//! something that would be WRONG on a core with no listener, not merely unused.
//!
//! The order is not arbitrary. The vault exists before TLS because the private
//! key is sealed inside it, and generating a second key would re-pin every
//! paired client.

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
use crate::storage::deps::StorageDeps;
use crate::transport::router::Services;
use crate::transport::state::{AppContext, ClientAuth, ClientIdentity, StatusProvider};
use crate::transport::status::{CoreStatusService, SidecarSource};
use crate::transport::tls::{self, TlsMaterial};
use crate::transport::{Core, FrameBus};

use tokio::sync::broadcast;

/// The two boot-only keys, read straight out of `settings.json`.
///
/// Deliberately not the settings service: `server.bindHost` and
/// `server.behindProxy` are excluded from the schema so a paired client cannot
/// widen network exposure or weaken the pairing posture over the API, and the
/// only reader being here is what enforces that. A missing or malformed file is
/// not an error, because a fresh install has neither and both keys have
/// defaults; it is logged so a hand-edited file with a typo is not silent.
struct RawSettings(serde_json::Map<String, serde_json::Value>);

impl RawSettings {
    fn bool(&self, key: &str) -> bool {
        self.0.get(key).and_then(serde_json::Value::as_bool) == Some(true)
    }

    /// Trimmed, and empty is treated as absent so a blank line in a hand-edited
    /// file does not bind the listener to nothing.
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

/// Breaks a genuine construction cycle between models and requirements.
///
/// `Requirements` needs a `Models` to compute a snapshot from, and `ModelsDeps`
/// needs a `RequirementsNotifier` so that deleting a model resurfaces it as
/// something to download. Neither can be built first.
///
/// The shim is handed to `ModelsDeps` and `StorageDeps` at construction and
/// filled once `Requirements` exists. Before that it drops notifications, which
/// is correct rather than lossy: nothing can delete a model or reclaim storage
/// before boot has finished wiring, so the only notifications it can swallow are
/// ones no caller could have produced.
///
/// Set-once, following `attach_status` and `attach_automated`. A `OnceLock`
/// rather than a `Mutex` because a second `Requirements` would own a second
/// coalescing recompute worker.
#[derive(Default)]
struct DeferredRequirements(std::sync::OnceLock<crate::requirements::Requirements>);

impl crate::storage::deps::RequirementsNotifier for DeferredRequirements {
    fn changed(&self) {
        if let Some(requirements) = self.0.get() {
            requirements.changed();
        }
    }
}

/// Breaks the fourth construction cycle, between chat and the tool subsystems.
///
/// `Chat` needs a [`ToolHost`](crate::chat::ToolHost) to offer the model any
/// tool at all, and that host is `HostedTools`, which joins extensions, MCP and
/// the LLM. But the extension subsystem needs `Chat` as its memory indexer, so
/// neither can be built first. The same shape as `DeferredRequirements` above,
/// `attach_status` and `attach_automated`: a set-once `OnceLock`.
///
/// **Before it is filled it behaves exactly as `NoTools`**, which is correct
/// rather than lossy: nothing can run a turn before boot has finished wiring, so
/// the only reads it can serve empty are ones no caller could have made.
///
/// A set-once rather than a `Mutex` for the reason every other seam here is: a
/// second `HostedTools` would mean a second view of the extension registry, and
/// a turn could then verify a content hash against one and execute against the
/// other.
#[derive(Default)]
struct DeferredTools(std::sync::OnceLock<Arc<dyn crate::chat::ToolHost>>);

impl DeferredTools {
    /// The live host, or `None` while boot is still wiring.
    fn get(&self) -> Option<&Arc<dyn crate::chat::ToolHost>> {
        self.0.get()
    }
}

#[async_trait::async_trait]
impl crate::chat::ToolHost for DeferredTools {
    fn is_live(&self) -> bool {
        self.get().is_some()
    }

    async fn installed_extension_tools(&self) -> Vec<crate::contract::api::Tool> {
        match self.get() {
            Some(h) => h.installed_extension_tools().await,
            None => Vec::new(),
        }
    }

    async fn get_tool(&self, tool_id: &str) -> Option<crate::contract::api::Tool> {
        self.get()?.get_tool(tool_id).await
    }

    async fn list_mcp_tools(&self) -> Vec<crate::contract::api::Tool> {
        match self.get() {
            Some(h) => h.list_mcp_tools().await,
            None => Vec::new(),
        }
    }

    async fn is_mcp_server(&self, id: &str) -> bool {
        match self.get() {
            Some(h) => h.is_mcp_server(id).await,
            None => false,
        }
    }

    async fn load_tool_embeddings(
        &self,
    ) -> std::collections::HashMap<String, crate::chat::deps::ToolEmbedding> {
        match self.get() {
            Some(h) => h.load_tool_embeddings().await,
            None => Default::default(),
        }
    }

    async fn filter_phase1(
        &self,
        query_vector: &[f32],
        options: crate::chat::Phase1Options,
    ) -> crate::chat::Phase1Result {
        match self.get() {
            Some(h) => h.filter_phase1(query_vector, options).await,
            None => Default::default(),
        }
    }

    async fn filter_phase2(
        &self,
        query: &str,
        candidates: Vec<crate::contract::api::ToolDescriptor>,
        endpoint: &crate::llm::LlmEndpointConfig,
        reasoning_budget: u32,
    ) -> Vec<crate::contract::api::ToolDescriptor> {
        match self.get() {
            Some(h) => {
                h.filter_phase2(query, candidates, endpoint, reasoning_budget)
                    .await
            }
            // Fails OPEN with its input, matching `NoTools` and a real phase 2
            // whose model is unreachable. An empty set would make "no filter"
            // and "filter rejected everything" indistinguishable.
            None => candidates,
        }
    }

    async fn verify_tool_fresh(&self, extension_id: &str) -> Result<()> {
        match self.get() {
            Some(h) => h.verify_tool_fresh(extension_id).await,
            None => Ok(()),
        }
    }

    fn validate_tool_args(
        &self,
        tool: &crate::contract::api::Tool,
        arguments_json: &str,
    ) -> Result<String> {
        match self.get() {
            Some(h) => h.validate_tool_args(tool, arguments_json),
            None => Ok(arguments_json.to_string()),
        }
    }

    async fn start_tool_call(
        &self,
        spec: crate::chat::deps::ToolCallSpec,
        events: tokio::sync::mpsc::UnboundedSender<crate::chat::deps::ToolCallEvent>,
    ) -> Result<Box<dyn crate::chat::deps::ToolCallController>> {
        match self.get() {
            Some(h) => h.start_tool_call(spec, events).await,
            None => Err(internal("the tool subsystem is not wired")),
        }
    }

    async fn call_mcp_tool(
        &self,
        server_id: &str,
        name: &str,
        args: serde_json::Map<String, serde_json::Value>,
    ) -> Result<serde_json::Value> {
        match self.get() {
            Some(h) => h.call_mcp_tool(server_id, name, args).await,
            None => Err(internal("the tool subsystem is not wired")),
        }
    }

    async fn resolve_mcp_tokens(&self, text: &str) -> crate::chat::McpTokens {
        match self.get() {
            Some(h) => h.resolve_mcp_tokens(text).await,
            None => Default::default(),
        }
    }
}

/// A booted core, plus the handles the frontend needs that the router does not.
pub struct Booted {
    pub core: Core,
    pub status: Arc<CoreStatusService>,
    /// The subsystem registry, so the frontend can stop children on the way
    /// down and the packages that own a subsystem can publish into it.
    pub sidecars: Arc<Manager>,
    /// The MCP subsystem, so the frontend can close its connections before the
    /// listener does.
    pub mcp: crate::mcp::Mcp,
    /// The warm-worker pool, so the frontend can stop live `deno` workers
    /// before the sidecar manager goes down. PTY-mode workers are deliberately
    /// NOT in the manager's `children` list (they enrol through
    /// `track_external`, which is Job Object membership only), so
    /// `sidecars.shutdown()` alone leaves them running.
    ///
    /// `None` on mobile, where the spawner does not exist at all: starting a
    /// downloaded interpreter is what W^X and App Store 2.5.2 forbid.
    pub extension_pool: Option<crate::extensions::worker::pool::WorkerPool>,
    /// The prompt scheduler, so the frontend can stop the arm loop before the
    /// listener closes. Without it a tick can be mid-`fire` (opening a session,
    /// starting a turn) while the routes that would serve the result go away.
    pub scheduled: crate::scheduled::Scheduler,
    /// Whether the chat tool seam was filled with the real `HostedTools`.
    ///
    /// A read port for the boot-seam suite (D-165), read off the live
    /// `OnceLock` rather than restated. `false` means the model is offered no
    /// tools at all while `/api/v1/tools` still lists a full catalog, which is
    /// the exact shape of the regression this field exists to catch.
    pub tools_wired: bool,
    /// Whether revocation actually revokes: see [`crate::auth::Auth::hooks_wired`].
    /// A read port for the boot-seam suite (D-165).
    pub revocation_wired: bool,
    /// The extension subsystem, the same handle `Services` carries.
    ///
    /// A read port for the boot-seam suite (D-165), like [`Booted::llm`].
    /// Cheap to clone and every clone drives the same registry and pool, so
    /// this is a second reference rather than a second subsystem.
    pub extensions: crate::extensions::Extensions,
    /// The in-process inference engine, when the feature is compiled in.
    ///
    /// A read port for the boot-seam suite (D-165/D-167), like
    /// [`Booted::llm`]. `Arc`, so this is the same engine the provider, the
    /// idle timer and the broker hold.
    #[cfg(feature = "local-inference")]
    pub local_engine: Arc<crate::inference::engine::LocalEngine>,
    /// The one `Llm` handle, the same one `Services` carries.
    ///
    /// Exposed for the boot-seam suite (D-165): the local-provider seam is the
    /// one whose loss was proven invisible, and it lives on this object. Every
    /// other consumer reaches it through `Services`, so this field is a read
    /// port rather than a second construction path.
    pub llm: Arc<crate::llm::Llm>,
    /// The embedding service, exposed as a read port for the same reason `llm`
    /// is: its local engine is a seam whose absence is silent everywhere else,
    /// because every caller of `Embedding::embed` degrades on failure by design.
    pub embedding: crate::memories::Embedding,
    /// `None` when `behindProxy` is set: a terminating proxy owns TLS and the
    /// core has no certificate of its own to pin.
    pub tls: Option<TlsMaterial>,
    /// The identity the embedded frontend makes every in-process request as.
    /// `None` for [`Frontend::Service`], which has no such client.
    ///
    /// Filled here rather than in `loca-core-ffi` because `Booted` exposes no
    /// `Auth` (it moves into `Services`), so the alternative is the ffi crate
    /// building a SECOND `Auth`. That would work today, since
    /// `ensure_local_client` is a pure upsert touching none of `Auth`'s stateful
    /// parts (the rate limiters, the ticket table, the in-flight PAKE sessions),
    /// but its safety would rest on nobody ever calling anything else on it.
    /// This file warns against a second instance of a handle four times over;
    /// one line here makes it structural instead.
    pub local_client: Option<ClientIdentity>,
}

/// Which frontend is booting this core.
///
/// A VALUE, not `#[cfg(any(target_os = "android", target_os = "ios"))]`, for the
/// reason `mcp/stdio.rs` already gives for `StdioSupport`: a `cfg` would leave
/// the desktop suite as the only thing that ever compiles the embedded arms, so
/// the mobile gap would be invisible on the machine where the code is written.
/// That is exactly how D-117 happened. As a value, a desktop test boots
/// `Embedded` and asserts what it skipped.
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

/// Construct every subsystem, for `frontend`.
///
/// [`Frontend::Embedded`] differs in exactly three places, each marked below,
/// and each is a thing that would be WRONG rather than merely unused on a core
/// with no listener.
pub async fn boot_with(config: Config, paths: Paths, frontend: Frontend) -> Result<Booted> {
    boot_with_store(config, paths, frontend, None).await
}

/// [`boot_with`], with the vault's credential store injectable.
///
/// `None` is the production path and keeps `Vault::new` verbatim. The seam
/// exists because a `Service` boot runs `tls::ensure` and then `vault.set`,
/// which writes through to the master key and on a developer machine means a
/// REAL entry in the login keychain; `LOCA_VAULT_FILE_STORE` cannot help,
/// because it is read inside `select()` and `set_var` is unsound in a threaded
/// test binary (`secrets/store.rs:242-244`). Without this, the one structural
/// guard on the reserved local client (that a desktop boot must NOT create it)
/// had no test and its mutation survived everything (M1 review, C1).
///
/// `Option` rather than a required store on purpose: `Vault::new` IS
/// `Vault::with_store(paths.secrets_enc_file(), store::select(root, channel))`,
/// so requiring one would make this function re-derive that pair and put a
/// second copy of it here. That is the D-049 shape.
///
/// Always compiled, never `#[cfg(feature = "in-memory")]`: gating it would mean
/// the shipped build compiles a different `boot_with` body than the tested one,
/// which is the D-117 family.
pub async fn boot_with_store(
    config: Config,
    paths: Paths,
    frontend: Frontend,
    store: Option<Arc<dyn crate::secrets::SecureStore>>,
) -> Result<Booted> {
    paths.ensure_dirs()?;

    // Refused at boot rather than tolerated, because the alternative is a core
    // that runs with its sandbox silently disarmed. Deno's `--deny-read` /
    // `--deny-write` are comma-separated with no escape, so a single comma
    // anywhere above the core root truncates the WHOLE deny list, not one entry
    // of it: the vault, the master key, the admin token and password, and the
    // SQLite database all stop being denied to every tool worker at once, while
    // a tool granted `$home` still holds read over the directory holding them.
    //
    // Naming the offending path matters here: the cause is a directory name the
    // operator chose (a home directory with a comma in it, or a comma in
    // `LOCA_CORE_HOME`), and it is fixable in one move once said out loud.
    // Nothing else in the core would ever mention it.
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
    // Built before `Auth` because `Auth` takes the revocation hooks at
    // construction and both of them need these two. Neither depends on anything
    // above, so moving them up costs nothing.
    let frames = Arc::new(FrameBus::new());
    let sessions = SessionsStore::new(Arc::clone(&paths));

    // **Revocation has to actually revoke.** A WebSocket authenticates ONCE, at
    // upgrade (`frame_bus.rs:185`), so without `close_sockets` a device whose
    // access was just removed keeps a working, streaming connection until it
    // happens to disconnect on its own. The oracle closes it inline in the
    // route and says why (`http/routes/pairing.ts:104`); the port moved that to
    // a hook and then never filled it, so every hook was still the no-op
    // default and revoking removed a database row and nothing else.
    //
    // `drop_settings_cache` is deliberately left at its default and that is NOT
    // an omission: the port has no per-client settings cache at all
    // (`load_overlay` reads `client_settings` on every call) and those rows
    // cascade with the `clients` row, so there is nothing to invalidate. The
    // field's "owed by D1" doc is stale rather than outstanding.
    let auth = Arc::new(Auth::new(db.clone(), Arc::clone(&paths)).with_hooks(
        crate::auth::RevocationHooks {
            gc_sessions: {
                let sessions = sessions.clone();
                Arc::new(move |client_id: String| {
                    let sessions = sessions.clone();
                    Box::pin(async move {
                        // Sessions are JSON files with no foreign key to
                        // cascade, so nothing else reclaims them: without this
                        // a revoked client's transcripts and attachments stay
                        // on disk forever.
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
    // The embedded frontend makes every in-process request as one reserved
    // client. Minted here, from the `Auth` boot already has, rather than in
    // `loca-core-ffi`: see the field's doc on `Booted`.
    let local_client = match frontend {
        Frontend::Embedded => Some(crate::auth::to_identity(auth.ensure_local_client().await?)),
        Frontend::Service => None,
    };
    // The two lines are ordered: the aggregate pulls its subsystem list from
    // the registry, and `attach_status` is what makes a subsystem change
    // recompute it. Without the second line the core status only moves when
    // some other signal pushes.
    let sidecars = Arc::new(Manager::new());
    let status = Arc::new(CoreStatusService::new(
        Arc::clone(&sidecars) as Arc<dyn SidecarSource>
    ));
    sidecars.attach_status((*status).clone());
    // Hoisted out of the context literal: the settings store broadcasts through
    // the same bus the transport serves from, and exactly one store exists.
    // Two would mean two caches and two locks over one file, which is the lost
    // update the write lock exists to prevent.
    let settings = crate::settings::Settings::new(&paths, db.clone(), Arc::clone(&frames));

    // `server.bindHost` and `server.behindProxy` are read RAW from settings.json,
    // never through the settings API: a paired client must not be able to widen
    // network exposure or weaken the pairing posture of the core it is paired
    // to. Both have an env var that wins, and the two precedence rules differ,
    // which is easy to get wrong in a port:
    //
    //   LOCA_CORE_HOST           wins when set to a NON-EMPTY value.
    //   LOCA_CORE_BEHIND_PROXY   wins whenever DEFINED, so `=0` means false AND
    //                            suppresses the file read. It accepts "1" or a
    //                            case-insensitive "true".
    //
    // The raw read lives HERE rather than in the settings module, on purpose:
    // these two keys must not be reachable through the settings service at all,
    // and keeping the only reader in boot is what makes that structural rather
    // than a rule someone has to remember.
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

    // **Embedded difference 1 of 3: no TLS material.**
    //
    // An embedded core has no listener to present a certificate on and no
    // pairing handshake to bind its pin. Generating one anyway would seal a key
    // in the vault, which is a real keychain write on the device, and leave a
    // certificate on disk that only misleads whoever finds it. `Booted::serve`
    // already refuses without TLS, which is the right answer here rather than a
    // gap.
    let tls = if behind_proxy || frontend.is_embedded() {
        None
    } else {
        let sealed = vault.get(tls::KEY_SECRET).await?;
        let (material, new_jwk) = tls::ensure(sealed.as_deref(), &config.host)?;
        if let Some(jwk) = new_jwk {
            vault.set(tls::KEY_SECRET, &jwk).await?;
        }
        // The public half goes beside the core root so a co-located tool can
        // trust this listener as a CA of one. The conformance harness also
        // reads it, to derive the pin independently of whatever the core
        // reports, which is the only version of that check worth having.
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
        // Derived from `frontend` plus the compile-time feature facts, in the
        // one place that reads them. Passing `frontend` through rather than
        // re-deriving from `tls_pin.is_none()` is deliberate: that expression is
        // `behind_proxy || frontend.is_embedded()`, so a PROXIED DESKTOP core
        // would report itself unable to pair, schedule or update.
        capabilities: crate::transport::health::Capabilities::detect(frontend),
        tls_pin: tls.as_ref().map(|t| t.pin.clone()),
        started_at: std::time::Instant::now(),
    };

    // Exactly one store, handed to every package that touches a session (D2's
    // routes, D4 chat, D9 storage, D10 scheduled prompts). It owns the
    // temporary-session map and the per-session lock table, so a second one
    // would hide a temporary session from another route and let two mutations
    // of one `session.json` interleave.
    // A no-op on a clean tree. An attachment directory whose session is gone is
    // swept before the listener binds, so no request can observe the half state.
    sessions.sweep_orphaned_dirs().await;

    // Downloads, built before models because models takes the seam.
    //
    // Exactly one `Net`. It owns the only `reqwest` clients in the download
    // path and counts its own calls, which is what the consent tests assert on;
    // a second instance would answer from a counter nothing checks. One caveat
    // worth knowing rather than discovering: `catalog::load` fetches
    // `catalog.json` through `reqwest` directly rather than through this
    // chokepoint, so a call counted there is invisible here. That fetch is
    // still behind a user action, so it is inside the consent rule, but
    // `Net::calls()` is not a total count of the core's outbound requests.
    let net = Arc::new(crate::downloads::net::Net::new()?);
    let queue = {
        // The catalog supplies the pinned sha256 a transfer verifies against.
        // Wiring the stub here instead would leave a download path that looks
        // like it works and silently skips its integrity check.
        //
        // This is a SECOND `Models`, and the cycle is why: the real one below
        // takes the download seam, which needs this queue, which needs a
        // catalog. Two instances of a handle is normally the bug this file
        // warns about, because the second answers from state the rest of the
        // core cannot see. It is safe here for a structural reason rather than
        // a promise: `Models`' only mutable state is its hardware cache, and
        // this instance is erased to `Arc<dyn CatalogHashes>` the moment it is
        // built. That trait has one method, `catalog_sha256`, so nothing can
        // reach the cache through it even by mistake. The catalog itself is
        // read from disk and re-read on demand, so there is no second copy of
        // it to go stale.
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
    // Recovers rows a previous run left mid-flight AND primes the snapshot
    // cache, so the first `models_snapshot` after a restart reports the
    // persisted queue rather than an empty one.
    //
    // `start`, not `normalize`: normalising alone settles the table against the
    // disk and then leaves every recovered row with no worker behind it, which
    // is the "Downloading" bar that never moves and that only cancel can clear.
    queue.start().await?;

    // The models handle is also D9's `ModelLocks`, so wiring it here is what
    // stops `GET /api/v1/storage` reporting every model folder as unlocked and
    // offering to delete a model the settings still require.
    // Built before `models` so both dep structs can hold it; filled after
    // `requirements` exists. See `DeferredRequirements`.
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

    // Download activity has two consumers, and both were built and then left
    // unwired: `note_downloads` had no caller outside its own tests, and the
    // contract's `downloads.snapshot` frame was never broadcast by anything. So
    // the core bar read Idle straight through a running download and no client
    // ever saw the queue move. Mirrors `main.ts:158-159` and `ws/hub.ts:454-455`.
    //
    // NOT wired here: the requirements recompute the oracle drives off this same
    // subscription (`hub.ts:456-470`). It carries a load-bearing guard against
    // the download-vs-present race, and it belongs to D8 with the rest of
    // requirements. Adding the broadcast without the guard would reintroduce the
    // race, so D8 owns extending this loop, not duplicating it.
    // One `Arc<dyn LocalSpeech>`, shared by the route slice and the module
    // broker's speech seam. Two would be two local engines with two queues, so
    // two callers would each believe they had the engine to themselves.
    //
    // Built HERE rather than beside `speech_voices` below, because the
    // reconciler has to be in scope for the download subscription that follows.
    // No `#[cfg]`: the feature is handled inside `build`, which returns
    // `(Arc::new(Unavailable), None)` with `local-speech` off and costs nothing.
    let (speech, speech_reconciler) = crate::speech::local::build(
        ctx.settings.clone(),
        ctx.paths.root().to_path_buf(),
        ctx.paths.models_dir().to_path_buf(),
        Arc::clone(&sidecars),
        (*status).clone(),
    );

    {
        let mut changes = queue.subscribe();
        let status = Arc::clone(&status);
        let frames = Arc::clone(&ctx.frames);
        let speech_downloads = speech_reconciler.clone();
        // Prime both consumers before the first change. A core that boots with
        // rows already in the table otherwise reports Idle, and shows an empty
        // queue, until something happens to move it.
        let initial = queue.snapshot().await.unwrap_or_default();
        status.note_downloads(&initial);
        // NOT optional, and the half that is easy to drop. Download rows survive
        // restarts, so without this the first broadcast after boot reads every
        // persisted `Completed` row as a FRESH completion and reloads a
        // multi-gigabyte model the moment anything touches the queue.
        // `sidecar-boot.ts:140-142` seeds for exactly this reason.
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
                        if let Some(reconciler) = &speech_downloads {
                            // NOT awaited. `note_downloads` chains into a model
                            // load, and awaiting it here freezes this consumer:
                            // for the whole load the loop makes no further
                            // `status.note_downloads` call and broadcasts no
                            // snapshot, so finishing a speech model download is
                            // exactly the moment the client's download progress
                            // and the core status bar stop updating. The Deno
                            // core fires this the same way and says why
                            // (`sidecar-boot.ts:118-121`, "awaiting it here
                            // would stall every settings PATCH"); the port
                            // inherited the structure without the `void`.
                            //
                            // Safe to run concurrently with itself: the
                            // last-seen map is behind a `Mutex` and the edge
                            // detection happens under it, so a second snapshot
                            // arriving mid-load re-detects nothing. The
                            // reconcile it triggers serialises on the engine
                            // slots.
                            let reconciler = Arc::clone(reconciler);
                            let entries = Arc::clone(&entries);
                            tokio::spawn(async move {
                                reconciler.note_downloads(&entries).await;
                            });
                        }
                    }
                    // A slow consumer that fell behind has missed rows, but the
                    // next send carries the whole snapshot, so recovering is
                    // just carrying on. Breaking here (what `while let Ok` does)
                    // would silently stop every future update for the life of
                    // the process.
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(target: "loca::downloads", "status feed lagged {n}");
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    // Bound rather than built inline in `Services` below, because boot has to
    // call `reconcile_interrupted_installs` on it and the literal moves it.
    //
    // That call is not optional and its order is load-bearing:
    // `queue.start()` deliberately skips `binaries` rows (a binaries row carries
    // no URL, so it is not resumable the way a model row is), which makes this
    // the ONLY thing that clears one left behind by a killed core. Without it
    // the core bar sticks on "Downloading" forever with no worker behind it.
    // Matches `main.ts:152-153`.
    let binaries = crate::downloads::binaries::installer::BinariesInstaller::new(
        Arc::clone(&net),
        queue,
        (*ctx.paths).clone(),
        ctx.config.channel,
        &ctx.config.binary_manifest_url(),
    );
    binaries.reconcile_interrupted_installs().await;

    // One `Mcp`, and the reason it is built here rather than in the router is
    // the lifecycle: it owns live connections and stdio children, so it needs a
    // `start()` after boot and a `shutdown()` before the listener closes, and a
    // value constructed inside `build` is reachable from neither.
    //
    // **Embedded difference 2 of 3: no stdio MCP.** A stdio server is a spawned
    // child running a downloaded interpreter, which is what Android's W^X policy
    // and App Store rule 2.5.2 forbid. The routes still serve and `plan` refuses
    // with a reason the user can act on ("Use a remote server instead"), which
    // is the honest shape rather than a spawn that fails mysteriously.
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

    // One `Llm`, built before `Services` because chat needs the same handle the
    // routes get. Two would mean two schedulers over one llama-server, and the
    // semaphore that keeps a local model from being asked for two turns at once
    // would be sized against a queue it could not see.
    let llm = Arc::new(crate::llm::Llm::new(ctx.vault.clone(), ctx.config.channel));
    llm.attach_status((*status).clone());

    // The in-process llama.cpp engine, and the three seams it fills. N2 landed
    // all of it with 90 default-gate and 134 feature-on tests and had no way to
    // reach production from its own module: `set_local_provider` had exactly two
    // references, its own definition and one inside `mod tests`. Without these
    // lines a local turn resolves `HttpProvider` and talks to a sidecar port
    // that Stage 3 removed, so the engine would have shipped complete, tested,
    // and never once executed.
    //
    // Sixth instance of the family (D-134, D-138, D-141, and the two before it),
    // and the first one the implementing package predicted in writing rather
    // than the orchestrator finding afterwards. N2's `blocked-on` said "until it
    // lands the engine is unreachable", which is why this is a wiring task and
    // not an incident.
    #[cfg(feature = "local-inference")]
    let local_engine = {
        let engine = Arc::new(crate::inference::engine::LocalEngine::new(
            ctx.settings.clone(),
            (*ctx.paths).clone(),
        ));
        llm.set_local_provider(Arc::new(crate::inference::provider::LocalProvider::new(
            Arc::clone(&engine),
        )));
        // The scheduler's stall watchdog unloads the model rather than only
        // logging: a wedged local turn otherwise holds the weights resident.
        llm.set_watchdog_handler(Some(crate::inference::idle::IdleUnload::watchdog_handler(
            Arc::clone(&engine),
        )));
        // Closes Q8 (D-167). Nothing published `SidecarKind::Llama`, so
        // `LlmLoading::still_loading` read `Disabled` and answered false for a
        // model that was mid-load: greetings and scheduled prompts fired into
        // the load and errored instead of waiting for it.
        engine.attach_status(Arc::clone(&sidecars));
        engine
    };

    let memories = crate::memories::MemoriesStore::new(Arc::clone(&ctx.paths), ctx.db.clone());

    // The local embedder, which was missing entirely until D-175. The oracle
    // runs a second `llama-server` on MiniLM (`sidecars/llama-embed.ts`); the
    // port had the trait, the model identity and `NoLocalEmbedder` and no
    // implementation, so memory RAG and tool-relevance phase 1 degraded on every
    // platform with nothing reporting it.
    //
    // NOT gated on `llm.provider`, unlike the chat engine above. Embeddings are
    // always-on infrastructure (the oracle's header says exactly that): a user
    // answering from an external provider still wants their memories injected,
    // and that is precisely the configuration where the chat engine is unloaded.
    //
    // Off a `local-inference` build there is genuinely no in-process engine, so
    // `NoLocalEmbedder` is the CORRECT implementation there rather than a stub:
    // relevance falls back to the external endpoint or to no filtering, which is
    // what a core without llama.cpp linked can actually do.
    #[cfg(feature = "local-inference")]
    let local_embedder: Arc<dyn crate::memories::LocalEmbedder> = Arc::new(
        crate::inference::embed::LlamaEmbedder::new((*ctx.paths).clone()),
    );
    #[cfg(not(feature = "local-inference"))]
    let local_embedder: Arc<dyn crate::memories::LocalEmbedder> =
        Arc::new(crate::memories::NoLocalEmbedder);

    let embedding =
        crate::memories::Embedding::new(ctx.settings.clone(), ctx.vault.clone(), local_embedder);
    // Chat takes `embedding` by value, and the extension `embed` route needs the
    // same configuration, so the clone is taken before the move rather than a
    // second `Embedding` being built from scratch.
    let tool_embedding = embedding.clone();
    // A third handle, for `Booted`'s read port. All three share one `Inner`, so
    // this is a clone of a pointer rather than a second service.
    let embedding_port = embedding.clone();

    // The voice list comes from the CATALOG, not the engine (D-039), which is
    // what makes the dropdown work on a core with no local speech engine at all,
    // which is every build with `local-speech` off and every mobile build.
    let speech_voices: Arc<dyn crate::speech::TtsVoiceSource> =
        Arc::new(crate::models::speech::CatalogVoices::new(models.clone()));

    // The scheduler, built BEFORE `Chat` because there is a genuine cycle:
    // `Chat` needs the scheduler to persist a confirmed draft, and the scheduler
    // needs `Chat` to open an automated session. The TypeScript resolves it with
    // lazy module singletons, which the port does not have, so it is broken by
    // `attach_automated` below: a set-once `OnceLock`, the same shape as
    // `attach_status` and boot's own `DeferredRequirements`.
    //
    // A scheduler whose runner was never attached REFUSES rather than silently
    // no-opping, for the same reason `NoSchedule` refuses.
    let scheduled = crate::scheduled::Scheduler::new(
        ctx.db.clone(),
        crate::scheduled::deps::SchedulerDeps::unwired().with_loading(Arc::new(
            crate::chat::LlmLoading::new(ctx.settings.clone(), Arc::clone(&sidecars)),
        )),
    );

    // Chat. The schedule seam is real (D10) and the idle seam is real whenever
    // `local-inference` is on, because idle-unload belongs to the local model
    // and with the feature off there is no local model to unload. `NoTools` is
    // the one still absent, and it is an honest absence rather than a
    // placeholder: the extension supervisor has no `WorkerSpawner` yet, so there
    // is nothing to run a tool WITH even though its routes serve. It degrades a
    // feature rather than faking it.
    // The tool seam, filled after the extension and MCP subsystems exist. See
    // `DeferredTools`: chat needs a tool host and the extension supervisor needs
    // chat, so one of the two has to be handed a shim.
    //
    // **Without this the model is offered NO tools at all** - not extension
    // tools, not MCP tools - because `ChatDeps::unwired` installs `NoTools`,
    // whose every catalog read is empty. It fails silently: a turn runs, the
    // model simply never sees a tool, and `/api/v1/tools` still lists them all
    // because that route reads the subsystems directly rather than through this
    // seam. So the client shows a full catalog and nothing is callable.
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

    // Without this line the whole chat module is unreachable: the hub parses a
    // `chat.start` frame, finds no inbound handler, and DROPS it. Every route in
    // the module can be perfect and no client can start a turn.
    ctx.frames.on_inbound(Arc::new(chat.clone()));

    // Closes the scheduler's half of the cycle. Without it every due tick logs
    // an error and `runNow` answers `server_unavailable`, which is the honest
    // shape of "no runner" rather than a silent no-op.
    scheduled.attach_automated(Arc::new(crate::scheduled::ChatAutomated::new(chat.clone())));

    // The extension subsystem.
    //
    // The indexer is the SAME `chat` handle `Services::memory_indexer` carries,
    // and that is load-bearing rather than tidy: a second one would summarise a
    // tool's write from a queue that does not gate on turn activity, which is
    // the thing D5's `MemoryIndexer` seam exists to prevent.
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

    // A second `SpeechService` over the SAME `Manager` and the SAME
    // `Arc<dyn LocalSpeech>` the route slice gets. Safe by its own design rather
    // than by luck: the service holds no state, and the two things that must be
    // single per core (the subsystem registry and the local engine with its
    // queue) are the arguments, not the instance.
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
        // Warms the local model before a tool's `llm` call, so an extension's
        // completion does not race a cold load. `NoModelToLoad` is correct with
        // the feature off: there is no local model, and the completion then
        // succeeds or fails on the endpoint alone.
        #[cfg(feature = "local-inference")]
        (Arc::clone(&local_engine) as Arc<dyn crate::extensions::broker::EnsureModelLoaded>),
        #[cfg(not(feature = "local-inference"))]
        Arc::new(crate::llm::NoModelToLoad),
    ));

    // The warm-worker pool. `.pool(...)` is what makes `refresh_permissions`
    // real: without it a grant the user just revoked stays baked into a warm
    // `deno` process until it idles out five minutes later, because the sandbox
    // flags are decided at spawn time and cannot be renegotiated.
    //
    // The spawner takes the INSTALLER's `Registry` clone, not a second one
    // (D-092): clones share the verification cache, and two independent caches
    // would let a tool keep passing its content-hash gate on state that had been
    // revoked.
    //
    // **Desktop only, and the gate is not incidental.** `worker::spawner` is
    // itself `#[cfg(not(android/ios))]` because it starts a DOWNLOADED
    // interpreter, which is exactly what Android's W^X policy and App Store rule
    // 2.5.2 forbid. A mobile core therefore has no pool, which is the same state
    // the builder documents as "no warm workers to refresh" and is correct
    // rather than degraded: it has no workers at all.
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
    // Applied only when there IS one, so a mobile core takes the builder's
    // documented no-pool path rather than carrying a stub that would report warm
    // workers it cannot have.
    if let Some(pool) = pool.clone() {
        extensions_builder = extensions_builder.pool(pool);
    }
    let extensions = extensions_builder.build();

    // The subsystem control seam: what the client's Retry button in the Services
    // field calls on a row stuck in `Error`. Re-homed onto the in-process
    // engines the sidecars became, since there is no child to restart now.
    //
    // Absent, `POST /api/v1/sidecars/{kind}/restart` answers a validation error
    // instead of silently succeeding: a human pressed Retry, and a 204 that did
    // nothing is worse than an error that says so.
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
                    // Dropping the weights IS the restart: `ensure_loaded` runs
                    // at the top of every turn and reloads from the current
                    // settings, which is what the oracle's "recompute the start
                    // args and re-start" amounts to without a process.
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
                    "{} is not a controllable subsystem",
                    crate::contract::api::SidecarKind::from(&other)
                ))),
            }
        }
    }

    sidecars.attach_control(Arc::new(InProcessControl {
        #[cfg(feature = "local-inference")]
        engine: Arc::clone(&local_engine),
        speech: speech_reconciler.clone(),
    }));

    // Closes the tool seam, and with it the fourth construction cycle. From
    // here the model can actually be offered extension and MCP tools; before
    // this line `DeferredTools` answered every catalog read empty.
    //
    // `HostedTools` is the one type that knows about extensions, MCP and the
    // LLM at once, which is why it is orchestrator-owned (`tool_host.rs`) and
    // why nothing else could have wired it.
    // Read here, while `chat` is still in scope: it moves into `Services` below.
    // Read AFTER the set, obviously, since that is the line under test.
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

    // Hide an extension's memories from every listing surface unless it is
    // installed: a downloaded-only, drifted or deleted extension is not active,
    // mirroring how its tools are gated (`main.ts:212`). Live, so install,
    // uninstall and drift take effect with no extra bookkeeping. The base-dir
    // mapping stays for every extension, so content still resolves after a
    // later install.
    //
    // Fourth D-138 gap, and the only one that was not merely a missing call: the
    // gate was declared sync against a registry the port had made a database, so
    // D12 could not implement it even though the trait's own doc assigned it. See
    // D-141. Unlike the other three, leaving this line out is now DETECTABLE:
    // `an_uninstalled_extensions_memories_leave_every_listing` fails with no gate
    // installed.
    memories.set_provider_gate(Some(Arc::new(
        crate::extensions::ExtensionProviderGate::new(extension_registry.clone()),
    )));

    // The extensions Loca ships, on a fresh install: the built-in everywhere,
    // plus the dev-only samples showcase on the dev channel. Awaits only the
    // DECISION (a few stats and one registry read per extension); the install
    // itself is one of the installer's own background jobs, exactly as the
    // oracle's `seedExtensionsIfNeeded` is (`main.ts:186`). Never fetches: in
    // production it installs only from the install-script-planted artefacts and
    // otherwise defers to the user-gated install.
    //
    // This call is the whole of D-134. `seed.rs` was ported complete and had no
    // production caller for as long as it existed, so a fresh Rust core shipped
    // with NO built-in extension: the baseline toolkit every user gets, absent,
    // silently, with every extension route answering correctly for a core that
    // has none. D12 could not have caught it, because the missing caller was
    // here rather than in its module.
    crate::extensions::install::seed::seed_extensions_if_needed(extensions.installer()).await;

    // Content-hash drift, in the BACKGROUND and deliberately not awaited: a
    // large extension takes seconds to walk and boot must not wait for it. A
    // drifted extension is flipped to `drift` and has its tools disabled, so the
    // chat-exposure gate blocks them until the user confirms a re-enable.
    //
    // The second D-134 instance, found by the first one's remediation and in the
    // same file for the same reason. Its absence is the one with teeth: without
    // it, an extension whose bytes changed under the core keeps running its
    // tools, which is precisely the check the content hash exists to be.
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

    // Requirements and greetings. Both are built ONCE and both must be:
    // `Requirements` owns the coalescing recompute worker and `Greetings` owns
    // the per-client dedup map, so a second `Greetings` would let one client
    // greet twice per window.
    let requirements = crate::requirements::Requirements::new(
        ctx.settings.clone(),
        models.clone(),
        Arc::new(binaries.clone()) as Arc<dyn crate::requirements::deps::Binaries>,
        Arc::clone(&ctx.frames),
    );
    // Closes the cycle. Until this line the four `deps.requirements.changed()`
    // call sites in `models/routes.rs` and `storage/mod.rs` run and do nothing,
    // which is the failure mode that reads as wired.
    let _ = deferred_requirements.0.set(requirements.clone());
    let greetings = crate::requirements::Greetings::new(
        chat.clone(),
        Arc::new(crate::chat::LlmLoading::new(
            ctx.settings.clone(),
            Arc::clone(&sidecars),
        )),
    );

    let services = Services {
        auth,
        memories,
        // The same `Chat`: it owns the background queue, so the indexer and the
        // chat engine must be one object or memories would be summarised by a
        // queue nothing gates on turn activity.
        memory_indexer: Arc::new(chat.clone()),
        sessions,
        chat: Arc::new(chat),
        storage: StorageDeps::unwired()
            .with_models(Arc::new(models.clone()))
            .with_requirements(Arc::clone(&deferred_requirements)
                as Arc<dyn crate::storage::deps::RequirementsNotifier>),
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
        // The SAME aggregate the health route reads and the sidecars feed. The
        // update slice writes to it (`note_update_staged`), so a second one here
        // would mean the bar a client polls and the bar an update moves are
        // different objects.
        status: Arc::clone(&status),
    };

    // After the router is built, so a server that answers immediately cannot
    // race a route that is not mounted yet. Reconnecting to a server the user
    // explicitly enabled is inside the no-non-consented-network rule; see
    // D-032, and this is the one call site if that reading changes.
    mcp.start();
    // After the router, for the same reason `mcp.start()` is: a timer that fires
    // before the routes exist would open an automated session the client cannot
    // then fetch.
    //
    // **Embedded difference 3 of 3: the arm loop does not start.** The scheduler
    // is the only subsystem that STARTS work rather than serving it, and
    // scheduled prompts are out of scope on mobile (they would need the app in
    // the foreground to fire at all). The routes still serve, so a prompt
    // already in the table is readable and deletable rather than invisible.
    //
    // Note `mcp.start()` above is deliberately NOT skipped: it reconnects to
    // remote MCP servers the user explicitly enabled, which is inside the
    // network rule per D-032, and remote MCP is exactly the half that still
    // works on a phone.
    if !frontend.is_embedded() {
        scheduled.start();
    }

    // After the router, like `mcp.start()` and the scheduler: the initial
    // reconcile can load a model, and a core that boots with speech already on
    // should not do that before the routes that report it exist.
    //
    // `spawn_settings_watch` owns its own `Settings::subscribe()` receiver and
    // the `SPEECH_KEYS` filter, so boot carries no key list. Not skipped on
    // embedded: local speech is IN scope on mobile (sherpa ships Android and iOS
    // builds), unlike extensions and stdio MCP.
    if let Some(reconciler) = &speech_reconciler {
        reconciler.spawn_settings_watch();
    }

    // Same shape, same reason: the pool's three tunables are settings, and until
    // they were wired the panel offered them and nothing read them. The watch
    // owns its own subscription and its own key list (`POOL_KEYS`), so this call
    // stays one line and cannot drift from what the reader uses.
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    if let Some(pool) = &pool {
        pool.spawn_settings_watch(ctx.settings.clone());
    }

    // An embedded core never calls `serve()`, so it would never reach the
    // `note_boot_done()` there and `booting` would stay true forever. `derive`
    // short-circuits on `booting`, so the mobile Core status bar would read
    // "Starting up" for the life of the app and never reach Busy, Downloading or
    // Updating. Found by M1's review (C3), which is the converse of the question
    // the three `Embedded difference` markers answer: those are things that must
    // NOT run with no listener, and this is a thing that MUST.
    //
    // Desktop keeps its call in `serve()` on purpose: there, "boot done" means
    // the listener is bound, and moving it here would report a core ready before
    // anything could reach it.
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
        // Commit a pending self-update, HERE and not earlier: a bound listener
        // implies the database opened and the TLS key unsealed, which is the
        // checkpoint `commit_update`'s own doc names. Committing at a fixed
        // uptime instead would let an ordinary restart of a working binary trip
        // a rollback.
        //
        // Third instance of D-134, and the one with the worst failure mode. With
        // no caller the marker was never deleted, so `attempts` incremented on
        // every boot until the core rolled back a PERFECTLY HEALTHY update. Self
        // update would have appeared to work and then spontaneously reverted,
        // and nothing in D13's suite could see it: the function is correct and
        // fully tested, and only the call was missing.
        crate::update::rollback::commit_update(&ctx.config, &ctx.paths).await;
        // A prefix, not the whole pin. The pin is public (clients pin it, and
        // it is shown during pairing), so redacting it would be wrong, but a
        // full base64 SHA-256 is the same shape as a sealed key and the log
        // scrubber cannot tell them apart. Twelve characters is enough to see
        // that the pin changed, which is the only thing this line is for.
        let pin = ctx.tls_pin.as_deref().map_or("none".to_string(), |p| {
            format!("{}...", p.chars().take(12).collect::<String>())
        });
        tracing::info!(target: "loca::boot", "listening on https://{addr} (pin={pin})");
        self.core.serve_bound(listener).await;
        Ok(())
    }
}

/// The boot-seam suite (D-165): a real boot, interrogated for the wiring that
/// no per-package test can see. Lives in its own file because it is the one
/// place in the crate that asserts about `boot.rs` as a whole.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod seams;
