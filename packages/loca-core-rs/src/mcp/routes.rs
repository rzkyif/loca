//! The MCP route slice: `/api/v1/mcp` and its twelve sub-paths.
//!
//! Thirteen routes, all behind the same bearer layer, matching the
//! TypeScript's `r.use("*", authed)`. There is no admin tier: a paired device
//! is already trusted to configure tool providers.
//!
//! Two path pairs overlap, and the two routers resolve them by different rules
//! that happen to agree. Hono matched in registration order, so `/prompts` beat
//! `/{id}` and `/prompts/{p}/resolve` beat `/prompts/{p}/{action}` because they
//! were declared first; axum prefers a static segment over a parameter, which
//! lands on the same answer. `routes::tests` pins both, because "the same
//! answer for a different reason" is exactly what quietly stops being true.
//!
//! Any change that affects which servers are enabled resyncs the connections
//! and then broadcasts `mcp.snapshot`, so every client repaints. A per-tool or
//! per-prompt toggle broadcasts without resyncing: it changes what a connected
//! server offers, not which servers are connected.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use serde::Serialize;

use crate::contract::api::{
    McpPrompt, McpRemoteAuth, McpResource, McpServer, McpStdioRuntime, McpTransportKind,
};
use crate::contract::inbound::{
    McpPromptResolveBody, McpServerBody, McpServerBodyKind, McpServerBodyRemoteAuth,
    McpServerBodyRuntime, McpServerPatchBody, McpServerPatchBodyKind, McpServerPatchBodyRemoteAuth,
    McpServerPatchBodyRuntime,
};
use crate::error::{validation, Result};
use crate::transport::http::Json;
use crate::transport::middleware::{from_fn_with_state, require_client};
use crate::transport::state::AppContext;

use super::registry::{NewServer, ServerPatch};
use super::Mcp;

// --- bodies and envelopes ----------------------------------------------------
//
// The create, patch and prompt-resolve bodies are generated: their Zod schemas
// live in `@loca/shared/validation/mcp.ts`, so `emit-zod.ts` sees them and
// `contract::inbound` carries a `Deserialize` that enforces `.strict()` and
// `name`'s `.min(1)`. That second one is load-bearing on the patch: the
// registry's update treats a blank name as "keep the stored one", so without
// the constraint an empty rename is a silent 200 where the TypeScript answers
// `validation_error`.
//
// `contract::api::McpServerInput` is NOT these: it is the registry-level input,
// carrying `hasAuth` / `oauthAuthorized` and no `authToken`, which is the
// opposite split from the HTTP body. The response wrappers below are object
// literals in the TypeScript and are declared here.
//
// `authToken` is a live bearer token. Both generated bodies get a redacting
// `Debug` from `SECRET_FIELDS` in `scripts/contract/generate.ts` rather than
// typify's derived one, which would print it in full: F2's other tripwire keys
// off type NAMES and this field has a plain `String` type, and `scrub_secrets`
// would not catch it either, since its JSON pattern is `"password"` and a short
// token clears the length rules.

/// `GET /api/v1/mcp`.
#[derive(Debug, Serialize)]
struct McpServersResponse {
    servers: Vec<McpServer>,
}

/// `GET /api/v1/mcp/prompts`.
#[derive(Debug, Serialize)]
struct McpPromptsResponse {
    prompts: Vec<McpPrompt>,
}

/// `GET /api/v1/mcp/resources`.
#[derive(Debug, Serialize)]
struct McpResourcesResponse {
    resources: Vec<McpResource>,
}

/// `POST /api/v1/mcp/{id}/prompts/{prompt}/resolve`.
#[derive(Debug, Serialize)]
struct McpPromptResolveResponse {
    text: String,
}

/// `POST /api/v1/mcp/{id}/oauth/start`. A null URL means the stored tokens
/// already worked and no browser is needed.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct McpOAuthStartResponse {
    authorization_url: Option<String>,
}

/// `DELETE /api/v1/mcp/{id}`.
#[derive(Debug, Serialize)]
struct McpDeleteResponse {
    ok: bool,
}

pub fn routes(ctx: &AppContext, mcp: Mcp) -> Router {
    Router::new()
        .route("/api/v1/mcp", get(list).post(create))
        .route("/api/v1/mcp/prompts", get(list_prompts))
        .route("/api/v1/mcp/resources", get(list_resources))
        .route(
            "/api/v1/mcp/{id}",
            get(get_one).patch(patch_one).delete(delete_one),
        )
        .route("/api/v1/mcp/{id}/reconnect", post(reconnect))
        .route("/api/v1/mcp/{id}/oauth/start", post(oauth_start))
        .route(
            "/api/v1/mcp/{id}/prompts/{prompt}/resolve",
            post(resolve_prompt),
        )
        .route(
            "/api/v1/mcp/{id}/tools/{tool}/always-available/{action}",
            post(set_always_available),
        )
        .route("/api/v1/mcp/{id}/tools/{tool}/{action}", post(set_tool))
        .route(
            "/api/v1/mcp/{id}/prompts/{prompt}/{action}",
            post(set_prompt),
        )
        .layer(from_fn_with_state(ctx.clone(), require_client))
        .with_state(mcp)
}

