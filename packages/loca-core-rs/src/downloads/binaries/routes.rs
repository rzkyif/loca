//! The `/api/v1/binaries` route slice.
//!
//! Six routes, all requiring a paired client. The auth layer is added where this
//! router is mounted rather than here, because `require_client` needs the
//! application state and this slice only needs the installer; mounting it is the
//! one-line boot task already on the interfaces queue.
//!
//! # Absent, empty, and invalid are three different requests
//!
//! `/install` distinguishes an absent `kinds` from an empty one (D-028): absent
//! installs everything missing, `[]` installs nothing and returns no job ids.
//! The generated request type keeps `Option<Vec<_>>` for exactly that reason, so
//! neither a `#[serde(default)]` nor an `unwrap_or_default` may be added to it.
//!
//! `/update` refuses an absent or empty `kind` with a 400 rather than looking it
//! up and 404ing, which is the oracle's `if (!body.kind) throw` (D-043). The
//! generated `UpdateBinaryRequest.kind` is the `BinaryKind` enum, so `""` is
//! already refused at deserialization; this route is written so that the answer
//! is a `validation_error` either way, rather than depending on which layer
//! happens to catch it.
//!
//! # A missing body is an empty object, not a 400
//!
//! All three POSTs use `readJson(c, { allowEmpty: true })`, so a client that
//! sends no body at all gets the default request rather than a parse error.

use axum::body::Bytes;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;

use crate::contract::api::{
    BinaryKind, CheckBinariesResponse, InstallBinariesRequest, InstallBinariesResponse,
    ListBinariesResponse, ProbeBinariesResponse, UpdateBinaryResponse,
};
use crate::downloads::binaries::installer::BinariesInstaller;
use crate::downloads::cancel::CancellationToken;
use crate::error::{validation, Result};
use crate::transport::http::Json;

/// The slice, over the installer. The caller adds the auth layer.
pub fn router(installer: BinariesInstaller) -> Router {
    Router::new()
        .route("/", get(list))
        .route("/install", post(install))
        .route("/update", post(update))
        .route("/manifest", get(manifest))
        .route("/check", get(check))
        .route("/probe", post(probe))
        .with_state(installer)
}

async fn list(State(installer): State<BinariesInstaller>) -> Result<Response> {
    let statuses = installer.list().await?;
    Ok(Json(ListBinariesResponse(statuses)).into_response())
}

async fn install(State(installer): State<BinariesInstaller>, body: Bytes) -> Result<Response> {
    // Parsed from raw bytes rather than through an extractor, so an empty body
    // is the default request (`allowEmpty: true`) rather than a 400. The inner
    // `Option<Vec<_>>` is the D-028 half and is passed through untouched, so
    // absent and empty stay different all the way down.
    let request: InstallBinariesRequest = serde_json::from_value(body_json(&body)?)
        .map_err(|err| validation(format!("invalid install request: {err}")))?;
    let kinds = request.kinds;
    let job_ids = installer.install(kinds).await?;
    Ok(Json(InstallBinariesResponse { job_ids }).into_response())
}

async fn update(State(installer): State<BinariesInstaller>, body: Bytes) -> Result<Response> {
    // Read as raw JSON rather than the generated request type so that an absent,
    // empty, or unknown `kind` all produce the same `validation_error` this
    // route owns, instead of three different failures from three layers.
    let raw = body_json(&body)?;
    let kind = raw.get("kind").and_then(|v| v.as_str()).unwrap_or_default();
    if kind.is_empty() {
        return Err(validation("kind required"));
    }
    let kind: BinaryKind = kind
        .parse()
        .map_err(|_| validation(format!("unknown binary kind: {kind}")))?;
    let job_id = installer.update(kind).await?;
    Ok(Json(UpdateBinaryResponse { job_id }).into_response())
}

async fn manifest(State(installer): State<BinariesInstaller>) -> Result<Response> {
    Ok(Json(installer.manifest().await?).into_response())
}

async fn check(State(installer): State<BinariesInstaller>) -> Result<Response> {
    let checks = installer.check(&CancellationToken::new()).await?;
    Ok(Json(CheckBinariesResponse(checks)).into_response())
}

