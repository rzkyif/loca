//! The two things a schedule needs that are not this package's: a way to open a
//! core-initiated turn, and a way to ask whether starting one right now would
//! race the local model's load.
//!
//! Both are the constructor parameters the oracle already treats as seams
//! (`services/prompt-scheduler.ts:144-147`), and both belong to D8: they are the
//! two shared backends behind greetings and scheduled prompts (D-090 for
//! `runAutomatedSession`, D-094 for `llmStillLoading`), ported once into
//! `chat/automated.rs` and consumed here.
//!
//! So the readiness probe is [`crate::chat::LoadingProbe`] rather than a trait
//! of ours. The turn is wrapped in one of ours, [`AutomatedRunner`], for a
//! reason the probe does not need: a test has to drive a due tick without a
//! language model, and `run_automated_session` takes a concrete `Chat`.

use std::sync::Arc;

use async_trait::async_trait;

use crate::chat::{AutomatedSessionInput, Chat, LoadingProbe};
use crate::contract::api::{SessionCreatedFocus, SessionCreatedReason};
use crate::error::Result;

/// One core-initiated turn, as a schedule asks for it.
///
/// Narrower than [`AutomatedSessionInput`] on purpose: a schedule always passes
/// `reason: "schedule"` and `focus: "show"`
/// (`prompt-scheduler.ts:284-288`, `:367-372`), so those are not parameters and
/// cannot be got wrong at a call site. [`ChatAutomated`] is the one place that
/// fills them in.
#[derive(Clone, Debug)]
pub struct ScheduledRun {
    pub owner_client_id: String,
    pub title: String,
    /// The automated user prompt that opens the session.
    pub instruction: String,
    pub scheduled_prompt_id: String,
}

/// Open a core-initiated session and start its turn.
///
/// Answers the new session's id and nothing else, because that is all a schedule
/// uses: `run_now` returns it as `{ sessionId }`
/// (`http/routes/scheduled-prompts.ts:52`) and the tick discards it. Widening
/// the seam to the whole `Session` would couple this package to a shape it never
/// reads.
#[async_trait]
pub trait AutomatedRunner: Send + Sync {
    async fn run(&self, input: ScheduledRun) -> Result<String>;
}

/// A core whose local model can never be loading: an external provider, or one
/// with no local inference at all.
///
/// What a router test mounts. Production uses [`crate::chat::LlmLoading`], which
/// already answers false on a core with no in-process engine, so this exists for
/// the tests and for a caller that has no `Settings` to hand rather than as a
/// second opinion about the model.
pub struct NeverLoading;

#[async_trait]
impl LoadingProbe for NeverLoading {
    async fn still_loading(&self) -> bool {
        false
    }
}

/// What the scheduler is built with. The automated runner is NOT here: it is
/// attached afterwards, because `Chat` needs the scheduler to persist a
/// confirmed draft and the scheduler needs `Chat` to open a turn, and something
/// has to be constructed first.
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
