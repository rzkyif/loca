//! The pairing and admin route slice.
//!
//! `/api/v1/pairing/*` and `/api/v1/admin/password`. The WebSocket ticket route
//! lives with the socket it serves, in the transport.
//!
//! Three of these are deliberately unauthenticated, and they are the only
//! unauthenticated routes outside `/api/v1/health`: minting a code with the
//! admin token, and the two handshake steps, which cannot require a bearer
//! because obtaining one is what they are for. Their protection is the code, the
//! CPace confirmation, and the per-IP budget.
//!
//! Inbound bodies deserialize through `contract::inbound`, whose `Deserialize`
//! already enforces the schema constraints, so nothing re-checks them here.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap};
use axum::response::Response;
use axum::routing::{delete, get, post};
use axum::Router;
use base64::alphabet;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use base64::Engine as _;
use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::contract::api::{
    PairedClientEntry, PairingCodeResponse, PakeFinishResponse, PakeStartResponse,
};
use crate::contract::inbound::{
    AdminPasswordSetRequest, ClientRevokeRequest, PairingCodeRequest, PakeFinishRequest,
    PakeStartRequest,
};
use crate::error::{not_found, AppError, ErrorCode, Result};
use crate::transport::http::{no_content, Json};
use crate::transport::middleware::{from_fn_with_state, require_admin_token, require_client};
use crate::transport::middleware::{AuthedClient, PeerAddr};
use crate::transport::state::AppContext;

use super::{Auth, PairedClient, PairingCode, PakeFinished, PakeStarted};

/// Padded standard base64 on the way out, matching `btoa` on the Client.
const B64: GeneralPurpose = GeneralPurpose::new(&alphabet::STANDARD, GeneralPurposeConfig::new());

