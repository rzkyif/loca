//! The on-disk shapes: the session document, one stored message, an attachment
//! record, and the list sidecar.
//!
//! These files are part of the persistence tree, which is one of the byte-exact
//! parity surfaces: an existing install's `sessions/` is read as-is. Every field
//! name below is fixed by what is already on disk.
//!
//! Per session, at `<root>/sessions/<sessionId>/`:
//!
//! - `session.json`, the whole document, pretty-printed.
//! - `meta.json`, the compact list sidecar. A cache, never the source of truth.
//! - `attachments/<attachmentId>_<filename>`, the attachment bytes.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::contract::api::{Session, SessionListEntry, SummaryPart, TokenUsage};

use super::summary::summarize_conversation;

/// One session, whole.
///
/// Timestamps are `i64` rather than the contract's `f64`. Every writer of these
/// files produces integer milliseconds, and an integer round-trips through
/// `serde_json` byte for byte where an `f64` would republish `1700000000000` as
/// `1700000000000.0`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionDoc {
    pub id: String,
    #[serde(rename = "ownerClientId")]
    pub owner_client_id: String,
    pub title: String,
    #[serde(rename = "createdAtMs")]
    pub created_at_ms: i64,
    #[serde(rename = "updatedAtMs")]
    pub updated_at_ms: i64,
    #[serde(
        rename = "tokenUsage",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub token_usage: Option<TokenUsage>,
    /// A temporary session lives only in the store's in-memory map and is never
    /// serialized, so this is only ever `None` on disk. It is carried here
    /// because the in-memory document is the same type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temporary: Option<bool>,
    /// Defaulted so an older or partial file still loads, matching the
    /// TypeScript's `parsed.messages ??= []`.
    #[serde(default)]
    pub messages: Vec<StoredMessage>,
    #[serde(default)]
    pub attachments: Vec<AttachmentRecord>,
}

impl SessionDoc {
    pub fn to_session(&self) -> Session {
        Session {
            id: self.id.clone(),
            owner_client_id: self.owner_client_id.clone(),
            title: self.title.clone(),
            created_at_ms: self.created_at_ms,
            updated_at_ms: self.updated_at_ms,
            token_usage: self.token_usage.clone(),
            temporary: self.temporary,
        }
    }
}

/// One message, exactly as it sits on disk.
///
/// **Not `contract::api::Message`, deliberately.** That type is
/// `#[serde(tag = "role", deny_unknown_fields)]` over nine closed variants,
/// which is right at the wire boundary and wrong for a file we wrote ourselves:
/// one message carrying a role or a field the enum does not know would fail the
/// whole `session.json` parse, and the read path treats a failed parse as an
/// absent session. A single drifted message would therefore erase a user's
/// entire transcript from every listing, for a cosmetic cause.
///
/// Keeping the raw map also makes the patch route a shallow merge rather than a
/// nine-variant field-by-field reconstruction, and it is what makes the
/// round-trip fidelity test in `tests.rs` meaningful. Strictness lives at the
/// wire (`contract::inbound::MessageInput` on the way in,
/// [`StoredMessage::validate`] on a patch); tolerance lives here.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StoredMessage(Map<String, Value>);

impl StoredMessage {
    pub fn new(map: Map<String, Value>) -> Self {
        Self(map)
    }

    /// The message as a JSON object, for a caller that needs a field this type
    /// does not name.
    pub fn as_map(&self) -> &Map<String, Value> {
        &self.0
    }

    pub fn into_map(self) -> Map<String, Value> {
        self.0
    }

    pub fn as_map_mut(&mut self) -> &mut Map<String, Value> {
        &mut self.0
    }

    pub fn role(&self) -> Option<&str> {
        self.0.get("role").and_then(Value::as_str)
    }

    pub fn id(&self) -> Option<&str> {
        self.0.get("id").and_then(Value::as_str)
    }

    /// `None` when absent or not a number, which `next_ord` skips exactly as the
    /// TypeScript's `m.ord > max` skips an `undefined`.
    pub fn ord(&self) -> Option<i64> {
        as_i64(self.0.get("ord")?)
    }

