//! One hop's worth of streaming state.
//!
//! Ports `core-engine/src/services/chat-stream-muxer.ts` in full: the
//! born-on-first-delta skeletons, the delta coalescer, the
//! reasoning-before-content finalize ordering, tool-call assembly with
//! stream-namespaced call ids, and the usage and truncation accounting.
//!
//! # Five orderings that are invariants, not style
//!
//! The TypeScript header says "do not reorder" and lists them. They are repeated
//! here because each one, broken, produces a transcript that is wrong in a way
//! no single-frame assertion notices:
//!
//! 1. **Reasoning finalizes at the FIRST content delta**, not after the stream,
//!    so the thought bubble closes with its duration while the reply is still
//!    streaming. Finalizing later leaves a bubble open through the whole reply.
//! 2. **Reasoning deltas arriving after the first content delta are dropped.**
//!    The bubble is closed; appending to it would emit a `chat.delta` for an id
//!    that already has its terminal snapshot.
//! 3. **Pending deltas flush before any final snapshot.** Otherwise a
//!    `chat.delta` can arrive after the `final: true` message it belongs to, and
//!    the client appends it to a closed bubble.
//! 4. **The tail flushes on BOTH the success and the error path.** A turn that
//!    fails mid-stream still owns the tokens it already produced.
//! 5. **The call id is `${streamId}:${index}:${modelId}`.** Two concurrent turns
//!    whose models both emit `call_1` would otherwise collide in the shared
//!    in-flight map, and a collision makes every askUser, permission and cancel
//!    forward fail its owner check and hang both prompts.
//!
//! # The live-content lag is deliberate
//!
//! A live message's `content` advances to the last FLUSHED position, never to
//! the bleeding edge. A mid-stream `chat.subscribe` replays the live snapshot
//! and then resumes deltas, so a snapshot carrying the not-yet-flushed tail
//! would be followed by a `chat.delta` repeating it, and the client would show
//! the fragment twice. Between flushes the live object lags by one coalesce
//! window, and that is the correct amount.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use futures_util::stream::StreamExt as _;
use serde_json::{Map, Value};
use tokio::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

use crate::contract::api::{ServerToClientFrame, TokenUsage};
use crate::error::AppError;
use crate::llm::{classify_provider_error, DeltaStream, LlmRoute, ProviderError};
use crate::sessions::doc::StoredMessage;
use crate::transport::FrameBus;

use super::stream::ActiveStream;
use super::writer::{LiveRef, TurnWriter};

/// One `chat.delta` frame per window instead of one per token.
///
/// A fast local model emits many small tokens, and a `serde_json::to_string`
/// plus a socket write each dominates the cost and grows the send buffer under a
/// slow consumer. Every token is still accumulated synchronously, so this is
/// lossless: only the wire frames are batched.
const COALESCE: Duration = Duration::from_millis(30);

/// A tool call the model asked for, resolved against the enabled catalog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingCall {
    /// Namespaced by stream and index. See invariant 5.
    pub call_id: String,
    pub extension_id: String,
    pub tool_name: String,
    /// As the model emitted it: a JSON string, not an object. `{}` when the
    /// model sent none.
    pub arguments: String,
    /// The name matched no enabled tool. Dispatched anyway, so the model gets a
    /// tool message explaining the failure on its next hop.
    pub unknown: bool,
}

/// What one hop produced, for the orchestrator to finalize and feed back.
#[derive(Default)]
pub struct TurnResult {
    pub assistant: Option<LiveRef>,
    pub reasoning: Option<LiveRef>,
    pub tool_calls: Vec<PendingCall>,
    pub interrupted: bool,
    pub truncated: bool,
    pub error: Option<AppError>,
    /// What this hop reported, for the caller to PERSIST. Broadcasting it is
    /// not enough on its own: the client renders the context gauge from the
    /// session document on load, so a usage that only ever rode a live frame is
    /// gone the moment the session is reopened.
    pub usage: Option<TokenUsage>,
}

/// Where a resolved tool name lives.
pub struct ResolvedTool {
    pub extension_id: String,
}

/// Assembles one tool call across the chunks that carry it.
///
/// Keyed by the provider's `index`, in a `BTreeMap` rather than a `HashMap`
/// because the index is also the namespace component of the call id and the
/// resolution order has to be stable: a `HashMap` would give two runs of the
/// same stream different call ids for the same calls.
#[derive(Default)]
struct Assembler {
    call_id: Option<String>,
    tool_name: String,
    args: String,
}

pub struct StreamMuxer {
    stream: Arc<ActiveStream>,
    frames: Arc<FrameBus>,
    route: LlmRoute,
}

