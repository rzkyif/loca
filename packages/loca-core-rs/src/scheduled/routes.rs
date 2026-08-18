//! The scheduled-prompts route slice: CRUD plus the manual run.
//!
//! Backs the Scheduled Prompts settings manager. Agent-created schedules do NOT
//! arrive here: they come through the schedule-confirm flow on the WebSocket and
//! reach the store via
//! [`SchedulePersist`](crate::chat::SchedulePersist), which is why the guards
//! live in the store rather than in these handlers.
//!
//! All five require a paired client (`bearerMiddleware`,
//! `http/routes/scheduled-prompts.ts:24`). The layer is applied to the slice
//! here, once, so a route added later cannot be added unauthenticated.

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
//
// The two envelopes and the patch body are the part of this slice the contract
// does not declare.
//
// `ScheduledPrompt` itself is hand-written in `store.rs`; see its doc comment
// for why (it is reachable from no `api/http.ts` type, so the bridge never saw
// it) and the interfaces-queue entry filed to F2.
//
// The patch body is `inbound::ScheduledPromptPatchBody`, generated from
// `validation/scheduled-prompt.ts`. Everything it declares survives the bridge:
// `.strict()` as `deny_unknown_fields`, and both `min(1)` checks as newtypes
// whose `Deserialize` parses through `FromStr`. Nothing is restated here.

#[derive(Debug, Clone, Serialize)]
struct ScheduledPromptsResponse {
    #[serde(rename = "scheduledPrompts")]
    scheduled_prompts: Vec<ScheduledPrompt>,
}

/// `{ "ok": true }` on delete, not a 204: it is what the client's `del` wrapper
/// reads (`loca-client/src/ui/lib/core/scheduled-prompts.ts:35`).
#[derive(Debug, Clone, Copy, Serialize)]
struct OkResponse {
    ok: bool,
}

#[derive(Debug, Clone, Serialize)]
struct RunResponse {
    #[serde(rename = "sessionId")]
    session_id: String,
}

/// The patch body narrowed to the domain shape the store speaks.
///
/// The two `min(1)` checks are already spent by the time this runs: they are on
/// the newtypes, so an empty title never deserializes. What the patch body does
/// NOT carry is a maximum, where the create body caps title at 200 and
/// instruction at 10 000, so a title refused on create is accepted on update.
/// That asymmetry is the oracle's own (`validation/scheduled-prompt.ts`: the
/// draft against the patch body) and is ported rather than corrected, so a
/// behaviour change cannot ride in on a refactor.
fn narrow_patch(body: inbound::ScheduledPromptPatchBody) -> Result<Patch> {
    Ok(Patch {
        title: body.title.map(|t| t.to_string()),
        instruction: body.instruction.map(|t| t.to_string()),
        schedule: body.schedule.as_ref().map(from_inbound).transpose()?,
        run_missed: body.run_missed,
        enabled: body.enabled,
    })
}

/// The create body narrowed to the domain shape the store speaks.
///
/// Parsed as the INBOUND draft, per `contract/mod.rs`'s rule and because it is
/// what `parseBody(scheduledPromptDraftSchema, ...)` does: that buys the title
/// and instruction length bounds, which typify DID carry, for free.
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
