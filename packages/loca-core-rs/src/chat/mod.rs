//! The chat orchestrator: turn assembly, streaming, the tool loop, title generation.
//!
//! Work package **D4** (`docs/rust-port/registry.md`). This module is that
//! package's owned path; nothing outside it writes here.
//!
//! # What lives here
//!
//! Three things, separated because they fail differently.
//!
//! 1. **The turn.** One `chat.start` WS frame becomes a task that builds a
//!    provider request, streams it, dispatches the tool calls the model asks
//!    for, loops, and emits `chat.*` / `tool.*` frames. Stateful, and the bulk.
//! 2. **Two seams other packages declared and left waiting.**
//!    [`sessions::ChatEngine`](crate::sessions::ChatEngine) backs the SSE
//!    fallback and the title-regenerate route;
//!    [`memories::MemoryIndexer`](crate::memories::MemoryIndexer) is how a
//!    memory mutation asks for a re-index.
//! 3. **The idle gate.** The background queue is D4's because its "is anything
//!    busy?" predicate is chat's own active-stream count.
//!
//! # What does NOT live here
//!
//! There is no `routes.rs`. Every HTTP route that reaches chat is D2's and goes
//! through `ChatEngine`, so D4's only wire surface is the WebSocket, reached
//! through F6's [`InboundHandler`](crate::transport::InboundHandler).
//!
//! # A turn outlives its client's connection
//!
//! The turn runs on its own task, not on the WS reader's, so a disconnect
//! cannot cancel it. That is stated as an invariant in `chat.ts:15-22` and the
//! Client depends on it: a transport drop detaches rather than interrupting,
//! which is what makes a core swap transparent. The catch-up buffers on
//! [`ActiveStream`] are what a reconnecting client replays.

pub mod automated;
pub mod background;
pub mod complexity;
pub mod deps;
pub mod muxer;
pub mod stream;
pub mod title;
pub mod tools;
pub mod transcript;
pub mod turn;
pub mod writer;

#[cfg(test)]
mod tests;

pub use automated::{
    run_automated_session, AutomatedSessionInput, LlmLoading, LoadingProbe, DEFAULT_TOOLS_HINT,
    TITLE_DATETIME_PLACEHOLDER,
};
pub use background::{BackgroundJob, BackgroundQueue, BusyFn, LlmSummaries};
pub use muxer::{PendingCall, ResolvedTool, StreamMuxer, TurnResult};
pub use stream::{ActiveStream, ActiveStreams, LiveMessage, StreamGuard};
pub use title::TitleGen;
pub use tools::{Dispatcher, InFlight, InFlightEntry};
pub use transcript::{to_chat_messages, TranscriptSources};
pub use writer::{LiveRef, TurnWriter};

pub use deps::{
    ChatDeps, McpTokens, ModelIdle, NoIdle, NoSchedule, NoTools, Phase1Options, Phase1Result,
    SchedulePersist, ToolCallController, ToolCallEvent, ToolCallSpec, ToolEmbedding, ToolHost,
    ToolLogLevel,
};

use std::collections::HashSet;
use std::sync::{Arc, Mutex, MutexGuard};

use futures_util::future::BoxFuture;
use futures_util::stream::{self as futures_stream, BoxStream, StreamExt as _};
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::task::JoinHandle;

use crate::contract::api::{AskUserAnswer, ScheduledPromptDraft, ServerToClientFrame};
use crate::contract::inbound::{
    ChatInterruptWs, ChatStartWs, ChatSubscribeWs, ScheduleConfirmResponse, ToolAskUserResponse,
    ToolCancel, ToolPermissionResponse,
};
use crate::error::Result;
use crate::llm::{ChatMessage, Llm, LlmRequest, LlmRoute, TurnOptions};
use crate::memories::indexer::{all_memory_ids, index_memory, SummaryGenerator};
use crate::memories::{Embedding, MemoriesStore, MemoryIndexer};
use crate::sessions::chat_engine::{ChatEngine, OneShotDelta, OneShotRequest};
use crate::sessions::{SessionsStore, StoredMessage};
use crate::settings::access::str_setting;
use crate::settings::Settings;
use crate::transport::{FrameBus, InboundHandler};