/// The mutable half of one hop, split out so the coalescer and the finalize
/// paths can share it without threading a dozen locals through every call.
struct Turn {
    assistant_text: String,
    reasoning_text: String,
    assistant: Option<LiveRef>,
    reasoning: Option<LiveRef>,
    /// Set on the first reasoning chunk and the first content chunk. The pair is
    /// what gives "Thought for Xs" without recomputing from timestamps.
    ///
    /// `tokio::time::Instant`, not `std::time::Instant`: a paused test runtime
    /// advances only tokio's clock, so a duration measured with `std::time`
    /// cannot move under one and a test of it passes whatever the code does.
    /// D-038 is the worked example.
    reasoning_started: Option<Instant>,
    content_started: Option<Instant>,
    pending_content: String,
    pending_reasoning: String,
    reasoning_finalized: bool,
    assemblers: BTreeMap<u32, Assembler>,
    usage: Option<TokenUsage>,
    last_finish_reason: Option<String>,
}

impl StreamMuxer {
    pub fn new(stream: Arc<ActiveStream>, frames: Arc<FrameBus>, route: LlmRoute) -> Self {
        Self {
            stream,
            frames,
            route,
        }
    }

    /// Drive one hop to completion.
    ///
    /// `resolve` maps a tool name to its owner, and is the enabled-and-exposable
    /// catalog collapsed to one lookup. A name it does not know becomes an
    /// `unknown` call rather than being dropped, so the model learns why.
    pub async fn run(
        &self,
        mut deltas: DeltaStream,
        writer: &mut TurnWriter,
        cancel: &CancellationToken,
        resolve: &(dyn Fn(&str) -> Option<ResolvedTool> + Send + Sync),
    ) -> TurnResult {
        let mut turn = Turn::new();
        let mut failure: Option<ProviderError> = None;
        let mut flush_due: Option<Instant> = None;

        loop {
            let next = if let Some(due) = flush_due {
                // A pending batch is waiting on the clock as well as on the
                // provider, so the select has three arms rather than two. The
                // sleep is what bounds latency for a model that goes quiet
                // mid-sentence: without it the tail would sit unsent until the
                // next token, which on a stalled provider is never.
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => break,
                    _ = tokio::time::sleep_until(due) => {
                        self.flush(&mut turn);
                        flush_due = None;
                        continue;
                    }
                    item = deltas.next() => item,
                }
            } else {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => break,
                    item = deltas.next() => item,
                }
            };

            let Some(item) = next else { break };
            let delta = match item {
                Ok(delta) => delta,
                Err(err) => {
                    failure = Some(err);
                    break;
                }
            };

