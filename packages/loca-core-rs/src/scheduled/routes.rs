//! The scheduled-prompts route slice: CRUD plus the manual run. Agent-created
//! schedules arrive via [`SchedulePersist`](crate::chat::SchedulePersist)
//! instead, which is why the guards live in the store.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use serde::Serialize;

use crate::contract::inbound::ScheduledPromptDraft as InboundDraft;
use crate::contract::{api, inbound};
use crate::error::Result;
use crate::transport::http::Json;
use crate::transport::middleware::{from_fn_with_state, require_client, AuthedClient};
use crate::transport::state::AppContext;

use super::spec::from_inbound;
use super::store::{Patch, ScheduledPrompt};
use super::Scheduler;

pub fn routes(ctx: &AppContext, scheduler: Scheduler) -> Router {
    Router::new()
        .route("/api/v1/scheduled-prompts", get(list).post(create))
        .route(
            "/api/v1/scheduled-prompts/{id}",
            axum::routing::patch(update).delete(remove),
        )
        .route("/api/v1/scheduled-prompts/{id}/run", post(run))
        .layer(from_fn_with_state(ctx.clone(), require_client))
        .with_state(scheduler)
}

// --- wire shapes -------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
struct ScheduledPromptsResponse {
    #[serde(rename = "scheduledPrompts")]
    scheduled_prompts: Vec<ScheduledPrompt>,
}

/// `{ "ok": true }` on delete, not a 204: what the client's `del` wrapper reads.
#[derive(Debug, Clone, Copy, Serialize)]
struct OkResponse {
    ok: bool,
}

#[derive(Debug, Clone, Serialize)]
struct RunResponse {
    #[serde(rename = "sessionId")]
    session_id: String,
}

/// The patch body narrowed to the store's shape. Unlike create, the patch body
/// carries no length maximum; that asymmetry is the contract's own.
fn narrow_patch(body: inbound::ScheduledPromptPatchBody) -> Result<Patch> {
    Ok(Patch {
        title: body.title.map(|t| t.to_string()),
        instruction: body.instruction.map(|t| t.to_string()),
        schedule: body.schedule.as_ref().map(from_inbound).transpose()?,
        run_missed: body.run_missed,
        enabled: body.enabled,
    })
}

/// The inbound create body narrowed to the store's shape.
fn narrow(draft: InboundDraft) -> Result<api::ScheduledPromptDraft> {
    Ok(api::ScheduledPromptDraft {
        title: draft.title.to_string(),
        instruction: draft.instruction.to_string(),
        schedule: from_inbound(&draft.schedule)?,
        run_missed: draft.run_missed,
    })
}

// --- handlers ----------------------------------------------------------------

async fn list(
    State(scheduler): State<Scheduler>,
    client: AuthedClient,
) -> Result<Json<ScheduledPromptsResponse>> {
    Ok(Json(ScheduledPromptsResponse {
        scheduled_prompts: scheduler.list(&client.id).await?,
    }))
}

async fn create(
    State(scheduler): State<Scheduler>,
    client: AuthedClient,
    Json(body): Json<InboundDraft>,
) -> Result<Response> {
    let created = scheduler.create(&client.id, &narrow(body)?).await?;
    Ok((StatusCode::CREATED, Json(created)).into_response())
}

async fn update(
    State(scheduler): State<Scheduler>,
    client: AuthedClient,
    Path(id): Path<String>,
    Json(body): Json<inbound::ScheduledPromptPatchBody>,
) -> Result<Json<ScheduledPrompt>> {
    Ok(Json(
        scheduler
            .update(&client.id, &id, &narrow_patch(body)?)
            .await?,
    ))
}

async fn remove(
    State(scheduler): State<Scheduler>,
    client: AuthedClient,
    Path(id): Path<String>,
) -> Result<Json<OkResponse>> {
    scheduler.delete(&client.id, &id).await?;
    Ok(Json(OkResponse { ok: true }))
}

async fn run(
    State(scheduler): State<Scheduler>,
    client: AuthedClient,
    Path(id): Path<String>,
) -> Result<Json<RunResponse>> {
    Ok(Json(RunResponse {
        session_id: scheduler.run_now(&client.id, &id).await?,
    }))
}
