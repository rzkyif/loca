//! The HTTP half of the command boundary: `core_http`.
//!
//! Deliberately the same shape as the client's `net_fetch` command with the
//! network taken out, so the client-side local transport is a near-copy of the
//! one it already has. What is dropped is dropped rather than ignored:
//!
//! - `mode` / `pin`: there is no TLS in process, so there is no trust posture
//!   to pick. Accepting a `mode` and ignoring it would let a caller believe it
//!   had pinned something.
//! - `capturedPin`: only ever filled by the unpaired capture probe.
//!
//! **An HTTP-level failure is not an error here.** A 401, a 404 and a 500 all
//! come back as an `Ok` carrying that status and the core's standard error
//! envelope in the body, exactly as they would over a socket. Only a request
//! that could not be formed at all, or a core that is not running, is an `Err`.
//! That is what lets the client reuse its existing response handling unchanged.

use std::collections::HashMap;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use serde::{Deserialize, Serialize};

use loca_core::error::{validation, Result};
use loca_core::transport::{ClientIdentity, Core, InProcRequest};

/// One request from the webview.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CoreHttpRequest {
    /// Path and query, or a whole URL. `InProcRequest` accepts either, so the
    /// caller can hand over whatever it already built.
    pub url: String,
    pub method: String,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    /// Base64, matching `net_fetch`. `None` and an empty body are the same
    /// thing here, because no core route distinguishes them.
    #[serde(default)]
    pub body_b64: Option<String>,
}

/// One reply to the webview.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CoreHttpReply {
    pub status: u16,
    pub headers: HashMap<String, String>,
    pub body_b64: String,
}