    pub fn created_at_ms(&self) -> Option<i64> {
        as_i64(self.0.get("createdAtMs")?)
    }

    /// The `content` field, which is either a string or an array of parts.
    pub fn content(&self) -> Option<&Value> {
        self.0.get("content")
    }

    pub fn set_id(&mut self, id: String) {
        self.0.insert("id".into(), Value::String(id));
    }

    pub fn set_ord(&mut self, ord: i64) {
        self.0.insert("ord".into(), Value::from(ord));
    }

    pub fn set_created_at_ms(&mut self, ms: i64) {
        self.0.insert("createdAtMs".into(), Value::from(ms));
    }

    /// Shallow-merge `patch` over this message, the way the TypeScript spreads
    /// `{ ...current, ...patch }`.
    pub fn merge(&mut self, patch: &Map<String, Value>) {
        for (key, value) in patch {
            self.0.insert(key.clone(), value.clone());
        }
    }

    /// Parse through the generated contract type.
    ///
    /// The strictness this borrows is the point: `deny_unknown_fields` on the
    /// per-role variants is what rejects an assistant-only field on a user
    /// message, at exactly the strictness `messagePatchSchemaByRole` had, without
    /// restating any of it.
    pub fn validate(&self) -> Result<crate::contract::api::Message, serde_json::Error> {
        serde_json::from_value(Value::Object(self.0.clone()))
    }
}

/// One attachment's record. A closed shape with no union, so unlike a message it
/// is a typed struct.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AttachmentRecord {
    pub id: String,
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(rename = "messageId")]
    pub message_id: String,
    pub filename: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
    #[serde(rename = "sizeBytes")]
    pub size_bytes: u64,
    #[serde(rename = "absPath")]
    pub abs_path: String,
    #[serde(rename = "createdAtMs")]
    pub created_at_ms: i64,
}

/// The small per-session sidecar `list` reads instead of parsing every full
/// transcript.
///
/// A **cache, never the source of truth**: on a missing or unreadable meta,
/// `list` falls back to the full document and backfills. Deserialization is the
/// whole validity check, and it has to be: a truncated write leaving a valid
/// `id` but no `updatedAtMs` would otherwise sort as garbage and send an absent
/// count over the wire. Every field but `summary` is required here for that
/// reason.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionMeta {
    pub id: String,
    #[serde(rename = "ownerClientId")]
    pub owner_client_id: String,
    pub title: String,
    #[serde(rename = "createdAtMs")]
    pub created_at_ms: i64,
    #[serde(rename = "updatedAtMs")]
    pub updated_at_ms: i64,
    #[serde(rename = "messageCount")]
    pub message_count: usize,
    #[serde(default)]
    pub summary: Vec<SummaryPart>,
}

impl SessionMeta {
    pub fn derive(doc: &SessionDoc) -> Self {
        Self {
            id: doc.id.clone(),
            owner_client_id: doc.owner_client_id.clone(),
            title: doc.title.clone(),
            created_at_ms: doc.created_at_ms,
            updated_at_ms: doc.updated_at_ms,
            message_count: doc.messages.len(),
            summary: summarize_conversation(&doc.messages),
        }
    }

    pub fn to_list_entry(&self) -> SessionListEntry {
        SessionListEntry {
            id: self.id.clone(),
            title: self.title.clone(),
            created_at_ms: self.created_at_ms,
            updated_at_ms: self.updated_at_ms,
            message_count: self.message_count as i64,
            summary: self.summary.clone(),
        }
    }
}

/// Highest `ord` plus one.
///
/// Max plus one rather than the message count, because `delete_message` leaves
/// gaps and a length-based ord would then collide with an existing one.
pub fn next_ord(messages: &[StoredMessage]) -> i64 {
    messages
        .iter()
        .filter_map(StoredMessage::ord)
        .max()
        .map_or(0, |max| max + 1)
}

fn as_i64(value: &Value) -> Option<i64> {
    value.as_i64().or_else(|| value.as_f64().map(|f| f as i64))
}
