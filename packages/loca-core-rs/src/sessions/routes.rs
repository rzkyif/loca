//! The sessions route slice: everything under `/api/v1/sessions`.
//!
//! Inbound bodies deserialize through `contract`, whose generated `Deserialize`
//! already enforces the Zod constraints. There is no second check after one.
//!
//! These routes broadcast nothing. The `session.updated` and `session.created`
//! frames are emitted by the chat orchestrator, title generation and the
//! automated-session path, never by a REST edit, and mirroring REST edits here
//! would be a product change rather than a port.

use std::collections::HashSet;
use std::convert::Infallible;
use std::sync::{Arc, Mutex};

use axum::extract::{Multipart, Path, State};
use axum::http::{header, HeaderValue};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
use axum::Router;
use futures_util::stream::{self, BoxStream, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::contract::api::{
    AppendMessageResponse, CreateSessionRequest, PatchMessageResponse, PatchSessionRequest,
    PatchSessionResponse, Session, SessionListEntry, TokenUsage, UploadAttachmentResponse,
};
use crate::contract::inbound::MessageInput;
use crate::error::{internal, validation, AppError, ErrorCode, Result};
use crate::files;
use crate::log::scrub_secrets;
use crate::transport::http::{no_content, Json};
use crate::transport::middleware::{from_fn_with_state, require_client, AuthedClient};
use crate::transport::state::AppContext;

use super::attachments::sanitize_filename;
use super::chat_engine::{ChatEngine, OneShotRequest};
use super::doc::StoredMessage;
use super::store::{self, Appended, CreateSessionInput, NewAttachment, SessionsStore};

#[derive(Clone)]
struct SessionsState {
    ctx: AppContext,
    store: SessionsStore,
    chat: Arc<dyn ChatEngine>,
    /// Sessions with an SSE chat stream in flight. Without it, two overlapping
    /// POSTs to one session would each append a user message and interleave two
    /// completions, corrupting the persisted order.
    sse_active: Arc<Mutex<HashSet<String>>>,
}

/// `store` and `chat` are threaded in rather than built here. The store holds
/// the temporary-session map and the per-session lock table, so a second
/// instance would hide a temporary session from every other route; `boot` owns
/// the single one and hands the same handle to every package that needs it.
pub fn routes(ctx: &AppContext, store: SessionsStore, chat: Arc<dyn ChatEngine>) -> Router {
    let state = SessionsState {
        ctx: ctx.clone(),
        store,
        chat,
        sse_active: Arc::new(Mutex::new(HashSet::new())),
    };
    Router::new()
        .route("/api/v1/sessions", get(list).post(create))
        .route(
            "/api/v1/sessions/{id}",
            get(get_one).patch(patch_one).delete(delete_one),
        )
        .route(
            "/api/v1/sessions/{id}/regenerate-title",
            post(regenerate_title),
        )
        .route("/api/v1/sessions/{id}/messages", post(append_message))
        .route(
            "/api/v1/sessions/{id}/messages/{msg_id}",
            patch(patch_message).delete(delete_message),
        )
        .route("/api/v1/sessions/{id}/attachments", post(upload_attachment))
        .route(
            "/api/v1/sessions/{id}/attachments/{att_id}",
            get(download_attachment),
        )
        .route("/api/v1/sessions/{id}/chat", post(chat_sse))
        .layer(from_fn_with_state(ctx.clone(), require_client))
        .with_state(state)
}

// --- sessions ----------------------------------------------------------------

/// A bare array, with no envelope around it.
async fn list(
    State(state): State<SessionsState>,
    client: AuthedClient,
) -> Result<Json<Vec<SessionListEntry>>> {
    Ok(Json(state.store.list(&client.id).await?))
}

async fn create(
    State(state): State<SessionsState>,
    client: AuthedClient,
    Json(body): Json<CreateSessionRequest>,
) -> Result<Json<Session>> {
    let session = state
        .store
        .create(CreateSessionInput {
            owner_client_id: client.id.clone(),
            title: body.title,
            temporary: body.temporary == Some(true),
        })
        .await?;
    // Creating any session means the client moved on from a prior temporary
    // one; reclaim the strays it did not, or could not, delete itself.
    state
        .store
        .sweep_client_temporary(&client.id, Some(&session.id))
        .await;
    Ok(Json(session))
}

/// The session plus its transcript.
///
/// `messages` are the stored maps rather than the generated `Message` enum, so a
/// message this build's contract cannot represent is still served instead of
/// failing the whole read. See `doc::StoredMessage`.
#[derive(Serialize)]
struct GetSessionBody {
    #[serde(flatten)]
    session: Session,
    messages: Vec<StoredMessage>,
}

async fn get_one(
    State(state): State<SessionsState>,
    client: AuthedClient,
    Path(id): Path<String>,
) -> Result<Json<GetSessionBody>> {
    let session = state.store.get_or_throw(&client.id, &id).await?;
    // Navigating to a session orphans any other temporary one this client owns,
    // since they are never listed and there is no way back to one.
    state
        .store
        .sweep_client_temporary(&client.id, Some(&session.id))
        .await;
    let messages = state.store.list_messages(&session.id).await;
    Ok(Json(GetSessionBody { session, messages }))
}

async fn patch_one(
    State(state): State<SessionsState>,
    client: AuthedClient,
    Path(id): Path<String>,
    Json(body): Json<PatchSessionRequest>,
) -> Result<Json<PatchSessionResponse>> {
    // Optional in the contract type, required by this route: a PATCH carrying
    // nothing to change is a malformed request, not a no-op.
    let title = body.title.ok_or_else(|| validation("title is required"))?;
    state.store.patch_title(&client.id, &id, &title).await?;
    Ok(Json(PatchSessionResponse { id, title }))
}

/// Generation streams in the background; the client learns the start, the new
/// title and the end from `session.updated` frames, so this only kicks it off.
async fn regenerate_title(
    State(state): State<SessionsState>,
    client: AuthedClient,
    Path(id): Path<String>,
) -> Result<Response> {
    let session = state.store.get_or_throw(&client.id, &id).await?;
    state.chat.regenerate_title(session.id, client.id.clone());
    Ok(no_content())
}

async fn delete_one(
    State(state): State<SessionsState>,
    client: AuthedClient,
    Path(id): Path<String>,
) -> Result<Response> {
    let attachments = state.store.delete(&client.id, &id).await?;
    for path in attachments {
        // Removing the session directory already took the attachments stored
        // inside it, so a missing file is the normal case. A real failure
        // (permissions, a directory in the way) is worth a warning but never
        // fails the delete.
        match store::remove_file(path.clone()).await {
            Ok(()) => {}
            Err(err) if err.code == ErrorCode::NotFound => {}
            Err(err) => tracing::warn!(
                target: "loca::http.sessions",
                "session delete: failed to remove attachment {}: {}", path.display(), err.message,
            ),
        }
    }
    Ok(no_content())
}

// --- messages ----------------------------------------------------------------

async fn append_message(
    State(state): State<SessionsState>,
    client: AuthedClient,
    Path(id): Path<String>,
    Json(body): Json<MessageInput>,
) -> Result<Json<AppendMessageResponse>> {
    let session = state.store.get_or_throw(&client.id, &id).await?;
    let appended = state
        .store
        .append_message(&session.id, to_stored(&body)?)
        .await?;
    Ok(Json(AppendMessageResponse {
        id: appended.id,
        ord: appended.ord,
    }))
}

/// The patch body is an object of the fields to change. Its validity is decided
/// by merging it into the stored message and parsing the result through the
/// generated per-role contract type, which is what rejects an assistant-only
/// field on a user message. See `SessionsStore::patch_message`.
async fn patch_message(
    State(state): State<SessionsState>,
    client: AuthedClient,
    Path((id, msg_id)): Path<(String, String)>,
    Json(patch): Json<Map<String, Value>>,
) -> Result<Json<PatchMessageResponse>> {
    let session = state.store.get_or_throw(&client.id, &id).await?;
    let Appended { id, ord } = state
        .store
        .patch_message(&session.id, &msg_id, &patch)
        .await?;
    Ok(Json(PatchMessageResponse { id, ord }))
}

async fn delete_message(
    State(state): State<SessionsState>,
    client: AuthedClient,
    Path((id, msg_id)): Path<(String, String)>,
) -> Result<Response> {
    let session = state.store.get_or_throw(&client.id, &id).await?;
    state.store.delete_message(&session.id, &msg_id).await?;
    Ok(no_content())
}

/// A validated `MessageInput` as the map the store persists.
///
/// Round-tripping through the generated type is what applies the Zod
/// constraints; nothing is re-checked afterwards. Every per-role schema is
/// strict, so there is no passthrough field to lose in the conversion.
fn to_stored(input: &MessageInput) -> Result<StoredMessage> {
    match serde_json::to_value(input) {
        Ok(Value::Object(map)) => Ok(StoredMessage::new(map)),
        _ => Err(internal(
            "a validated message did not serialize to an object",
        )),
    }
}

// --- attachments -------------------------------------------------------------

/// Bounded by the router-wide request body limit, and by nothing else. There is
/// no attachment-specific cap in the core, and adding one would be a new
/// behaviour rather than a port.
async fn upload_attachment(
    State(state): State<SessionsState>,
    client: AuthedClient,
    Path(id): Path<String>,
    mut form: Multipart,
) -> Result<Json<UploadAttachmentResponse>> {
    let session = state.store.get_or_throw(&client.id, &id).await?;

    let mut file: Option<(String, Option<String>, Vec<u8>)> = None;
    let mut message_id: Option<String> = None;
    // Fields are read in arrival order, so both are collected in one pass rather
    // than assuming the client sends them in a particular order.
    while let Some(field) = form
        .next_field()
        .await
        .map_err(|err| validation(format!("malformed multipart body: {err}")))?
    {
        match field.name() {
            Some("file") => {
                let name = field.file_name().unwrap_or_default().to_string();
                let mime = field.content_type().map(str::to_string);
                let bytes = field.bytes().await.map_err(|err| {
                    validation(format!("could not read the uploaded file: {err}"))
                })?;
                file = Some((name, mime, bytes.to_vec()));
            }
            Some("messageId") => {
                message_id = field.text().await.ok();
            }
            _ => {}
        }
    }

    let (Some((name, mime, bytes)), Some(message_id)) = (file, message_id) else {
        return Err(validation("file + messageId required"));
    };

    let dir = state.ctx.paths.session_attachments_dir(&session.id);
    store::create_dir_all(dir.clone()).await?;
    let attachment_id = super::store::new_id();
    let filename = sanitize_filename(&name);
    let abs_path = dir.join(format!("{attachment_id}_{filename}"));
    let size_bytes = bytes.len() as u64;
    files::write_atomic(abs_path.clone(), bytes).await?;

    let record = state
        .store
        .record_attachment(
            &session.id,
            NewAttachment {
                message_id,
                filename,
                mime: mime.filter(|m| !m.is_empty()),
                size_bytes,
                abs_path: abs_path.to_string_lossy().into_owned(),
            },
        )
        .await?;
    Ok(Json(UploadAttachmentResponse {
        id: record.id,
        abs_path: record.abs_path,
        filename: record.filename,
    }))
}

async fn download_attachment(
    State(state): State<SessionsState>,
    client: AuthedClient,
    Path((id, att_id)): Path<(String, String)>,
) -> Result<Response> {
    let session = state.store.get_or_throw(&client.id, &id).await?;
    let record = state.store.get_attachment(&session.id, &att_id).await?;
    let bytes = store::read_file(record.abs_path.clone().into()).await?;
    // The mime came from the upload's own `Content-Type`, so it is client
    // supplied and cannot go into a response header unchecked. An illegal value
    // falls back rather than failing the download.
    let content_type = record
        .mime
        .as_deref()
        .and_then(|m| HeaderValue::from_str(m).ok())
        .unwrap_or_else(|| HeaderValue::from_static("application/octet-stream"));
    Ok(([(header::CONTENT_TYPE, content_type)], bytes).into_response())
}

// --- the SSE chat fallback ---------------------------------------------------

#[derive(Deserialize)]
struct ChatRequest {
    content: String,
}

/// The chat fallback for a client with no WebSocket. One-shot completion: no
/// tools, no reasoning trace, no multiplexing.
///
/// The order below is the invariant. The stream slot is claimed before anything
/// is persisted, so two overlapping requests cannot interleave their turns, and
/// it is released by a guard rather than by a completion callback, so a client
/// that disconnects mid-stream releases it too.
async fn chat_sse(
    State(state): State<SessionsState>,
    client: AuthedClient,
    Path(id): Path<String>,
    Json(body): Json<ChatRequest>,
) -> Result<Response> {
    let session = state.store.get_or_throw(&client.id, &id).await?;
    if body.content.is_empty() {
        return Err(validation("body must be { content: string }"));
    }

    let guard = SseSlot::claim(Arc::clone(&state.sse_active), session.id.clone())?;

    let mut user = Map::new();
    user.insert("role".into(), Value::String("user".into()));
    user.insert("content".into(), Value::String(body.content));
    state
        .store
        .append_message(&session.id, StoredMessage::new(user))
        .await?;

    let history = state.store.list_messages(&session.id).await;
    let deltas = state.chat.one_shot(OneShotRequest {
        owner_client_id: client.id.clone(),
        session_id: session.id.clone(),
        history,
    });

    let turn = SseTurn {
        deltas,
        store: state.store.clone(),
        session_id: session.id,
        text: String::new(),
        usage: None,
        finished: false,
        _guard: guard,
    };
    let body = stream::unfold(turn, |mut turn| async move {
        turn.next()
            .await
            .map(|event| (Ok::<_, Infallible>(event), turn))
    });
    Ok(Sse::new(body).into_response())
}

/// The session's SSE slot, released on drop.
///
/// Drop rather than a completion callback: the stream is dropped when the client
/// disconnects, when it errors and when it ends, so all three paths release the
/// slot without any of them having to remember to.
struct SseSlot {
    active: Arc<Mutex<HashSet<String>>>,
    session_id: String,
}

impl SseSlot {
    fn claim(active: Arc<Mutex<HashSet<String>>>, session_id: String) -> Result<Self> {
        {
            let mut set = active.lock().unwrap_or_else(|p| p.into_inner());
            if !set.insert(session_id.clone()) {
                return Err(AppError::new(
                    ErrorCode::Conflict,
                    "this session already has an in-flight chat stream",
                ));
            }
        }
        Ok(Self { active, session_id })
    }
}

impl Drop for SseSlot {
    fn drop(&mut self) {
        self.active
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&self.session_id);
    }
}

