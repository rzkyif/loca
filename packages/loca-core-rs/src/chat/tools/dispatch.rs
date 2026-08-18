//! One tool call, run to completion, with its outcome folded into the tool
//! message the orchestrator already announced.
//!
//! Ports `core-engine/src/services/chat-tool-dispatch.ts` in full.
//!
//! # Two paths, and the asymmetry between them is deliberate
//!
//! An MCP tool runs on its server: a straight request and response, with no
//! local permissions, no progress, and no askUser flow. An extension tool runs
//! in the worker sandbox with all of that forwarded to the client as prompts.
//!
//! Only the extension path emits `tool.result` / `tool.error` frames. The MCP
//! path emits none: its outcome reaches the client through the tool message's
//! own terminal snapshot. That reads like an oversight and is not, so it is
//! ported as-is and tested in both directions; unifying them would change what
//! the client renders for an MCP failure.
//!
//! # Nothing here ends the turn
//!
//! Every failure below produces a `status: "failed"` tool message that is pushed
//! into the history and shown to the model on the next hop. A tool that does not
//! exist, an extension tampered with since boot, arguments the schema rejects, a
//! worker that dies: all of them are answers, not turn-enders.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::{Map, Value};
use tokio::sync::mpsc;

use crate::contract::api::{ServerToClientFrame, ServerToClientFrameLevel};
use crate::sessions::doc::StoredMessage;
use crate::sessions::{history, SessionsStore};
use crate::transport::FrameBus;

use crate::chat::deps::{ToolCallController, ToolCallEvent, ToolCallSpec, ToolHost, ToolLogLevel};
use crate::chat::muxer::PendingCall;
use crate::chat::stream::ActiveStream;
use crate::chat::writer::{LiveRef, TurnWriter};

/// One running extension tool call, addressable by call id.
///
/// The map is process-wide (one per [`Chat`](crate::chat)), which is what makes
/// the stream-namespaced call id load-bearing: two concurrent turns whose models
/// both emit `call_1` would otherwise collide here, and a collision makes every
/// forwarded askUser answer fail its owner check and hang both prompts.
pub struct InFlightEntry {
    pub client_id: String,
    pub controller: Arc<dyn ToolCallController>,
    pub stream: Arc<ActiveStream>,
}

#[derive(Default)]
pub struct InFlight {
    entries: Mutex<HashMap<String, InFlightEntry>>,
}

impl InFlight {
    pub fn new() -> Self {
        Self::default()
    }

    fn insert(&self, call_id: String, entry: InFlightEntry) {
        lock(&self.entries).insert(call_id, entry);
    }

    fn remove(&self, call_id: &str) {
        lock(&self.entries).remove(call_id);
    }

    /// The call `call_id` names, if this client owns the turn that started it.
    ///
    /// The ownership check lives here rather than at each of the four `forward*`
    /// call sites, so a new control frame cannot forget it.
    pub fn owned(&self, call_id: &str, client_id: &str) -> Option<Arc<dyn ToolCallController>> {
        let entries = lock(&self.entries);
        let entry = entries.get(call_id)?;
        (entry.client_id == client_id).then(|| Arc::clone(&entry.controller))
    }

    /// The owning turn, for a control frame that has to clear a prompt as well
    /// as answer it.
    pub fn owned_stream(&self, call_id: &str, client_id: &str) -> Option<Arc<ActiveStream>> {
        let entries = lock(&self.entries);
        let entry = entries.get(call_id)?;
        (entry.client_id == client_id).then(|| Arc::clone(&entry.stream))
    }
}

pub struct Dispatcher {
    tools: Arc<dyn ToolHost>,
    frames: Arc<FrameBus>,
    sessions: SessionsStore,
    in_flight: Arc<InFlight>,
}

impl Dispatcher {
    pub fn new(
        tools: Arc<dyn ToolHost>,
        frames: Arc<FrameBus>,
        sessions: SessionsStore,
        in_flight: Arc<InFlight>,
    ) -> Self {
        Self {
            tools,
            frames,
            sessions,
            in_flight,
        }
    }

