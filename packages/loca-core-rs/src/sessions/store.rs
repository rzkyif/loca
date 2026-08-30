//! [`SessionsStore`]: sessions, messages and attachment records, backed by
//! plain JSON files on disk.
//!
//! One directory per session under `<root>/sessions/`. Each session is a single
//! `session.json` holding its metadata, messages and attachment records;
//! attachment bytes live beside it under `attachments/` (written by the upload
//! route, recorded here). Nothing here touches SQLite, deliberately.
//!
//! A read-modify-write of one `session.json` spans an await, so a per-session
//! lock serializes every mutation on a given id. `files::write_atomic` handles
//! the other half (no two writers share a temp file).
//!
//! Owner scoping surfaces as `session_not_found`, never `forbidden`, so probing
//! cannot tell "not yours" from "does not exist".

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::{Map, Value};
use tokio::sync::OwnedMutexGuard;

use crate::contract::api::{Session, SessionListEntry, TokenUsage};
use crate::error::{internal, validation, AppError, ErrorCode, Result};
use crate::files;
use crate::paths::Paths;

use super::doc::{next_ord, AttachmentRecord, SessionDoc, SessionMeta, StoredMessage};

/// Where a message landed. Both routes and the chat writer need the assigned
/// ord, not just the id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Appended {
    pub id: String,
    pub ord: i64,
}

/// A new session, before it has an id.
#[derive(Clone, Debug, Default)]
pub struct CreateSessionInput {
    pub owner_client_id: String,
    pub title: Option<String>,
    /// A temporary session lives only in the in-memory map: it never reaches
    /// disk and is excluded from every listing. Fixed at creation; there is no
    /// later reclassification.
    pub temporary: bool,
}

/// One attachment's details, before the store assigns it an id and a timestamp.
#[derive(Clone, Debug)]
pub struct NewAttachment {
    pub message_id: String,
    pub filename: String,
    pub mime: Option<String>,
    pub size_bytes: u64,
    pub abs_path: String,
}

/// One session's on-disk footprint, for the storage screen, which spans clients.
#[derive(Clone, Debug)]
pub struct SessionFootprint {
    pub id: String,
    pub owner_client_id: String,
    pub title: String,
    pub updated_at_ms: i64,
    pub size_bytes: u64,
}

/// What an owner-agnostic delete removed.
#[derive(Clone, Debug)]
pub struct DeletedSession {
    pub owner_client_id: String,
    pub attachment_paths: Vec<PathBuf>,
}

type LockTable = Arc<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>;

struct Inner {
    paths: Arc<Paths>,
    /// Temporary sessions, keyed by id. They never reach disk: reads check here
    /// first, writes keep a document that is already here in memory, and the
    /// directory removal drops it. Listings walk disk only, so they exclude
    /// these for free. Process-lifetime: a restart wipes every one.
    ephemeral: Mutex<HashMap<String, SessionDoc>>,
    locks: LockTable,
}

/// The store. `Clone` over an `Arc`, and constructed once by `boot`: the
/// ephemeral map and the lock table are state, so a second instance would give a
/// temporary session created through one route a store another route cannot see.
#[derive(Clone)]
pub struct SessionsStore {
    inner: Arc<Inner>,
}

impl SessionsStore {
    pub fn new(paths: Arc<Paths>) -> Self {
        Self {
            inner: Arc::new(Inner {
                paths,
                ephemeral: Mutex::new(HashMap::new()),
                locks: Arc::new(Mutex::new(HashMap::new())),
            }),
        }
    }

    // --- sessions ------------------------------------------------------------

