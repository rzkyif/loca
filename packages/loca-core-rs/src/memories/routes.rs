//! The memories route slice: everything under `/api/v1/memories`.
//!
//! Memory CRUD plus rescan, backing the memories manager and the client's
//! autocomplete. Tools reach the same store through the module broker rather
//! than through these routes, so the tool-facing rules (a disabled memory is
//! unreadable, lookups are by title) live there and not here.
//!
//! Every mutation schedules a background index pass, exactly where the
//! TypeScript does. Scheduling is fire-and-forget: it must not fail a request,
//! and with no queue wired it does nothing at all.
//!
//! Four routes answer `{ "ok": true }` with a 200 rather than a 204. That is not
//! a style choice to correct, it is what the client's `del`/`put` wrappers see
//! today.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use serde::Serialize;

use crate::contract::api::{Memory, MemoryKind, MemoryMeta};
use crate::contract::inbound::{
    MemoryCreateBody, MemoryCreateBodyKind, MemoryFileBody, MemoryPatchBody,
};
use crate::error::Result;
use crate::transport::http::Json;
use crate::transport::middleware::{from_fn_with_state, require_client};
use crate::transport::state::AppContext;

use super::indexer::MemoryIndexer;
use super::store::{MemoriesStore, RescanResponse};

// --- bodies and envelopes ----------------------------------------------------
//
// The three request bodies are generated: their Zod schemas live in
// `@loca/shared/validation/memories.ts`, so `emit-zod.ts` sees them and
// `contract::inbound` carries a `Deserialize` that enforces `.strict()` and
// `title`'s `.min(1)`. Only the three response envelopes are declared here,
// because those are object literals in the TypeScript handlers and no
// generator can see an object literal. Everything else the contract declares
// (`Memory`, `MemoryMeta`, `MemoryKind`) is imported from `contract::api`.

/// The wire spelling of a memory kind, as the create body carries it.
///
/// The body's enum is generated from the Zod schema and the store's is
/// generated from the TypeScript domain type, so they are two Rust types for
/// one wire value and this is the seam between them.
fn create_kind(kind: MemoryCreateBodyKind) -> MemoryKind {
    match kind {
        MemoryCreateBodyKind::Knowledge => MemoryKind::Knowledge,
        MemoryCreateBodyKind::Skill => MemoryKind::Skill,
    }
}

#[derive(Debug, Clone, Serialize)]
struct MemoriesListResponse {
    memories: Vec<MemoryMeta>,
}

#[derive(Debug, Clone, Serialize)]
struct MemoryFileResponse {
    content: String,
}

/// `{ "ok": true }`, which four of the ten routes return instead of a 204.
#[derive(Debug, Clone, Copy, Serialize)]
struct OkResponse {
    ok: bool,
}

impl OkResponse {
    fn new() -> Self {
        Self { ok: true }
    }
}

#[derive(Clone)]
struct MemoriesState {
    store: MemoriesStore,
    indexer: Arc<dyn MemoryIndexer>,
}

impl MemoriesState {
    fn schedule(&self, memory_id: Option<&str>) {
        self.indexer.schedule(memory_id);
    }
}

/// `store` and `indexer` are threaded in rather than built here: the store owns
/// the mutation lock and the extension provider registry, so a second instance
/// would let two writes interleave and would answer `list` from a registry
/// nobody wired.
pub fn routes(ctx: &AppContext, store: MemoriesStore, indexer: Arc<dyn MemoryIndexer>) -> Router {
    let state = MemoriesState { store, indexer };
    Router::new()
        .route("/api/v1/memories", get(list).post(create))
        .route("/api/v1/memories/rescan", post(rescan))
        .route("/api/v1/memories/{id}/reindex", post(reindex))
        .route(
            "/api/v1/memories/{id}",
            get(get_one).patch(patch_one).delete(delete_one),
        )
        .route(
            "/api/v1/memories/{id}/files/{name}",
            get(get_file).put(put_file).delete(delete_file),
        )
        .layer(from_fn_with_state(ctx.clone(), require_client))
        .with_state(state)
}

