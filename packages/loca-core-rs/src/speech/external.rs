//! The external, OpenAI-compatible speech providers.
//!
//! Two calls, every attempt bounded at [`EXTERNAL_TIMEOUT_MS`]: `POST
//! {baseUrl}/audio/transcriptions` (multipart) and `POST {baseUrl}/audio/speech`
//! (JSON in, WAV bytes out). Key resolution is shared with the LLM and
//! embedding paths and lives in [`crate::external`].
//!
//! A failure the provider blames on itself is retried twice with backoff, which
//! is what the SDK does on the other side and what stops one 429 losing a
//! recording the user cannot make again (D-042).
//!
//! Cancellation needs no plumbing: dropping the response future cancels the
//! request, and axum drops the handler future when the client disconnects, so
//! an abandoned transcription stops the upstream work (and the billing) on its
//! own.

use std::time::Duration;

use reqwest::multipart::{Form, Part};
use serde_json::{json, Value};

use crate::error::{internal, AppError, ErrorCode, Result};
use crate::external::EXTERNAL_TIMEOUT_MS;

use super::engine::{AudioClip, Synthesis};

/// Retries after a failed attempt, so three attempts at most. The number is the
/// SDK's `maxRetries: 2` on the other side, and it is what an audio call gets
/// there while the chat path deliberately gets none.
const EXTERNAL_MAX_RETRIES: u32 = 2;

/// The wait before the first retry; each further one doubles it.
const RETRY_BACKOFF: Duration = Duration::from_millis(500);

/// A resolved external endpoint: everything the caller had to read out of
/// settings and the vault before a request could be made.
pub struct Endpoint {
    pub base_url: String,
    pub model: String,
    pub api_key: String,
}

impl Endpoint {
    /// The SDK joins its `baseURL` with the operation path, tolerating a
    /// trailing slash on the configured value. Users paste both forms.
    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base_url.trim_end_matches('/'))
    }
}

/// The HTTP client for the external providers.
///
/// One per service, so connections are reused across calls.
pub struct ExternalSpeech {
    /// `None` when the client could not be built at all, which means the TLS
    /// backend failed to initialise. Held rather than propagated so building a
    /// route slice stays infallible; every call then reports it instead.
    client: Option<reqwest::Client>,
    timeout: Duration,
}

impl Default for ExternalSpeech {
    fn default() -> Self {
        Self::new()
    }
}

impl ExternalSpeech {
    pub fn new() -> Self {
        Self::with_timeout(Duration::from_millis(EXTERNAL_TIMEOUT_MS))
    }

    /// One construction path, so a test can shorten the deadline without
    /// leaving the production value untested: [`timeout`](Self::timeout) is what
    /// pins that.
    pub fn with_timeout(timeout: Duration) -> Self {
        match reqwest::Client::builder().timeout(timeout).build() {
            Ok(client) => Self {
                client: Some(client),
                timeout,
            },
            Err(err) => {
                tracing::error!(
                    target: "loca::speech",
                    "the external speech client could not be built, so external providers are unusable: {err}",
                );
                Self {
                    client: None,
                    timeout,
                }
            }
        }
    }

    /// The deadline every attempt is bounded by.
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    fn client(&self) -> Result<&reqwest::Client> {
        self.client
            .as_ref()
            .ok_or_else(|| internal("the external HTTP client could not be initialised"))
    }

