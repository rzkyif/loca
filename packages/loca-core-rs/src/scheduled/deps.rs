//! The scheduler's two external seams: opening a core-initiated turn
//! ([`AutomatedRunner`], wrapped so tests can tick without a model) and the
//! model-loading probe ([`crate::chat::LoadingProbe`], shared with greetings).

use std::sync::Arc;

use async_trait::async_trait;

use crate::chat::{AutomatedSessionInput, Chat, LoadingProbe};
use crate::contract::api::{SessionCreatedFocus, SessionCreatedReason};
use crate::error::Result;

/// One core-initiated turn. Narrower than [`AutomatedSessionInput`]: a schedule
/// always uses `reason: "schedule"` and `focus: "show"`.
#[derive(Clone, Debug)]
pub struct ScheduledRun {
    pub owner_client_id: String,
    pub title: String,
    /// The automated user prompt that opens the session.
    pub instruction: String,
    pub scheduled_prompt_id: String,
}

/// Open a core-initiated session and start its turn; answers the session id.
#[async_trait]
pub trait AutomatedRunner: Send + Sync {
    async fn run(&self, input: ScheduledRun) -> Result<String>;
}

/// A core whose local model can never be loading. Production uses
/// [`crate::chat::LlmLoading`]; this serves tests and callers without `Settings`.
pub struct NeverLoading;

#[async_trait]
impl LoadingProbe for NeverLoading {
    async fn still_loading(&self) -> bool {
        false
    }
}

/// What the scheduler is built with. The runner is attached afterwards because
/// `Chat` and the scheduler need each other.
#[derive(Clone)]
pub struct SchedulerDeps {
    pub loading: Arc<dyn LoadingProbe>,
}

impl SchedulerDeps {
    /// No local model to wait for.
    pub fn unwired() -> Self {
        Self {
            loading: Arc::new(NeverLoading),
        }
    }

    pub fn with_loading(mut self, loading: Arc<dyn LoadingProbe>) -> Self {
        self.loading = loading;
        self
    }
}

/// The production runner: a schedule's turn is an ordinary chat turn that
/// nobody typed.
pub struct ChatAutomated {
    chat: Chat,
}

impl ChatAutomated {
    pub fn new(chat: Chat) -> Self {
        Self { chat }
    }
}

#[async_trait]
impl AutomatedRunner for ChatAutomated {
    async fn run(&self, input: ScheduledRun) -> Result<String> {
        let session = crate::chat::run_automated_session(
            &self.chat,
            AutomatedSessionInput {
                owner_client_id: input.owner_client_id,
                title: input.title,
                instruction: input.instruction,
                reason: SessionCreatedReason::Schedule,
                scheduled_prompt_id: Some(input.scheduled_prompt_id),
                focus: SessionCreatedFocus::Show,
            },
        )
        .await?;
        Ok(session.id)
    }
}
