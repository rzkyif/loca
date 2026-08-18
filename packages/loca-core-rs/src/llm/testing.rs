//! Providers that answer without a model, and a fake upstream that answers over
//! real HTTP.
//!
//! Compiled for this crate's tests and under the `in-memory` feature, following
//! `transport::testing`, so D4 can drive the chat orchestrator against a
//! scripted completion rather than a language model.

// Test support: a failure to build a fixture is a broken test, and panicking
// says so at the line that caused it. Never compiled into a production build.
#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::stream::{self, StreamExt as _};

use crate::contract::api::TokenUsage;

use super::provider::{ChatProvider, DeltaStream, ProviderError};
use super::types::{LlmDelta, LlmRequest};

/// A provider that replays a fixed script.
#[derive(Clone)]
pub struct ScriptedProvider {
    items: Arc<Vec<Result<LlmDelta, ProviderError>>>,
    gap: Option<Duration>,
    seen: Arc<Mutex<Vec<LlmRequest>>>,
}

impl ScriptedProvider {
    /// Stream `text` a word at a time, then a `stop` and a usage chunk, which is
    /// the shape every OpenAI-compatible provider produces.
    pub fn text(text: &str) -> Self {
        let mut items: Vec<Result<LlmDelta, ProviderError>> = text
            .split_inclusive(' ')
            .map(|chunk| {
                Ok(LlmDelta {
                    content_delta: Some(chunk.to_string()),
                    ..LlmDelta::default()
                })
            })
            .collect();
        items.push(Ok(LlmDelta {
            finish_reason: Some("stop".to_string()),
            ..LlmDelta::default()
        }));
        items.push(Ok(LlmDelta {
            usage: Some(TokenUsage {
                prompt: 10,
                completion: 3,
                total: 13,
            }),
            ..LlmDelta::default()
        }));
        Self::from_items(items)
    }

    /// Fail the completion before it produces anything.
    pub fn failing(err: ProviderError) -> Self {
        Self::from_items(vec![Err(err)])
    }

    pub fn from_items(items: Vec<Result<LlmDelta, ProviderError>>) -> Self {
        Self {
            items: Arc::new(items),
            gap: None,
            seen: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Wait this long before each item, so a test can drive the stall watchdog
    /// on a paused clock.
    pub fn with_gap(mut self, gap: Duration) -> Self {
        self.gap = Some(gap);
        self
    }

    /// Every request this provider was asked for, in order.
    pub fn requests(&self) -> Vec<LlmRequest> {
        self.seen.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl ChatProvider for ScriptedProvider {
    fn stream(&self, req: LlmRequest) -> DeltaStream {
        self.seen
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(req);
        let gap = self.gap;
        stream::iter(
            self.items
                .iter()
                .map(|item| match item {
                    Ok(delta) => Ok(delta.clone()),
                    Err(err) => Err(err.clone()),
                })
                .collect::<Vec<_>>(),
        )
        .then(move |item| async move {
            if let Some(gap) = gap {
                tokio::time::sleep(gap).await;
            }
            item
        })
        .boxed()
    }
}

/// A provider whose completion never produces anything and never ends.
///
/// The wedged server the watchdog exists for, and the way a test holds a slot
/// open for as long as it needs.
#[derive(Clone, Default)]
pub struct PendingProvider;

impl PendingProvider {
    pub fn new() -> Self {
        Self
    }
}

impl ChatProvider for PendingProvider {
    fn stream(&self, _req: LlmRequest) -> DeltaStream {
        stream::pending().boxed()
    }
}

/// A real HTTP server standing in for an OpenAI-compatible provider.
///
/// It records the request body and replays a recorded SSE file, which is what
/// makes the provider tests exercise `reqwest`, the decoder and the auth header
/// rather than mocking them out. The direct analogue of the TypeScript suite's
/// `capturingFetch` over `tests/fixtures/openai/`, replaying the same two
/// fixtures.
///
/// Real sockets, so a test using this must NOT run on a paused clock: the
/// virtual clock jumps while bytes are still in the kernel.
pub struct FakeUpstream {
    pub base_url: String,
    seen: Arc<Mutex<Vec<serde_json::Value>>>,
    headers: Arc<Mutex<Vec<(String, String)>>>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

/// What the fake answers with.
pub enum UpstreamScript {
    /// Replay a file from `src/llm/testdata/`.
    Fixture(&'static str),
    /// Answer with this status and body instead.
    Error(u16, &'static str),
    /// Answer 200 with exactly this body, for a stream no fixture file should
    /// be dignified with: an empty one, or a deliberately malformed one.
    Raw(&'static str),
}

impl FakeUpstream {
    pub async fn start(script: UpstreamScript) -> Self {
        let seen: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
        let headers: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let (status, body) = match script {
            UpstreamScript::Fixture(name) => (
                200,
                std::fs::read_to_string(
                    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join("src/llm/testdata")
                        .join(name),
                )
                .unwrap_or_else(|e| panic!("fixture {name}: {e}")),
            ),
            UpstreamScript::Error(status, body) => (status, body.to_string()),
            UpstreamScript::Raw(body) => (200, body.to_string()),
        };

        let answer = Arc::new(body);
        let recorded = Arc::clone(&seen);
        let recorded_headers = Arc::clone(&headers);
        let app = axum::Router::new().route(
            "/v1/chat/completions",
            axum::routing::post(move |req_headers: axum::http::HeaderMap, body: String| {
                let recorded = Arc::clone(&recorded);
                let recorded_headers = Arc::clone(&recorded_headers);
                let answer = Arc::clone(&answer);
                async move {
                    for (name, value) in &req_headers {
                        recorded_headers.lock().unwrap().push((
                            name.as_str().to_string(),
                            value.to_str().unwrap_or_default().to_string(),
                        ));
                    }
                    recorded
                        .lock()
                        .unwrap()
                        .push(serde_json::from_str(&body).unwrap_or(serde_json::Value::Null));
                    axum::http::Response::builder()
                        .status(status)
                        .header("content-type", "text/event-stream")
                        .body(axum::body::Body::from(answer.to_string()))
                        .unwrap_or_default()
                }
            }),
        );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = shutdown_rx.await;
                })
                .await;
        });
        Self {
            base_url: format!("http://{addr}/v1"),
            seen,
            headers,
            shutdown: Some(shutdown_tx),
        }
    }

    /// The bodies this upstream was sent, parsed.
    pub fn bodies(&self) -> Vec<serde_json::Value> {
        self.seen.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The value of a request header, from the first request.
    pub fn header(&self, name: &str) -> Option<String> {
        self.headers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    }
}

impl Drop for FakeUpstream {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }
}
