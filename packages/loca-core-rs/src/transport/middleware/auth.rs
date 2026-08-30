//! Authentication layers and the extractors that read what they resolved.
//!
//! A route slice guards itself:
//!
//! ```ignore
//! Router::new()
//!     .route("/api/v1/settings", get(list).patch(patch))
//!     .layer(from_fn_with_state(ctx.clone(), require_client))
//! ```
//!
//! and its handlers take an [`AuthedClient`] extractor. The transport does not
//! know how a bearer becomes a client; it calls the [`ClientAuth`] seam the
//! pairing package implements.

use axum::extract::{FromRequestParts, Request, State};
use axum::http::header;
use axum::http::request::Parts;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::error::{AppError, ErrorCode, Result};

use super::super::state::{AppContext, ClientAuth, ClientIdentity};

/// The socket peer address, as the listener saw it. Deliberately NOT
/// `X-Forwarded-For` or `X-Real-IP`, which a client sets: rotating them would
/// hand every pairing guess a fresh, empty rate-limit bucket. Behind a proxy the
/// per-IP limiter collapses to one shared bucket, which is stricter and safe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerAddr(pub String);

/// The peer name used when there is no socket at all: the in-process path.
pub const LOCAL_PEER: &str = "local";

impl<S: Send + Sync> FromRequestParts<S> for PeerAddr {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(parts
            .extensions
            .get::<PeerAddr>()
            .cloned()
            .unwrap_or_else(|| PeerAddr(LOCAL_PEER.to_string())))
    }
}

/// The authenticated client, for a handler behind [`require_client`].
#[derive(Clone, Debug)]
pub struct AuthedClient(pub ClientIdentity);

impl std::ops::Deref for AuthedClient {
    type Target = ClientIdentity;
    fn deref(&self) -> &ClientIdentity {
        &self.0
    }
}

impl<S: Send + Sync> FromRequestParts<S> for AuthedClient {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<ClientIdentity>()
            .cloned()
            .map(AuthedClient)
            // Unreachable when the slice applied `require_client`. A 401
            // rather than a panic, so forgetting the layer fails the request
            // rather than the core.
            .ok_or_else(|| {
                tracing::error!(
                    target: "loca::http",
                    "{} {} reads AuthedClient without require_client on its route",
                    parts.method, parts.uri.path(),
                );
                AppError::new(
                    ErrorCode::MissingToken,
                    "missing Authorization: Bearer header",
                )
            })
    }
}

/// Resolve `Authorization: Bearer <token>` to a [`ClientIdentity`] and put it
/// in the request extensions.
///
/// An identity already in the extensions is honoured as-is and no bearer is
/// required, which is how the in-process mobile client is pre-authenticated.
/// Safe because `http::Extensions` cannot be populated from the wire: the
/// listener inserts nothing but a [`PeerAddr`], and it must stay that way.
pub async fn require_client(
    State(ctx): State<AppContext>,
    mut req: Request,
    next: Next,
) -> Response {
    if req.extensions().get::<ClientIdentity>().is_none() {
        match resolve(ctx.auth.as_ref(), &req) {
            Ok(client) => {
                req.extensions_mut().insert(client);
            }
            Err(err) => return logged(err, &req),
        }
    }
    next.run(req).await
}

/// Guard the pairing-code mint with the on-disk admin token.
pub async fn require_admin_token(
    State(ctx): State<AppContext>,
    req: Request,
    next: Next,
) -> Response {
    let provided = header_str(&req, "x-admin-token");
    if let Err(err) = ctx.auth.verify_admin_token(provided.as_deref()) {
        return logged(err, &req);
    }
    next.run(req).await
}

fn resolve(auth: &dyn ClientAuth, req: &Request) -> Result<ClientIdentity> {
    let header = header_str(req, header::AUTHORIZATION.as_str()).unwrap_or_default();
    auth.authenticate(bearer(&header))
}

/// `Bearer <token>`, case-insensitive on the scheme, trimmed.
fn bearer(header: &str) -> Option<&str> {
    let rest = header
        .strip_prefix("Bearer ")
        .or_else(|| header.strip_prefix("bearer "))
        .or_else(|| {
            let (scheme, rest) = header.split_once(' ')?;
            scheme.eq_ignore_ascii_case("bearer").then_some(rest)
        })?;
    let token = rest.trim();
    (!token.is_empty()).then_some(token)
}

fn header_str(req: &Request, name: &str) -> Option<String> {
    req.headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

/// Render an error a layer produced, with the method and path a handler would
/// not have had.
fn logged(err: AppError, req: &Request) -> Response {
    super::super::http::log_error(
        req.method().as_str(),
        req.uri().path(),
        err.code,
        &err.message,
    );
    err.into_response()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn a_bearer_header_is_parsed_case_insensitively_and_trimmed() {
        assert_eq!(bearer("Bearer abc"), Some("abc"));
        assert_eq!(bearer("bearer abc"), Some("abc"));
        assert_eq!(bearer("BEARER   abc  "), Some("abc"));
    }

    #[test]
    fn anything_that_is_not_a_bearer_resolves_to_no_token() {
        // Each is `missing_token`, not `invalid_token`: nothing to reject.
        for header in ["", "abc", "Basic abc", "Bearer", "Bearer   ", "Bearerabc"] {
            assert_eq!(bearer(header), None, "wrongly parsed: {header:?}");
        }
    }
}
