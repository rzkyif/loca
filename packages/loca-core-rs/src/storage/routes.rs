//! The storage route slice: `/api/v1/storage` and its two reclaim actions.
//!
//! All three require a paired client. There is no admin-only tier here: a
//! paired device is already trusted with the sessions and settings these rows
//! describe.
//!
//! Inbound bodies deserialise through `contract::api` and nothing revalidates
//! after them: a missing or wrong-typed field is serde's `validation_error`.

use axum::response::Response;
use axum::routing::{get, post};
use axum::Router;

use crate::contract::api::{ClearStorageCategoryRequest, DeleteStoragePathsRequest, StorageTree};
use crate::error::Result;
use crate::transport::http::{no_content, Json};
use crate::transport::middleware::{from_fn_with_state, require_client};
use crate::transport::state::AppContext;

use super::deps::StorageDeps;
use super::StorageService;

pub fn routes(ctx: &AppContext, deps: StorageDeps) -> Router {
    let service = StorageService::new(ctx, deps);
    Router::new()
        .route("/api/v1/storage", get(tree))
        .route("/api/v1/storage/delete", post(delete))
        .route("/api/v1/storage/clear", post(clear))
        .layer(from_fn_with_state(ctx.clone(), require_client))
        .with_state(service)
}

/// The full on-disk tree, every category and every size, for the Settings
/// storage field.
async fn tree(
    axum::extract::State(service): axum::extract::State<StorageService>,
) -> Result<Json<StorageTree>> {
    Ok(Json(service.tree().await?))
}

/// Reclaim the selected rows. Core re-derives each row's lock and refuses
/// anything in use; a path that matches no row is skipped.
async fn delete(
    axum::extract::State(service): axum::extract::State<StorageService>,
    Json(body): Json<DeleteStoragePathsRequest>,
) -> Result<Response> {
    service.delete_paths(&body.paths).await?;
    Ok(no_content())
}

/// Empty a whole category of its unlocked rows. For `settings` this is the
/// factory reset.
async fn clear(
    axum::extract::State(service): axum::extract::State<StorageService>,
    Json(body): Json<ClearStorageCategoryRequest>,
) -> Result<Response> {
    service.clear_category(&body.category_id).await?;
    Ok(no_content())
}