    pub async fn create(&self, input: CreateSessionInput) -> Result<Session> {
        let id = new_id();
        let now = now_ms();
        let mut doc = SessionDoc {
            id: id.clone(),
            owner_client_id: input.owner_client_id,
            title: input.title.unwrap_or_default(),
            created_at_ms: now,
            updated_at_ms: now,
            token_usage: None,
            temporary: None,
            messages: Vec::new(),
            attachments: Vec::new(),
        };
        if input.temporary {
            // Born in the map, never on disk. The id is marked first so the
            // write below routes to memory instead of the filesystem.
            doc.temporary = Some(true);
            self.ephemeral_mut().insert(id, doc.clone());
        } else {
            self.write_doc(&doc).await?;
        }
        Ok(doc.to_session())
    }

    /// Every session this client owns, newest first.
    ///
    /// Reads the small `meta.json` sidecar rather than parsing every full
    /// transcript. On a missing or unreadable meta (a session written before the
    /// sidecar existed, or a failed meta write) it falls back to the full
    /// document and backfills, so the next call is fast again.
    pub async fn list(&self, owner_client_id: &str) -> Result<Vec<SessionListEntry>> {
        let mut metas: Vec<SessionMeta> = Vec::new();
        for name in self.session_dir_names().await {
            let meta = match self.read_meta(&name).await {
                Some(meta) => meta,
                None => {
                    let Some(doc) = self.read_doc(&name).await else {
                        continue;
                    };
                    let derived = SessionMeta::derive(&doc);
                    self.write_meta(&doc).await;
                    derived
                }
            };
            if meta.owner_client_id != owner_client_id {
                continue;
            }
            metas.push(meta);
        }
        metas.sort_by_key(|m| std::cmp::Reverse(m.updated_at_ms));
        Ok(metas.iter().map(SessionMeta::to_list_entry).collect())
    }

    pub async fn get_or_throw(&self, owner_client_id: &str, session_id: &str) -> Result<Session> {
        Ok(self
            .require_owned(owner_client_id, session_id)
            .await?
            .to_session())
    }

    pub async fn patch_title(
        &self,
        owner_client_id: &str,
        session_id: &str,
        title: &str,
    ) -> Result<()> {
        let _lock = self.lock(session_id).await;
        let mut doc = self.require_owned(owner_client_id, session_id).await?;
        doc.title = title.to_string();
        doc.updated_at_ms = now_ms();
        self.write_doc(&doc).await
    }

    /// Owner-agnostic, and a missing session is a silent no-op: the stream that
    /// reports usage can outlive the session it was reporting for.
    pub async fn set_token_usage(&self, session_id: &str, usage: Option<TokenUsage>) -> Result<()> {
        let _lock = self.lock(session_id).await;
        let Some(mut doc) = self.read_doc(session_id).await else {
            return Ok(());
        };
        doc.token_usage = usage;
        doc.updated_at_ms = now_ms();
        self.write_doc(&doc).await
    }

    /// Removes the session and returns the attachment paths it recorded, so the
    /// caller can unlink any that lived outside the session directory.
    pub async fn delete(&self, owner_client_id: &str, session_id: &str) -> Result<Vec<PathBuf>> {
        let _lock = self.lock(session_id).await;
        let doc = self.require_owned(owner_client_id, session_id).await?;
        let paths = attachment_paths(&doc);
        self.remove_session_dir(session_id).await;
        Ok(paths)
    }

    /// Drop every temporary session this client owns except `keep_id`.
    ///
    /// A client has at most one reachable temporary session, so any other it owns
    /// was orphaned by navigating away without a clean delete. Called on each
    /// creation and navigation, so the memory and any attachment bytes are
    /// reclaimed rather than waiting for a restart.
    pub async fn sweep_client_temporary(&self, owner_client_id: &str, keep_id: Option<&str>) {
        let stale: Vec<String> = self
            .ephemeral_mut()
            .values()
            .filter(|d| d.owner_client_id == owner_client_id && Some(d.id.as_str()) != keep_id)
            .map(|d| d.id.clone())
            .collect();
        for id in stale {
            self.remove_session_dir(&id).await;
        }
    }

    // --- messages ------------------------------------------------------------