/// The orchestrator. One per core.
///
/// A second instance would give `chat.interrupt` a map that does not contain
/// the stream `chat.start` created, and every interrupt, askUser answer and
/// permission response would silently do nothing. Cheap to clone; every clone
/// shares one active-stream map, one in-flight controller map and one queue.
#[derive(Clone)]
pub struct Chat {
    inner: Arc<ChatInner>,
}

impl Chat {
    /// Whether the tool host THIS chat holds can offer tools.
    ///
    /// A read port for the boot-seam suite (D-165). Deliberately asks the chat
    /// object rather than boot's local variable: the first version of that test
    /// read the `OnceLock` boot filled and stayed green when the shim was never
    /// handed to chat, which is half the wiring and the half that matters.
    pub fn tools_live(&self) -> bool {
        self.inner.deps.tools.is_live()
    }
}

/// Everything a turn reads or writes, in one value so `turn.rs` takes one
/// argument rather than a dozen.
pub(crate) struct ChatInner {
    pub sessions: SessionsStore,
    pub memories: MemoriesStore,
    pub embedding: Embedding,
    pub settings: Settings,
    pub llm: Llm,
    pub frames: Arc<FrameBus>,
    pub deps: ChatDeps,
    pub streams: Arc<ActiveStreams>,
    pub in_flight: Arc<InFlight>,
    pub dispatcher: Dispatcher,
    pub titles: TitleGen,
    pub queue: Arc<BackgroundQueue>,
    pub summaries: Arc<dyn SummaryGenerator>,
    /// Live turn tasks, so `shutdown` can wait for them rather than cancelling
    /// a tool call mid-flight.
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

/// What `Chat::new` needs. A struct rather than fourteen positional arguments,
/// so `boot` reads as a list of names and a later addition does not renumber
/// every call site.
pub struct ChatConfig {
    pub sessions: SessionsStore,
    pub memories: MemoriesStore,
    pub embedding: Embedding,
    pub settings: Settings,
    pub llm: Llm,
    pub frames: Arc<FrameBus>,
    pub deps: ChatDeps,
    pub summaries: Arc<dyn SummaryGenerator>,
}

impl Chat {
    pub fn new(config: ChatConfig) -> Self {
        let streams = Arc::new(ActiveStreams::new(Arc::clone(&config.deps.idle)));
        let in_flight = Arc::new(InFlight::new());
        let dispatcher = Dispatcher::new(
            Arc::clone(&config.deps.tools),
            Arc::clone(&config.frames),
            config.sessions.clone(),
            Arc::clone(&in_flight),
        );
        let titles = TitleGen::new(
            config.sessions.clone(),
            config.settings.clone(),
            config.llm.clone(),
            Arc::clone(&config.frames),
        );
        // The idle gate: background work runs only while no session has a turn
        // in flight. The predicate reads the same map the turns register in, so
        // there is no second source of truth to drift.
        let gate = Arc::clone(&streams);
        let queue = BackgroundQueue::new(Arc::new(move || !gate.is_empty()));
        Self {
            inner: Arc::new(ChatInner {
                sessions: config.sessions,
                memories: config.memories,
                embedding: config.embedding,
                settings: config.settings,
                llm: config.llm,
                frames: config.frames,
                deps: config.deps,
                streams,
                in_flight,
                dispatcher,
                titles,
                queue,
                summaries: config.summaries,
                tasks: Mutex::new(Vec::new()),
            }),
        }
    }

    /// Session ids with a turn in flight: generating, or awaiting or executing
    /// a tool. A session NOT in this set is idle.
    pub fn active_session_ids(&self) -> HashSet<String> {
        self.inner.streams.active_session_ids()
    }

    /// Publish the in-flight turn count into the core-status aggregate.
    pub fn attach_status(&self, status: crate::transport::status::CoreStatusService) {
        self.inner.streams.attach_status(status);
    }

    /// Cancel every turn, stop the queue, and WAIT for the turns to finish.
    ///
    /// The wait is the point. A turn with a tool call in flight holds an MCP
    /// session or an extension worker, and reaping those first turns a clean
    /// cancel into a mid-call failure the user sees as a broken tool. Called
    /// before `mcp.shutdown()` and `sidecars.shutdown()` for that reason.
    pub async fn shutdown(&self) {
        self.inner.streams.cancel_all();
        self.inner.queue.dispose();
        let tasks: Vec<JoinHandle<()>> = lock(&self.inner.tasks).drain(..).collect();
        for task in tasks {
            let _ = task.await;
        }
    }

