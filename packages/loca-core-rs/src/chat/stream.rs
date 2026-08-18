//! The per-turn record and the registry of in-flight turns.
//!
//! Ports `core-engine/src/services/chat-types.ts` and the stream-lifecycle half
//! of `chat.ts` (`:82-87`, `:104-153`, `:215-264`).
//!
//! # The two buffers, and why a turn needs them at all
//!
//! A turn keeps running when its client disconnects. That is stated as an
//! invariant in `chat.ts:15-22` and the Client depends on it: a transport drop
//! detaches rather than interrupting (`streaming.svelte.ts:105-113`), which is
//! what makes a core swap transparent. Broadcasting to a client with no live
//! connection is a no-op and nothing is buffered on the bus, so the turn holds
//! its own catch-up state:
//!
//! - [`ActiveStream::live_messages`], the born-but-not-yet-finalized messages of
//!   this turn in birth order, each with the `afterId` it was born at, and
//! - [`ActiveStream::outstanding_prompts`], the tool prompts awaiting an answer.
//!
//! A reconnecting client sends `chat.subscribe` and gets both replayed, after
//! which live deltas resume on the new socket because the owning client id is
//! stable across reconnect. A core restart drops both, which is accepted.
//!
//! Both are `IndexMap`, not `HashMap`, and that is load-bearing rather than
//! tidy. The TypeScript replays `Map` values, which iterate in insertion order,
//! and the client positions each replayed message by the `afterId` of the one
//! before it. Replaying a reasoning bubble after the assistant bubble it
//! precedes puts the transcript back in the wrong order.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};

use indexmap::IndexMap;
use tokio_util::sync::CancellationToken;

use crate::contract::api::ServerToClientFrame;
use crate::sessions::doc::StoredMessage;
use crate::transport::status::CoreStatusService;

use super::deps::ModelIdle;

/// A message announced to the client but not yet persisted.
///
/// `message` is the live object the muxer fills in, shared rather than copied so
/// a catch-up snapshot reflects the stream so far. `after_id` is the position it
/// was born at, which a replay has to reproduce because the client has no other
/// way to place it.
#[derive(Clone)]
pub struct LiveMessage {
    pub message: Arc<Mutex<StoredMessage>>,
    pub after_id: Option<String>,
}

/// One in-flight turn.
pub struct ActiveStream {
    pub stream_id: String,
    pub session_id: String,
    /// The client that started this turn. Every control verb is checked against
    /// it, so one paired client can never interrupt, cancel, or answer a prompt
    /// belonging to another's turn.
    pub client_id: String,
    /// Cancellation for the whole turn. Replaces the TypeScript's
    /// `AbortController`: D3 has no signal on its request type and cancels by
    /// drop, so this token is what the hop loop, the tool dispatcher and the
    /// classifier all select against.
    pub cancel: CancellationToken,
    buffers: Mutex<Buffers>,
}

#[derive(Default)]
struct Buffers {
    live: IndexMap<String, LiveMessage>,
    prompts: IndexMap<String, ServerToClientFrame>,
}

impl ActiveStream {
    fn new(stream_id: String, session_id: String, client_id: String) -> Self {
        Self {
            stream_id,
            session_id,
            client_id,
            cancel: CancellationToken::new(),
            buffers: Mutex::new(Buffers::default()),
        }
    }

    /// Record a message as born, at the position it was born at.
    pub fn note_born(
        &self,
        id: String,
        message: Arc<Mutex<StoredMessage>>,
        after_id: Option<String>,
    ) {
        lock(&self.buffers)
            .live
            .insert(id, LiveMessage { message, after_id });
    }

    /// Drop a message from the catch-up buffer. Called on finalize: once it is
    /// persisted a reconnecting client reads it from the session instead.
    pub fn note_finalized(&self, id: &str) {
        lock(&self.buffers).live.shift_remove(id);
    }

    /// The catch-up buffer, in birth order.
    pub fn live_messages(&self) -> Vec<LiveMessage> {
        lock(&self.buffers).live.values().cloned().collect()
    }

    /// Retain a tool prompt so a reconnecting client can still answer it. At
    /// most one prompt per call is open at a time, so the call id is key enough.
    pub fn retain_prompt(&self, call_id: String, frame: ServerToClientFrame) {
        lock(&self.buffers).prompts.insert(call_id, frame);
    }

    /// Drop a retained prompt once it is answered or its call ends, so a later
    /// resubscribe does not re-show an already-handled prompt.
    pub fn clear_prompt(&self, call_id: &str) {
        lock(&self.buffers).prompts.shift_remove(call_id);
    }

    /// The open prompts, in the order they were opened.
    pub fn outstanding_prompts(&self) -> Vec<ServerToClientFrame> {
        lock(&self.buffers).prompts.values().cloned().collect()
    }
}