    pub async fn list_messages(&self, session_id: &str) -> Vec<StoredMessage> {
        self.read_doc(session_id)
            .await
            .map(|d| d.messages)
            .unwrap_or_default()
    }

    pub async fn append_message(
        &self,
        session_id: &str,
        message: StoredMessage,
    ) -> Result<Appended> {
        let _lock = self.lock(session_id).await;
        let mut doc = self.require_exists(session_id).await?;
        let mut stored = fill_in(message);
        let ord = next_ord(&doc.messages);
        stored.set_ord(ord);
        let appended = Appended {
            id: stored.id().unwrap_or_default().to_string(),
            ord,
        };
        doc.messages.push(stored);
        doc.updated_at_ms = now_ms();
        self.write_doc(&doc).await?;
        Ok(appended)
    }

    /// Insert directly after `after_id` in chronological order, so a regenerated
    /// mid-history turn lands in its slot instead of at the tail.
    ///
    /// `None`, or an id no longer present, appends. A tail insert shifts nothing,
    /// so it assigns only the new message's ord. A true mid-history splice
    /// renumbers the whole document to keep ord equal to the array index; nothing
    /// outside this store depends on specific ord values, only their order.
    pub async fn insert_message_after(
        &self,
        session_id: &str,
        message: StoredMessage,
        after_id: Option<&str>,
    ) -> Result<Appended> {
        let _lock = self.lock(session_id).await;
        let mut doc = self.require_exists(session_id).await?;
        let mut stored = fill_in(message);
        let id = stored.id().unwrap_or_default().to_string();
        let anchor = after_id.and_then(|id| doc.messages.iter().position(|m| m.id() == Some(id)));
        let ord = match anchor {
            // A true mid-history splice: everything after it shifts, so the
            // whole document is renumbered and the new ord is its index.
            Some(idx) if idx + 1 < doc.messages.len() => {
                doc.messages.insert(idx + 1, stored);
                renumber(&mut doc.messages);
                (idx + 1) as i64
            }
            // A tail insert: appending, an anchor no longer present, or the
            // current tail. Nothing shifts, so only the new message gets an ord.
            _ => {
                let ord = next_ord(&doc.messages);
                stored.set_ord(ord);
                doc.messages.push(stored);
                ord
            }
        };
        doc.updated_at_ms = now_ms();
        self.write_doc(&doc).await?;
        Ok(Appended { id, ord })
    }

    /// Remove every message strictly between the anchor and the next-newer
    /// `user` message, which is the old contents of the anchor's turn. Returns
    /// the removed ids so the caller can broadcast the deletions. An unknown
    /// anchor is a no-op.
    pub async fn delete_turn(&self, session_id: &str, anchor_id: &str) -> Result<Vec<String>> {
        let _lock = self.lock(session_id).await;
        let mut doc = self.require_exists(session_id).await?;
        let Some(anchor) = doc.messages.iter().position(|m| m.id() == Some(anchor_id)) else {
            return Ok(Vec::new());
        };
        let mut end = anchor + 1;
        while end < doc.messages.len() && doc.messages[end].role() != Some("user") {
            end += 1;
        }
        if end == anchor + 1 {
            return Ok(Vec::new());
        }
        let removed: Vec<String> = doc
            .messages
            .drain(anchor + 1..end)
            .map(|m| m.id().unwrap_or_default().to_string())
            .collect();
        renumber(&mut doc.messages);
        doc.updated_at_ms = now_ms();
        self.write_doc(&doc).await?;
        Ok(removed)
    }

    pub async fn get_message(&self, session_id: &str, message_id: &str) -> Result<StoredMessage> {
        let doc = self.require_exists(session_id).await?;
        doc.messages
            .into_iter()
            .find(|m| m.id() == Some(message_id))
            .ok_or_else(|| message_not_found(message_id))
    }