    /// Run one call and fold its outcome into `message`.
    ///
    /// Returns the display bubbles the tool pushed, BORN but deliberately NOT
    /// finalized: the caller persists them after the tool message, so the
    /// durable order (tool, then its displays) equals the live order. Finalizing
    /// them here would persist them ahead of the still-running tool message.
    pub async fn execute(
        &self,
        stream: &Arc<ActiveStream>,
        pending: &PendingCall,
        message: &LiveRef,
        writer: &mut TurnWriter,
    ) -> Vec<LiveRef> {
        let mut displays: Vec<LiveRef> = Vec::new();
        if pending.unknown {
            fail(message, format!("tool {} not available", pending.tool_name));
            return displays;
        }
        if self.tools.is_mcp_server(&pending.extension_id).await {
            self.execute_mcp(stream, pending, message).await;
            return displays;
        }
        let tool_id = format!("{}::{}", pending.extension_id, pending.tool_name);
        let Some(tool) = self.tools.get_tool(&tool_id).await else {
            fail(message, "tool not found");
            return displays;
        };

        // Pre-flight, on EVERY call so a reused warm worker is re-checked:
        // re-verify the extension's content hash, then validate the model's
        // arguments against the tool's declared schema and fill defaults, so the
        // worker receives normalised arguments rather than raw model output.
        let normalized = match self.tools.verify_tool_fresh(&pending.extension_id).await {
            Ok(()) => self.tools.validate_tool_args(&tool, &pending.arguments),
            Err(err) => Err(err),
        };
        let normalized = match normalized {
            Ok(args) => args,
            Err(err) => {
                let text = err.to_string();
                self.send(
                    stream,
                    ServerToClientFrame::ToolError {
                        call_id: pending.call_id.clone(),
                        code: None,
                        error: text.clone(),
                    },
                );
                fail(message, text);
                return displays;
            }
        };

        let user_message =
            history::last_user_text(&self.sessions.list_messages(&stream.session_id).await)
                .unwrap_or_default();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let controller = match self
            .tools
            .start_tool_call(
                ToolCallSpec {
                    extension_id: pending.extension_id.clone(),
                    tool,
                    arguments_json: normalized,
                    user_message,
                    session_id: Some(stream.session_id.clone()),
                    locale: None,
                },
                tx,
            )
            .await
        {
            Ok(controller) => Arc::<dyn ToolCallController>::from(controller),
            Err(err) => {
                let text = err.to_string();
                self.send(
                    stream,
                    ServerToClientFrame::ToolError {
                        call_id: pending.call_id.clone(),
                        code: None,
                        error: text.clone(),
                    },
                );
                fail(message, text);
                return displays;
            }
        };
        self.in_flight.insert(
            pending.call_id.clone(),
            InFlightEntry {
                client_id: stream.client_id.clone(),
                controller: Arc::clone(&controller),
                stream: Arc::clone(stream),
            },
        );

        // An interrupt that arrived before the call started still cancels it:
        // the turn is already over, and the worker would otherwise run to
        // completion with nobody waiting.
        let mut cancelled = stream.cancel.is_cancelled();
        if cancelled {
            controller.cancel();
        }

        let outcome = {
            let done = controller.done();
            tokio::pin!(done);
            // The event channel closing is not the call ending: the terminal
            // state is the controller's business. The arm is DISABLED rather
            // than awaited-forever, because a `pending().await` in the handler
            // never returns to the select and the controller is then never
            // polled again. A host that drops its sender the moment it has
            // emitted everything (which is the normal shape) would deadlock
            // every call.
            let mut events_closed = false;
            loop {
                tokio::select! {
                    biased;
                    // A chat interrupt cancels the tool and then KEEPS WAITING
                    // for its terminal event, so the worker's own cancel path
                    // runs and the tool message gets a real outcome. Abandoning
                    // it here would leave the message `pending` forever.
                    _ = stream.cancel.cancelled(), if !cancelled => {
                        cancelled = true;
                        controller.cancel();
                    }
                    event = rx.recv(), if !events_closed => match event {
                        Some(event) => {
                            self.on_event(stream, pending, message, writer, &mut displays, event)
                                .await;
                        }
                        None => events_closed = true,
                    },
                    outcome = &mut done => break outcome,
                }
            }
        };
        // Events queued before the terminal state still belong to this call. The
        // TypeScript's handler is synchronous, so it had already seen them.
        while let Ok(event) = rx.try_recv() {
            self.on_event(stream, pending, message, writer, &mut displays, event)
                .await;
        }

        match outcome {
            Ok(result) => {
                self.send(
                    stream,
                    ServerToClientFrame::ToolResult {
                        call_id: pending.call_id.clone(),
                        result: result.clone(),
                    },
                );
                let mut guard = lock_msg(message);
                let map = guard.as_map_mut();
                map.insert("status".into(), Value::String("completed".into()));
                map.insert("result".into(), result);
            }
            Err(err) => {
                let text = err.to_string();
                self.send(
                    stream,
                    ServerToClientFrame::ToolError {
                        call_id: pending.call_id.clone(),
                        code: None,
                        error: text.clone(),
                    },
                );
                fail(message, text);
            }
        }
        stream.clear_prompt(&pending.call_id);
        self.in_flight.remove(&pending.call_id);
        displays
    }

