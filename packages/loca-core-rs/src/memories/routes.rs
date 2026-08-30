//! `/api/v1/memories`: CRUD plus rescan. Every mutation schedules a
//! fire-and-forget index pass. Four routes answer `{ "ok": true }` with a 200
//! rather than a 204, which is what the client's wrappers expect.

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

/// The create body's kind enum and the store's are two generated types for
/// one wire value.
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

/// `store` is threaded in rather than built here: it owns the mutation lock
/// and the provider registry, so there must be exactly one.
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

/// 201 with the created memory. `title`'s `.min(1)` is enforced by the
/// generated `Deserialize`; a whitespace-only title is rejected by the store.
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
    // Every memory: a rescan can adopt files whose ids this call never sees.
    state.schedule(None);
    Ok(Json(result))
}

/// The read turns an unknown id into a 404 instead of scheduling nothing.
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

/// Fields apply in a fixed order; an empty patch is a 200 no-op that still
/// schedules. `enabled` is written first, which is why an empty `title` must
/// be refused by the deserializer rather than half-applying.
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
