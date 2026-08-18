//! One turn, start to finish.
//!
//! Ports `core-engine/src/services/chat.ts:286-581`: route resolution, the
//! endpoint gate, the anchor, tool selection, memory injection, MCP token
//! expansion, and the hop loop with its five exits.
//!
//! # The five exits
//!
//! ```text
//! for hop in 0..max_hops {
//!     cancelled            -> chat.done { interrupted }
//!     ... stream the hop, finalize what it produced ...
//!     interrupted          -> chat.done { interrupted }
//!     error                -> chat.error { code, message }
//!     no tool calls        -> chat.done { stop | length }, then title generation
//!     otherwise: run every tool call, append the results, loop
//! }
//! chat.done { hop_limit }
//! ```
//!
//! Reaching the hop cap is a stop condition, not an error: the conversation up
//! to that point is valid and fully persisted, and no synthetic assistant
//! message is added.
//!
//! # Three orderings inside the loop that are invariants
//!
//! 1. The reasoning message joins the history WITHOUT a second `finalize`. The
//!    muxer already finalized it, at the first content delta; finalizing again
//!    would insert it twice.
//! 2. The tool message is finalized BEFORE its display bubbles, so the durable
//!    order matches the live order the client saw. The dispatcher returns
//!    displays deliberately unfinalized to make that possible.
//! 3. Only the tool message joins the history. Displays are client-facing and
//!    never reach the model.

use std::sync::Arc;

use serde_json::{Map, Value};

use crate::contract::api::{
    ChatDoneReason, ServerToClientFrame, ServerToClientFrameOp, ServerToClientFramePayload,
};
use crate::contract::inbound::ChatStartWs;
use crate::error::{AppError, ErrorCode, Result};
use crate::llm::{LlmRequest, LlmRoute, TurnOptions};
use crate::memories::injection::{has_memories, relevant_memories, relevant_memories_block};
use crate::sessions::attachments::AttachmentCache;
use crate::sessions::doc::StoredMessage;
use crate::sessions::history::{last_user_id, last_user_text};
use crate::settings::access::{bool_setting, num_setting, str_setting};

use super::complexity::classify_complexity;
use super::muxer::{ResolvedTool, StreamMuxer};
use super::stream::ActiveStream;
use super::tools::selection::{build_tool_list, enabled_tools_by_name, SelectionInputs};
use super::transcript::{to_chat_messages, TranscriptSources};
use super::writer::{LiveRef, TurnWriter};
use super::ChatInner;

/// The `tools.maxHops` fallback: how many model round trips one turn may take.
const DEFAULT_MAX_TOOL_HOPS: f64 = 5.0;

