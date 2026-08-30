//! The socket-free path: how the mobile app reaches the core.
//!
//! On mobile there is no listener, deliberately. `127.0.0.1` is not app-private
//! on Android, so binding it would let any installed app on the device reach
//! the Core. The client instead links the library and calls it in-process
//! through Tauri commands.
//!
//! The seam is [`super::Core::router`], an `axum::Router` and so a
//! `tower::Service`. Calling it is `oneshot`: no socket, no TLS, no bound port,
//! and the same routing, extractors, middleware and error mapping as the wire
//! path. The request and reply shapes mirror the client's `net_fetch` command,
//! keeping the marshalling here rather than in untested Tauri glue.

use std::collections::HashMap;

use axum::body::{to_bytes, Body};
use axum::http::{HeaderName, HeaderValue, Method, Request};
use tower::ServiceExt;

use crate::error::{internal, validation, Result};

use super::middleware::body_limit::MAX_BODY_BYTES;
use super::state::ClientIdentity;
use super::Core;

/// One in-process request.
#[derive(Clone, Debug)]
pub struct InProcRequest {
    pub method: String,
    /// Path and query. A full URL is accepted too, so a caller can hand over
    /// whatever the client-side code already built.
    pub url: String,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
    /// The pre-authenticated local client, inserted into the request
    /// extensions so the auth layer skips the bearer check. Only server-side
    /// code can set an extension, so a socket cannot forge one.
    pub client: Option<ClientIdentity>,
}

impl InProcRequest {
    pub fn get(url: impl Into<String>) -> Self {
        Self {
            method: "GET".to_string(),
            url: url.into(),
            headers: HashMap::new(),
            body: Vec::new(),
            client: None,
        }
    }

    pub fn as_client(mut self, client: ClientIdentity) -> Self {
        self.client = Some(client);
        self
    }

    pub fn with_body(mut self, method: &str, content_type: &str, body: Vec<u8>) -> Self {
        self.method = method.to_string();
        self.headers
            .insert("content-type".to_string(), content_type.to_string());
        self.body = body;
        self
    }
}

/// One in-process reply.
#[derive(Clone, Debug)]
pub struct InProcResponse {
    pub status: u16,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

impl Core {
    /// Handle a request with no socket underneath it. The body is buffered:
    /// mobile's streaming surfaces are WS frames, through [`Core::connect`].
    pub async fn call(&self, req: InProcRequest) -> Result<InProcResponse> {
        let request = build_request(req)?;
        let response = self
            .router()
            .oneshot(request)
            .await
            .map_err(|e| internal(format!("in-process dispatch failed: {e}")))?;

        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .filter_map(|(k, v)| {
                v.to_str()
                    .ok()
                    .map(|v| (k.as_str().to_lowercase(), v.to_string()))
            })
            .collect();
        let body = to_bytes(response.into_body(), MAX_BODY_BYTES)
            .await
            .map_err(|e| internal(format!("in-process body read failed: {e}")))?
            .to_vec();

        Ok(InProcResponse {
            status,
            headers,
            body,
        })
    }
}

fn build_request(req: InProcRequest) -> Result<Request<Body>> {
    let method = Method::from_bytes(req.method.as_bytes())
        .map_err(|e| validation(format!("bad method {:?}: {e}", req.method)))?;
    let mut builder = Request::builder().method(method).uri(&req.url);

    for (name, value) in &req.headers {
        // A header that cannot be represented fails the call: a silently
        // dropped content-type would be rejected far downstream instead.
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|e| validation(format!("bad header name {name:?}: {e}")))?;
        let value = HeaderValue::from_str(value)
            .map_err(|e| validation(format!("bad header value for {name:?}: {e}")))?;
        builder = builder.header(name, value);
    }

    let mut request = builder
        .body(Body::from(req.body))
        .map_err(|e| validation(format!("bad request: {e}")))?;
    if let Some(client) = req.client {
        request.extensions_mut().insert(client);
    }
    Ok(request)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::transport::testing;

    #[tokio::test]
    async fn a_request_reaches_the_router_with_no_listener_bound() {
        let core = testing::core();
        let res = core
            .call(InProcRequest::get("/api/v1/health"))
            .await
            .unwrap();
        assert_eq!(res.status, 200);
        assert_eq!(res.headers["content-type"], "application/json");
        let body: serde_json::Value = serde_json::from_slice(&res.body).unwrap();
        assert_eq!(body["status"], "ok");
    }

    #[tokio::test]
    async fn an_unknown_path_returns_the_same_envelope_it_would_over_the_wire() {
        let core = testing::core();
        let res = core.call(InProcRequest::get("/api/v1/nope")).await.unwrap();
        assert_eq!(res.status, 404);
        let body: serde_json::Value = serde_json::from_slice(&res.body).unwrap();
        assert_eq!(body["error"]["code"], "not_found");
    }

    #[tokio::test]
    async fn a_malformed_method_or_header_fails_the_call_rather_than_being_dropped() {
        let core = testing::core();
        let mut req = InProcRequest::get("/api/v1/health");
        req.method = "BAD METHOD".to_string();
        assert!(core.call(req).await.is_err());

        let mut req = InProcRequest::get("/api/v1/health");
        req.headers.insert("bad name".to_string(), "v".to_string());
        assert!(core.call(req).await.is_err());
    }

    #[tokio::test]
    async fn a_full_url_is_accepted_as_well_as_a_path() {
        let core = testing::core();
        let res = core
            .call(InProcRequest::get("https://127.0.0.1:7800/api/v1/health"))
            .await
            .unwrap();
        assert_eq!(res.status, 200);
    }
}
