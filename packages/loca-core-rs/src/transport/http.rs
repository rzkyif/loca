//! How an [`AppError`] becomes a response, and the extractors route slices use.
//!
//! Every non-2xx the API returns is the same envelope,
//! `{ error: { code, message, details? } }`, and the status comes from the code.
//! The mechanism is [`IntoResponse`] on `AppError`: axum implements
//! `IntoResponse` for any `Result` of two `IntoResponse`s, so a handler written
//! as
//!
//! ```ignore
//! async fn get_thing(client: AuthedClient, Path(id): Path<String>) -> Result<Json<Thing>> {
//!     Ok(Json(store.get(&client.id, &id).await?))
//! }
//! ```
//!
//! gets the envelope for free and never mentions it.

use axum::extract::rejection::JsonRejection;
use axum::extract::{FromRequest, Request};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};

use crate::error::{AppError, ErrorCode};
use crate::log::scrub_secrets;

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        // Response bodies are not scrubbed by the logger, and an
        // `internal_error` can echo an upstream URL carrying a token.
        let scrubbed = AppError {
            code: self.code,
            message: scrub_secrets(&self.message),
            details: self.details,
        };
        let status =
            StatusCode::from_u16(scrubbed.status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        (status, axum::Json(scrubbed.to_body())).into_response()
    }
}

/// A handled error is a warning, an internal one is an error. Called by the
/// middleware that sees the finished response, so the line carries the method
/// and path a handler does not have.
pub(crate) fn log_error(method: &str, path: &str, code: ErrorCode, message: &str) {
    if code == ErrorCode::InternalError {
        tracing::error!(target: "loca::http", "{method} {path} -> {}: {message}", code.as_str());
    } else {
        tracing::warn!(target: "loca::http", "{method} {path} -> {}: {message}", code.as_str());
    }
}

/// JSON, both directions. As an extractor it maps a malformed body to
/// `validation_error` instead of axum's plain-text rejection, keeping serde's
/// message (which names the offending field); as a response it is `axum::Json`.
#[derive(Debug, Clone, Copy, Default)]
pub struct Json<T>(pub T);

impl<T, S> FromRequest<S> for Json<T>
where
    axum::Json<T>: FromRequest<S, Rejection = JsonRejection>,
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        match axum::Json::<T>::from_request(req, state).await {
            Ok(axum::Json(value)) => Ok(Json(value)),
            Err(rejection) => Err(rejection_to_error(rejection)),
        }
    }
}

impl<T: serde::Serialize> IntoResponse for Json<T> {
    fn into_response(self) -> Response {
        axum::Json(self.0).into_response()
    }
}

fn rejection_to_error(rejection: JsonRejection) -> AppError {
    // A body over the size cap arrives as a `BytesRejection` carrying a 413,
    // and the client shows a distinct path for it. The pair (413,
    // validation_error) is unreachable through `AppError`, whose status comes
    // from its code, so it is built directly.
    if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE {
        return AppError::new(ErrorCode::ValidationError, "request body too large");
    }
    AppError::new(ErrorCode::ValidationError, rejection.body_text())
}

/// The oversize-body response: 413 with a `validation_error` code. See
/// [`rejection_to_error`] for why this is not an `AppError`.
pub(crate) fn payload_too_large() -> Response {
    let body = AppError::new(ErrorCode::ValidationError, "request body too large").to_body();
    (StatusCode::PAYLOAD_TOO_LARGE, axum::Json(body)).into_response()
}

/// 204, for a route whose success carries nothing.
pub fn no_content() -> Response {
    StatusCode::NO_CONTENT.into_response()
}

/// Raw bytes with a content type, for the responses that are not JSON: the WAV
/// a TTS synthesis returns, an attachment read back, an exported file.
pub fn bytes(content_type: &'static str, body: Vec<u8>) -> Response {
    (
        [(header::CONTENT_TYPE, HeaderValue::from_static(content_type))],
        body,
    )
        .into_response()
}

/// Unmatched path.
pub async fn not_found() -> Response {
    AppError::new(ErrorCode::NotFound, "no such endpoint").into_response()
}

/// Matched path, wrong method. `not_found` because the wire contract has no
/// `method_not_allowed` code.
pub async fn method_not_allowed() -> Response {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        axum::Json(AppError::new(ErrorCode::NotFound, "method not allowed").to_body()),
    )
        .into_response()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::error::{internal, not_found as not_found_err, validation, Details};
    use axum::body::to_bytes;

    async fn body_of(res: Response) -> serde_json::Value {
        let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn an_app_error_becomes_the_wire_envelope_at_its_own_status() {
        let res = not_found_err("no such session").into_response();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            body_of(res).await,
            serde_json::json!({ "error": { "code": "not_found", "message": "no such session" } })
        );

        let mut details = Details::new();
        details.insert("field".into(), serde_json::json!("name"));
        let res = validation("name is required")
            .with_details(details)
            .into_response();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            body_of(res).await["error"]["details"]["field"],
            serde_json::json!("name")
        );

        // One representative per remaining status class, so a change to the
        // code-to-status table shows up here.
        for (err, want) in [
            (
                AppError::new(ErrorCode::MissingToken, "x"),
                StatusCode::UNAUTHORIZED,
            ),
            (
                AppError::new(ErrorCode::SessionBusy, "x"),
                StatusCode::CONFLICT,
            ),
            (
                AppError::new(ErrorCode::PairingRateLimited, "x"),
                StatusCode::TOO_MANY_REQUESTS,
            ),
            (
                AppError::new(ErrorCode::InsufficientStorage, "x"),
                StatusCode::INSUFFICIENT_STORAGE,
            ),
        ] {
            assert_eq!(err.into_response().status(), want);
        }
    }

    #[tokio::test]
    async fn an_error_message_is_scrubbed_on_its_way_to_the_wire() {
        let res =
            internal("upstream said: Bearer sk-abcdefghijklmnopqrstuvwxyz012345").into_response();
        let body = body_of(res).await;
        let message = body["error"]["message"].as_str().unwrap();
        assert!(
            !message.contains("sk-abcdefghijklmnopqrstuvwxyz012345"),
            "token reached the wire: {message}"
        );
    }

    #[tokio::test]
    async fn the_fallbacks_carry_the_envelope_too() {
        let res = not_found().await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        assert_eq!(body_of(res).await["error"]["code"], "not_found");

        let res = method_not_allowed().await;
        assert_eq!(res.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(body_of(res).await["error"]["code"], "not_found");

        // The one status the error type cannot express on its own.
        let res = payload_too_large();
        assert_eq!(res.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(body_of(res).await["error"]["code"], "validation_error");
    }

    #[tokio::test]
    async fn no_content_and_bytes_are_what_they_say() {
        assert_eq!(no_content().status(), StatusCode::NO_CONTENT);
        let res = bytes("audio/wav", vec![1, 2, 3]);
        assert_eq!(res.headers()[header::CONTENT_TYPE], "audio/wav");
        assert_eq!(
            to_bytes(res.into_body(), usize::MAX)
                .await
                .unwrap()
                .to_vec(),
            vec![1, 2, 3]
        );
    }
}