pub(super) async fn run(
    chat: &ChatInner,
    stream: Arc<ActiveStream>,
    frame: ChatStartWs,
) -> Result<()> {
    // Two reads of the same settings, and the split is load-bearing.
    //
    // `resolved` is what the turn ACTS on: every `*_setting` read below sees the
    // schema's default rather than whatever fallback the call site happens to
    // hardcode (D-072). `sparse` is what the ENDPOINT is resolved from, and it
    // must stay sparse: `llm/resolver.rs` documents two of its fallbacks as
    // deliberately different from the schema's, and `llm.port`'s schema default
    // is not channel-adjusted, so a resolved record would point a `dev` core at
    // the stable channel's llama-server. See this package's status notes.
    let resolved = chat
        .settings
        .load_effective_resolved(&stream.client_id)
        .await?;
    let sparse = chat.settings.load_effective(&stream.client_id).await?;
    // Reload the local model if idle-unloading stopped it. A no-op for an
    // external provider or a core with no local inference.
    chat.deps.idle.ensure_loaded(&resolved).await?;

    let route = resolve_route(chat, &stream, &resolved, &sparse, &frame).await;

    let endpoint = chat.llm.resolve_endpoint(&sparse, route).await?;
    // An external provider cannot stream until it is configured: an empty base
    // URL, API key or model otherwise reaches the HTTP client and fails with an
    // opaque network or auth error. Local always resolves a loopback URL plus a
    // placeholder key and model, so this only ever trips for an unconfigured
    // external (or dual-model) provider.
    if endpoint.base_url.is_empty() || endpoint.api_key.is_empty() || endpoint.model.is_empty() {
        send(
            chat,
            &stream,
            ServerToClientFrame::ChatError {
                code: crate::contract::api::ErrorCode::ProviderError,
                message: "This model provider isn't set up yet. Open Settings and add the \
                          provider's Base URL, API Key, and Model."
                    .to_string(),
                stream_id: stream.stream_id.clone(),
            },
        );
        return Ok(());
    }

    let max_hops = num_setting(&resolved, "tools.maxHops", DEFAULT_MAX_TOOL_HOPS).max(0.0) as u64;
    // The client composes the effective prompt per turn, because its context
    // block holds client-local facts (date, time, OS), and sends it on the
    // frame. An empty string deliberately means "no system prompt"; the core
    // setting is the fallback for a frame that carries none.
    let mut system_prompt = match &frame.system_prompt {
        Some(prompt) => prompt.clone(),
        None => str_setting(&resolved, "prompts.defaultSystemPrompt", ""),
    };

    let mut history = chat.sessions.list_messages(&stream.session_id).await;
    // An explicit anchor is a regenerate (edit and resend): the old turn's
    // messages are deleted server-side and the new ones are inserted into its
    // slot.
    let anchor_id: Option<String> = match frame.anchor_message_id.as_ref() {
        Some(anchor) => {
            let anchor = anchor.to_string();
            let removed = chat
                .sessions
                .delete_turn(&stream.session_id, &anchor)
                .await?;
            for id in &removed {
                send(
                    chat,
                    &stream,
                    ServerToClientFrame::SessionUpdated {
                        op: ServerToClientFrameOp::MessageDeleted,
                        payload: Some(ServerToClientFramePayload {
                            message_id: Some(id.clone()),
                            ..ServerToClientFramePayload::default()
                        }),
                        session_id: stream.session_id.clone(),
                    },
                );
            }
            if !removed.is_empty() {
                history = chat.sessions.list_messages(&stream.session_id).await;
            }
            Some(anchor)
        }
        None => last_user_id(&history).map(str::to_string),
    };
    // Condition the model on the anchor's turn, not on anything newer: a
    // mid-history regenerate stops the transcript at the anchor, inclusive. For
    // a fresh tail turn the anchor IS the newest message, so this is a no-op.
    if let Some(anchor) = &anchor_id {
        if let Some(index) = history.iter().position(|m| m.id() == Some(anchor.as_str())) {
            history.truncate(index + 1);
        }
    }

    let mut writer = TurnWriter::new(
        Arc::clone(&stream),
        chat.sessions.clone(),
        Arc::clone(&chat.frames),
        anchor_id,
    );

    let query_text = last_user_text(&history);
    let selection = build_tool_list(
        chat.deps.tools.as_ref(),
        &chat.embedding,
        SelectionInputs {
            settings: &resolved,
            endpoint: &endpoint,
            stream_id: &stream.stream_id,
            cancel: &stream.cancel,
            query_text: query_text.as_deref(),
        },
    )
    .await;
    // Finalized by the orchestrator rather than by the selection, so the bubble
    // lands at the turn's cursor: message ordering stays in one place.
    if let Some(bubble) = &selection.filter_message {
        writer.finalize(bubble).await?;
    }
    let tools = selection.tools;

    // The tools hint is rendered client-side but belongs in the prompt only on
    // turns where the model actually got tools, which only the core knows.
    if !tools.is_empty() {
        if let Some(hint) = &frame.tools_hint {
            system_prompt = join_block(&system_prompt, hint);
        }
    }

    // Relevant memories, on the same embedding machinery as the tool filter. A
    // bubble is emitted only when the user HAS memories: with none there is
    // nothing to select and an empty bubble every turn is noise.
    if bool_setting(&resolved, "memories.enabled", true) && has_memories(&chat.memories).await {
        let mut bubble = new_memory_filter();
        match relevant_memories(
            &chat.memories,
            &chat.embedding,
            query_text.as_deref(),
            selection.query_vector.as_deref(),
            num_setting(&resolved, "memories.maxRelevant", 3.0).max(0.0) as usize,
            bool_setting(&resolved, "memories.skills.autoRelevancy", true),
        )
        .await
        {
            Ok(memories) => {
                if let Some(block) = relevant_memories_block(&memories) {
                    system_prompt = join_block(&system_prompt, &block);
                }
                bubble.insert(
                    "relevant".into(),
                    Value::Array(memories.iter().map(relevant_entry).collect()),
                );
            }
            // Non-fatal: the turn runs without memory context, and the bubble
            // says so rather than the failure being invisible.
            Err(err) => {
                tracing::warn!(
                    target: "loca::chat",
                    "stream {}: memory relevance failed: {err}",
                    stream.stream_id,
                );
                bubble.insert("status".into(), Value::String("error".into()));
                bubble.insert("errorMessage".into(), Value::String(err.to_string()));
            }
        }
        writer.finalize(&live(bubble)).await?;
    }

    // `@resource` and `/prompt` MCP references in the user's message, resolved
    // against the live servers. The claimed stems go to the memory expander so a
    // slug naming both an MCP resource and a memory expands once, MCP winning.
    let mcp = chat
        .deps
        .tools
        .resolve_mcp_tokens(query_text.as_deref().unwrap_or_default())
        .await;
    if let Some(block) = &mcp.block {
        system_prompt = join_block(&system_prompt, block);
    }

    // Tool name to owner, collapsed to one lookup for the muxer. Read once for
    // the whole turn: the catalog does not change mid-turn, and re-reading it
    // per hop would be a round trip per hop for an answer that cannot move.
    let by_name = enabled_tools_by_name(chat.deps.tools.as_ref()).await;
    let resolve = move |name: &str| -> Option<ResolvedTool> {
        by_name
            .iter()
            .find(|(tool_name, _)| tool_name == name)
            .map(|(_, extension_id)| ResolvedTool {
                extension_id: extension_id.clone(),
            })
    };

    // Reading and base64-encoding image attachments is the costly part of
    // building the provider messages, and the whole transcript is rebuilt every
    // hop. One cache for the turn, so each attachment is read and encoded once.
    let mut attachments: AttachmentCache = AttachmentCache::new();
    let sources = TranscriptSources {
        sessions: &chat.sessions,
        memories: &chat.memories,
        session_id: &stream.session_id,
    };
    let muxer = StreamMuxer::new(Arc::clone(&stream), Arc::clone(&chat.frames), route);

    for _hop in 0..max_hops {
        if stream.cancel.is_cancelled() {
            done(chat, &stream, ChatDoneReason::Interrupted);
            return Ok(());
        }
        let messages = to_chat_messages(
            &sources,
            &history,
            &system_prompt,
            &mcp.claimed,
            &mut attachments,
        )
        .await?;
        let request = LlmRequest::new(endpoint.clone(), messages).with_tools(tools.clone());
        // The queue refusal (`server_busy`) comes back from HERE rather than
        // from the stream, so it keeps its own code instead of being run through
        // the provider-error classifier.
        let deltas = match chat
            .llm
            .stream(
                request,
                TurnOptions {
                    client_id: stream.client_id.clone(),
                    parallel_slots: None,
                },
            )
            .await
        {
            Ok(deltas) => deltas,
            Err(err) => {
                error(chat, &stream, &err);
                return Ok(());
            }
        };

        let result = muxer
            .run(deltas, &mut writer, &stream.cancel, &resolve)
            .await;

        // Already finalized inside the muxer, at the first content delta or on
        // its way out, which is what keeps reasoning persisting before the
        // assistant. Here it only joins the transcript.
        if let Some(reasoning) = &result.reasoning {
            history.push(snapshot(reasoning));
        }
        // An assistant with no content, no tool calls and no truncation note has
        // nothing to say and nothing to carry, so it is dropped rather than
        // persisted as an empty bubble.
        if let Some(assistant) = &result.assistant {
            if !is_empty_assistant(assistant) {
                writer.finalize(assistant).await?;
                history.push(snapshot(assistant));
            }
        }
        if result.interrupted {
            done(chat, &stream, ChatDoneReason::Interrupted);
            return Ok(());
        }
        if let Some(err) = &result.error {
            error(chat, &stream, err);
            return Ok(());
        }
        if result.tool_calls.is_empty() {
            // A natural stop, or a context-window cutoff: the assistant bubble
            // carries the "cut off" note in the second case.
            done(
                chat,
                &stream,
                if result.truncated {
                    ChatDoneReason::Length
                } else {
                    ChatDoneReason::Stop
                },
            );
            // Fire and forget. The generator self-guards on an existing title,
            // so calling it on every stop costs two reads in the no-op case.
            chat.spawn_title(&stream.session_id, &stream.client_id);
            return Ok(());
        }

        // Sequentially, and it stays sequential: the tool messages join the
        // history in order and the permission prompts are keyed per call and
        // replayed in that order on a resubscribe.
        for pending in &result.tool_calls {
            let message = live(new_tool_message(pending));
            writer.born(&message);
            tracing::info!(
                target: "loca::chat",
                "stream {}: tool call {}/{} ({}) starting",
                stream.stream_id, pending.extension_id, pending.tool_name, pending.call_id,
            );
            let displays = chat
                .dispatcher
                .execute(&stream, pending, &message, &mut writer)
                .await;
            writer.finalize(&message).await?;
            // After the tool message, in birth order, so the durable order
            // matches the live order the client saw.
            for display in &displays {
                writer.finalize(display).await?;
            }
            history.push(snapshot(&message));
        }
    }

    done(chat, &stream, ChatDoneReason::HopLimit);
    Ok(())
}