/// Padded or unpadded on the way in, matching Deno's `decodeBase64`, which
/// accepts both. The generated inbound types have already constrained the
/// alphabet to `[A-Za-z0-9+/]+=*`, so nothing base64url-shaped reaches here.
const B64_IN: GeneralPurpose = GeneralPurpose::new(
    &alphabet::STANDARD,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

// --- wire responses ----------------------------------------------------------
//
// The pairing responses come from `contract::api`. Rotation is the exception:
// its token is consumed by the Client's transport rather than rendered, so it
// has no shared interface and is declared here.

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RotateResponse {
    token: String,
}

/// The shared context plus the concrete pairing service, which is threaded in
/// because it holds the in-flight handshakes, the ticket table and the
/// rate-limit buckets: a second instance would have its own.
#[derive(Clone)]
struct PairingState {
    ctx: AppContext,
    auth: Arc<Auth>,
}

pub fn routes(ctx: &AppContext, auth: Arc<Auth>) -> Router {
    let state = PairingState {
        ctx: ctx.clone(),
        auth,
    };

    // Obtaining a bearer is what these are for, so they cannot require one.
    let public = Router::new()
        .route("/api/v1/pairing/codes", post(mint_code))
        .route("/api/v1/pairing/pake/start", post(pake_start))
        .route("/api/v1/pairing/pake/finish", post(pake_finish))
        .with_state(state.clone());

    let bearer = Router::new()
        .route("/api/v1/pairing/clients", get(list_clients))
        .route("/api/v1/pairing/clients/{id}", delete(revoke_client))
        .route("/api/v1/pairing/rotate", post(rotate))
        .layer(from_fn_with_state(ctx.clone(), require_client))
        .with_state(state.clone());

    // Device access, not a paired device: only someone who can read
    // `<core>/.admin-token` may set or replace the password.
    let admin = Router::new()
        .route("/api/v1/admin/password", post(set_admin_password))
        .layer(from_fn_with_state(ctx.clone(), require_admin_token))
        .with_state(state);

    public.merge(bearer).merge(admin)
}

// --- handlers ----------------------------------------------------------------

/// Mint a pairing code. Two authorization paths, and only two: `X-Admin-Token`
/// is device access and is high-entropy enough to carry no rate limit; a bearer
/// plus the admin password is an already-paired client minting remotely, with
/// the password as an argon2id-hashed, throttled second factor.
async fn mint_code(
    State(state): State<PairingState>,
    PeerAddr(ip): PeerAddr,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<PairingCodeResponse>> {
    let body: PairingCodeRequest = body_or_default(&body)?;
    match header_str(&headers, "x-admin-token") {
        // Presenting a token means the caller intends that path, so a wrong
        // one is reported rather than retried as a bearer.
        Some(token) => state.auth.verify_admin_token(Some(&token)).await?,
        None => {
            let client = state.auth.authenticate(bearer(&headers)).await?;
            state
                .auth
                .verify_admin_password(
                    body.password.as_deref().map(String::as_str),
                    &ip,
                    &client.id,
                )
                .await?;
        }
    }
    let ttl_sec = body.ttl_sec.and_then(|secs| u32::try_from(secs).ok());
    Ok(Json(to_code_response(
        state.auth.mint_pairing_code(ttl_sec).await?,
    )))
}

/// Step 1 of the handshake: the Client runs the CPace initiator keyed by the
/// pairing code and sends its session id and first message; Core answers with
/// its own.
async fn pake_start(
    State(state): State<PairingState>,
    PeerAddr(ip): PeerAddr,
    Json(body): Json<PakeStartRequest>,
) -> Result<Json<PakeStartResponse>> {
    let sid = decode_b64(&body.sid, "sid")?;
    let msg_a = decode_b64(&body.msg_a, "msgA")?;
    let started = state
        .auth
        .pake_start(
            &sid,
            &msg_a,
            &body.client_name,
            &ip,
            &server_pin(&state.ctx),
        )
        .await?;
    Ok(Json(to_start_response(started)))
}

/// Step 2: the Client confirms, and its confirmation binds the certificate pin
/// it observed. Core verifies against its own pin, so a wrong code and a
/// substituted certificate both fail here.
async fn pake_finish(
    State(state): State<PairingState>,
    Json(body): Json<PakeFinishRequest>,
) -> Result<Json<PakeFinishResponse>> {
    let confirm_c = decode_b64(&body.confirm_c, "confirmC")?;
    let finished = state
        .auth
        .pake_finish(&body.pake_id, &confirm_c, &server_pin(&state.ctx))
        .await?;
    Ok(Json(to_finish_response(finished)))
}

async fn list_clients(
    State(state): State<PairingState>,
    client: AuthedClient,
) -> Result<Json<Vec<PairedClientEntry>>> {
    let listed = state.auth.list_clients(&client.id).await?;
    Ok(Json(listed.into_iter().map(to_client_entry).collect()))
}

/// Remove a paired device.
///
/// A client may always remove itself, and `me` is an alias for its own id.
/// Removing a different device is privileged, authorized the same two ways as
/// minting a code: without the gate one paired device could lock out the rest.
async fn revoke_client(
    State(state): State<PairingState>,
    client: AuthedClient,
    PeerAddr(ip): PeerAddr,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response> {
    let target = if id == "me" { client.id.clone() } else { id };
    if target != client.id {
        let body: ClientRevokeRequest = body_or_default(&body)?;
        authorize_privileged(
            &state,
            &headers,
            body.password.as_deref().map(String::as_str),
            &ip,
            &client.id,
        )
        .await?;
    }
    if !state.auth.revoke_client(&target).await? {
        return Err(not_found(format!("no paired client with id {target}")));
    }
    tracing::info!(target: "loca::http.pairing", "client {} revoked client {target}", client.id);
    Ok(no_content())
}

async fn rotate(
    State(state): State<PairingState>,
    client: AuthedClient,
) -> Result<Json<RotateResponse>> {
    let token = state.auth.rotate_token(&client.id).await?;
    Ok(Json(RotateResponse {
        token: token.into_inner(),
    }))
}

/// Set or replace the admin password. The layer has already proved device
/// access; the length floor is enforced by the generated inbound type.
async fn set_admin_password(
    State(state): State<PairingState>,
    Json(body): Json<AdminPasswordSetRequest>,
) -> Result<Response> {
    state.auth.set_admin_password(&body.password).await?;
    Ok(no_content())
}

// --- helpers -----------------------------------------------------------------

/// The value folded into the pairing handshake, binding the TLS channel to it:
/// Core's own certificate fingerprint, so a man in the middle that re-terminates
/// TLS is detected. Behind a terminating HTTPS proxy the Client validates the
/// proxy's certificate through WebPKI and cannot observe Core's, so both sides
/// fold nothing. A Core that has a certificate always folds its real one, so a
/// client that folded nothing fails the confirmation.
///
/// **An empty pin has two causes.** The deliberate one above; or the Core has no
/// TLS material at all, which today means an embedded Core, and falls through
/// the `unwrap_or_default()` rather than the `behind_proxy` branch. That is safe
/// only because an embedded Core binds no listener. **Give one a listener and
/// pairing proceeds with no channel binding at all**, so anything that does
/// must refuse to pair here first.
fn server_pin(ctx: &AppContext) -> String {
    if ctx.behind_proxy {
        String::new()
    } else {
        ctx.tls_pin.clone().unwrap_or_default()
    }
}

/// Authorize a privileged fleet action the same two ways minting a code is: the
/// admin token, or the admin password as a second factor. The caller is already
/// bearer-authenticated, so there is no third path and no identity to
/// establish.
async fn authorize_privileged(
    state: &PairingState,
    headers: &HeaderMap,
    password: Option<&str>,
    ip: &str,
    client_id: &str,
) -> Result<()> {
    match header_str(headers, "x-admin-token") {
        Some(token) => state.auth.verify_admin_token(Some(&token)).await,
        None => {
            state
                .auth
                .verify_admin_password(password, ip, client_id)
                .await
        }
    }
}

/// Parse a body that is allowed to be absent: the mint and the revoke both take
/// an optional one, so empty is the default rather than a validation error.
fn body_or_default<T: DeserializeOwned + Default>(body: &Bytes) -> Result<T> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(T::default());
    }
    Ok(serde_json::from_slice(body)?)
}

fn header_str(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

/// `Bearer <token>`, case-insensitive on the scheme. Read by the two routes
/// `require_client` deliberately does not guard, where a caller may legitimately
/// have no bearer; `authenticate` turns `None` into `missing_token`.
fn bearer(headers: &HeaderMap) -> Option<&str> {
    let header = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, rest) = header.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let token = rest.trim();
    (!token.is_empty()).then_some(token)
}

fn decode_b64(value: &str, field: &str) -> Result<Vec<u8>> {
    B64_IN.decode(value).map_err(|_| {
        AppError::new(
            ErrorCode::ValidationError,
            format!("{field} must be base64"),
        )
    })
}

fn to_code_response(code: PairingCode) -> PairingCodeResponse {
    PairingCodeResponse {
        code: code.code,
        expires_at_ms: code.expires_at_ms,
    }
}

fn to_start_response(started: PakeStarted) -> PakeStartResponse {
    PakeStartResponse {
        pake_id: started.pake_id,
        msg_b: B64.encode(started.msg_b),
    }
}

fn to_finish_response(finished: PakeFinished) -> PakeFinishResponse {
    PakeFinishResponse {
        confirm_s: B64.encode(finished.confirm_s),
        token: finished.token.into_inner(),
        client_id: finished.client_id,
        core_version: finished.core_version,
    }
}

fn to_client_entry(client: PairedClient) -> PairedClientEntry {
    PairedClientEntry {
        id: client.id,
        name: client.name,
        created_at_ms: client.created_at_ms,
        last_seen_ms: client.last_seen_ms,
        is_me: client.is_me,
    }
}