    /// Begin a turn. Returns as soon as the task is spawned.
    pub async fn start(&self, client_id: &str, frame: ChatStartWs) {
        let stream_id = frame.stream_id.to_string();
        let session_id = frame.session_id.to_string();
        // An unknown session is logged and dropped with no frame, matching the
        // oracle: the id came from a client that should not have it.
        let session = match self
            .inner
            .sessions
            .get_or_throw(client_id, &session_id)
            .await
        {
            Ok(session) => session,
            Err(err) => {
                tracing::warn!(
                    target: "loca::chat",
                    "chat.start for unknown session {session_id}: {err}",
                );
                return;
            }
        };
        // Claimed under one lock, so two frames arriving together cannot both
        // register. The predicate is (client, session), NOT session alone: two
        // different clients CAN run turns on the same session, and every frame
        // is scoped to its owner.
        let Some(guard) = self.inner.streams.claim(&stream_id, &session.id, client_id) else {
            self.inner.frames.broadcast_to_client(
                client_id,
                &ServerToClientFrame::ChatError {
                    code: crate::contract::api::ErrorCode::SessionBusy,
                    message: "another stream is active on this session".to_string(),
                    stream_id,
                },
            );
            return;
        };

        let inner = Arc::clone(&self.inner);
        let task = tokio::spawn(async move {
            // The guard is moved in, so its `Drop` deregisters the stream on
            // every exit including a cancel at shutdown. A leaked entry would
            // make `active_session_ids` never go empty, wedging the background
            // queue's idle gate forever with nothing in the log.
            let guard = guard;
            let stream = Arc::clone(guard.stream());
            if let Err(err) = turn::run(&inner, Arc::clone(&stream), frame).await {
                tracing::error!(
                    target: "loca::chat",
                    "stream {} crashed: {err}",
                    stream.stream_id,
                );
                inner.frames.broadcast_to_client(
                    &stream.client_id,
                    &ServerToClientFrame::ChatError {
                        code: crate::contract::api::ErrorCode::InternalError,
                        message: err.message.clone(),
                        stream_id: stream.stream_id.clone(),
                    },
                );
            }
        });
        let mut tasks = lock(&self.inner.tasks);
        tasks.retain(|t| !t.is_finished());
        tasks.push(task);
    }

    /// Cancel a turn this client owns. Returns immediately; the turn winds
    /// itself down and persists everything it streamed.
    pub fn interrupt(&self, stream_id: &str, client_id: &str) {
        if let Some(stream) = self.inner.streams.owned(stream_id, client_id) {
            stream.cancel.cancel();
        }
    }

    /// Re-attach a client to a session it just opened or reconnected to.
    ///
    /// Replays this turn's born-but-not-yet-finalized messages in birth order
    /// with the `after_id` each was born at, then re-sends every open tool
    /// prompt. A no-op when nothing is in flight, and invisible to a client that
    /// does not own the stream.
    pub fn resubscribe(&self, client_id: &str, session_id: &str) {
        let Some(stream) = self.inner.streams.owned_on_session(client_id, session_id) else {
            return;
        };
        for live in stream.live_messages() {
            let validated = {
                let guard = live.message.lock().unwrap_or_else(|e| e.into_inner());
                guard.validate()
            };
            match validated {
                Ok(message) => self.inner.frames.broadcast_to_client(
                    client_id,
                    &ServerToClientFrame::ChatMessage {
                        after_id: live.after_id.clone(),
                        final_: false,
                        message,
                        session_id: stream.session_id.clone(),
                        stream_id: stream.stream_id.clone(),
                    },
                ),
                Err(err) => tracing::error!(
                    target: "loca::chat",
                    "stream {}: a live message does not satisfy the wire contract, \
                     so the catch-up snapshot was dropped: {err}",
                    stream.stream_id,
                ),
            }
        }
        // The worker stayed paused the whole time, so an open prompt is still
        // answerable.
        for frame in stream.outstanding_prompts() {
            self.inner.frames.broadcast_to_client(client_id, &frame);
        }
    }