/// The client may pin a route; otherwise the complexity classifier decides, but
/// only when dual-model routing is on.
///
/// An absent `route` is the ONLY path that reaches the classifier, and until
/// D-066 no frame could take it: the schema's `.default("default")` filled the
/// field in during parse, so the gate below was false for every frame ever sent.
async fn resolve_route(
    chat: &ChatInner,
    stream: &ActiveStream,
    resolved: &crate::settings::SettingsRecord,
    sparse: &crate::settings::SettingsRecord,
    frame: &ChatStartWs,
) -> LlmRoute {
    if let Some(route) = &frame.route {
        return match route {
            crate::contract::inbound::ChatStartWsRoute::Secondary => LlmRoute::Secondary,
            crate::contract::inbound::ChatStartWsRoute::Default => LlmRoute::Default,
        };
    }
    if !bool_setting(resolved, "dualModel.enabled", false) {
        return LlmRoute::Default;
    }
    let history = chat.sessions.list_messages(&stream.session_id).await;
    let Some(last) = last_user_text(&history).filter(|t| !t.is_empty()) else {
        return LlmRoute::Default;
    };
    // The classifier reads its prompt and budget from the resolved record and
    // its endpoint from the sparse one, for the same reason the turn does.
    classify_complexity(&chat.llm, resolved, sparse, &last, &stream.cancel).await
}

