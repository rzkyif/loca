//! The router: every route the core serves, assembled once.
//!
//! **Orchestrator-owned.** A package writes its own `<module>/routes.rs` and
//! files a mount request; it does not edit this file.
//!
//! One router, not two. The TypeScript split its routes across two apps because
//! the engine was extracted behind a runtime abstraction that the port deletes,
//! so there is nothing left for the split to express.
//!
//! # Mounting a slice
//!
//! A slice is a free function returning a `Router` with fully qualified paths
//! and its own auth layer already applied, so mounting it is one line that
//! carries no path string:
//!
//! ```ignore
//! // packages/loca-core-rs/src/settings/routes.rs
//! pub fn routes(ctx: &AppContext) -> Router {
//!     Router::new()
//!         .route("/api/v1/settings", get(list).patch(patch))
//!         .layer(from_fn_with_state(ctx.clone(), require_client))
//!         .with_state(ctx.clone())
//! }
//! ```
//!
//! The slice owning its prefixes rather than being nested under one is what
//! lets a package that serves two of them (models and binaries; requirements
//! and greetings) still mount in a single line, and it means the orchestrator
//! cannot mis-mount a slice by mistyping a prefix.
//!
//! Overlapping routes panic when this function runs, and it runs in a test, so
//! a duplicate mount is a failing build rather than a route that silently
//! shadows another.

use std::sync::Arc;

use axum::Router;

use super::middleware::{body_limit, cors, from_fn, from_fn_with_state, require_client};
use super::state::AppContext;
use super::{health, http, ws};
use crate::auth::Auth;
use crate::sessions::{ChatEngine, SessionsStore, UnavailableChat};
use crate::sidecars::Manager;
use crate::storage::deps::StorageDeps;
use crate::transport::status::CoreStatusService;

/// The service handles a route slice needs that [`AppContext`] does not carry.
///
/// A handle lands here when it is a concrete type a slice needs by value (the
/// pairing slice needs `Auth` itself, not the `dyn ClientAuth` in `ctx`, and a
/// trait object cannot be narrowed back), or when exactly one instance must
/// exist process-wide. The second case is the load-bearing one: `Auth` owns the
/// handshake map and the rate-limit buckets, `SessionsStore` owns the
/// temporary-session map and the per-session lock table, and `Manager` owns the
/// subsystem registry. A second instance of any of them answers from state the
/// rest of the core cannot see, which is a lost update or a ticket that is
/// never spendable rather than a compile error.
///
/// Boot writes the literal in full and so does [`Services::unwired`], on
/// purpose: adding a field breaks both, which is what stops a package from
/// landing with its seam silently left at the stub.
pub struct Services {
    pub auth: Arc<Auth>,
    pub memories: crate::memories::MemoriesStore,
    pub memory_indexer: Arc<dyn crate::memories::MemoryIndexer>,
    pub sessions: SessionsStore,
    pub chat: Arc<dyn ChatEngine>,
    pub storage: StorageDeps,
    pub sidecars: Arc<Manager>,
    pub speech: Arc<dyn crate::speech::LocalSpeech>,
    /// The voice list, separate from the engine because it is a catalog fact and
    /// must fill on a core with no local speech engine at all (D-039).
    pub speech_voices: Arc<dyn crate::speech::TtsVoiceSource>,
    pub llm: Arc<crate::llm::Llm>,
    pub models: crate::models::Models,
    /// The binary installer, for the `/api/v1/binaries` slice.
    ///
    /// Threaded rather than built here for the same reason as `mcp`: it sits on
    /// a `DownloadQueue` whose recovery pass has to run at boot, before any
    /// request can observe a half-written row.
    pub binaries: crate::downloads::binaries::installer::BinariesInstaller,
    /// Threaded rather than built here, and the reason is the lifecycle rather
    /// than the sharing. `Mcp` owns live connections to remote servers and
    /// child processes: something has to call `start()` after boot and
    /// `shutdown()` before the listener closes, and a value constructed inside
    /// `build` is reachable from neither.
    pub mcp: crate::mcp::Mcp,
    /// The extension subsystem. Serves its own 21 routes plus the flat
    /// `/api/v1/tools` catalog, which merges extension tools with MCP's.
    pub extensions: crate::extensions::Extensions,
    /// The requirements snapshot and the greeting trigger. Both are single per
    /// core: one owns the coalescing recompute worker, the other the per-client
    /// dedup map that stops a client greeting twice in a window.
    pub requirements: crate::requirements::Requirements,
    pub greetings: crate::requirements::Greetings,
    /// The prompt scheduler. Single per core: it owns the arm loop, and a second
    /// would mean two timers racing to fire the same row.
    pub scheduled: crate::scheduled::Scheduler,
    /// The ONE aggregate status service, threaded here for the update slice.
    ///
    /// `ctx.status` is the same object behind a `dyn StatusProvider`, which is
    /// the read side; the update routes need the concrete handle because they
    /// WRITE (`note_update_staged`). Passing the existing one rather than
    /// building a second is the whole point: two would mean the bar reported by
    /// `/api/v1/health` and the one an update moves are different objects.
    pub status: Arc<CoreStatusService>,
}

