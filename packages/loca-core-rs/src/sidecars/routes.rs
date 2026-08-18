//! `GET /api/v1/sidecars/status`.
//!
//! Live resource usage for the core process and whatever subsystems run in a
//! child of their own. The pid set is fixed HERE (the published sidecar pids
//! plus this process) and never taken from the request, so a caller cannot use
//! this to probe an arbitrary process on the machine.
//!
//! `POST /api/v1/sidecars/:kind/{start,restart}` is served, and re-homed onto
//! the in-process engines the sidecars became. It was absent for a while and
//! that was a REGRESSION rather than a simplification: the client's Retry
//! button in the Services field (`ServicesField.svelte:99`) is the recovery
//! path for a subsystem stuck in `Error`, and it had no route to call.
//!
//! `start` and `restart` are equivalent, exactly as in the oracle
//! (`http/routes/sidecars.ts:60`): both mean "apply the current settings
//! again". For the language model that is dropping the resident weights, since
//! `ensure_loaded` runs at the top of every turn and reloads from settings; for
//! speech it is a reconcile.
//!
//! **`stop` is deliberately NOT served**, and that is a real difference rather
//! than an oversight. The oracle could stop a child process and leave it
//! stopped. There is no process now, and whether an engine is resident is
//! derived from settings, so a "stopped" state would be re-derived away by the
//! next turn. Nothing in the client calls it (`lib/core/sidecars.ts` exposes
//! `status` and `restart` only).

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::Router;

use crate::contract::api::{
    ProcessMetricsLite, SidecarKind, SidecarSnapshot, SidecarsStatusResponse,
};
use crate::transport::http::Json;
use crate::transport::middleware::{from_fn_with_state, require_client};
use crate::transport::state::AppContext;
use crate::transport::status::SidecarSource;

use super::Manager;

#[derive(Clone)]
struct SidecarsState {
    sidecars: Arc<Manager>,
}

pub fn routes(ctx: &AppContext, sidecars: Arc<Manager>) -> Router {
    Router::new()
        .route("/api/v1/sidecars/status", get(status))
        .route("/api/v1/sidecars/{kind}/start", post(restart))
        .route("/api/v1/sidecars/{kind}/restart", post(restart))
        .layer(from_fn_with_state(ctx.clone(), require_client))
        .with_state(SidecarsState { sidecars })
}

async fn status(State(state): State<SidecarsState>) -> Json<SidecarsStatusResponse> {
    let sidecars = state.sidecars.statuses();

    let core_pid = std::process::id();
    let mut pids = vec![core_pid];
    pids.extend(sidecars.iter().filter_map(pid_of));
    let samples = state.sidecars.metrics().sample(&pids);

    let core = samples.get(&core_pid).copied();
    Json(SidecarsStatusResponse {
        sidecars: sidecars
            .into_iter()
            .map(|mut snapshot| {
                if let Some(sample) = pid_of(&snapshot).and_then(|pid| samples.get(&pid)) {
                    snapshot.rss_mb = Some(sample.rss_mb);
                    snapshot.cpu_pct = Some(sample.cpu_pct);
                }
                snapshot
            })
            .collect(),
        core: ProcessMetricsLite {
            pid: i64::from(core_pid),
            // Zero rather than absent when the sample failed, because the field
            // is required by the contract and the row reads as "no usage"
            // rather than disappearing.
            rss_mb: core.map_or(0.0, |s| s.rss_mb),
            cpu_pct: core.map_or(0.0, |s| s.cpu_pct),
        },
    })
}

/// The wire carries a pid as a JSON integer. Anything that is not a plausible
/// pid is dropped rather than cast, so a garbage value cannot become a pid we
/// then sample.
fn pid_of(snapshot: &SidecarSnapshot) -> Option<u32> {
    u32::try_from(snapshot.pid?).ok().filter(|pid| *pid >= 1)
}

/// `POST /api/v1/sidecars/:kind/{start,restart}`.
///
/// 204 on success, matching the oracle. An unknown or uncontrollable kind is a
/// validation error rather than a 404: the path names a KIND from a closed set,
/// so "there is no such subsystem to restart" is a bad request about a known
/// vocabulary, not a missing resource.
async fn restart(
    State(state): State<SidecarsState>,
    Path(kind): Path<String>,
) -> crate::error::Result<axum::http::StatusCode> {
    let parsed: SidecarKind = kind
        .parse()
        .map_err(|_| crate::error::validation(format!("unknown subsystem \"{kind}\"")))?;
    state.sidecars.restart(parsed).await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}
