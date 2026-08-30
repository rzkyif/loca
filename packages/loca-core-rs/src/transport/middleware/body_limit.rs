//! The upper bound on a single request body.
//!
//! Generous enough for legitimate attachment uploads and STT audio, but it caps
//! an unbounded body so a client (or the unauthenticated pairing caller) cannot
//! exhaust memory. WebSocket frames are not affected: they have their own cap
//! in `super::super::ws`.
//!
//! Two halves, because a body can arrive either way: a declared
//! `Content-Length` over the cap is refused before the body is read, and a
//! chunked body that declares nothing is capped by `DefaultBodyLimit` at
//! extraction time. Without the first a client could announce a gigabyte and
//! have us start reading it; without the second it could omit the header.

use axum::extract::{DefaultBodyLimit, Request};
use axum::http::header;
use axum::middleware::Next;
use axum::response::Response;

/// 100 MiB.
pub const MAX_BODY_BYTES: usize = 100 * 1024 * 1024;

/// The `DefaultBodyLimit` half, applied once to the whole router.
pub fn limit() -> DefaultBodyLimit {
    DefaultBodyLimit::max(MAX_BODY_BYTES)
}

/// The declared-length half.
pub async fn layer(req: Request, next: Next) -> Response {
    let declared = req
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    if declared.is_some_and(|n| n > MAX_BODY_BYTES as u64) {
        tracing::warn!(
            target: "loca::http",
            "{} {} -> 413: declared body of {} bytes exceeds {MAX_BODY_BYTES}",
            req.method(), req.uri().path(), declared.unwrap_or_default(),
        );
        return super::super::http::payload_too_large();
    }
    next.run(req).await
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use axum::body::{to_bytes, Body};
    use axum::http::StatusCode;
    use axum::routing::post;
    use axum::Router;
    use tower::ServiceExt;

    fn app() -> Router {
        Router::new()
            .route(
                "/echo",
                post(|body: String| async move { body.len().to_string() }),
            )
            .layer(axum::middleware::from_fn(layer))
            .layer(limit())
    }

    async fn post_body(len: Option<u64>, body: Vec<u8>) -> Response {
        let mut builder = Request::builder().method("POST").uri("/echo");
        if let Some(len) = len {
            builder = builder.header(header::CONTENT_LENGTH, len.to_string());
        }
        app()
            .oneshot(builder.body(Body::from(body)).unwrap())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_body_within_the_cap_passes_through() {
        let res = post_body(Some(3), b"abc".to_vec()).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(to_bytes(res.into_body(), usize::MAX).await.unwrap(), "3");
    }

    #[tokio::test]
    async fn a_declared_length_over_the_cap_is_refused_before_the_body_is_read() {
        let res = post_body(Some(MAX_BODY_BYTES as u64 + 1), Vec::new()).await;
        assert_eq!(res.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let body: serde_json::Value =
            serde_json::from_slice(&to_bytes(res.into_body(), usize::MAX).await.unwrap()).unwrap();
        assert_eq!(body["error"]["code"], "validation_error");
    }

    #[tokio::test]
    async fn a_length_that_is_not_a_number_does_not_bypass_the_cap() {
        // Unparseable reads as "undeclared", still covered at extraction.
        let res = app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/echo")
                    .header(header::CONTENT_LENGTH, "not-a-number")
                    .body(Body::from("abc"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(res.status() == StatusCode::OK || res.status() == StatusCode::BAD_REQUEST);
    }
}