    pub fn forward_ask_user(
        &self,
        call_id: &str,
        request_id: &str,
        answers: Vec<AskUserAnswer>,
        client_id: &str,
    ) {
        let Some(controller) = self.inner.in_flight.owned(call_id, client_id) else {
            return;
        };
        self.clear_prompt(call_id, client_id);
        controller.respond_ask_user(request_id, answers);
    }

    pub fn forward_permission(
        &self,
        call_id: &str,
        request_id: &str,
        allow: bool,
        client_id: &str,
    ) {
        let Some(controller) = self.inner.in_flight.owned(call_id, client_id) else {
            return;
        };
        self.clear_prompt(call_id, client_id);
        controller.respond_permission(request_id, allow);
    }

    /// Settle a schedule confirm.
    ///
    /// Two guards rather than one, and the second is what makes this
    /// idempotent: a response whose `request_id` is not the OPEN confirm (stale,
    /// replayed, or forged) must not persist anything, because the insert would
    /// otherwise run once per replay.
    ///
    /// The persist happens BEFORE the tool is unblocked, so a storage failure
    /// answers the tool `false` and the tool never reports "scheduled" for
    /// something that was not stored.
    pub fn forward_schedule(
        &self,
        call_id: &str,
        request_id: &str,
        accepted: bool,
        draft: Option<ScheduledPromptDraft>,
        client_id: &str,
    ) {
        let Some(controller) = self.inner.in_flight.owned(call_id, client_id) else {
            return;
        };
        if !controller.has_pending_schedule(request_id) {
            return;
        }
        self.clear_prompt(call_id, client_id);
        let confirmed = accepted && draft.is_some();
        if confirmed {
            if let Some(draft) = &draft {
                if let Err(err) = self.inner.deps.schedule.create(client_id, draft) {
                    tracing::error!(
                        target: "loca::chat",
                        "schedule confirm: persisting the draft failed: {err}",
                    );
                    controller.respond_schedule(request_id, false, None);
                    return;
                }
            }
        }
        controller.respond_schedule(request_id, confirmed, confirmed.then_some(draft).flatten());
    }

    pub fn forward_cancel(&self, call_id: &str, client_id: &str) {
        if let Some(controller) = self.inner.in_flight.owned(call_id, client_id) {
            controller.cancel();
        }
    }

    fn clear_prompt(&self, call_id: &str, client_id: &str) {
        if let Some(stream) = self.inner.in_flight.owned_stream(call_id, client_id) {
            stream.clear_prompt(call_id);
        }
    }
}

impl ChatInner {
    /// Fire and forget: the turn is over and the client learns the outcome from
    /// `session.updated` frames.
    pub(crate) fn spawn_title(&self, session_id: &str, client_id: &str) {
        let titles = self.titles.clone();
        let session_id = session_id.to_string();
        let client_id = client_id.to_string();
        tokio::spawn(async move {
            titles.maybe_generate(&session_id, &client_id).await;
        });
    }
}

impl InboundHandler for Chat {
    /// Seven kinds, each deserialised into its `contract::inbound` type, which
    /// is where the validation lives: deserialising IS the validation, so there
    /// is no second hand-written check after it.
    ///
    /// A frame that fails to deserialise is logged and dropped with nothing sent
    /// back, matching the oracle's hub. An unrecognised kind lands here rather
    /// than in the transport, which forwards unknown kinds deliberately.
    fn handle(&self, client_id: &str, kind: &str, frame: Value) {
        let chat = self.clone();
        let client_id = client_id.to_string();
        match kind {
            "chat.start" => {
                let Some(parsed) = parse::<ChatStartWs>(kind, &frame) else {
                    return;
                };
                tokio::spawn(async move { chat.start(&client_id, parsed).await });
            }
            "chat.interrupt" => {
                let Some(parsed) = parse::<ChatInterruptWs>(kind, &frame) else {
                    return;
                };
                chat.interrupt(&parsed.stream_id, &client_id);
            }
            "chat.subscribe" => {
                let Some(parsed) = parse::<ChatSubscribeWs>(kind, &frame) else {
                    return;
                };
                chat.resubscribe(&client_id, &parsed.session_id);
            }
            "tool.askuser_response" => {
                let Some(parsed) = parse::<ToolAskUserResponse>(kind, &frame) else {
                    return;
                };
                // Validated as the inbound shape above; re-read as the API shape
                // the controller takes, from the same bytes. The two are
                // generated from one source, so this cannot lose a field.
                let Some(answers) = field::<Vec<AskUserAnswer>>(&frame, "answers") else {
                    return;
                };
                chat.forward_ask_user(&parsed.call_id, &parsed.request_id, answers, &client_id);
            }
            "tool.permission_response" => {
                let Some(parsed) = parse::<ToolPermissionResponse>(kind, &frame) else {
                    return;
                };
                chat.forward_permission(
                    &parsed.call_id,
                    &parsed.request_id,
                    parsed.allow,
                    &client_id,
                );
            }
            "tool.cancel" => {
                let Some(parsed) = parse::<ToolCancel>(kind, &frame) else {
                    return;
                };
                chat.forward_cancel(&parsed.call_id, &client_id);
            }
            "schedule.confirm_response" => {
                let Some(parsed) = parse::<ScheduleConfirmResponse>(kind, &frame) else {
                    return;
                };
                let draft = parsed
                    .draft
                    .as_ref()
                    .and_then(|_| field::<ScheduledPromptDraft>(&frame, "draft"));
                chat.forward_schedule(
                    &parsed.call_id,
                    &parsed.request_id,
                    parsed.accepted,
                    draft,
                    &client_id,
                );
            }
            other => tracing::warn!(target: "loca::chat", "unknown ws frame kind: {other}"),
        }
    }
}

impl ChatEngine for Chat {
    fn one_shot(&self, req: OneShotRequest) -> BoxStream<'static, Result<OneShotDelta>> {
        let inner = Arc::clone(&self.inner);
        futures_stream::once(async move { one_shot(inner, req).await })
            .flat_map(|started| match started {
                Ok(deltas) => deltas,
                Err(err) => futures_stream::once(async move { Err(err) }).boxed(),
            })
            .boxed()
    }

