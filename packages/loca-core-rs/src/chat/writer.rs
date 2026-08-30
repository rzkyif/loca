//! One turn's message announcements and persistence.
//!
//! The server owns message identity and order. [`TurnWriter::born`] announces a
//! message at its live position before any delta touches it;
//! [`TurnWriter::finalize`] persists it at the durable insertion cursor and
//! emits the terminal snapshot. The client never mints an id for a chat-born
//! message, so live order and persisted order converge by construction.
//!
//! Two cursors: `cursor` is the last PERSISTED id and decides where the next
//! `finalize`
//! inserts; `live_cursor` is the last ANNOUNCED id and decides what `after_id`
//! a birth carries. They differ only while two messages of one hop are live at
//! once (reasoning plus assistant), and reconverge because finalization happens
//! in birth order.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use crate::contract::api::ServerToClientFrame;
use crate::error::Result;
use crate::sessions::doc::StoredMessage;
use crate::sessions::SessionsStore;
use crate::transport::FrameBus;

use super::stream::ActiveStream;

/// A message the turn is building. Shared rather than owned, because the muxer
/// fills content into the same object the catch-up buffer holds.
pub type LiveRef = Arc<Mutex<StoredMessage>>;

pub struct TurnWriter {
    stream: Arc<ActiveStream>,
    store: SessionsStore,
    frames: Arc<FrameBus>,
    /// Last persisted id; where the next finalize inserts.
    cursor: Option<String>,
    /// Last announced id; where the next birth points its `after_id`.
    live_cursor: Option<String>,
    born_ids: HashSet<String>,
}

impl TurnWriter {
    /// `anchor_id` is the user message this turn hangs off, so both cursors
    /// start there and the first message of the turn lands immediately after it.
    pub fn new(
        stream: Arc<ActiveStream>,
        store: SessionsStore,
        frames: Arc<FrameBus>,
        anchor_id: Option<String>,
    ) -> Self {
        Self {
            stream,
            store,
            frames,
            cursor: anchor_id.clone(),
            live_cursor: anchor_id,
            born_ids: HashSet::new(),
        }
    }

    /// Announce a message at its live position and buffer it for catch-up.
    pub fn born(&mut self, message: &LiveRef) {
        let id = message_id(message);
        self.stream
            .note_born(id.clone(), Arc::clone(message), self.live_cursor.clone());
        self.emit(message, self.live_cursor.clone(), false);
        self.born_ids.insert(id.clone());
        self.live_cursor = Some(id);
    }

    /// Persist a message at the durable cursor and emit its terminal snapshot.
    /// A message finalized with no prior birth is POSITIONED by this frame, so
    /// it carries the live cursor and advances it; one that was born carries
    /// `None`, since re-positioning would move a bubble the client has placed.
    pub async fn finalize(&mut self, message: &LiveRef) -> Result<()> {
        let id = message_id(message);
        let first_emission = !self.born_ids.contains(&id);
        let snapshot = clone_stored(message);
        let appended = self
            .store
            .insert_message_after(&self.stream.session_id, snapshot, self.cursor.as_deref())
            .await?;
        set_ord(message, appended.ord);
        self.cursor = Some(id.clone());
        // Persisted, so a reconnecting client reads it from the session rather
        // than from the catch-up buffer.
        self.stream.note_finalized(&id);
        let after_id = if first_emission {
            self.live_cursor.clone()
        } else {
            None
        };
        self.emit(message, after_id, true);
        if first_emission {
            self.live_cursor = Some(id);
        }
        Ok(())
    }

    /// The last persisted id, which is where a caller that persists outside the
    /// writer would insert.
    pub fn cursor(&self) -> Option<&str> {
        self.cursor.as_deref()
    }

    fn emit(&self, message: &LiveRef, after_id: Option<String>, final_: bool) {
        let validated = {
            let guard = lock(message);
            guard.validate()
        };
        match validated {
            Ok(message) => self.frames.broadcast_to_client(
                &self.stream.client_id,
                &ServerToClientFrame::ChatMessage {
                    after_id,
                    final_,
                    message,
                    session_id: self.stream.session_id.clone(),
                    stream_id: self.stream.stream_id.clone(),
                },
            ),
            // A message the contract will not accept is a bug here rather than
            // bad input, and the frame is typed, so there is nothing truthful
            // to send. The persist already happened, so it appears on the next
            // reload; logged loudly because that is the only symptom.
            Err(err) => tracing::error!(
                target: "loca::chat",
                "stream {}: a chat-born message does not satisfy the wire contract, \
                 so its snapshot was dropped: {err}",
                self.stream.stream_id,
            ),
        }
    }
}

fn message_id(message: &LiveRef) -> String {
    lock(message).id().unwrap_or_default().to_string()
}

fn clone_stored(message: &LiveRef) -> StoredMessage {
    lock(message).clone()
}

fn set_ord(message: &LiveRef, ord: i64) {
    lock(message).set_ord(ord);
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}
