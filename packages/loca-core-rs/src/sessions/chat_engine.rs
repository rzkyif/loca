//! The seam between the session routes and the chat orchestrator (D4).
//!
//! Declared here and implemented in `chat`, following `ClientAuth`,
//! `StatusProvider` and `SidecarSource`: the consumer declares the seam so its
//! routes compile and are testable before the producer exists, and a test
//! injects a scripted engine instead of a language model.
//!
//! The split is deliberate and narrow. The session routes own the HTTP envelope
//! and every store mutation, in an order that is itself the invariant (claim the
//! stream slot, persist the user message, read the history, generate, persist
//! the assistant message). The engine owns text in, deltas out, and touches the
//! store on this path not at all. Everything the generation needs beyond the
//! transcript (the system prompt, the endpoint, the scheduler) depends on D1 and
//! D3, which is why it lives on the engine's side.

use futures_util::stream::{self, BoxStream};

use crate::contract::api::TokenUsage;
use crate::error::{AppError, ErrorCode, Result};

use super::doc::StoredMessage;

/// One turn's input for the SSE fallback.
pub struct OneShotRequest {
    pub owner_client_id: String,
    pub session_id: String,
    /// The persisted transcript, oldest first, including the user message the
    /// route has just written.
    pub history: Vec<StoredMessage>,
}

/// One chunk of a completion. Either half may be absent: a chunk carrying only
/// usage produces no output line.
#[derive(Clone, Debug, Default)]
pub struct OneShotDelta {
    pub content_delta: Option<String>,
    pub usage: Option<TokenUsage>,
}

pub trait ChatEngine: Send + Sync {
    /// A one-shot completion. No tools, no reasoning trace, no multiplexing;
    /// the WebSocket path covers all three.
    ///
    /// A stream rather than an async method, which keeps the trait object-safe
    /// without an `async-trait` dependency.
    fn one_shot(&self, req: OneShotRequest) -> BoxStream<'static, Result<OneShotDelta>>;

    /// Regenerate this session's title, in the background. Returns immediately:
    /// the client learns the start, the new title and the end from
    /// `session.updated` frames the engine broadcasts.
    fn regenerate_title(&self, session_id: String, owner_client_id: String);
}

/// The engine before there is one.
///
/// Installed by `boot` until D4 lands, and what a store-only test uses. It fails
/// rather than returning an empty completion, because an empty completion is
/// indistinguishable from a model that had nothing to say.
pub struct UnavailableChat;

impl ChatEngine for UnavailableChat {
    fn one_shot(&self, _req: OneShotRequest) -> BoxStream<'static, Result<OneShotDelta>> {
        Box::pin(stream::once(async {
            Err(AppError::new(
                ErrorCode::ServerUnavailable,
                "no chat engine is configured",
            ))
        }))
    }

    fn regenerate_title(&self, session_id: String, _owner_client_id: String) {
        tracing::warn!(
            target: "loca::http.sessions",
            "title regeneration requested for {session_id} with no chat engine configured",
        );
    }
}
