//! Allowlisted CORS.
//!
//! The core's only browser caller is the Tauri webview, whose origin is
//! `tauri://localhost` on macOS and Linux and `https://tauri.localhost` on
//! Windows, plus the Vite dev server on a loopback port. Native HTTP clients
//! (the Tauri Rust side, curl, anything using a raw socket) do not enforce CORS
//! at all, so omitting the headers for them is harmless.
//!
//! Why this matters even on loopback: the API accepts `X-Admin-Token`, a custom
//! header. Without an origin allowlist a malicious page could trigger a
//! preflight against a core bound on `0.0.0.0` and then send the token-stealing
//! request. Reflecting `*` alongside `X-Admin-Token` in `Allow-Headers` would
//! be a wildcard credential grant.
//!
//! Hand-written rather than `tower-http`'s `CorsLayer` because three behaviours
//! here are not that crate's defaults and all three are load bearing: a
//! preflight from a disallowed origin answers 204 with no CORS headers at all;
//! a request with no `Origin` gets no CORS headers; and the headers are applied
//! to the finished response rather than before the handler, so a handler that
//! returns its own `Response` (the `audio/wav` body) does not drop them.

use axum::extract::Request;
use axum::http::{header, HeaderName, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::Response;

const ALLOWED_ORIGINS: [&str; 2] = ["tauri://localhost", "https://tauri.localhost"];
const ALLOW_METHODS: &str = "GET,POST,PUT,PATCH,DELETE,OPTIONS";
const ALLOW_HEADERS: &str = "Authorization,Content-Type,X-Admin-Token";
const MAX_AGE: &str = "86400";

/// Whether an `Origin` may see a response.
///
/// Also used on the WS upgrade, where rejecting a cross-origin browser upgrade
/// is defence in depth on top of the ticket check.
pub fn is_origin_allowed(origin: &str) -> bool {
    if ALLOWED_ORIGINS.contains(&origin) {
        return true;
    }
    is_loopback_origin(origin)
}

/// `^https?://(localhost|127\.0\.0\.1)(:\d+)?$`, spelled out rather than
/// compiled, because a regex here would be a per-request allocation on the hot
/// path and the grammar is four cases wide.
///
/// The anchoring is the point: `https://x.tauri.localhost` and
/// `http://127.0.0.2` are not loopback, and a substring match would accept
/// both.
fn is_loopback_origin(origin: &str) -> bool {
    let rest = match origin.strip_prefix("https://") {
        Some(rest) => rest,
        None => match origin.strip_prefix("http://") {
            Some(rest) => rest,
            None => return false,
        },
    };
    let (host, port) = match rest.split_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (rest, None),
    };
    if host != "localhost" && host != "127.0.0.1" {
        return false;
    }
    match port {
        None => true,
        Some(p) => !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()),
    }
}

pub async fn layer(req: Request, next: Next) -> Response {
    let origin = req
        .headers()
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let allowed = origin.as_deref().is_some_and(is_origin_allowed);

    // Preflight: answered here, never routed. A disallowed origin still gets a
    // 204, just without the headers that would let it proceed.
    if req.method() == Method::OPTIONS {
        let mut res = Response::new(axum::body::Body::empty());
        *res.status_mut() = StatusCode::NO_CONTENT;
        if allowed {
            apply(&mut res, origin.as_deref().unwrap_or_default());
        }
        return res;
    }

    let mut res = next.run(req).await;
    if allowed {
        apply(&mut res, origin.as_deref().unwrap_or_default());
    }
    // No `Origin` (curl, the Tauri Rust side, server to server) means no CORS
    // headers. CORS only gates browser-driven cross-origin fetches.
    res
}