    /// Merge `patch` into a message, then validate the result.
    ///
    /// The validation is the merged message parsing as a `contract::api::Message`
    /// of its own role. That type is per-role strict with `deny_unknown_fields`,
    /// so it rejects an assistant-only field on a user message without any of it
    /// being restated here. A `role` key is refused before the merge, because
    /// merging one would let a patch re-role a message.
    ///
    /// `id`, `role` and `ord` are pinned from the stored message afterwards: a
    /// content patch can never re-key, re-role or reorder.
    pub async fn patch_message(
        &self,
        session_id: &str,
        message_id: &str,
        patch: &Map<String, Value>,
    ) -> Result<Appended> {
        if patch.contains_key("role") {
            return Err(validation("a message patch cannot change `role`"));
        }
        let _lock = self.lock(session_id).await;
        let mut doc = self.require_exists(session_id).await?;
        let idx = doc
            .messages
            .iter()
            .position(|m| m.id() == Some(message_id))
            .ok_or_else(|| message_not_found(message_id))?;

        let current = doc.messages[idx].clone();
        let (id, role, ord) = (
            current.id().map(str::to_string),
            current.role().map(str::to_string),
            current.ord(),
        );
        let mut merged = current;
        merged.merge(patch);
        if let Some(id) = id.clone() {
            merged.set_id(id);
        }
        if let Some(role) = role {
            merged
                .as_map_mut()
                .insert("role".into(), Value::String(role));
        }
        if let Some(ord) = ord {
            merged.set_ord(ord);
        }
        merged
            .validate()
            .map_err(|err| validation(format!("invalid message patch: {err}")))?;

        doc.messages[idx] = merged;
        doc.updated_at_ms = now_ms();
        self.write_doc(&doc).await?;
        Ok(Appended {
            id: id.unwrap_or_default(),
            ord: ord.unwrap_or_default(),
        })
    }

    pub async fn delete_message(&self, session_id: &str, message_id: &str) -> Result<()> {
        let _lock = self.lock(session_id).await;
        let mut doc = self.require_exists(session_id).await?;
        let idx = doc
            .messages
            .iter()
            .position(|m| m.id() == Some(message_id))
            .ok_or_else(|| message_not_found(message_id))?;
        doc.messages.remove(idx);
        doc.updated_at_ms = now_ms();
        self.write_doc(&doc).await
    }

    // --- attachments ---------------------------------------------------------

    pub async fn record_attachment(
        &self,
        session_id: &str,
        new: NewAttachment,
    ) -> Result<AttachmentRecord> {
        let _lock = self.lock(session_id).await;
        let mut doc = self.require_exists(session_id).await?;
        let record = AttachmentRecord {
            id: new_id(),
            session_id: session_id.to_string(),
            message_id: new.message_id,
            filename: new.filename,
            mime: new.mime,
            size_bytes: new.size_bytes,
            abs_path: new.abs_path,
            created_at_ms: now_ms(),
        };
        doc.attachments.push(record.clone());
        // No `updatedAtMs` bump: recording an attachment is not a conversation
        // event, and bumping it would reorder the session list on an upload.
        self.write_doc(&doc).await?;
        Ok(record)
    }

    pub async fn get_attachment(
        &self,
        session_id: &str,
        attachment_id: &str,
    ) -> Result<AttachmentRecord> {
        self.read_doc(session_id)
            .await
            .and_then(|d| d.attachments.into_iter().find(|a| a.id == attachment_id))
            .ok_or_else(|| AppError::new(ErrorCode::NotFound, "attachment not found"))
    }

    // --- storage view, all clients -------------------------------------------