    /// Relay an audio clip to `{baseUrl}/audio/transcriptions`.
    ///
    /// `language` is the effective hint: the request-pinned one when the caller
    /// sent it, else `stt.external.language`, else absent. The local engine
    /// auto-detects, which is why the hint reaches this path only.
    pub async fn transcribe(
        &self,
        endpoint: &Endpoint,
        audio: AudioClip,
        language: Option<String>,
    ) -> Result<String> {
        let AudioClip {
            bytes,
            filename,
            mime,
        } = audio;
        let size = bytes.len();
        // Probe the client-supplied type on an empty part first: `mime_str`
        // consumes the part, so a bad value would otherwise cost the bytes, and
        // a header a client chose must never be able to fail the request.
        let mime = mime.filter(|m| Part::bytes(Vec::new()).mime_str(m).is_ok());

        // Rebuilt per attempt: a `Form` is consumed by the request it is sent
        // on, so a retry needs its own copy of the clip.
        let build = || -> Result<reqwest::RequestBuilder> {
            let mut file = Part::bytes(bytes.clone()).file_name(filename.clone());
            if let Some(mime) = &mime {
                file = file.mime_str(mime).map_err(|err| {
                    internal(format!("rejected an already-validated mime: {err}"))
                })?;
            }
            let mut form = Form::new()
                .text("model", endpoint.model.clone())
                .text("response_format", "json")
                .part("file", file);
            if let Some(language) = &language {
                form = form.text("language", language.clone());
            }
            Ok(self
                .client()?
                .post(endpoint.url("/audio/transcriptions"))
                .bearer_auth(&endpoint.api_key)
                .multipart(form))
        };

        let started = std::time::Instant::now();
        tracing::info!(
            target: "loca::stt",
            "external transcription starting (audio {size} bytes, model {})", endpoint.model,
        );
        let body = send_retrying(build).await.map_err(stt_error)?;
        let parsed: Value = serde_json::from_slice(&body).map_err(|err| {
            stt_error(AppError::new(
                ErrorCode::ProviderError,
                format!("the transcription response was not JSON: {err}"),
            ))
        })?;
        // Absent `text` is an empty transcript rather than a failure, matching
        // the endpoints that answer 200 with nothing to say.
        let text = parsed
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        tracing::info!(
            target: "loca::stt",
            "external transcription done in {}ms (audio {size} bytes, out {} chars)",
            started.elapsed().as_millis(), text.len(),
        );
        Ok(text)
    }

    /// Relay a synthesis request to `{baseUrl}/audio/speech`, returning WAV
    /// bytes.
    ///
    /// `voice` is the caller's already-resolved external voice, never the local
    /// voice id on the request body.
    pub async fn synthesize(
        &self,
        endpoint: &Endpoint,
        req: &Synthesis,
        voice: &str,
    ) -> Result<Vec<u8>> {
        let mut body = json!({
            "model": endpoint.model,
            "voice": voice,
            "input": req.text,
            // The client's playback queue and the broker's sample-rate probe
            // both assume WAV, so the format is not negotiable per request.
            "response_format": "wav",
        });
        // A zero or absent speed is omitted rather than sent: the field is
        // optional upstream and 0 is not a valid rate.
        if let (Some(speed), Some(object)) = (req.speed.filter(|s| *s != 0.0), body.as_object_mut())
        {
            object.insert("speed".to_string(), json!(speed));
        }

        let started = std::time::Instant::now();
        tracing::info!(
            target: "loca::tts",
            "external synthesis starting (model {}, {} chars)", endpoint.model, req.text.len(),
        );
        let bytes = send_retrying(|| {
            Ok(self
                .client()?
                .post(endpoint.url("/audio/speech"))
                .bearer_auth(&endpoint.api_key)
                .json(&body))
        })
        .await
        .map_err(tts_error)?;
        tracing::info!(
            target: "loca::tts",
            "external synthesis done in {}ms (out {} bytes)",
            started.elapsed().as_millis(), bytes.len(),
        );
        Ok(bytes)
    }
}

/// One attempt's failure, and whether repeating it could plausibly change the
/// answer.
///
/// Retryability is decided from the HTTP status, not from the mapped
/// [`ErrorCode`]: a 400 and a 500 both classify as `provider_error` and only one
/// of them is worth sending the audio again for.
struct Failed {
    error: AppError,
    retryable: bool,
}