async fn list(State(mcp): State<Mcp>) -> Result<Json<McpServersResponse>> {
    Ok(Json(McpServersResponse {
        servers: mcp.list().await?,
    }))
}

async fn list_prompts(State(mcp): State<Mcp>) -> Result<Json<McpPromptsResponse>> {
    Ok(Json(McpPromptsResponse {
        prompts: mcp.list_prompts().await?,
    }))
}

async fn list_resources(State(mcp): State<Mcp>) -> Result<Json<McpResourcesResponse>> {
    Ok(Json(McpResourcesResponse {
        resources: mcp.list_resources().await?,
    }))
}

/// Create, then store the token, then record that a token exists.
///
/// Three steps rather than one because the vault key is derived from the
/// server's id, which does not exist until the row does.
async fn create(State(mcp): State<Mcp>, Json(body): Json<McpServerBody>) -> Result<Response> {
    let token = body.auth_token.clone();
    let server = mcp.create(new_server(body)).await?;
    if let Some(has_auth) = mcp.apply_auth_token(&server.id, token.as_deref()).await? {
        mcp.update(
            &server.id,
            ServerPatch {
                has_auth: Some(has_auth),
                ..ServerPatch::default()
            },
        )
        .await?;
    }
    resync(&mcp).await?;
    let created = mcp.get(&server.id).await?;
    Ok((StatusCode::CREATED, Json(created)).into_response())
}

async fn get_one(State(mcp): State<Mcp>, Path(id): Path<String>) -> Result<Json<McpServer>> {
    Ok(Json(mcp.get(&id).await?))
}

async fn patch_one(
    State(mcp): State<Mcp>,
    Path(id): Path<String>,
    Json(body): Json<McpServerPatchBody>,
) -> Result<Json<McpServer>> {
    let has_auth = mcp
        .apply_auth_token(&id, body.auth_token.as_deref())
        .await?;
    let mut patch = server_patch(body);
    patch.has_auth = has_auth;
    mcp.update(&id, patch.clone()).await?;
    // A change to the connection config has to drop the live session so the
    // resync reconnects with the new settings. A rename alone leaves it up.
    if patch.changes_connection() {
        mcp.disconnect(&id).await;
    }
    resync(&mcp).await?;
    Ok(Json(mcp.get(&id).await?))
}

async fn delete_one(
    State(mcp): State<Mcp>,
    Path(id): Path<String>,
) -> Result<Json<McpDeleteResponse>> {
    mcp.delete(&id).await?;
    resync(&mcp).await?;
    Ok(Json(McpDeleteResponse { ok: true }))
}

/// Force a reconnect attempt, after fixing a misconfigured server.
async fn reconnect(State(mcp): State<Mcp>, Path(id): Path<String>) -> Result<Json<McpServer>> {
    mcp.disconnect(&id).await;
    resync(&mcp).await?;
    Ok(Json(mcp.get(&id).await?))
}

/// Begin the OAuth 2.1 sign-in and return the authorization URL for the client
/// to open in a browser. A null URL means the stored tokens already worked.
///
/// The token exchange completes later, when the browser redirects to the
/// loopback listener, which flips the server to authorized and reconnects.
async fn oauth_start(
    State(mcp): State<Mcp>,
    Path(id): Path<String>,
) -> Result<Json<McpOAuthStartResponse>> {
    let server = mcp.get(&id).await?;
    if !Mcp::can_sign_in(&server) {
        return Err(validation("OAuth is only for a remote server with a url"));
    }
    let url = server.url.clone().unwrap_or_default();
    let authorization_url = mcp.start_oauth(&id, &url).await?;
    Ok(Json(McpOAuthStartResponse { authorization_url }))
}

async fn set_tool(
    State(mcp): State<Mcp>,
    Path((id, tool, action)): Path<(String, String, String)>,
) -> Result<Json<McpServer>> {
    let server = mcp.set_tool_enabled(&id, &tool, enabled(&action)?).await?;
    mcp.broadcast();
    Ok(Json(server))
}

async fn set_always_available(
    State(mcp): State<Mcp>,
    Path((id, tool, action)): Path<(String, String, String)>,
) -> Result<Json<McpServer>> {
    let server = mcp
        .set_tool_always_available(&id, &tool, enabled(&action)?)
        .await?;
    mcp.broadcast();
    Ok(Json(server))
}