    fn regenerate_title(&self, session_id: String, owner_client_id: String) {
        let titles = self.inner.titles.clone();
        tokio::spawn(async move {
            titles.regenerate(&session_id, &owner_client_id).await;
        });
    }
}

/// The storage screen's view of the sessions on disk.
///
/// On `Chat` rather than on `SessionsStore`, because the seam asks two
/// questions and only one of them is the store's: which sessions have an agent
/// mid-turn is this module's fact, and the alternative is boot assembling a
/// pair-holding adapter that would then be the only place the two are joined.
#[async_trait::async_trait]
impl crate::storage::deps::SessionsSource for Chat {
    async fn list(&self) -> Result<Vec<crate::storage::deps::SessionSummary>> {
        Ok(self
            .inner
            .sessions
            .list_all()
            .await
            .into_iter()
            .map(|footprint| crate::storage::deps::SessionSummary {
                id: footprint.id,
                title: footprint.title,
                updated_at_ms: footprint.updated_at_ms,
                size_bytes: footprint.size_bytes,
            })
            .collect())
    }

    async fn active_ids(&self) -> Result<std::collections::HashSet<String>> {
        Ok(self.active_session_ids())
    }

    /// Owner-agnostic: the storage screen deletes by directory, and a session
    /// belonging to another paired client is still this core's disk.
    async fn delete(&self, session_id: &str) -> Result<()> {
        if let Some(deleted) = self.inner.sessions.delete_by_id(session_id).await {
            for path in deleted.attachment_paths {
                // Removing the session directory already took the attachments
                // stored inside it, so a missing file is the normal case. Same
                // handling as the delete route (`sessions/routes.rs:180-193`),
                // where a real failure is worth a warning and never fails the
                // delete.
                match crate::sessions::store::remove_file(path.clone()).await {
                    Ok(()) => {}
                    Err(err) if err.code == crate::error::ErrorCode::NotFound => {}
                    Err(err) => tracing::warn!(
                        target: "loca::storage",
                        "session delete: failed to remove attachment {}: {}",
                        path.display(), err.message,
                    ),
                }
            }
        }
        Ok(())
    }
}

impl MemoryIndexer for Chat {
    fn schedule(&self, memory_id: Option<&str>) {
        let inner = Arc::clone(&self.inner);
        let queue = Arc::clone(&self.inner.queue);
        match memory_id {
            Some(id) => {
                let id = id.to_string();
                let key = format!("memory-index:{id}");
                queue.enqueue(BackgroundJob::new(key, move || index_one(inner, id)));
            }
            None => queue.enqueue(BackgroundJob::new("memory-index:*", move || {
                index_all(inner)
            })),
        }
    }
}

