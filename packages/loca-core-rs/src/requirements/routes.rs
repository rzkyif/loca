//! The requirements route slice, plus the greeting trigger. All three routes
//! require a paired client.

use std::collections::BTreeSet;

use axum::extract::{Query, State};
use axum::routing::{get, post};
use axum::Router;
use serde::Deserialize;

use crate::contract::api::{
    BinaryKind, DownloadRequirementsResponse, RequiredFileType, RequirementsSnapshot,
};
use crate::contract::inbound;
use crate::error::Result;
use crate::transport::http::Json;
use crate::transport::middleware::{from_fn_with_state, require_client, AuthedClient};
use crate::transport::state::AppContext;

use super::compute::{binary_source_to_kind, group_from_file};
use super::greetings::{Greetings, RunResponse};
use super::Requirements;

/// `?probe=1` and nothing else: this switches on outbound requests, so only the
/// exact spelling the confirm modal sends counts.
#[derive(Debug, Default, Deserialize)]
struct SnapshotQuery {
    #[serde(default)]
    probe: Option<String>,
}

#[derive(Clone)]
struct SliceState {
    requirements: Requirements,
    greetings: Greetings,
}

pub fn routes(ctx: &AppContext, requirements: Requirements, greetings: Greetings) -> Router {
    Router::new()
        .route("/api/v1/requirements", get(snapshot))
        .route("/api/v1/requirements/download", post(download))
        .route("/api/v1/greetings/run", post(run_greeting))
        .layer(from_fn_with_state(ctx.clone(), require_client))
        .with_state(SliceState {
            requirements,
            greetings,
        })
}

/// The full required-files list plus the missing subset for the current config.
async fn snapshot(
    State(state): State<SliceState>,
    Query(query): Query<SnapshotQuery>,
) -> Result<Json<RequirementsSnapshot>> {
    let probe_sizes = query.probe.as_deref() == Some("1");
    Ok(Json(state.requirements.snapshot(probe_sizes).await?))
}

/// Download everything missing: enqueue each missing model group and install the
/// missing binaries.
async fn download(State(state): State<SliceState>) -> Result<Json<DownloadRequirementsResponse>> {
    let snap = state.requirements.snapshot(false).await?;

    // `BTreeSet` so the enqueue order is stable across runs.
    let groups: BTreeSet<_> = snap
        .missing
        .iter()
        .filter(|m| m.type_ == RequiredFileType::Model)
        .filter_map(|m| group_from_file(m.group))
        .collect();
    let mut model_job_ids = Vec::new();
    for group in groups {
        // One group failing must not starve the others; it stays missing.
        match state.requirements.models().ensure_kind(group).await {
            Ok(res) => model_job_ids.extend(res.enqueued),
            Err(err) => tracing::error!(
                target: "loca::requirements",
                "requirements download: ensure {group:?} failed: {err}",
            ),
        }
    }

    let kinds: Vec<BinaryKind> = snap
        .missing
        .iter()
        .filter(|m| m.type_ == RequiredFileType::Binary)
        .filter_map(|m| binary_source_to_kind(&m.source))
        .collect();
    // `install` fetches the manifest when uncached, so never call it for nothing.
    let binary_job_ids = if kinds.is_empty() {
        Vec::new()
    } else {
        state.requirements.binaries().install(kinds).await?
    };

    Ok(Json(DownloadRequirementsResponse {
        binary_job_ids,
        model_job_ids,
    }))
}

/// Mint a greeting session for this client, subject to its dedup window.
async fn run_greeting(
    State(state): State<SliceState>,
    client: AuthedClient,
    Json(body): Json<inbound::GreetingRunBody>,
) -> Json<RunResponse> {
    Json(state.greetings.run(&client.id, body))
}
