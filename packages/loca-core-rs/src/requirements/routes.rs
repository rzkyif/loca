//! The requirements route slice, plus the greeting trigger.
//!
//! Two prefixes in one slice, which is the case `transport::router`'s module doc
//! names: a slice owns its full paths, so serving both still mounts in one line.
//!
//! All three routes require a paired client, matching `bearerMiddleware()` on
//! both oracle routers.

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

/// `?probe=1` and nothing else.
///
/// The comparison is against the literal `"1"`, matching
/// `http/routes/requirements.ts:22`. `?probe=true` is deliberately NOT a probe:
/// this is the switch between a local-only snapshot and one that reaches
/// HuggingFace and GitHub, so it accepts exactly the spelling the client sends
/// when the user opens the confirm modal, and treats everything else as off.
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
///
/// `missing` already excludes binaries that are unavailable on this platform.
async fn download(State(state): State<SliceState>) -> Result<Json<DownloadRequirementsResponse>> {
    let snap = state.requirements.snapshot(false).await?;

    // A `BTreeSet` rather than a `HashSet`: the enqueue order is what the
    // download queue works through, so a set that reordered per run would make
    // the same request download the same files in a different order.
    let groups: BTreeSet<_> = snap
        .missing
        .iter()
        .filter(|m| m.type_ == RequiredFileType::Model)
        .filter_map(|m| group_from_file(m.group))
        .collect();
    let mut model_job_ids = Vec::new();
    for group in groups {
        // One group failing to enqueue (a malformed source spec, say) must not
        // abort the request and starve the OTHER groups and the binary installs
        // below. Log and continue: the group stays missing and the popup keeps
        // offering it.
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
    // Skipped entirely when nothing is missing, and that is not just tidiness:
    // `install` loads the binary manifest, which FETCHES when there is no cache,
    // so calling it with an empty list would put an outbound request behind a
    // request that has nothing to download.
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