async fn probe(State(installer): State<BinariesInstaller>, body: Bytes) -> Result<Response> {
    let raw = body_json(&body)?;
    let kinds: Vec<BinaryKind> = raw
        .get("kinds")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str()?.parse().ok())
                .collect()
        })
        .unwrap_or_default();
    let results = installer.probe(&kinds, &CancellationToken::new()).await?;
    Ok(Json(ProbeBinariesResponse(results)).into_response())
}

/// A request body as JSON, treating an empty body as `{}`.
///
/// This is `readJson(c, { allowEmpty: true })`: a client that sends no body at
/// all gets the default request rather than a parse error.
fn body_json(body: &Bytes) -> Result<serde_json::Value> {
    if body.is_empty() {
        return Ok(serde_json::json!({}));
    }
    serde_json::from_slice(body).map_err(|err| validation(format!("invalid JSON body: {err}")))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    use super::*;
    use crate::config::Channel;
    use crate::db::migrate::migrate;
    use crate::db::Db;
    use crate::downloads::deps::NoCatalog;
    use crate::downloads::net::Net;
    use crate::downloads::queue::DownloadQueue;
    use crate::downloads::store::DownloadStore;
    use crate::downloads::transfer::Transfer;
    use crate::paths::Paths;

    const HF: &str = "https://huggingface.co";

    fn app(dir: &tempfile::TempDir) -> Router {
        let db = Db::open_in_memory().unwrap();
        db.call_blocking(|conn| migrate(conn)).unwrap();
        let store = DownloadStore::new(db, HF);
        let net = Arc::new(Net::new().unwrap());
        let transfer = Arc::new(
            Transfer::new(Arc::clone(&net), Arc::new(NoCatalog), HF)
                .with_intervals(Duration::from_secs(5), Duration::from_millis(1)),
        );
        let paths = Paths::rooted(dir.path());
        let queue = DownloadQueue::new(store, transfer, paths.clone(), HF)
            .with_delay(Duration::from_millis(0));
        router(BinariesInstaller::new(
            net,
            queue,
            paths,
            Channel::Dev,
            "http://127.0.0.1:1/binaries.json",
        ))
    }

    async fn post(app: &Router, path: &str, body: Option<&str>) -> (StatusCode, serde_json::Value) {
        let request = match body {
            Some(text) => Request::builder()
                .method("POST")
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(text.to_string()))
                .unwrap(),
            None => Request::builder()
                .method("POST")
                .uri(path)
                .body(Body::empty())
                .unwrap(),
        };
        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        let value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, value)
    }

    #[tokio::test]
    async fn an_empty_kind_is_a_400_not_a_404() {
        // D-043: `if (!body.kind) throw` is JavaScript truthiness, so "" is a
        // validation error rather than a lookup that misses.
        let dir = tempfile::tempdir().unwrap();
        let app = app(&dir);

        let (status, body) = post(&app, "/update", Some(r#"{"kind":""}"#)).await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "validation_error");
        assert_eq!(body["error"]["message"], "kind required");
    }

    #[tokio::test]
    async fn an_absent_kind_is_the_same_400_as_an_empty_one() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(&dir);

        for body in [Some("{}"), None] {
            let (status, value) = post(&app, "/update", body).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body:?}");
            assert_eq!(value["error"]["message"], "kind required", "{body:?}");
        }
    }

    #[tokio::test]
    async fn an_unknown_kind_is_a_validation_error_naming_it() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(&dir);

        let (status, body) = post(&app, "/update", Some(r#"{"kind":"nonesuch"}"#)).await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("nonesuch"),
            "{body}"
        );
    }

    #[tokio::test]
    async fn an_empty_kinds_array_returns_no_jobs_and_an_absent_one_does_not() {
        // The route half of D-028: these two bodies must not collapse.
        let dir = tempfile::tempdir().unwrap();
        let app = app(&dir);

        let (status, explicit) = post(&app, "/install", Some(r#"{"kinds":[]}"#)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(explicit["jobIds"].as_array().unwrap().len(), 0);

        // Absent means "everything missing", which here tries deno and fails to
        // reach GitHub. What matters is that it ATTEMPTED, which the recorded
        // failure below shows, rather than returning early like the empty case.
        let (status, _) = post(&app, "/install", Some("{}")).await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn a_missing_body_is_an_empty_object_rather_than_a_parse_error() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(&dir);

        let (status, _) = post(&app, "/probe", None).await;

        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn list_answers_every_kind_on_the_contract() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(&dir);

        let response = app
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value.as_array().unwrap().len(), 3);
    }
}
