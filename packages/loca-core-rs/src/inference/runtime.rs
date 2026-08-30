//! One blocking turn onto a bounded delta stream. Decode is blocking CPU work
//! on a `Send`-only `Context`, so it runs on a blocking thread that cannot be
//! cancelled by the runtime; cancellation is cooperative via the channel, and
//! the channel is bounded so a stalled consumer applies backpressure instead of
//! buffering a whole generation.

use std::sync::Arc;

use futures_util::stream::StreamExt as _;
use tokio::sync::mpsc;

use crate::llm::{DeltaStream, LlmDelta, LlmRequest, ProviderError};

use super::engine::{ContextState, LocalEngine};
use super::generate::{self, DeltaSink};

/// Small on purpose: a slow consumer should throttle generation.
const DELTA_CHANNEL_CAPACITY: usize = 8;

/// Run one turn and stream its deltas.
pub fn stream_turn(engine: Arc<LocalEngine>, req: LlmRequest) -> DeltaStream {
    let (tx, rx) = mpsc::channel::<Result<LlmDelta, ProviderError>>(DELTA_CHANNEL_CAPACITY);

    tokio::spawn(async move {
        let Some(ready) = engine.ready().await else {
            let _ = tx
                .send(Err(ProviderError::transport(
                    "no local model is loaded. Check the model path in settings, \
                     and that the file has finished downloading.",
                )))
                .await;
            return;
        };

        // Owned so it can cross to the blocking thread; also the serialisation
        // point for concurrent local turns.
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
                // After every delta already produced, never instead of them.
                let _ = sink.tx.blocking_send(Err(err));
            }
        })
        .await;

        if let Err(err) = joined {
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

/// The context this turn runs in; one built for a different `LoadSpec` is
/// dropped since its KV cache belongs to another model or window.
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
        // Blocking is the backpressure.
        self.tx.blocking_send(Ok(delta)).is_ok()
    }

    fn wanted(&self) -> bool {
        // A dropped receiver is the only cancellation signal.
        !self.tx.is_closed()
    }
}