impl Services {
    /// Every seam at its stub. What a router test mounts, and the shape boot
    /// starts from before it fills in the handles that exist.
    pub fn unwired(ctx: &AppContext, auth: Arc<Auth>) -> crate::error::Result<Self> {
        // Bound first, not built twice: `mcp` spawns stdio children through the
        // same registry the sidecar routes report from, so two managers here
        // would give a test a core whose children are invisible to its own
        // status route.
        let sidecars = Arc::new(Manager::new());
        let sidecars_for_status = Arc::clone(&sidecars);
        let net = Arc::new(crate::downloads::net::Net::new()?);
        // Hoisted out of the literal: requirements needs the same `Models` and
        // the same binaries installer the `models`/`binaries` fields carry, and
        // greetings needs a concrete `Chat` rather than the `UnavailableChat`
        // trait object the `chat` field holds.
        let models = crate::models::Models::new(ctx, crate::models::deps::ModelsDeps::unwired());
        let binaries = crate::downloads::binaries::installer::BinariesInstaller::new(
            Arc::clone(&net),
            crate::downloads::queue::DownloadQueue::new(
                crate::downloads::store::DownloadStore::new(ctx.db.clone(), ""),
                Arc::new(crate::downloads::transfer::Transfer::new(
                    net,
                    Arc::new(crate::downloads::deps::NoCatalog),
                    "",
                )),
                (*ctx.paths).clone(),
                "",
            ),
            (*ctx.paths).clone(),
            ctx.config.channel,
            "",
        );
        let llm = Arc::new(crate::llm::Llm::new(ctx.vault.clone(), ctx.config.channel));
        let chat = crate::chat::Chat::new(crate::chat::ChatConfig {
            sessions: SessionsStore::new(Arc::clone(&ctx.paths)),
            memories: crate::memories::MemoriesStore::new(Arc::clone(&ctx.paths), ctx.db.clone()),
            embedding: crate::memories::Embedding::new(
                ctx.settings.clone(),
                ctx.vault.clone(),
                Arc::new(crate::memories::NoLocalEmbedder),
            ),
            settings: ctx.settings.clone(),
            llm: (*llm).clone(),
            frames: Arc::clone(&ctx.frames),
            deps: crate::chat::deps::ChatDeps::unwired(),
            summaries: Arc::new(crate::chat::background::LlmSummaries::new(
                (*llm).clone(),
                ctx.settings.clone(),
            )),
        });
        Ok(Self {
            auth,
            memories: crate::memories::MemoriesStore::new(Arc::clone(&ctx.paths), ctx.db.clone()),
            memory_indexer: Arc::new(crate::memories::NoIndexer),
            sessions: SessionsStore::new(Arc::clone(&ctx.paths)),
            chat: Arc::new(UnavailableChat),
            storage: StorageDeps::unwired(),
            sidecars: Arc::clone(&sidecars),
            speech: Arc::new(crate::speech::Unavailable),
            speech_voices: Arc::new(crate::speech::NoVoices),
            llm: Arc::clone(&llm),
            models: models.clone(),
            // Unwired like the rest: a router test needs the slice mounted, not
            // a live download queue. Boot builds the real one.
            binaries: binaries.clone(),
            extensions: crate::extensions::Extensions::builder(
                crate::extensions::install::runner::Installer::new(
                    crate::extensions::install::runner::InstallerConfig {
                        paths: (*ctx.paths).clone(),
                        config: (*ctx.config).clone(),
                        settings: ctx.settings.clone(),
                        registry: crate::extensions::registry::Registry::new(
                            ctx.db.clone(),
                            (*ctx.paths).clone(),
                        ),
                        // A client, not a request: `NoNetwork` is `#[cfg(test)]`,
                        // and constructing this one reaches nothing. Every route
                        // a router test exercises stops short of using it.
                        http: Arc::new(crate::extensions::install::net::HttpClient::new()?),
                        sidecars: Arc::clone(&sidecars),
                        memories: Arc::new(crate::extensions::install::runner::NoMemories),
                        events: Arc::new(crate::extensions::install::runner::SilentEvents),
                    },
                ),
                Arc::new(crate::extensions::install::runner::NoMemories),
                Arc::clone(&ctx.frames),
            )
            .build(),
            // Unwired like the rest: a router test needs the slice mounted and
            // behind auth, which is what `every_slice_is_mounted_and_behind_auth`
            // checks. Boot builds the real pair.
            requirements: crate::requirements::Requirements::new(
                ctx.settings.clone(),
                models,
                Arc::new(binaries) as Arc<dyn crate::requirements::deps::Binaries>,
                Arc::clone(&ctx.frames),
            ),
            greetings: crate::requirements::Greetings::new(
                chat,
                Arc::new(crate::chat::LlmLoading::new(
                    ctx.settings.clone(),
                    Arc::clone(&sidecars),
                )),
            ),
            // Unwired: no runner attached, so a due tick logs and `runNow`
            // refuses. A router test needs the slice mounted and behind auth.
            scheduled: crate::scheduled::Scheduler::new(
                ctx.db.clone(),
                crate::scheduled::deps::SchedulerDeps::unwired(),
            ),
            mcp: crate::mcp::Mcp::new(
                Arc::clone(&ctx.paths),
                ctx.db.clone(),
                ctx.vault.clone(),
                Arc::clone(&ctx.frames),
                sidecars,
                crate::mcp::stdio::StdioSupport::Available,
            ),
            // A router test gets its own aggregate rather than a stub: it is
            // cheap, it has no external dependency, and a stub would make the
            // update slice's status writes unobservable in exactly the tests
            // that should observe them.
            status: Arc::new(CoreStatusService::new(Arc::clone(&sidecars_for_status)
                as Arc<dyn crate::transport::status::SidecarSource>)),
        })
    }
}