    /// An MCP call: parse the model's arguments, call the server, fold the
    /// returned content in. No frames either way.
    async fn execute_mcp(
        &self,
        stream: &Arc<ActiveStream>,
        pending: &PendingCall,
        message: &LiveRef,
    ) {
        // A parse failure leaves the arguments empty rather than failing the
        // call: the server validates against its own schema and its error is
        // more useful than ours.
        let args: Map<String, Value> = serde_json::from_str(&pending.arguments).unwrap_or_default();
        let call = self
            .tools
            .call_mcp_tool(&pending.extension_id, &pending.tool_name, args);
        let result = tokio::select! {
            biased;
            _ = stream.cancel.cancelled() => {
                fail(message, "interrupted");
                return;
            }
            result = call => result,
        };
        match result {
            Ok(value) => {
                let text = mcp_text(&value);
                if value.get("isError").and_then(Value::as_bool) == Some(true) {
                    fail(
                        message,
                        if text.is_empty() {
                            "MCP tool returned an error".to_string()
                        } else {
                            text
                        },
                    );
                } else {
                    let mut guard = lock_msg(message);
                    let map = guard.as_map_mut();
                    map.insert("status".into(), Value::String("completed".into()));
                    // The joined text when there is any, and the raw result
                    // otherwise: a tool answering with structured content only
                    // must not come back as an empty string.
                    map.insert(
                        "result".into(),
                        if text.is_empty() {
                            value
                        } else {
                            Value::String(text)
                        },
                    );
                }
            }
            Err(err) => fail(message, err.to_string()),
        }
    }