/// One memory's pass, re-armed when the embedding step could not run.
///
/// A boxed future rather than an `async fn`, because it re-enqueues ITSELF on
/// the deferred path and an `impl Future` that appears in its own body has no
/// nameable type for the compiler to prove `Send` of.
fn index_one(inner: Arc<ChatInner>, id: String) -> BoxFuture<'static, Result<()>> {
    Box::pin(async move {
        let outcome = index_memory(
            &inner.memories,
            &inner.embedding,
            &inner.summaries,
            &inner.settings,
            &id,
        )
        .await?;
        if outcome.embedding_deferred {
            // The embedding model is not downloaded yet. Re-arm rather than
            // drop, so the vector lands once it is rather than waiting for the
            // next unrelated mutation.
            let queue = Arc::clone(&inner.queue);
            let key = format!("memory-index:{id}");
            queue.enqueue_deferred(BackgroundJob::new(key, move || index_one(inner, id)));
        }
        Ok(())
    })
}

/// Every memory, as one job: a boot or rescan pass. Individual failures are
/// logged and the pass continues, so one bad memory does not stop the rest.
fn index_all(inner: Arc<ChatInner>) -> BoxFuture<'static, Result<()>> {
    Box::pin(async move {
        for id in all_memory_ids(&inner.memories).await? {
            if let Err(err) = index_one(Arc::clone(&inner), id.clone()).await {
                tracing::warn!(target: "loca::bgqueue", "indexing memory {id} failed: {err}");
            }
        }
        Ok(())
    })
}

/// The SSE fallback's completion: text in, deltas out, and no store writes.
///
/// No tools, no reasoning trace, no multiplexing; the WebSocket path covers all
/// three. The system prompt is `prompts.defaultSystemPrompt` read from the
/// SPARSE core record, which is what the route reads today.
async fn one_shot(
    inner: Arc<ChatInner>,
    req: OneShotRequest,
) -> Result<BoxStream<'static, Result<OneShotDelta>>> {
    let settings = inner.settings.load().await?;
    let endpoint = inner
        .llm
        .resolve_endpoint(&settings, LlmRoute::Default)
        .await?;
    let system_prompt = str_setting(&settings, "prompts.defaultSystemPrompt", "");
    let messages = plain_history(&req.history, &system_prompt);
    let deltas = inner
        .llm
        .stream(
            LlmRequest::new(endpoint, messages),
            TurnOptions {
                client_id: req.owner_client_id.clone(),
                parallel_slots: None,
            },
        )
        .await?;
    Ok(deltas
        .map(|item| match item {
            Ok(delta) => Ok(OneShotDelta {
                content_delta: delta.content_delta,
                usage: delta.usage,
            }),
            Err(err) => Err(crate::llm::classify_provider_error(&err)),
        })
        .boxed())
}

/// The transcript as this path takes it: text only, no attachments, no tool
/// replay. The full builder is the WebSocket path's.
fn plain_history(history: &[StoredMessage], system_prompt: &str) -> Vec<ChatMessage> {
    let mut out = Vec::new();
    if !system_prompt.is_empty() {
        out.push(ChatMessage::system(system_prompt));
    }
    for message in history {
        let text = crate::sessions::content_to_text(message.content());
        match message.role().unwrap_or_default() {
            "user" => out.push(ChatMessage::user(text)),
            "assistant" => out.push(ChatMessage::assistant(text)),
            "system" => out.push(ChatMessage::system(text)),
            _ => {}
        }
    }
    out
}

fn parse<T: DeserializeOwned>(kind: &str, frame: &Value) -> Option<T> {
    match serde_json::from_value::<T>(frame.clone()) {
        Ok(parsed) => Some(parsed),
        Err(err) => {
            tracing::warn!(target: "loca::chat", "invalid {kind} frame: {err}");
            None
        }
    }
}

fn field<T: DeserializeOwned>(frame: &Value, key: &str) -> Option<T> {
    let value = frame.get(key)?.clone();
    match serde_json::from_value::<T>(value) {
        Ok(parsed) => Some(parsed),
        Err(err) => {
            tracing::warn!(target: "loca::chat", "invalid {key} on a chat frame: {err}");
            None
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}