pub fn build(ctx: AppContext, services: Services) -> Router {
    let Services {
        auth,
        memories,
        memory_indexer,
        sessions,
        chat,
        storage,
        sidecars,
        speech,
        speech_voices,
        llm,
        models,
        binaries,
        mcp,
        extensions,
        requirements,
        greetings,
        scheduled,
        status,
    } = services;
    Router::new()
        // Transport's own routes.
        .merge(health::routes(&ctx))
        .merge(ws::routes(&ctx))
        // Route slices, one line each, in registry order. Add yours here.
        .merge(crate::auth::routes::routes(&ctx, auth))
        .merge(crate::settings::routes::routes(&ctx, ctx.settings.clone()))
        .merge(crate::sessions::routes::routes(&ctx, sessions, chat))
        .merge(crate::memories::routes::routes(
            &ctx,
            memories,
            memory_indexer,
        ))
        .merge(crate::storage::routes::routes(&ctx, storage))
        .merge(crate::sidecars::routes::routes(&ctx, Arc::clone(&sidecars)))
        .merge(crate::speech::routes::routes(
            &ctx,
            Arc::clone(&sidecars),
            speech,
            speech_voices,
        ))
        .merge(crate::llm::routes::routes(&ctx, llm))
        .merge(crate::models::routes::routes(&ctx, models))
        // Nested rather than merged, and it is the one slice that is. Its
        // routes are relative and it asks the caller for auth, so the prefix
        // and the layer are applied here.
        .nest(
            "/api/v1/binaries",
            crate::downloads::binaries::routes::router(binaries)
                .layer(from_fn_with_state(ctx.clone(), require_client)),
        )
        .merge(crate::mcp::routes::routes(&ctx, mcp))
        .merge(crate::extensions::routes::routes(&ctx, extensions.clone()))
        // The flat catalog is a SECOND router from the same slice, at a
        // different prefix. Merging only the first one compiles perfectly and
        // leaves `GET /api/v1/tools` a 404.
        .merge(crate::extensions::routes::tools_routes(&ctx, extensions))
        .merge(crate::requirements::routes::routes(
            &ctx,
            requirements,
            greetings,
        ))
        .merge(crate::scheduled::routes::routes(&ctx, scheduled))
        // Self-update. Takes the concrete `CoreStatusService` because it WRITES
        // (`note_update_staged`), which had no production caller anywhere until
        // this mount: without it the client's core bar could not enter the
        // "updating" state at all, and the whole rollback module was dead code.
        .merge(crate::update::routes::routes(&ctx, status))
        .fallback(http::not_found)
        .method_not_allowed_fallback(http::method_not_allowed)
        .layer(from_fn(body_limit::layer))
        .layer(body_limit::limit())
        .layer(from_fn(cors::layer))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use crate::transport::testing;

    /// Assembling the router is what detects an overlapping mount, and axum
    /// does it by panicking. Running it here turns that into a failing test
    /// rather than a core that dies on its first boot after a bad merge.
    #[tokio::test]
    async fn the_router_assembles_without_conflicting_routes() {
        let _ = testing::core().router();
    }

    /// One representative path per mounted slice, and the assertion is that it
    /// answers 401 rather than 404.
    ///
    /// # Why this test exists
    ///
    /// Deleting the extension mount from `routes()` above left all 437
    /// `extensions::` tests GREEN, and the assembly test above green too. Nothing
    /// in the crate's ~1780 tests noticed that a whole slice had stopped serving;
    /// only the conformance suite did, and that needs a release build and
    /// minutes. A slice can be perfectly implemented, perfectly unit-tested, and
    /// unreachable (D-100).
    ///
    /// # Why 401 and not 200
    ///
    /// 401 proves two things at once and 200 would prove neither cleanly: the
    /// route is MOUNTED (a missing mount is 404, from the router's fallback) and
    /// its auth layer IS applied (an unlayered route would answer 200 or 500 to
    /// an anonymous request). A slice that lost `require_client` is as much a
    /// defect as one that lost its mount, and this catches both.
    ///
    /// Add a line here when you add a slice. It is the cheapest guard in the
    /// crate against the most invisible failure in it.
    #[tokio::test]
    async fn every_slice_is_mounted_and_behind_auth() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;

        // One real route per slice, with its real method. A renamed route or a
        // changed method fails here too, which is intended: both are ways for a
        // slice to stop answering what the client asks it.
        let paths = [
            ("settings", "GET", "/api/v1/settings"),
            ("sessions", "GET", "/api/v1/sessions"),
            ("memories", "GET", "/api/v1/memories"),
            ("storage", "GET", "/api/v1/storage"),
            ("sidecars", "GET", "/api/v1/sidecars/status"),
            ("speech", "GET", "/api/v1/stt/status"),
            ("llm", "POST", "/api/v1/llm/autocorrect"),
            ("models", "GET", "/api/v1/models"),
            ("mcp", "GET", "/api/v1/mcp"),
            ("update", "GET", "/api/v1/update/check"),
            ("binaries", "GET", "/api/v1/binaries"),
            ("extensions", "GET", "/api/v1/extensions"),
            ("tools", "GET", "/api/v1/tools"),
            ("requirements", "GET", "/api/v1/requirements"),
            ("scheduled", "GET", "/api/v1/scheduled-prompts"),
        ];

        let router = testing::core().router();
        let mut unmounted = Vec::new();
        for (slice, method, path) in paths {
            let request = Request::builder()
                .method(method)
                .uri(path)
                .body(Body::empty())
                .expect("request builds");
            let status = router
                .clone()
                .oneshot(request)
                .await
                .expect("router answers")
                .status();
            if status != axum::http::StatusCode::UNAUTHORIZED {
                unmounted.push(format!("{slice} ({path}) answered {status}, expected 401"));
            }
        }
        assert!(
            unmounted.is_empty(),
            "slice(s) not mounted behind auth:\n  {}",
            unmounted.join("\n  "),
        );
    }
}