fn send(chat: &ChatInner, stream: &ActiveStream, frame: ServerToClientFrame) {
    chat.frames.broadcast_to_client(&stream.client_id, &frame);
}

fn done(chat: &ChatInner, stream: &ActiveStream, reason: ChatDoneReason) {
    send(
        chat,
        stream,
        ServerToClientFrame::ChatDone {
            reason,
            stream_id: stream.stream_id.clone(),
        },
    );
}

fn error(chat: &ChatInner, stream: &ActiveStream, err: &AppError) {
    send(
        chat,
        stream,
        ServerToClientFrame::ChatError {
            code: wire_code(err.code),
            message: err.message.clone(),
            stream_id: stream.stream_id.clone(),
        },
    );
}

/// An assistant that streamed nothing, called nothing and was not cut off.
fn is_empty_assistant(message: &LiveRef) -> bool {
    let guard = message.lock().unwrap_or_else(|e| e.into_inner());
    let map = guard.as_map();
    let has_content = map
        .get("content")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.is_empty());
    let has_calls = map
        .get("toolCalls")
        .and_then(Value::as_array)
        .is_some_and(|calls| !calls.is_empty());
    let truncated = map.get("truncated").and_then(Value::as_bool) == Some(true);
    !has_content && !has_calls && !truncated
}