    /// Every session across every client, with its on-disk footprint. The
    /// storage screen spans clients; `list` is the per-client session bar.
    pub async fn list_all(&self) -> Vec<SessionFootprint> {
        let mut out = Vec::new();
        for name in self.session_dir_names().await {
            let Some(doc) = self.read_doc(&name).await else {
                continue;
            };
            // The attachment walk recurses, so the whole footprint goes to the
            // blocking pool in one hop rather than an await per directory entry.
            let doc_file = self.session_file(&doc.id);
            let attachments = self.inner.paths.session_attachments_dir(&doc.id);
            let size_bytes =
                tokio::task::spawn_blocking(move || file_size(&doc_file) + dir_size(&attachments))
                    .await
                    .unwrap_or(0);
            out.push(SessionFootprint {
                id: doc.id.clone(),
                owner_client_id: doc.owner_client_id,
                title: doc.title,
                updated_at_ms: doc.updated_at_ms,
                size_bytes,
            });
        }
        out
    }

    /// Delete without knowing the owner, for the storage screen. `None` when the
    /// session does not exist.
    pub async fn delete_by_id(&self, session_id: &str) -> Option<DeletedSession> {
        let _lock = self.lock(session_id).await;
        let doc = self.read_doc(session_id).await?;
        let deleted = DeletedSession {
            owner_client_id: doc.owner_client_id.clone(),
            attachment_paths: attachment_paths(&doc),
        };
        self.remove_session_dir(session_id).await;
        Some(deleted)
    }

    // --- boot ----------------------------------------------------------------

    /// Remove any session directory with no `session.json`.
    ///
    /// The only way one exists is a temporary session whose attachments reached
    /// disk while its document lived in memory, then lost to an unclean
    /// shutdown. A persistent session writes `session.json` at creation, before
    /// any attachment, so this never touches a real one. Run once at boot.
    pub async fn sweep_orphaned_dirs(&self) {
        for name in self.session_dir_names().await {
            // Existence, not parseability: a corrupt but present `session.json`
            // is a real session that reads skip, not an orphan to delete.
            if is_file(self.session_file(&name)).await {
                continue;
            }
            remove_dir_all_quiet(self.inner.paths.session_dir(&name)).await;
            tracing::info!(
                target: "loca::sessions-store",
                "swept orphaned session dir {name} (no session.json)",
            );
        }
    }

    // --- helpers -------------------------------------------------------------

    async fn require_owned(&self, owner_client_id: &str, session_id: &str) -> Result<SessionDoc> {
        match self.read_doc(session_id).await {
            Some(doc) if doc.owner_client_id == owner_client_id => Ok(doc),
            // A session owned by someone else reads as absent, so a wrong owner
            // and a wrong id are indistinguishable from outside.
            _ => Err(session_not_found(session_id)),
        }
    }

    async fn require_exists(&self, session_id: &str) -> Result<SessionDoc> {
        self.read_doc(session_id)
            .await
            .ok_or_else(|| session_not_found(session_id))
    }

    /// The whole document, or `None` when it is absent, unreadable or corrupt.
    ///
    /// All three collapse to absent on purpose. A truncated file must not poison
    /// `list`, `list_all` or the boot sweep for every other session, and a file
    /// statted present but unreadable a moment later is a race with a concurrent
    /// delete rather than something to raise into a listing.
    async fn read_doc(&self, session_id: &str) -> Option<SessionDoc> {
        // A snapshot, so a temporary session reads exactly like a persistent one.
        // Handing out the live document would let an in-place mutation, harmless
        // on the disk path's throwaway parse, corrupt the canonical state.
        if let Some(doc) = self.ephemeral_mut().get(session_id) {
            return Some(doc.clone());
        }
        if !is_safe_segment(session_id) {
            return None;
        }
        match files::read_json::<SessionDoc>(self.session_file(session_id)).await {
            Ok(doc) => Some(doc),
            Err(err) if err.code == ErrorCode::NotFound => None,
            Err(err) => {
                tracing::warn!(
                    target: "loca::sessions-store",
                    "unreadable session.json for {session_id}; skipping: {}", err.message,
                );
                None
            }
        }
    }