/// Send, and retry a failure the provider blames on itself.
///
/// `build` produces a fresh request per attempt, because a body is consumed by
/// the request it rides on. The deadline on the client bounds each attempt
/// separately, exactly as the SDK's does.
async fn send_retrying(build: impl Fn() -> Result<reqwest::RequestBuilder>) -> Result<Vec<u8>> {
    let mut attempt = 0;
    loop {
        let outcome = match build()?.send().await {
            Ok(response) => read_or_classify(response).await,
            Err(err) => Err(Failed {
                error: transport_error(&err),
                retryable: true,
            }),
        };
        let failed = match outcome {
            Ok(bytes) => return Ok(bytes),
            Err(failed) => failed,
        };
        if !failed.retryable || attempt >= EXTERNAL_MAX_RETRIES {
            return Err(failed.error);
        }
        let backoff = RETRY_BACKOFF * 2u32.pow(attempt);
        tracing::warn!(
            target: "loca::speech",
            "external speech attempt {} failed ({}), retrying in {}ms",
            attempt + 1, failed.error.message, backoff.as_millis(),
        );
        tokio::time::sleep(backoff).await;
        attempt += 1;
    }
}

/// The statuses the SDK retries: a request timeout, a lock conflict, a rate
/// limit, and anything the provider blames on itself.
fn is_retryable_status(status: u16) -> bool {
    matches!(status, 408 | 409 | 429) || status >= 500
}

/// The response body, or the failure the provider reported.
async fn read_or_classify(response: reqwest::Response) -> std::result::Result<Vec<u8>, Failed> {
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(|err| Failed {
            error: transport_error(&err),
            retryable: true,
        })?
        .to_vec();
    if status.is_success() {
        return Ok(bytes);
    }
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let parsed: Option<Value> = serde_json::from_str(&text).ok();
    let code = parsed
        .as_ref()
        .and_then(|v| v.pointer("/error/code").or_else(|| v.get("code")))
        .and_then(Value::as_str)
        .map(str::to_string);
    let message = parsed
        .as_ref()
        .and_then(|v| v.pointer("/error/message"))
        .and_then(Value::as_str)
        .map(str::to_string)
        // No structured message: carry what arrived, the way the SDK folds the
        // raw body into the error it throws.
        .unwrap_or_else(|| format!("HTTP {}: {text}", status.as_u16()));
    Err(Failed {
        error: classify(Some(status.as_u16()), code.as_deref(), &message),
        retryable: is_retryable_status(status.as_u16()),
    })
}

/// A connect failure or a timeout, which carries no HTTP status and so lands on
/// the generic branch, exactly as the SDK's connection errors do.
///
/// The two messages are the SDK's own, and the detail stays in the log rather
/// than in the answer: `reqwest::Error`'s `Display` appends the URL it was
/// calling, which is the user's endpoint (an internal hostname, a port, and
/// whatever else they configured) and has no business in a response body.
fn transport_error(err: &reqwest::Error) -> AppError {
    tracing::warn!(target: "loca::speech", "external speech request failed: {err}");
    let message = if err.is_timeout() {
        "Request timed out."
    } else {
        "Connection error."
    };
    AppError::new(ErrorCode::ProviderError, message)
}

/// Map a provider failure onto a stable code.
///
/// The structured fields are tried first and the message only after, because a
/// message match is a heuristic and a status is not.
pub fn classify(status: Option<u16>, code: Option<&str>, message: &str) -> AppError {
    let code_is = |want: &str| code == Some(want);
    let error = if status == Some(401) || code_is("invalid_api_key") {
        ErrorCode::ProviderUnauthorized
    } else if status == Some(429) || code_is("rate_limit_exceeded") {
        ErrorCode::ProviderRateLimited
    } else if status == Some(503) || status == Some(504) {
        ErrorCode::ServerUnavailable
    } else if code_is("context_length_exceeded") || mentions_context_window(message) {
        ErrorCode::ContextWindowExceeded
    } else {
        ErrorCode::ProviderError
    };
    AppError::new(error, message)
}

/// Case-insensitive, and two needles rather than three: "maximum context
/// length" contains "context length", so matching it separately would be
/// redundant.
fn mentions_context_window(message: &str) -> bool {
    let lower = message.to_lowercase();
    lower.contains("context length") || lower.contains("context window")
}

fn stt_error(err: AppError) -> AppError {
    AppError::new(err.code, format!("external STT failed: {}", err.message))
}

fn tts_error(err: AppError) -> AppError {
    AppError::new(err.code, format!("external TTS failed: {}", err.message))
}