async fn list(State(state): State<MemoriesState>) -> Result<Json<MemoriesListResponse>> {
    Ok(Json(MemoriesListResponse {
        memories: state.store.list().await?,
    }))
}

/// 201, and the created memory in full.
///
/// `title`'s `.min(1)` is enforced by the generated `Deserialize`, so `""` is a
/// `validation_error` before this runs. `"   "` still reaches the store, which
/// trims and rejects it with the same code.
async fn create(
    State(state): State<MemoriesState>,
    Json(body): Json<MemoryCreateBody>,
) -> Result<Response> {
    let memory = state
        .store
        .create(create_kind(body.kind), &body.title, &body.content)
        .await?;
    state.schedule(Some(&memory.id));
    Ok((StatusCode::CREATED, Json(memory)).into_response())
}

async fn rescan(State(state): State<MemoriesState>) -> Result<Json<RescanResponse>> {
    let result = state.store.rescan().await?;
    // Every memory, not just the changed ones: a rescan can adopt files whose
    // ids this call never sees, and a pass over a fresh memory is cheap.
    state.schedule(None);
    Ok(Json(result))
}

/// Re-run the indexer for one memory. The read below is what turns an unknown id
/// into a 404 instead of silently scheduling nothing.
async fn reindex(
    State(state): State<MemoriesState>,
    Path(id): Path<String>,
) -> Result<Json<OkResponse>> {
    state.store.get(&id).await?;
    state.schedule(Some(&id));
    Ok(Json(OkResponse::new()))
}

async fn get_one(
    State(state): State<MemoriesState>,
    Path(id): Path<String>,
) -> Result<Json<Memory>> {
    Ok(Json(state.store.get(&id).await?))
}

/// The three fields are applied in a fixed order, and a patch carrying none of
/// them is a 200 no-op that still schedules a pass.
///
/// The order is why `title`'s `.min(1)` has to be enforced in the deserializer
/// rather than by the store: `enabled` is written first, so a body pairing a
/// valid `enabled` with an empty `title` would half-apply before the rename
/// failed. Refusing the body outright is what the TypeScript does.
async fn patch_one(
    State(state): State<MemoriesState>,
    Path(id): Path<String>,
    Json(body): Json<MemoryPatchBody>,
) -> Result<Json<Memory>> {
    if let Some(enabled) = body.enabled {
        state.store.set_enabled(&id, enabled).await?;
    }
    if let Some(title) = body.title.as_deref() {
        state.store.rename(&id, title).await?;
    }
    if let Some(content) = body.content.as_deref() {
        state.store.replace_content(&id, content).await?;
    }
    state.schedule(Some(&id));
    Ok(Json(state.store.get(&id).await?))
}

async fn delete_one(
    State(state): State<MemoriesState>,
    Path(id): Path<String>,
) -> Result<Json<OkResponse>> {
    state.store.delete(&id).await?;
    Ok(Json(OkResponse::new()))
}

async fn get_file(
    State(state): State<MemoriesState>,
    Path((id, name)): Path<(String, String)>,
) -> Result<Json<MemoryFileResponse>> {
    Ok(Json(MemoryFileResponse {
        content: state.store.get_file(&id, &name).await?,
    }))
}

async fn put_file(
    State(state): State<MemoriesState>,
    Path((id, name)): Path<(String, String)>,
    Json(body): Json<MemoryFileBody>,
) -> Result<Json<OkResponse>> {
    state.store.write_file(&id, &name, &body.content).await?;
    Ok(Json(OkResponse::new()))
}

async fn delete_file(
    State(state): State<MemoriesState>,
    Path((id, name)): Path<(String, String)>,
) -> Result<Json<OkResponse>> {
    state.store.delete_file(&id, &name).await?;
    Ok(Json(OkResponse::new()))
}