    async fn write_doc(&self, doc: &SessionDoc) -> Result<()> {
        // A temporary session lives only in the map: commit the snapshot the
        // caller mutated, mirroring how the disk path commits by rename, and
        // return before any filesystem access.
        {
            let mut map = self.ephemeral_mut();
            if map.contains_key(&doc.id) {
                map.insert(doc.id.clone(), doc.clone());
                return Ok(());
            }
        }
        create_dir_all(self.inner.paths.session_dir(&doc.id)).await?;
        files::write_json_atomic(self.session_file(&doc.id), doc).await?;
        self.write_meta(doc).await;
        Ok(())
    }

    async fn read_meta(&self, session_id: &str) -> Option<SessionMeta> {
        if !is_safe_segment(session_id) {
            return None;
        }
        files::read_json::<SessionMeta>(self.meta_file(session_id))
            .await
            .ok()
    }

    async fn write_meta(&self, doc: &SessionDoc) {
        let meta = SessionMeta::derive(doc);
        // Compact, unlike `session.json`: the sidecar is machine-read on every
        // list and is never opened by hand.
        let bytes = match serde_json::to_vec(&meta) {
            Ok(bytes) => bytes,
            Err(err) => {
                tracing::warn!(
                    target: "loca::sessions-store",
                    "could not serialize session meta for {}: {err}", doc.id,
                );
                return;
            }
        };
        let path = self.meta_file(&doc.id);
        if let Err(err) = files::write_atomic(path.clone(), bytes).await {
            // Any previous meta is now stale and a reader cannot tell, so it is
            // deleted and `list` falls back to the full document. A crash
            // between the two writes leaves a stale meta until the next
            // mutation, a window that self-heals.
            let _ = remove_file(path).await;
            tracing::warn!(
                target: "loca::sessions-store",
                "could not write session meta for {}: {}", doc.id, err.message,
            );
        }
    }

    async fn remove_session_dir(&self, session_id: &str) {
        self.ephemeral_mut().remove(session_id);
        if !is_safe_segment(session_id) {
            return;
        }
        remove_dir_all_quiet(self.inner.paths.session_dir(session_id)).await;
    }

    /// The names of every session directory.
    async fn session_dir_names(&self) -> Vec<String> {
        read_dir_names(self.inner.paths.sessions_dir()).await
    }

    fn session_file(&self, session_id: &str) -> PathBuf {
        self.inner
            .paths
            .session_dir(session_id)
            .join("session.json")
    }

    fn meta_file(&self, session_id: &str) -> PathBuf {
        self.inner.paths.session_dir(session_id).join("meta.json")
    }

    fn ephemeral_mut(&self) -> std::sync::MutexGuard<'_, HashMap<String, SessionDoc>> {
        // Every section under this lock is a map get, insert or remove, so
        // recovering from poisoning keeps the store usable rather than turning
        // one panic into a permanently dead session store.
        self.inner
            .ephemeral
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Serialize this session's mutations. Held for the whole read-modify-write.
    async fn lock(&self, session_id: &str) -> SessionLock {
        let gate = {
            let mut table = self
                .inner
                .locks
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            Arc::clone(table.entry(session_id.to_string()).or_default())
        };
        SessionLock {
            locks: Arc::clone(&self.inner.locks),
            id: session_id.to_string(),
            guard: Some(gate.lock_owned().await),
        }
    }
}

/// Held for one mutation, and reaped on release.
///
/// The entry is dropped once nothing holds it, so the map does not grow by one
/// per session id ever touched. The check runs under the table lock and after
/// the guard is released: a waiter holds its own `Arc`, so the count cannot be
/// one while anyone is queued, and nothing can clone the entry meanwhile.
struct SessionLock {
    locks: LockTable,
    id: String,
    guard: Option<OwnedMutexGuard<()>>,
}

impl Drop for SessionLock {
    fn drop(&mut self) {
        // The guard owns an `Arc` of the gate, so it has to go before the count
        // means anything.
        self.guard.take();
        let mut table = self
            .locks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if table
            .get(&self.id)
            .is_some_and(|gate| Arc::strong_count(gate) == 1)
        {
            table.remove(&self.id);
        }
    }
}