/// Every in-flight turn, keyed by stream id.
///
/// One instance per core. A second would give `chat.interrupt` a map that does
/// not contain the stream `chat.start` created, and every interrupt, askUser
/// answer and permission response would silently do nothing.
pub struct ActiveStreams {
    streams: Mutex<HashMap<String, Arc<ActiveStream>>>,
    idle: Arc<dyn ModelIdle>,
    status: Mutex<Option<CoreStatusService>>,
}

impl ActiveStreams {
    pub fn new(idle: Arc<dyn ModelIdle>) -> Self {
        Self {
            streams: Mutex::new(HashMap::new()),
            idle,
            status: Mutex::new(None),
        }
    }

    /// Publish the in-flight turn count into the core-status aggregate.
    ///
    /// A method on the shared handle rather than a constructor argument, because
    /// `boot` builds the status service and the chat handle in an order that
    /// puts one of them second either way. Unattached, the count simply
    /// publishes nowhere and every test runs without a status service.
    pub fn attach_status(&self, status: CoreStatusService) {
        *lock(&self.status) = Some(status);
    }

    /// Claim a slot for a new turn, or refuse because this client already has
    /// one on this session.
    ///
    /// The check and the insert are ONE critical section. Split across two,
    /// two `chat.start` frames arriving together could both see no active
    /// stream and both register, and the second would overwrite the first in
    /// the map: the first turn would then run to completion with no entry, so
    /// its interrupt would do nothing and its `note_active_streams` would
    /// undercount permanently. D-046 is the same shape, one layer down.
    pub fn claim(
        self: &Arc<Self>,
        stream_id: &str,
        session_id: &str,
        client_id: &str,
    ) -> Option<StreamGuard> {
        let stream = {
            let mut streams = lock(&self.streams);
            if streams
                .values()
                .any(|s| s.client_id == client_id && s.session_id == session_id)
            {
                return None;
            }
            let stream = Arc::new(ActiveStream::new(
                stream_id.to_string(),
                session_id.to_string(),
                client_id.to_string(),
            ));
            streams.insert(stream_id.to_string(), Arc::clone(&stream));
            stream
        };
        self.publish_count();
        // A turn is beginning, so cancel any pending idle-unload. `run` reloads
        // the model before scheduling if it was already unloaded.
        self.idle.note_activity();
        Some(StreamGuard {
            streams: Arc::clone(self),
            stream,
        })
    }

    /// The stream `stream_id` names, if this client owns it.
    pub fn owned(&self, stream_id: &str, client_id: &str) -> Option<Arc<ActiveStream>> {
        lock(&self.streams)
            .get(stream_id)
            .filter(|s| s.client_id == client_id)
            .cloned()
    }

    /// This client's in-flight turn on this session, if any.
    pub fn owned_on_session(&self, client_id: &str, session_id: &str) -> Option<Arc<ActiveStream>> {
        lock(&self.streams)
            .values()
            .find(|s| s.client_id == client_id && s.session_id == session_id)
            .cloned()
    }

    /// Session ids with a turn in flight: generating, or awaiting or executing a
    /// tool.
    ///
    /// A stream stays registered for the whole turn and is removed by
    /// [`StreamGuard`]'s drop, so a session NOT in this set is idle. Two
    /// consumers: the background queue's idle gate, and the storage view's
    /// refusal to delete an active session.
    pub fn active_session_ids(&self) -> HashSet<String> {
        lock(&self.streams)
            .values()
            .map(|s| s.session_id.clone())
            .collect()
    }

    pub fn len(&self) -> usize {
        lock(&self.streams).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Cancel every turn. The shutdown path, and what makes a Ctrl-C stop tool
    /// calls politely rather than reaping their workers underneath them.
    pub fn cancel_all(&self) {
        for stream in lock(&self.streams).values() {
            stream.cancel.cancel();
        }
    }

    fn publish_count(&self) {
        let n = self.len() as u32;
        if let Some(status) = lock(&self.status).as_ref() {
            status.note_active_streams(n);
        }
    }
}

/// Deregisters its stream when the turn ends, however it ends.
///
/// A guard rather than a trailing statement because the task can be cancelled
/// at shutdown, and a leaked entry is invisible: `active_session_ids` would
/// never go empty again, so the background queue's idle gate would never open
/// and every memory summary would silently stop running, with nothing in the
/// log to say so.
pub struct StreamGuard {
    streams: Arc<ActiveStreams>,
    stream: Arc<ActiveStream>,
}

impl StreamGuard {
    pub fn stream(&self) -> &Arc<ActiveStream> {
        &self.stream
    }
}

impl Drop for StreamGuard {
    fn drop(&mut self) {
        lock(&self.streams.streams).remove(&self.stream.stream_id);
        self.streams.publish_count();
        // The last turn ending is what arms the idle-unload timer. A no-op
        // unless this was the last one and the feature is on.
        self.streams.idle.on_turn_end(self.streams.len() as u32);
    }
}

/// A poisoned lock means another thread panicked while holding it. Every
/// critical section here is a map insert or a lookup with no invariant that
/// spans an unwind, so recovering the guard is correct and propagating the
/// panic would take the whole core down for one wedged turn.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}