/// One turn in flight, driven by `stream::unfold`.
struct SseTurn {
    deltas: BoxStream<'static, Result<crate::sessions::chat_engine::OneShotDelta>>,
    store: SessionsStore,
    session_id: String,
    text: String,
    usage: Option<TokenUsage>,
    finished: bool,
    _guard: SseSlot,
}

#[derive(Serialize)]
struct ContentDeltaPayload<'a> {
    #[serde(rename = "contentDelta")]
    content_delta: &'a str,
}

#[derive(Serialize)]
struct DonePayload {
    done: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    usage: Option<TokenUsage>,
}

#[derive(Serialize)]
struct ErrorPayload {
    error: String,
}

impl SseTurn {
    /// The next line to send, or `None` to end the stream.
    async fn next(&mut self) -> Option<Event> {
        if self.finished {
            return None;
        }
        loop {
            match self.deltas.next().await {
                Some(Ok(delta)) => {
                    if let Some(usage) = delta.usage {
                        self.usage = Some(usage);
                    }
                    match delta.content_delta {
                        Some(text) if !text.is_empty() => {
                            self.text.push_str(&text);
                            return Some(data(&ContentDeltaPayload {
                                content_delta: &text,
                            }));
                        }
                        // A chunk carrying only usage produces no output line.
                        _ => continue,
                    }
                }
                Some(Err(err)) => {
                    self.finished = true;
                    // Scrubbed on its way to the wire: an upstream provider
                    // error can echo a URL with a token in it.
                    return Some(data(&ErrorPayload {
                        error: scrub_secrets(&err.message),
                    }));
                }
                None => {
                    self.finished = true;
                    self.persist_assistant().await;
                    return Some(data(&DonePayload {
                        done: true,
                        usage: self.usage.clone(),
                    }));
                }
            }
        }
    }

    async fn persist_assistant(&mut self) {
        if self.text.is_empty() {
            return;
        }
        let mut message = Map::new();
        message.insert("role".into(), Value::String("assistant".into()));
        message.insert(
            "content".into(),
            Value::String(std::mem::take(&mut self.text)),
        );
        if let Err(err) = self
            .store
            .append_message(&self.session_id, StoredMessage::new(message))
            .await
        {
            tracing::warn!(
                target: "loca::http.sessions",
                "could not persist the assistant turn for {}: {}", self.session_id, err.message,
            );
        }
    }
}

/// One `data:` line. A serialization failure cannot happen for these three
/// shapes, and reporting it as the turn's error is more useful than a panic.
fn data<T: Serialize>(payload: &T) -> Event {
    match serde_json::to_string(payload) {
        Ok(json) => Event::default().data(json),
        Err(err) => Event::default().data(format!(r#"{{"error":"{err}"}}"#)),
    }
}