            if let Some(reason) = delta.finish_reason {
                turn.last_finish_reason = Some(reason);
            }
            if let Some(usage) = delta.usage {
                turn.usage = Some(usage);
            }
            for chunk in &delta.tool_calls {
                let asm = turn.assemblers.entry(chunk.index).or_default();
                // `id` and `name` may arrive on the first fragment only, or be
                // repeated. A later fragment that carries either overwrites,
                // matching the TypeScript; one that carries neither extends the
                // arguments and nothing else.
                if let Some(id) = &chunk.id {
                    asm.call_id = Some(id.clone());
                }
                if let Some(name) = &chunk.name {
                    asm.tool_name = name.clone();
                }
                if let Some(args) = &chunk.arguments_delta {
                    asm.args.push_str(args);
                }
            }
            if let Some(text) = delta.content_delta.filter(|s| !s.is_empty()) {
                if turn.content_started.is_none() {
                    turn.content_started = Some(Instant::now());
                    let message = new_message(&self.route, "assistant");
                    writer.born(&message);
                    turn.assistant = Some(message);
                    // Invariant 1: the thought bubble closes HERE, at the first
                    // content, not on the way out.
                    self.finalize_reasoning(&mut turn, writer, false).await;
                }
                turn.assistant_text.push_str(&text);
                turn.pending_content.push_str(&text);
                flush_due.get_or_insert_with(|| Instant::now() + COALESCE);
            }
            if let Some(text) = delta.reasoning_delta.filter(|s| !s.is_empty()) {
                // Invariant 2. Not an `else`: one chunk can legitimately carry
                // both, and the content half must still land.
                if turn.content_started.is_none() {
                    if turn.reasoning_started.is_none() {
                        turn.reasoning_started = Some(Instant::now());
                        let message = new_message(&self.route, "reasoning");
                        writer.born(&message);
                        turn.reasoning = Some(message);
                    }
                    turn.reasoning_text.push_str(&text);
                    turn.pending_reasoning.push_str(&text);
                    flush_due.get_or_insert_with(|| Instant::now() + COALESCE);
                }
            }
        }

        // Invariant 4: the tail flushes on every path out of the loop, before
        // anything else is decided.
        self.flush(&mut turn);

        // A cancel usually lands here rather than as a stream error: dropping
        // the stream ends the iteration without producing one.
        if cancel.is_cancelled() {
            self.settle(&mut turn, writer, true).await;
            return TurnResult {
                assistant: turn.assistant,
                reasoning: turn.reasoning,
                interrupted: true,
                ..TurnResult::default()
            };
        }
        if let Some(err) = failure {
            self.settle(&mut turn, writer, true).await;
            let classified = classify_provider_error(&err);
            tracing::error!(
                target: "loca::chat",
                "stream {}: llm call failed ({}): {}",
                self.stream.stream_id, classified.code.as_str(), classified.message,
            );
            return TurnResult {
                assistant: turn.assistant,
                reasoning: turn.reasoning,
                error: Some(classified),
                ..TurnResult::default()
            };
        }

        let resolved = self.resolve_calls(&turn, resolve);

        // A tool-call-only hop still needs an assistant message: the persisted
        // `toolCalls` are what lets a later transcript rebuild replay them, and
        // without it every following `tool` message is orphaned. It never
        // streamed content, so it was never born, and its finalize IS its first
        // emission.
        if turn.assistant.is_none() && !resolved.is_empty() {
            turn.assistant = Some(new_message(&self.route, "assistant"));
        }
        if let (Some(assistant), false) = (turn.assistant.as_ref(), resolved.is_empty()) {
            set_tool_calls(assistant, &resolved);
        }
        self.settle(&mut turn, writer, false).await;

        // The model hit the context window on a non-tool hop, so the reply is
        // cut off. When thinking consumed the whole window there is no content
        // at all, and an empty assistant is synthesised to carry the note: the
        // client renders "cut off" from this flag, and with no message there is
        // nothing to render it on. A tool hop finishes `tool_calls`, not
        // `length`, which is why the emptiness of `resolved` is part of the test.
        let truncated = turn.last_finish_reason.as_deref() == Some("length") && resolved.is_empty();
        if truncated {
            let assistant = turn
                .assistant
                .get_or_insert_with(|| new_message(&self.route, "assistant"));
            set_flag(assistant, "truncated");
        }

        if let Some(usage) = turn.usage.clone() {
            self.frames.broadcast_to_client(
                &self.stream.client_id,
                &ServerToClientFrame::ChatUsage {
                    stream_id: self.stream.stream_id.clone(),
                    token_usage: usage,
                },
            );
        }

        TurnResult {
            assistant: turn.assistant,
            reasoning: turn.reasoning,
            tool_calls: resolved,
            truncated,
            usage: turn.usage,
            ..TurnResult::default()
        }
    }

    /// Emit whatever has accumulated since the last flush, and advance each live
    /// object to exactly what has now been EMITTED.
    ///
    /// The advance belongs here and not at the append, and the reason is the
    /// resubscribe path rather than efficiency: see the module header.
    fn flush(&self, turn: &mut Turn) {
        if !turn.pending_reasoning.is_empty() {
            if let Some(message) = turn.reasoning.as_ref() {
                self.send_delta(message, std::mem::take(&mut turn.pending_reasoning));
                set_content(message, &turn.reasoning_text);
            } else {
                turn.pending_reasoning.clear();
            }
        }
        if !turn.pending_content.is_empty() {
            if let Some(message) = turn.assistant.as_ref() {
                self.send_delta(message, std::mem::take(&mut turn.pending_content));
                set_content(message, &turn.assistant_text);
            } else {
                turn.pending_content.clear();
            }
        }
    }

    fn send_delta(&self, message: &LiveRef, delta: String) {
        let message_id = lock(message).id().unwrap_or_default().to_string();
        self.frames.broadcast_to_client(
            &self.stream.client_id,
            &ServerToClientFrame::ChatDelta {
                delta,
                message_id,
                stream_id: self.stream.stream_id.clone(),
            },
        );
    }

    /// Close the thought bubble: flush first so no `chat.delta` for its id can
    /// trail its terminal snapshot (invariant 3), then stamp the duration and
    /// the paired assistant id, then persist.
    async fn finalize_reasoning(
        &self,
        turn: &mut Turn,
        writer: &mut TurnWriter,
        interrupted: bool,
    ) {
        if turn.reasoning_finalized {
            return;
        }
        let Some(message) = turn.reasoning.clone() else {
            return;
        };
        turn.reasoning_finalized = true;
        self.flush(turn);
        set_content(&message, &turn.reasoning_text);
        let end = turn.content_started.unwrap_or_else(Instant::now);
        let started = turn.reasoning_started.unwrap_or(end);
        let elapsed = end.saturating_duration_since(started).as_millis() as i64;
        {
            let mut guard = lock(&message);
            guard
                .as_map_mut()
                .insert("reasoningDurationMs".into(), Value::from(elapsed));
            if let Some(assistant) = turn.assistant.as_ref() {
                let paired = lock(assistant).id().unwrap_or_default().to_string();
                guard
                    .as_map_mut()
                    .insert("pairedAssistantId".into(), Value::String(paired));
            }
            if interrupted {
                guard
                    .as_map_mut()
                    .insert("interrupted".into(), Value::Bool(true));
            }
        }
        if let Err(err) = writer.finalize(&message).await {
            tracing::error!(
                target: "loca::chat",
                "stream {}: persisting the reasoning message failed: {err}",
                self.stream.stream_id,
            );
        }
    }

    /// Fill the streamed text into both skeletons on the way out.
    ///
    /// Runs on EVERY exit path, so a partial message from an interrupt or a
    /// provider failure comes back for the caller to finalize rather than being
    /// dropped. That is what makes "live equals reload" true after an interrupt.
    async fn settle(&self, turn: &mut Turn, writer: &mut TurnWriter, interrupted: bool) {
        self.finalize_reasoning(turn, writer, interrupted).await;
        if let Some(assistant) = turn.assistant.as_ref() {
            set_content(assistant, &turn.assistant_text);
            if interrupted {
                set_flag(assistant, "interrupted");
            }
        }
    }

    fn resolve_calls(
        &self,
        turn: &Turn,
        resolve: &(dyn Fn(&str) -> Option<ResolvedTool> + Send + Sync),
    ) -> Vec<PendingCall> {
        turn.assemblers
            .iter()
            .map(|(index, asm)| {
                // Invariant 5. The model only needs `tool_calls[].id` and the
                // result's `tool_call_id` to match EACH OTHER within a request,
                // and they still do: both carry this same value.
                let model_id = asm
                    .call_id
                    .clone()
                    .unwrap_or_else(|| ulid::Ulid::new().to_string());
                let call_id = format!("{}:{index}:{model_id}", self.stream.stream_id);
                let arguments = if asm.args.is_empty() {
                    "{}".to_string()
                } else {
                    asm.args.clone()
                };
                match resolve(&asm.tool_name) {
                    Some(found) => PendingCall {
                        call_id,
                        extension_id: found.extension_id,
                        tool_name: asm.tool_name.clone(),
                        arguments,
                        unknown: false,
                    },
                    // Not dropped: the model gets a tool message saying the tool
                    // was unavailable, which it can act on. Dropping it would
                    // leave the model waiting for a result that never comes.
                    None => PendingCall {
                        call_id,
                        extension_id: "unknown".to_string(),
                        tool_name: asm.tool_name.clone(),
                        arguments,
                        unknown: true,
                    },
                }
            })
            .collect()
    }
}

