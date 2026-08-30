//! The router: every route the core serves, assembled once.
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
//! A slice owning its own prefixes rather than being nested under one lets a
//! package serving two of them still mount in a single line, and leaves no
//! prefix here to mistype. Overlapping routes panic when this function runs,
//! and it runs in a test.

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

/// The service handles a route slice needs that [`AppContext`] does not carry:
/// a concrete type a slice needs by value (the pairing slice needs `Auth`, not
/// the `dyn ClientAuth` in `ctx`), or a handle exactly one of which may exist
/// process-wide. A second `Auth`, `SessionsStore` or `Manager` answers from
/// state the rest of the core cannot see, silently.
///
/// Boot and [`Services::unwired`] both write the literal in full, so adding a
/// field breaks both rather than leaving a seam at its stub.
pub struct Services {
    pub auth: Arc<Auth>,
    pub memories: crate::memories::MemoriesStore,
    pub memory_indexer: Arc<dyn crate::memories::MemoryIndexer>,
    pub sessions: SessionsStore,
    pub chat: Arc<dyn ChatEngine>,
    pub storage: StorageDeps,
    pub sidecars: Arc<Manager>,
    pub speech: Arc<dyn crate::speech::LocalSpeech>,
    /// The voice list, separate from the engine: it is a catalog fact and must
    /// fill on a core with no local speech engine at all.
    pub speech_voices: Arc<dyn crate::speech::TtsVoiceSource>,
    pub llm: Arc<crate::llm::Llm>,
    pub models: crate::models::Models,
    /// The binary installer. Threaded rather than built here because it sits on
    /// a `DownloadQueue` whose recovery pass has to run at boot, before any
    /// request can observe a half-written row.
    pub binaries: crate::downloads::binaries::installer::BinariesInstaller,
    /// Threaded for its lifecycle: `Mcp` owns live connections and child
    /// processes, so something must call `start()` after boot and `shutdown()`
    /// before the listener closes, and neither can reach a value built here.
    pub mcp: crate::mcp::Mcp,
    /// The extension subsystem. Serves its own routes plus the flat
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
    /// The one aggregate status service. `ctx.status` is the same object behind
    /// a `dyn StatusProvider`, the read side; the update routes need the
    /// concrete handle because they write (`note_update_staged`), and a second
    /// instance would move a bar `/api/v1/health` never reports.
    pub status: Arc<CoreStatusService>,
}

impl Services {
    /// Every seam at its stub. What a router test mounts, and the shape boot
    /// starts from before it fills in the handles that exist.
    pub fn unwired(ctx: &AppContext, auth: Arc<Auth>) -> crate::error::Result<Self> {
        // One manager: `mcp` spawns stdio children through the same registry
        // the sidecar routes report from.
        let sidecars = Arc::new(Manager::new());
        let sidecars_for_status = Arc::clone(&sidecars);
        let net = Arc::new(crate::downloads::net::Net::new()?);
        // Hoisted out of the literal: requirements needs the same `Models` and
        // installer the `models`/`binaries` fields carry, and greetings needs a
        // concrete `Chat` rather than the `chat` field's trait object.
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
            // A router test needs the slice mounted, not a live queue.
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
                        // Constructing a client reaches nothing, and every
                        // route a router test exercises stops short of it.
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
            // Mounted and behind auth is all a router test needs.
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
            // No runner attached, so a due tick logs and `runNow` refuses.
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
            // A real aggregate, not a stub: a stub would make the update
            // slice's status writes unobservable.
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
        // The one nested slice: its routes are relative and it asks the caller
        // for auth, so the prefix and the layer are applied here.
        .nest(
            "/api/v1/binaries",
            crate::downloads::binaries::routes::router(binaries)
                .layer(from_fn_with_state(ctx.clone(), require_client)),
        )
        .merge(crate::mcp::routes::routes(&ctx, mcp))
        .merge(crate::extensions::routes::routes(&ctx, extensions.clone()))
        // A second router from the same slice, at a different prefix. Merging
        // only the first compiles fine and leaves `GET /api/v1/tools` a 404.
        .merge(crate::extensions::routes::tools_routes(&ctx, extensions))
        .merge(crate::requirements::routes::routes(
            &ctx,
            requirements,
            greetings,
        ))
        .merge(crate::scheduled::routes::routes(&ctx, scheduled))
        // Self-update. Takes the concrete `CoreStatusService` because it writes
        // (`note_update_staged`), without which the core bar can never enter
        // the "updating" state.
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

    /// axum detects an overlapping mount by panicking during assembly, so
    /// running it here turns that into a failing test rather than a bad boot.
    #[tokio::test]
    async fn the_router_assembles_without_conflicting_routes() {
        let _ = testing::core().router();
    }

    /// One representative path per mounted slice, asserting 401. A slice can be
    /// perfectly implemented, perfectly unit-tested, and never mounted; its own
    /// suite stays green either way. 401 catches both halves at once: a missing
    /// mount answers 404 from the fallback, and a route that lost
    /// `require_client` answers 200 or 500. Add a line when you add a slice.
    #[tokio::test]
    async fn every_slice_is_mounted_and_behind_auth() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;

        // One real route per slice, with its real method: a rename or a method
        // change is another way for a slice to stop answering.
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