    async fn on_event(
        &self,
        stream: &Arc<ActiveStream>,
        pending: &PendingCall,
        message: &LiveRef,
        writer: &mut TurnWriter,
        displays: &mut Vec<LiveRef>,
        event: ToolCallEvent,
    ) {
        let call_id = pending.call_id.clone();
        match event {
            ToolCallEvent::Progress {
                progress,
                label,
                description,
            } => {
                // Persisted on the message so a reloaded bubble keeps the tool's
                // own wording rather than falling back to the tool name.
                {
                    let mut guard = lock_msg(message);
                    let map = guard.as_map_mut();
                    if let Some(label) = &label {
                        map.insert("label".into(), Value::String(label.clone()));
                    }
                    if let Some(description) = &description {
                        map.insert("description".into(), Value::String(description.clone()));
                    }
                    map.insert("progress".into(), Value::from(progress));
                }
                self.send(
                    stream,
                    ServerToClientFrame::ToolProgress {
                        call_id,
                        description,
                        label,
                        progress,
                    },
                );
            }
            ToolCallEvent::AskUserRequest {
                request_id,
                questions,
            } => self.emit_prompt(
                stream,
                &pending.call_id,
                ServerToClientFrame::ToolAskuserRequest {
                    call_id,
                    questions,
                    request_id,
                },
            ),
            ToolCallEvent::ScheduleRequest { request_id, draft } => self.emit_prompt(
                stream,
                &pending.call_id,
                ServerToClientFrame::ScheduleConfirmRequest {
                    call_id,
                    draft,
                    request_id,
                },
            ),
            ToolCallEvent::PermissionRequest {
                request_id,
                permission,
                resource,
                api_name,
                declared,
                reason,
            } => self.emit_prompt(
                stream,
                &pending.call_id,
                ServerToClientFrame::ToolPermissionRequest {
                    api_name,
                    call_id,
                    declared,
                    extension_id: pending.extension_id.clone(),
                    permission_kind: permission,
                    reason,
                    request_id,
                    resource,
                    tool_name: pending.tool_name.clone(),
                },
            ),
            ToolCallEvent::Log {
                level,
                message: line,
            } => self.send(
                stream,
                ServerToClientFrame::ToolLog {
                    call_id,
                    level: wire_level(level),
                    message: line,
                },
            ),
            ToolCallEvent::Display { content } => {
                // A one-way push: born now so the client sees it in order, after
                // the running tool message. See `execute`'s doc for why it is
                // not finalized here.
                let display = new_display(&pending.call_id, content);
                writer.born(&display);
                displays.push(display);
            }
            ToolCallEvent::Cancelled => {
                self.send(stream, ServerToClientFrame::ToolCancelled { call_id })
            }
        }
    }

    /// Retain a prompt for catch-up, then send it. Retain first: a client that
    /// reconnects between the two must still find it.
    fn emit_prompt(&self, stream: &Arc<ActiveStream>, call_id: &str, frame: ServerToClientFrame) {
        stream.retain_prompt(call_id.to_string(), frame.clone());
        self.send(stream, frame);
    }

    fn send(&self, stream: &Arc<ActiveStream>, frame: ServerToClientFrame) {
        self.frames.broadcast_to_client(&stream.client_id, &frame);
    }
}

/// The `content[].text` join every MCP result carries. Non-text content is
/// dropped, which is what the caller falls back to the raw value for.
fn mcp_text(value: &Value) -> String {
    let Some(content) = value.get("content").and_then(Value::as_array) else {
        return String::new();
    };
    content
        .iter()
        .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect::<Vec<&str>>()
        .join("\n")
}

fn fail(message: &LiveRef, error: impl Into<String>) {
    let mut guard = lock_msg(message);
    let map = guard.as_map_mut();
    map.insert("status".into(), Value::String("failed".into()));
    map.insert("error".into(), Value::String(error.into()));
}

fn new_display(call_id: &str, content: crate::contract::api::DisplayContent) -> LiveRef {
    let mut map = Map::new();
    map.insert("id".into(), Value::String(ulid::Ulid::new().to_string()));
    map.insert("ord".into(), Value::from(-1));
    map.insert("role".into(), Value::String("display".into()));
    map.insert("callId".into(), Value::String(call_id.to_string()));
    map.insert(
        "content".into(),
        serde_json::to_value(content).unwrap_or(Value::Null),
    );
    map.insert("createdAtMs".into(), Value::from(now_ms()));
    Arc::new(Mutex::new(StoredMessage::new(map)))
}

fn wire_level(level: ToolLogLevel) -> ServerToClientFrameLevel {
    match level {
        ToolLogLevel::Debug => ServerToClientFrameLevel::Debug,
        ToolLogLevel::Info => ServerToClientFrameLevel::Info,
        ToolLogLevel::Warn => ServerToClientFrameLevel::Warn,
        ToolLogLevel::Error => ServerToClientFrameLevel::Error,
    }
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

fn lock_msg(message: &LiveRef) -> std::sync::MutexGuard<'_, StoredMessage> {
    message.lock().unwrap_or_else(|e| e.into_inner())
}