impl Turn {
    fn new() -> Self {
        Self {
            assistant_text: String::new(),
            reasoning_text: String::new(),
            assistant: None,
            reasoning: None,
            reasoning_started: None,
            content_started: None,
            pending_content: String::new(),
            pending_reasoning: String::new(),
            reasoning_finalized: false,
            assemblers: BTreeMap::new(),
            usage: None,
            last_finish_reason: None,
        }
    }
}

/// A chat-born message skeleton: a server-minted id, the unassigned `ord`
/// sentinel, and the route it was produced on.
fn new_message(route: &LlmRoute, role: &str) -> LiveRef {
    let mut map = Map::new();
    map.insert("id".into(), Value::String(ulid::Ulid::new().to_string()));
    map.insert("ord".into(), Value::from(-1));
    map.insert("role".into(), Value::String(role.to_string()));
    map.insert("content".into(), Value::String(String::new()));
    map.insert("createdAtMs".into(), Value::from(now_ms()));
    map.insert(
        "modelUsed".into(),
        Value::String(route.as_str().to_string()),
    );
    Arc::new(Mutex::new(StoredMessage::new(map)))
}

fn set_content(message: &LiveRef, content: &str) {
    lock(message)
        .as_map_mut()
        .insert("content".into(), Value::String(content.to_string()));
}

fn set_flag(message: &LiveRef, key: &str) {
    lock(message)
        .as_map_mut()
        .insert(key.into(), Value::Bool(true));
}

fn set_tool_calls(message: &LiveRef, calls: &[PendingCall]) {
    let calls: Vec<Value> = calls
        .iter()
        .map(|call| {
            let mut map = Map::new();
            map.insert("callId".into(), Value::String(call.call_id.clone()));
            map.insert(
                "extensionId".into(),
                Value::String(call.extension_id.clone()),
            );
            map.insert("toolName".into(), Value::String(call.tool_name.clone()));
            map.insert("arguments".into(), Value::String(call.arguments.clone()));
            map.insert("status".into(), Value::String("pending".into()));
            Value::Object(map)
        })
        .collect();
    lock(message)
        .as_map_mut()
        .insert("toolCalls".into(), Value::Array(calls));
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}