fn snapshot(message: &LiveRef) -> StoredMessage {
    message.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

fn live(map: Map<String, Value>) -> LiveRef {
    Arc::new(std::sync::Mutex::new(StoredMessage::new(map)))
}

fn new_memory_filter() -> Map<String, Value> {
    let mut map = Map::new();
    map.insert("id".into(), Value::String(ulid::Ulid::new().to_string()));
    map.insert("ord".into(), Value::from(-1));
    map.insert("role".into(), Value::String("memory_filter".into()));
    map.insert("status".into(), Value::String("complete".into()));
    map.insert("createdAtMs".into(), Value::from(now_ms()));
    map
}

fn relevant_entry(memory: &crate::memories::injection::RelevantMemory) -> Value {
    let mut map = Map::new();
    map.insert("memoryId".into(), Value::String(memory.memory_id.clone()));
    map.insert(
        "kind".into(),
        serde_json::to_value(memory.kind).unwrap_or(Value::Null),
    );
    map.insert("title".into(), Value::String(memory.title.clone()));
    map.insert("summary".into(), Value::String(memory.summary.clone()));
    map.insert("score".into(), Value::from(memory.score));
    Value::Object(map)
}

fn new_tool_message(pending: &super::muxer::PendingCall) -> Map<String, Value> {
    let mut map = Map::new();
    map.insert("id".into(), Value::String(ulid::Ulid::new().to_string()));
    map.insert("ord".into(), Value::from(-1));
    map.insert("role".into(), Value::String("tool".into()));
    map.insert("callId".into(), Value::String(pending.call_id.clone()));
    map.insert(
        "extensionId".into(),
        Value::String(pending.extension_id.clone()),
    );
    map.insert("toolName".into(), Value::String(pending.tool_name.clone()));
    map.insert("arguments".into(), Value::String(pending.arguments.clone()));
    map.insert("status".into(), Value::String("running".into()));
    map.insert("createdAtMs".into(), Value::from(now_ms()));
    map
}

/// Append a block to the system prompt, or become it when there is none.
fn join_block(prompt: &str, block: &str) -> String {
    if prompt.is_empty() {
        block.to_string()
    } else {
        format!("{prompt}\n\n{block}")
    }
}

/// The core's error codes and the wire contract's are two enums with the same
/// 47 names, generated from the same source. An exhaustive match rather than a
/// serde round trip, so a variant added to either side is a compile error here
/// rather than a frame that silently degrades to `internal_error`.
macro_rules! map_codes {
    ($($variant:ident),* $(,)?) => {
        fn wire_code(code: ErrorCode) -> crate::contract::api::ErrorCode {
            match code {
                $(ErrorCode::$variant => crate::contract::api::ErrorCode::$variant,)*
            }
        }
    };
}

map_codes!(
    InvalidToken,
    Revoked,
    MissingToken,
    AdminTokenRequired,
    AdminPasswordRequired,
    AdminPasswordInvalid,
    AdminPasswordRateLimited,
    InvalidPairingCode,
    PairingCodeExpired,
    PairingCodeClaimed,
    PairingRateLimited,
    NotFound,
    Forbidden,
    ValidationError,
    Conflict,
    SessionBusy,
    SessionNotFound,
    MessageNotFound,
    StreamNotFound,
    ContextWindowExceeded,
    ServerBusy,
    ServerUnavailable,
    ProviderError,
    ProviderUnauthorized,
    ProviderRateLimited,
    NoLocaJson,
    InvalidLocaJson,
    DepsInstallFailed,
    TarballFetchFailed,
    ExtractFailed,
    ExtensionNotFound,
    ToolNotFound,
    PermissionsRequired,
    PermissionsRevoked,
    ExtensionHashDrift,
    ExtensionAlreadyInstalled,
    ToolNameCollision,
    ModelNotFound,
    BinaryNotFound,
    ChecksumMismatch,
    SignatureInvalid,
    ManifestFetchFailed,
    UpdateFailed,
    ManualRollbackRequired,
    UpdateInProgress,
    InsufficientStorage,
    InternalError,
);

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}