async fn set_prompt(
    State(mcp): State<Mcp>,
    Path((id, prompt, action)): Path<(String, String, String)>,
) -> Result<Json<McpServer>> {
    let server = mcp
        .set_prompt_enabled(&id, &prompt, enabled(&action)?)
        .await?;
    mcp.broadcast();
    Ok(Json(server))
}

/// Resolve a prompt's messages into one instruction string with the given
/// arguments. A live round-trip to the server.
async fn resolve_prompt(
    State(mcp): State<Mcp>,
    Path((id, prompt)): Path<(String, String)>,
    Json(body): Json<McpPromptResolveBody>,
) -> Result<Json<McpPromptResolveResponse>> {
    let text = mcp
        .resolve_prompt(&id, &prompt, body.arguments.into_iter().collect())
        .await?;
    Ok(Json(McpPromptResolveResponse { text }))
}

/// The `{action}` segment is `enable` or `disable`, and nothing else.
fn enabled(action: &str) -> Result<bool> {
    match action {
        "enable" => Ok(true),
        "disable" => Ok(false),
        _ => Err(validation("bad action")),
    }
}

/// Reconcile connections to the current enabled set, then repaint.
async fn resync(mcp: &Mcp) -> Result<()> {
    mcp.resync().await
}

// The bodies' three enums are generated from the Zod schemas and the registry's
// three from the TypeScript domain types, so each wire value has two Rust
// spellings and these six functions are the seam. Exhaustive matches on
// purpose: adding a variant on either side has to fail the build rather than
// fall through to a default.

fn create_kind(kind: McpServerBodyKind) -> McpTransportKind {
    match kind {
        McpServerBodyKind::Stdio => McpTransportKind::Stdio,
        McpServerBodyKind::Remote => McpTransportKind::Remote,
    }
}

fn create_runtime(runtime: McpServerBodyRuntime) -> McpStdioRuntime {
    match runtime {
        McpServerBodyRuntime::Custom => McpStdioRuntime::Custom,
        McpServerBodyRuntime::Deno => McpStdioRuntime::Deno,
    }
}

fn create_remote_auth(auth: McpServerBodyRemoteAuth) -> McpRemoteAuth {
    match auth {
        McpServerBodyRemoteAuth::None => McpRemoteAuth::None,
        McpServerBodyRemoteAuth::Bearer => McpRemoteAuth::Bearer,
        McpServerBodyRemoteAuth::Oauth => McpRemoteAuth::Oauth,
    }
}

fn patch_kind(kind: McpServerPatchBodyKind) -> McpTransportKind {
    match kind {
        McpServerPatchBodyKind::Stdio => McpTransportKind::Stdio,
        McpServerPatchBodyKind::Remote => McpTransportKind::Remote,
    }
}

fn patch_runtime(runtime: McpServerPatchBodyRuntime) -> McpStdioRuntime {
    match runtime {
        McpServerPatchBodyRuntime::Custom => McpStdioRuntime::Custom,
        McpServerPatchBodyRuntime::Deno => McpStdioRuntime::Deno,
    }
}

fn patch_remote_auth(auth: McpServerPatchBodyRemoteAuth) -> McpRemoteAuth {
    match auth {
        McpServerPatchBodyRemoteAuth::None => McpRemoteAuth::None,
        McpServerPatchBodyRemoteAuth::Bearer => McpRemoteAuth::Bearer,
        McpServerPatchBodyRemoteAuth::Oauth => McpRemoteAuth::Oauth,
    }
}

fn new_server(body: McpServerBody) -> NewServer {
    NewServer {
        name: body.name.into(),
        kind: create_kind(body.kind),
        command: body.command,
        args: body.args,
        runtime: body.runtime.map(create_runtime),
        deno_allow_all: body.deno_allow_all,
        deno_permissions: body.deno_permissions,
        url: body.url,
        remote_auth: body.remote_auth.map(create_remote_auth),
        enabled: body.enabled,
        // Both flags are the vault's business, not the caller's: `has_auth` is
        // written from what `apply_auth_token` actually stored, and
        // `oauth_authorized` only ever by a completed sign-in.
        has_auth: None,
        oauth_authorized: None,
    }
}

fn server_patch(body: McpServerPatchBody) -> ServerPatch {
    ServerPatch {
        name: body.name.map(Into::into),
        kind: body.kind.map(patch_kind),
        command: body.command,
        args: body.args,
        runtime: body.runtime.map(patch_runtime),
        deno_allow_all: body.deno_allow_all,
        deno_permissions: body.deno_permissions,
        url: body.url,
        remote_auth: body.remote_auth.map(patch_remote_auth),
        enabled: body.enabled,
        has_auth: None,
        oauth_authorized: None,
    }
}