/// Dispatch one request in process, as `client`.
///
/// Takes the pieces rather than the whole [`crate::LocalCore`] so a test drives
/// this exact function against a fixture core. A test that needed its own
/// construction would be testing a copy (D-084).
pub async fn call(
    core: &Core,
    client: &ClientIdentity,
    req: CoreHttpRequest,
) -> Result<CoreHttpReply> {
    let body = match req.body_b64.as_deref() {
        // An undecodable body fails the call rather than being sent as nothing.
        // Silently substituting an empty body turns a client bug into a
        // confusing rejection several layers downstream.
        Some(encoded) => B64
            .decode(encoded.as_bytes())
            .map_err(|e| validation(format!("bad request body: {e}")))?,
        None => Vec::new(),
    };

    let mut inproc = InProcRequest::get(req.url);
    inproc.method = req.method;
    inproc.headers = req.headers;
    inproc.body = body;
    // The whole auth story on this path. Only server-side code can put an
    // extension on a request, so this cannot be forged from the webview: the
    // command layer supplies the identity, not the caller.
    let inproc = inproc.as_client(client.clone());

    let res = core.call(inproc).await?;
    Ok(CoreHttpReply {
        status: res.status,
        headers: res.headers,
        body_b64: B64.encode(&res.body),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use loca_core::transport::testing;

    /// A fixture core whose database has its schema and the reserved local
    /// client row, which is what production has after boot.
    ///
    /// `testing::core()` opens an in-memory database and does NOT migrate it,
    /// so any route that touches a table answers 500 rather than doing its job.
    /// Migrating here rather than working around it keeps the assertions below
    /// about this crate instead of about the fixture.
    async fn core() -> loca_core::transport::Core {
        let core = bare_core();
        add_local_row(&core).await;
        core
    }

    /// The same, without the reserved row, so a test can stage its absence.
    fn bare_core() -> loca_core::transport::Core {
        let core = testing::core();
        core.context()
            .db
            .call_blocking(|conn| loca_core::db::migrate(conn))
            .expect("migrate the fixture database");
        core
    }

    async fn add_local_row(core: &loca_core::transport::Core) {
        let ctx = core.context();
        loca_core::auth::Auth::new(ctx.db.clone(), std::sync::Arc::clone(&ctx.paths))
            .ensure_local_client()
            .await
            .expect("the reserved local client row");
    }

    fn patch_settings() -> CoreHttpRequest {
        CoreHttpRequest {
            url: "/api/v1/settings".to_string(),
            method: "PATCH".to_string(),
            headers: HashMap::from([("content-type".to_string(), "application/json".to_string())]),
            body_b64: Some(B64.encode(br#"{"llm.temperature":0.5}"#)),
        }
    }

    fn client() -> ClientIdentity {
        ClientIdentity {
            id: "local".to_string(),
            name: "This device".to_string(),
            created_at_ms: 0,
            last_seen_ms: 0,
        }
    }

    fn get(url: &str) -> CoreHttpRequest {
        CoreHttpRequest {
            url: url.to_string(),
            method: "GET".to_string(),
            headers: HashMap::new(),
            body_b64: None,
        }
    }

    fn body(reply: &CoreHttpReply) -> serde_json::Value {
        let raw = B64.decode(reply.body_b64.as_bytes()).unwrap();
        serde_json::from_slice(&raw).unwrap()
    }

    #[tokio::test]
    async fn a_request_reaches_the_router_with_no_listener_bound() {
        let core = core().await;
        let reply = call(&core, &client(), get("/api/v1/health")).await.unwrap();
        assert_eq!(reply.status, 200);
        assert_eq!(body(&reply)["status"], "ok");
    }

    #[tokio::test]
    async fn response_headers_survive_the_round_trip() {
        let core = core().await;
        let reply = call(&core, &client(), get("/api/v1/health")).await.unwrap();
        assert_eq!(
            reply.headers.get("content-type").map(String::as_str),
            Some("application/json"),
        );
    }

    #[tokio::test]
    async fn an_http_failure_is_a_successful_call_carrying_the_status() {
        // The property the client's whole error handling rests on: a 404 is a
        // reply, not an `Err`. If this ever inverts, every core error becomes an
        // opaque IPC string and the error envelope stops being readable.
        let core = core().await;
        let reply = call(&core, &client(), get("/api/v1/nope")).await.unwrap();
        assert_eq!(reply.status, 404);
        assert_eq!(body(&reply)["error"]["code"], "not_found");
    }

    #[tokio::test]
    async fn an_authenticated_route_succeeds_because_the_identity_was_injected() {
        // `testing::core()` uses a `StubAuth` that knows no tokens at all, so a
        // 200 here can only come from the injected identity. That is what makes
        // this a test of the injection rather than of the route.
        let core = core().await;
        let reply = call(&core, &client(), get("/api/v1/settings"))
            .await
            .unwrap();
        assert_eq!(reply.status, 200);
    }

    #[tokio::test]
    async fn the_same_route_is_401_when_no_identity_is_injected() {
        // The other half, and the one that gives the test above its teeth: drive
        // the underlying request WITHOUT `as_client` and watch the same route
        // refuse. Without this, `call` could be injecting nothing and the test
        // above would still pass on a route that happened not to be guarded.
        let core = core().await;
        let res = core
            .call(InProcRequest::get("/api/v1/settings"))
            .await
            .unwrap();
        assert_eq!(res.status, 401);
    }

    #[tokio::test]
    async fn a_body_arrives_as_the_exact_bytes_that_were_encoded() {
        let core = core().await;
        let reply = call(&core, &client(), patch_settings()).await.unwrap();
        // A 200 proves the bytes arrived and parsed: the same call with a
        // corrupted or empty body is a 400 from the deserializer.
        assert_eq!(reply.status, 200, "body: {:?}", body(&reply));
    }

    #[tokio::test]
    async fn a_per_client_write_needs_the_reserved_row_to_exist() {
        // The whole justification for `Auth::ensure_local_client`, staged both
        // ways rather than asserted once (D-015). `client_settings.client_id`
        // is `REFERENCES clients(id)`, so the in-process identity cannot be
        // invented: without the row the write fails on the foreign key, and
        // every per-client setting on mobile would be unwritable.
        //
        // This is not hypothetical. It is what this test did before the row was
        // added, and the failure was a 500 reading "FOREIGN KEY constraint
        // failed", which is exactly how it would have reached a user.
        let bare = bare_core();
        let refused = call(&bare, &client(), patch_settings()).await.unwrap();
        assert_eq!(refused.status, 500);
        assert_eq!(body(&refused)["error"]["code"], "internal_error");

        add_local_row(&bare).await;
        let accepted = call(&bare, &client(), patch_settings()).await.unwrap();
        assert_eq!(accepted.status, 200, "body: {:?}", body(&accepted));
    }

    #[tokio::test]
    async fn the_reserved_row_is_unreachable_with_any_bearer() {
        // The row stores the literal `local` as its `token_hash`, and
        // `authenticate` compares `sha256_hex(bearer)`, which is always 64 hex
        // characters. So no bearer can ever resolve to this identity, which is
        // what keeps a row that bypasses auth from being a way in.
        let core = bare_core();
        let ctx = core.context();
        let auth = loca_core::auth::Auth::new(ctx.db.clone(), std::sync::Arc::clone(&ctx.paths));
        auth.ensure_local_client().await.unwrap();

        for attempt in ["local", "", "Local", &"a".repeat(64)] {
            assert!(
                auth.authenticate(Some(attempt)).await.is_err(),
                "bearer {attempt:?} resolved to a client",
            );
        }
    }

    #[tokio::test]
    async fn a_corrupted_body_does_not_reach_the_router_as_an_empty_one() {
        let core = core().await;
        let mut req = get("/api/v1/settings");
        req.method = "PATCH".to_string();
        req.body_b64 = Some("not base64!!".to_string());
        assert!(call(&core, &client(), req).await.is_err());
    }

    #[tokio::test]
    async fn a_malformed_method_or_header_fails_the_call() {
        let core = core().await;
        let mut req = get("/api/v1/health");
        req.method = "BAD METHOD".to_string();
        assert!(call(&core, &client(), req).await.is_err());

        let mut req = get("/api/v1/health");
        req.headers.insert("bad name".to_string(), "v".to_string());
        assert!(call(&core, &client(), req).await.is_err());
    }

    #[tokio::test]
    async fn an_absent_body_is_an_empty_one() {
        let core = core().await;
        let reply = call(&core, &client(), get("/api/v1/health")).await.unwrap();
        assert_eq!(reply.status, 200);
    }
}
