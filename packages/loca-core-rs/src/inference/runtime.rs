//! Getting one blocking turn onto a stream, and off it again when the consumer
//! leaves.
//!
//! Three constraints decide the shape here and none of them is negotiable:
//!
//! - llama.cpp decode is blocking CPU work, so it cannot run on the async
//!   runtime's worker threads without starving every other task in the core.
//! - [`shim::Context`] is `Send` and deliberately not `Sync`, so exactly one
//!   turn may touch it and the guard has to travel to the blocking thread.
//! - `spawn_blocking` tasks are NOT cancellable by the runtime: they run to
//!   completion whatever happens to the future awaiting them. So cancellation
//!   has to be cooperative, and the loop has to notice.
//!
//! The channel is what makes all three work at once. It is BOUNDED (D-096): an
//! unbounded one would let a stalled WebSocket consumer buffer an entire
//! generation in memory, which is the failure mode the frame bus was swept for.

use std::sync::Arc;

use futures_util::stream::StreamExt as _;
use tokio::sync::mpsc;

use crate::llm::{DeltaStream, LlmDelta, LlmRequest, ProviderError};

use super::engine::{ContextState, LocalEngine};
use super::generate::{self, DeltaSink};

/// How many deltas may be in flight before the token loop waits.
///
/// Small on purpose. A slow consumer should apply backpressure to generation
/// rather than let it run ahead into memory, and a few tokens of slack is enough
/// to keep the loop from stalling on ordinary scheduling jitter.
const DELTA_CHANNEL_CAPACITY: usize = 8;

/// Run one turn and stream its deltas.
pub fn stream_turn(engine: Arc<LocalEngine>, req: LlmRequest) -> DeltaStream {
    let (tx, rx) = mpsc::channel::<Result<LlmDelta, ProviderError>>(DELTA_CHANNEL_CAPACITY);

    tokio::spawn(async move {
        let Some(ready) = engine.ready().await else {
            // The turn arrived with no model resident. `ensure_loaded` has
            // already decided this is not a failure (the file is missing, or the
            // provider is external), so the diagnosis belongs here, where there
            // is a request to attach it to.
            let _ = tx
                .send(Err(ProviderError::transport(
                    "no local model is loaded. Check the model path in settings, \
                     and that the file has finished downloading.",
                )))
                .await;
            return;
        };

        // Owned, so it can cross onto the blocking thread and be held there for
        // the whole generation. This is also the serialisation point: a second
        // local turn waits here, which is what `--parallel 1` bought and what
        // D3's scheduler already assumes.
        let guard = engine.context().lock_owned().await;
        let sender = tx.clone();

        let joined = tokio::task::spawn_blocking(move || {
            let mut guard = guard;
            let state = match context_for(&mut guard, &ready) {
                Ok(state) => state,
                Err(err) => {
                    let _ = sender.blocking_send(Err(err));
                    return;
                }
            };
            let mut sink = ChannelSink { tx: sender };
            if let Err(err) = generate::run_turn(&ready, state, &req, &mut sink) {
                // AFTER every delta the turn already produced, never instead of
                // them: the channel makes that ordering structural rather than a
                // code path that can be inverted. Content the model really
                // generated is never discarded by a later failure.
                let _ = sink.tx.blocking_send(Err(err));
            }
        })
        .await;

        if let Err(err) = joined {
            // The blocking thread panicked or was aborted at shutdown. Saying so
            // is better than a stream that simply ends, which a consumer reads
            // as a complete but empty answer.
            let _ = tx
                .send(Err(ProviderError::transport(format!(
                    "the local inference thread stopped: {err}"
                ))))
                .await;
        }
    });

    futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    })
    .boxed()
}

/// The context this turn runs in, created or replaced as needed.
///
/// A context built for a different [`crate::inference::options::LoadSpec`] is
/// dropped rather than reused: it holds the KV cache of a model or a window that
/// is no longer the one being asked for.
fn context_for<'a>(
    guard: &'a mut Option<ContextState>,
    ready: &super::engine::Ready,
) -> Result<&'a mut ContextState, ProviderError> {
    let stale = guard.as_ref().is_some_and(|state| state.spec != ready.spec);
    if stale {
        *guard = None;
    }
    if guard.is_none() {
        *guard = Some(
            ContextState::new(Arc::clone(&ready.model), &ready.spec)
                .map_err(|err| ProviderError::transport(err.message))?,
        );
    }
    guard
        .as_mut()
        .ok_or_else(|| ProviderError::transport("the inference context vanished"))
}

/// The sink the token loop writes through.
struct ChannelSink {
    tx: mpsc::Sender<Result<LlmDelta, ProviderError>>,
}

impl DeltaSink for ChannelSink {
    fn emit(&mut self, delta: LlmDelta) -> bool {
        // Blocking, and that is the backpressure: a consumer that has stopped
        // reading stops generation instead of being buffered ahead of.
        self.tx.blocking_send(Ok(delta)).is_ok()
    }

    fn wanted(&self) -> bool {
        // Dropping the stream drops the receiver, which closes the channel. That
        // is the only cancellation signal there is, by design.
        !self.tx.is_closed()
    }
}