/// Fill in the two fields the client may omit: a fresh id and the current time.
/// An empty string and a zero timestamp count as omitted. The ord is assigned by
/// the caller, the only one that knows whether this is a tail insert.
fn fill_in(mut message: StoredMessage) -> StoredMessage {
    if message.id().is_none_or(str::is_empty) {
        message.set_id(new_id());
    }
    if message.created_at_ms().is_none_or(|ms| ms == 0) {
        message.set_created_at_ms(now_ms());
    }
    message
}

/// Reassign every ord to its array index.
fn renumber(messages: &mut [StoredMessage]) {
    for (i, message) in messages.iter_mut().enumerate() {
        message.set_ord(i as i64);
    }
}

fn attachment_paths(doc: &SessionDoc) -> Vec<PathBuf> {
    doc.attachments
        .iter()
        .map(|a| PathBuf::from(&a.abs_path))
        .collect()
}

// --- filesystem ---------------------------------------------------------------
//
// `std::fs` on the blocking pool rather than `tokio::fs`, matching `files`:
// `tokio::fs` is a `spawn_blocking` per call anyway. One hop per operation.

async fn blocking<T, F>(f: F) -> Result<T>
where
    F: FnOnce() -> std::io::Result<T> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|err| internal(format!("filesystem task failed: {err}")))?
        .map_err(AppError::from)
}

pub(super) async fn create_dir_all(path: PathBuf) -> Result<()> {
    blocking(move || std::fs::create_dir_all(path)).await
}

pub(super) async fn read_file(path: PathBuf) -> Result<Vec<u8>> {
    blocking(move || std::fs::read(path)).await
}

pub async fn remove_file(path: PathBuf) -> Result<()> {
    blocking(move || std::fs::remove_file(path)).await
}

/// Idempotent: a missing path is not an error, and neither is anything else.
async fn remove_dir_all_quiet(path: PathBuf) {
    let _ = tokio::task::spawn_blocking(move || std::fs::remove_dir_all(path)).await;
}

async fn is_file(path: PathBuf) -> bool {
    tokio::task::spawn_blocking(move || path.is_file())
        .await
        .unwrap_or(false)
}

/// Immediate subdirectory names. Empty when the directory itself is absent,
/// which is the state before the first session is created.
async fn read_dir_names(path: PathBuf) -> Vec<String> {
    tokio::task::spawn_blocking(move || {
        let Ok(entries) = std::fs::read_dir(path) else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect()
    })
    .await
    .unwrap_or_default()
}

fn file_size(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

fn dir_size(path: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| match entry.file_type() {
            Ok(t) if t.is_dir() => dir_size(&entry.path()),
            _ => file_size(&entry.path()),
        })
        .sum()
}

/// A session id becomes a directory name, so it has to be one path segment and
/// nothing else.
///
/// Ids are generated here and are always ULIDs, but they arrive back from the
/// client as URL path parameters, and axum percent-decodes those. Without this,
/// `..%2F..%2Fsomething` would resolve outside `sessions/`. Rejecting reads as
/// absent rather than as a distinct error, which is the same posture owner
/// scoping takes.
fn is_safe_segment(id: &str) -> bool {
    !id.is_empty()
        && id != "."
        && id != ".."
        && !id.contains('/')
        && !id.contains('\\')
        && !id.contains('\0')
}

pub(super) fn new_id() -> String {
    ulid::Ulid::new().to_string()
}

pub(super) fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn session_not_found(session_id: &str) -> AppError {
    AppError::new(
        ErrorCode::SessionNotFound,
        format!("session {session_id} not found"),
    )
}

fn message_not_found(message_id: &str) -> AppError {
    AppError::new(
        ErrorCode::MessageNotFound,
        format!("message {message_id} not found"),
    )
}