fn apply(res: &mut Response, origin: &str) {
    let headers = res.headers_mut();
    if let Ok(value) = HeaderValue::from_str(origin) {
        headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, value);
    }
    headers.insert(header::VARY, HeaderValue::from_static("Origin"));
    for (name, value) in [
        (header::ACCESS_CONTROL_ALLOW_METHODS, ALLOW_METHODS),
        (header::ACCESS_CONTROL_ALLOW_HEADERS, ALLOW_HEADERS),
        (header::ACCESS_CONTROL_MAX_AGE, MAX_AGE),
    ] {
        headers.insert::<HeaderName>(name, HeaderValue::from_static(value));
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::routing::get;
    use axum::Router;
    use tower::ServiceExt;

    #[test]
    fn the_tauri_webview_origins_are_allowed() {
        assert!(is_origin_allowed("tauri://localhost"));
        assert!(is_origin_allowed("https://tauri.localhost"));
    }

    #[test]
    fn loopback_is_allowed_on_any_port() {
        assert!(is_origin_allowed("http://localhost"));
        assert!(is_origin_allowed("http://localhost:1420"));
        assert!(is_origin_allowed("http://127.0.0.1:7800"));
        assert!(is_origin_allowed("https://localhost:8443"));
    }

    #[test]
    fn everything_else_is_rejected() {
        for origin in [
            "https://evil.example",
            "http://attacker.local",
            "null",
            "file://",
            // A subdomain of the allowed host is a different origin.
            "https://x.tauri.localhost",
            // A neighbouring loopback address is not loopback.
            "http://127.0.0.2",
            // Anchoring: neither a prefix nor a suffix match counts.
            "http://localhost.evil.example",
            "https://evil.example/http://localhost",
            "http://localhost:notaport",
            "http://localhost:",
        ] {
            assert!(!is_origin_allowed(origin), "wrongly allowed: {origin}");
        }
    }

    fn app() -> Router {
        Router::new()
            .route("/x", get(|| async { "ok" }))
            // A handler that builds its own response, which is where headers
            // set before the handler would be lost.
            .route(
                "/raw",
                get(|| async { super::super::super::http::bytes("audio/wav", vec![0]) }),
            )
            .layer(axum::middleware::from_fn(layer))
    }

    async fn request(method: Method, path: &str, origin: Option<&str>) -> Response {
        let mut builder = Request::builder().method(method).uri(path);
        if let Some(origin) = origin {
            builder = builder.header(header::ORIGIN, origin);
        }
        app()
            .oneshot(builder.body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn an_allowed_origin_is_reflected_with_vary() {
        let res = request(Method::GET, "/x", Some("tauri://localhost")).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            res.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN],
            "tauri://localhost"
        );
        assert_eq!(res.headers()[header::VARY], "Origin");
    }

    #[tokio::test]
    async fn a_cross_origin_request_gets_no_headers() {
        let res = request(Method::GET, "/x", Some("https://evil.example")).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert!(!res
            .headers()
            .contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN));
        assert!(!res
            .headers()
            .contains_key(header::ACCESS_CONTROL_ALLOW_HEADERS));
    }

    #[tokio::test]
    async fn no_origin_means_no_headers_and_no_interference() {
        let res = request(Method::GET, "/x", None).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert!(!res
            .headers()
            .contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN));
    }

    #[tokio::test]
    async fn a_preflight_from_a_disallowed_origin_is_a_bare_204() {
        let res = request(Method::OPTIONS, "/x", Some("https://evil.example")).await;
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
        assert!(!res
            .headers()
            .contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN));
    }

    #[tokio::test]
    async fn a_preflight_from_an_allowed_origin_advertises_every_method_and_header() {
        let res = request(Method::OPTIONS, "/x", Some("tauri://localhost")).await;
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
        let methods = res.headers()[header::ACCESS_CONTROL_ALLOW_METHODS]
            .to_str()
            .unwrap();
        for m in ["GET", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"] {
            assert!(methods.contains(m), "Allow-Methods missing {m}: {methods}");
        }
        let headers = res.headers()[header::ACCESS_CONTROL_ALLOW_HEADERS]
            .to_str()
            .unwrap();
        for h in ["Authorization", "Content-Type", "X-Admin-Token"] {
            assert!(headers.contains(h), "Allow-Headers missing {h}: {headers}");
        }
        assert_eq!(res.headers()[header::ACCESS_CONTROL_MAX_AGE], "86400");
    }

    /// The reason the headers go on the finished response: a handler returning
    /// its own `Response` replaces anything staged before it ran.
    #[tokio::test]
    async fn a_handler_built_response_still_carries_the_headers() {
        let res = request(Method::GET, "/raw", Some("tauri://localhost")).await;
        assert_eq!(res.headers()[header::CONTENT_TYPE], "audio/wav");
        assert_eq!(
            res.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN],
            "tauri://localhost"
        );
    }
}
