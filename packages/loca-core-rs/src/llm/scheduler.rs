//! Concurrency control in front of the provider.
//!
//! For the local model: at most `parallel_slots` completions in flight, with
//! queued work picked per-client round robin so one client cannot monopolise the
//! model, and a queue deeper than `parallel_slots * 4` refused outright. External
//! providers bypass all of it: they enforce their own rate limits, and queueing
//! locally would add latency while protecting nothing.
//!
//! The scheduler sits in FRONT of [`super::ChatProvider`] rather than inside any
//! implementation of it, which is what keeps it transport-independent: the same
//! queue governs today's `llama-server` over HTTP and Stage 3's in-process
//! engine, and there is exactly one place `llm.local.parallelSlots` means
//! something.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use futures_util::stream::{self, StreamExt as _};
use tokio::sync::oneshot;

use crate::error::{validation, AppError, ErrorCode, Result};
use crate::transport::status::CoreStatusService;

use super::provider::{ChatProvider, DeltaStream, ProviderError};
use super::types::LlmRequest;

/// Serial by default: one local completion at a time and the rest queue.
///
/// Matches `--parallel` on the `llama-server` command line, which runs a single
/// slot so each turn gets the whole context window. Two numbers that must agree;
/// raising one alone either starves the queue or oversubscribes the model.
const DEFAULT_PARALLEL_SLOTS: f64 = 1.0;

/// Beyond `slots * this` waiting, a new turn is refused rather than queued. A
/// queue longer than this is not a busy model, it is a client that will have
/// given up before its turn arrives.
const QUEUE_DEPTH_MULTIPLIER: u32 = 4;

const DEFAULT_CALL_TIMEOUT_MS: u64 = 60_000;
const WATCHDOG_GRACE_MS: u64 = 60_000;

/// Where a turn came from, and whether it queues.
pub struct ScheduleOptions {
    pub client_id: String,
    /// Whether the upstream is the core's own local model. **Not the same
    /// question as `LlmEndpointConfig::llama_body_extras`**: an external
    /// provider on a loopback gateway takes the llama sampler fields and does
    /// not queue. Read `is_local_provider` off the resolved endpoint.
    pub is_local: bool,
    /// How many concurrent local completions the model is configured for, from
    /// `llm.local.parallelSlots`.
    ///
    /// An `f64` because that is how it arrives: a JSON settings value read
    /// through `num_setting`. Taking a `u32` here would move the "1.5 slots is
    /// not a thing" rejection to whichever caller floors it first, and there is
    /// more than one caller.
    pub parallel_slots: Option<f64>,
}

/// Why the watchdog fired, for the handler that restarts the model.
#[derive(Clone, Debug)]
pub struct WatchdogInfo {
    pub client_id: String,
    pub elapsed_ms: u64,
}

pub type WatchdogHandler = Arc<dyn Fn(WatchdogInfo) + Send + Sync>;

/// Where the queue depth is published.
///
/// A seam rather than a direct dependency on the aggregator so a test can record
/// what was pushed. The production implementation is the core-status service,
/// which is the push direction D-014 settled: the subsystem tells the aggregator,
/// the aggregator does not poll the subsystem.
pub trait QueueObserver: Send + Sync {
    fn note_llm_queue(&self, active: u32, queued: u32);
}

impl QueueObserver for CoreStatusService {
    fn note_llm_queue(&self, active: u32, queued: u32) {
        CoreStatusService::note_llm_queue(self, active, queued);
    }
}

/// The queue. Cheap to clone; every clone shares one set of slots.
#[derive(Clone)]
pub struct LlmScheduler {
    inner: Arc<Inner>,
}

struct Inner {
    state: Mutex<State>,
    status: Mutex<Option<Arc<dyn QueueObserver>>>,
    watchdog: Mutex<Option<WatchdogHandler>>,
    call_timeout_ms: AtomicU64,
}

struct State {
    active: u32,
    parallel_slots: u32,
    /// One queue per client, plus the rotation over them. Kept as the
    /// TypeScript has it: a client's entry exists only while it has someone
    /// waiting, so the rotation never visits an empty queue.
    queues: HashMap<String, VecDeque<Waiter>>,
    client_order: Vec<String>,
    next_client_idx: usize,
    next_waiter_id: u64,
}

struct Waiter {
    id: u64,
    tx: oneshot::Sender<Permit>,
}

impl Default for LlmScheduler {
    fn default() -> Self {
        Self::new()
    }
}

impl LlmScheduler {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                state: Mutex::new(State {
                    active: 0,
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    parallel_slots: DEFAULT_PARALLEL_SLOTS as u32,
                    queues: HashMap::new(),
                    client_order: Vec::new(),
                    next_client_idx: 0,
                    next_waiter_id: 0,
                }),
                status: Mutex::new(None),
                watchdog: Mutex::new(None),
                call_timeout_ms: AtomicU64::new(DEFAULT_CALL_TIMEOUT_MS),
            }),
        }
    }

    /// Publish queue depth to the core-status aggregator.
    pub fn attach_status(&self, status: Arc<dyn QueueObserver>) {
        *lock(&self.inner.status) = Some(status);
    }

    /// What to do when a local slot stops making progress. Typically: restart
    /// the model. The scheduler only aborts the stream and frees the slot;
    /// restarting is the supervisor's job.
    pub fn set_watchdog_handler(&self, handler: Option<WatchdogHandler>) {
        *lock(&self.inner.watchdog) = handler;
    }

    pub fn set_parallel_slots(&self, n: f64) -> Result<()> {
        if !n.is_finite() || n.fract() != 0.0 || n < 1.0 {
            return Err(validation(format!("invalid parallelSlots: {n}")));
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let slots = n as u32;
        let (active, queued) = {
            let mut state = lock(&self.inner.state);
            state.parallel_slots = slots;
            (state.active, state.queue_len())
        };
        // Raising the ceiling can free a slot for someone already waiting.
        self.inner.dispatch_available();
        self.publish(active, queued);
        Ok(())
    }

    pub fn set_call_timeout_ms(&self, ms: f64) -> Result<()> {
        if !ms.is_finite() || ms < 1_000.0 {
            return Err(validation(format!("invalid callTimeoutMs: {ms}")));
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        self.inner
            .call_timeout_ms
            .store(ms as u64, Ordering::Relaxed);
        Ok(())
    }

    /// Run a completion, queueing it when the local model is busy.
    ///
    /// Awaiting this IS waiting in the queue. Dropping the future while it waits
    /// removes the entry, so an interrupted turn never wakes up later, takes a
    /// slot, and starts an upstream request it is about to abandon.
    ///
    /// The returned stream owns the slot: it is released when the stream ends or
    /// is dropped, whichever comes first.
    pub async fn schedule(
        &self,
        provider: &dyn ChatProvider,
        req: LlmRequest,
        opts: ScheduleOptions,
    ) -> Result<DeltaStream> {
        if !opts.is_local {
            // External providers enforce their own limits; a local queue in
            // front of them would add latency and protect nothing.
            return Ok(provider.stream(req));
        }
        if let Some(slots) = opts.parallel_slots {
            self.set_parallel_slots(slots)?;
        }
        let permit = self.acquire(&opts.client_id).await?;
        let timeout_ms = self.inner.call_timeout_ms.load(Ordering::Relaxed) + WATCHDOG_GRACE_MS;
        Ok(watched(
            provider.stream(req),
            permit,
            Arc::clone(&self.inner),
            opts.client_id,
            Duration::from_millis(timeout_ms),
        ))
    }

    /// Take a slot, waiting in this client's queue if they are all busy, or
    /// refusing outright once the queue is already too deep.
    ///
    /// The depth check lives HERE rather than in the caller so that testing it
    /// and acting on it happen under one lock. Split across two critical
    /// sections, two callers can both read `queued == max - 1`, both conclude
    /// there is room, and both enqueue, leaving `max + 1` waiting. That is
    /// benign (one extra waiter on a queue whose whole purpose is shedding
    /// load) and it is also free to prevent, which is the argument for
    /// preventing it: the TypeScript could not hit it at all, because its event
    /// loop made check-then-act atomic for free, so a reader comparing the two
    /// would have no reason to suspect the port had a race here.
    async fn acquire(&self, client_id: &str) -> Result<Permit> {
        let waiting = {
            let mut state = lock(&self.inner.state);
            let queued = state.queue_len();
            let max = state.parallel_slots * QUEUE_DEPTH_MULTIPLIER;
            if state.active >= state.parallel_slots && queued >= max {
                return Err(AppError::new(
                    ErrorCode::ServerBusy,
                    format!(
                        "the local model is busy ({queued} waiting, {} slots)",
                        state.parallel_slots
                    ),
                ));
            }
            if state.active < state.parallel_slots {
                state.active += 1;
                let depth = (state.active, state.queue_len());
                drop(state);
                self.publish(depth.0, depth.1);
                return Ok(Permit {
                    inner: Some(Arc::clone(&self.inner)),
                });
            }
            let (tx, rx) = oneshot::channel();
            let id = state.next_waiter_id;
            state.next_waiter_id += 1;
            state.push_waiter(client_id, Waiter { id, tx });
            let depth = (state.active, state.queue_len());
            drop(state);
            self.publish(depth.0, depth.1);
            (rx, id)
        };
        let (rx, id) = waiting;
        // Removes this entry if the future is dropped while it is still queued.
        // If the permit was already sent, the entry is gone and `rx`'s own drop
        // releases the slot instead, which is why the permit travels through the
        // channel as a value rather than as a wake-up.
        let _guard = QueuedGuard {
            inner: Arc::clone(&self.inner),
            client_id: client_id.to_string(),
            id,
        };
        Ok(match rx.await {
            Ok(permit) => permit,
            // Only reachable if the sender were dropped without sending, which
            // nothing does. Taking the slot uncounted would be worse than
            // taking it counted.
            Err(_) => Permit {
                inner: Some(Arc::clone(&self.inner)),
            },
        })
    }

    fn publish(&self, active: u32, queued: u32) {
        let status = lock(&self.inner.status).clone();
        if let Some(status) = status {
            status.note_llm_queue(active, queued);
        }
    }

    /// In-flight and waiting counts, for tests and diagnostics.
    pub fn depth(&self) -> (u32, u32) {
        let state = lock(&self.inner.state);
        (state.active, state.queue_len())
    }
}

impl State {
    fn queue_len(&self) -> u32 {
        self.queues
            .values()
            .map(|q| u32::try_from(q.len()).unwrap_or(u32::MAX))
            .sum()
    }

    fn push_waiter(&mut self, client_id: &str, waiter: Waiter) {
        match self.queues.get_mut(client_id) {
            Some(queue) => queue.push_back(waiter),
            None => {
                let mut queue = VecDeque::new();
                queue.push_back(waiter);
                self.queues.insert(client_id.to_string(), queue);
                self.client_order.push(client_id.to_string());
            }
        }
    }

    /// The next waiter in rotation, or `None` when nobody is queued.
    ///
    /// The index bookkeeping is the whole of the fairness guarantee: after
    /// serving a client the rotation point moves past it, and when that client's
    /// queue empties and its entry is removed, the point stays where the removed
    /// entry was so the next client is not skipped.
    fn take_next(&mut self) -> Option<Waiter> {
        if self.client_order.is_empty() {
            return None;
        }
        let len = self.client_order.len();
        let start = self.next_client_idx % len;
        for offset in 0..len {
            let idx = (start + offset) % len;
            let client_id = self.client_order[idx].clone();
            let Some(queue) = self.queues.get_mut(&client_id) else {
                continue;
            };
            let Some(waiter) = queue.pop_front() else {
                continue;
            };
            if queue.is_empty() {
                self.queues.remove(&client_id);
                self.client_order.remove(idx);
                self.next_client_idx = idx;
            } else {
                self.next_client_idx = idx + 1;
            }
            return Some(waiter);
        }
        None
    }

    /// Drop a still-queued waiter, pruning the client's entry exactly as
    /// [`Self::take_next`] does when a queue empties.
    fn remove_waiter(&mut self, client_id: &str, id: u64) {
        let Some(queue) = self.queues.get_mut(client_id) else {
            return;
        };
        if let Some(pos) = queue.iter().position(|w| w.id == id) {
            queue.remove(pos);
        }
        if queue.is_empty() {
            self.queues.remove(client_id);
            if let Some(idx) = self.client_order.iter().position(|c| c == client_id) {
                self.client_order.remove(idx);
                if self.next_client_idx > idx {
                    self.next_client_idx -= 1;
                }
            }
        }
    }
}

impl Inner {
    /// Hand a free slot to the next waiter, if there is both.
    fn dispatch_available(self: &Arc<Self>) {
        loop {
            let waiter = {
                let mut state = lock(&self.state);
                if state.active >= state.parallel_slots {
                    return;
                }
                match state.take_next() {
                    Some(waiter) => {
                        state.active += 1;
                        waiter
                    }
                    None => return,
                }
            };
            let permit = Permit {
                inner: Some(Arc::clone(self)),
            };
            // A receiver that has gone away leaves the permit here. Dropping it
            // outside the lock releases the slot and loops, rather than
            // re-entering the lock from inside `Drop`.
            match waiter.tx.send(permit) {
                Ok(()) => {
                    self.publish_depth();
                    return;
                }
                Err(mut permit) => {
                    // Consume it here rather than through `Drop`, so the loop
                    // rather than recursion handles a run of dead waiters.
                    permit.inner = None;
                    let mut state = lock(&self.state);
                    state.active -= 1;
                }
            }
        }
    }

    fn release(self: &Arc<Self>) {
        {
            let mut state = lock(&self.state);
            state.active = state.active.saturating_sub(1);
        }
        self.dispatch_available();
        self.publish_depth();
    }

    fn publish_depth(self: &Arc<Self>) {
        let depth = {
            let state = lock(&self.state);
            (state.active, state.queue_len())
        };
        let status = lock(&self.status).clone();
        if let Some(status) = status {
            status.note_llm_queue(depth.0, depth.1);
        }
    }
}

/// A held slot. Releasing is `Drop`, so every exit path frees it: the stream
/// ending, the stream being dropped mid-completion, a panic, or the watchdog.
pub struct Permit {
    inner: Option<Arc<Inner>>,
}

impl Drop for Permit {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.take() {
            inner.release();
        }
    }
}

/// Removes a waiter that was dropped while still queued.
struct QueuedGuard {
    inner: Arc<Inner>,
    client_id: String,
    id: u64,
}

impl Drop for QueuedGuard {
    fn drop(&mut self) {
        let (active, queued) = {
            let mut state = lock(&self.inner.state);
            state.remove_waiter(&self.client_id, self.id);
            (state.active, state.queue_len())
        };
        let status = lock(&self.inner.status).clone();
        if let Some(status) = status {
            status.note_llm_queue(active, queued);
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // A poisoned lock means some other thread panicked mid-update. The queue is
    // still structurally sound and refusing every completion afterwards would be
    // a worse failure than continuing.
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// Wrap a completion in the stall watchdog, holding the slot for its lifetime.
fn watched(
    inner_stream: DeltaStream,
    permit: Permit,
    scheduler: Arc<Inner>,
    client_id: String,
    timeout: Duration,
) -> DeltaStream {
    struct Watched {
        stream: DeltaStream,
        // Held, not read: dropping this is what frees the slot.
        _permit: Permit,
        scheduler: Arc<Inner>,
        client_id: String,
        timeout: Duration,
        /// A `tokio` instant, not a `std` one, so it follows the same clock the
        /// timeout does. With `std::time::Instant` the reported elapsed time is
        /// measured against a different clock from the deadline, and on a paused
        /// runtime the two disagree entirely, which makes a watchdog test unable
        /// to see a watchdog that stopped working.
        started_at: tokio::time::Instant,
    }

    stream::unfold(
        Some(Watched {
            stream: inner_stream,
            _permit: permit,
            scheduler,
            client_id,
            timeout,
            started_at: tokio::time::Instant::now(),
        }),
        |state| async move {
            let mut watched = state?;
            // The bound is on SILENCE, not on total duration: the timer restarts
            // with every delta, so a slow but progressing completion (a big
            // thinking model on CPU) is never cut off, and only a server that
            // never produced a first token, or stopped producing, trips it.
            // Bounding the total instead passes the same "a wedged stream
            // errors" test and breaks every long generation.
            match tokio::time::timeout(watched.timeout, watched.stream.next()).await {
                Ok(Some(item)) => Some((item, Some(watched))),
                Ok(None) => None,
                Err(_) => {
                    let elapsed_ms =
                        u64::try_from(watched.started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
                    let stalled_ms = watched.timeout.as_millis();
                    tracing::error!(
                        target: "loca::llm-scheduler",
                        "watchdog: local slot made no progress for {stalled_ms}ms \
                         (client {}); aborting upstream",
                        watched.client_id,
                    );
                    let handler = lock(&watched.scheduler.watchdog).clone();
                    if let Some(handler) = handler {
                        handler(WatchdogInfo {
                            client_id: watched.client_id.clone(),
                            elapsed_ms,
                        });
                    }
                    // Ending here drops the inner stream, which tears the
                    // upstream request down. That is the abort.
                    Some((
                        Err(ProviderError::aborted(format!(
                            "llm scheduler watchdog stall after {stalled_ms}ms"
                        ))),
                        None,
                    ))
                }
            }
        },
    )
    .boxed()
}

#[cfg(test)]
#[allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::llm::provider::ProviderErrorKind;
    use crate::llm::testing::{PendingProvider, ScriptedProvider};
    use crate::llm::types::{ChatMessage, LlmEndpointConfig};
    use std::sync::atomic::AtomicU32;

    fn request() -> LlmRequest {
        LlmRequest::new(LlmEndpointConfig::default(), vec![ChatMessage::user("hi")])
    }

    fn local(client_id: &str) -> ScheduleOptions {
        ScheduleOptions {
            client_id: client_id.to_string(),
            is_local: true,
            parallel_slots: None,
        }
    }

    async fn drain(stream: DeltaStream) -> String {
        stream
            .filter_map(|item| async move { item.ok().and_then(|d| d.content_delta) })
            .collect::<Vec<_>>()
            .await
            .join("")
    }

    #[tokio::test]
    async fn parallel_slots_rejects_zero_negative_and_fractional_values() {
        let scheduler = LlmScheduler::new();
        for n in [0.0, -1.0, 1.5, f64::NAN, f64::INFINITY] {
            let err = scheduler.set_parallel_slots(n).unwrap_err();
            assert_eq!(err.code, ErrorCode::ValidationError, "{n} must be rejected");
        }
        scheduler.set_parallel_slots(8.0).unwrap();
    }

    #[tokio::test]
    async fn call_timeout_rejects_anything_under_a_second() {
        let scheduler = LlmScheduler::new();
        for ms in [0.0, 500.0, 999.0, f64::INFINITY] {
            assert!(scheduler.set_call_timeout_ms(ms).is_err(), "{ms}");
        }
        scheduler.set_call_timeout_ms(10_000.0).unwrap();
    }

    #[tokio::test]
    async fn an_external_provider_bypasses_the_semaphore_entirely() {
        let scheduler = LlmScheduler::new();
        scheduler.set_parallel_slots(1.0).unwrap();
        let provider = ScriptedProvider::text("Hello world");
        // Five at once against one slot. If the semaphore applied, four of them
        // would be waiting on the first and this would not finish.
        let mut streams = Vec::new();
        for _ in 0..5 {
            streams.push(
                scheduler
                    .schedule(
                        &provider,
                        request(),
                        ScheduleOptions {
                            client_id: "c".into(),
                            is_local: false,
                            parallel_slots: None,
                        },
                    )
                    .await
                    .unwrap(),
            );
        }
        for stream in streams {
            assert_eq!(drain(stream).await, "Hello world");
        }
        assert_eq!(scheduler.depth(), (0, 0), "nothing was ever counted");
    }

    #[tokio::test]
    async fn a_full_queue_is_refused_rather_than_joined() {
        let scheduler = LlmScheduler::new();
        scheduler.set_parallel_slots(1.0).unwrap();
        let provider = PendingProvider::new();
        // One in flight, then four waiting fills the queue at slots * 4.
        let _held = scheduler
            .schedule(&provider, request(), local("c"))
            .await
            .unwrap();
        let mut queued = Vec::new();
        for _ in 0..4 {
            let scheduler = scheduler.clone();
            let provider = provider.clone();
            queued.push(tokio::spawn(async move {
                scheduler.schedule(&provider, request(), local("c")).await
            }));
        }
        wait_for(&scheduler, |(active, waiting)| active == 1 && waiting == 4).await;

        // Bounded: without the depth guard this would queue rather than
        // return, and the failure would present as a hung test.
        let refused = tokio::time::timeout(
            Duration::from_secs(5),
            scheduler.schedule(&provider, request(), local("c")),
        )
        .await
        .expect("a full queue must be refused rather than joined");
        let Err(err) = refused else {
            panic!("a full queue must be refused, not joined");
        };
        assert_eq!(err.code, ErrorCode::ServerBusy);
        assert_eq!(err.status(), 503);
        assert!(err.message.contains("4 waiting"), "{}", err.message);
        for task in queued {
            task.abort();
        }
    }

    #[tokio::test]
    async fn queued_turns_are_served_round_robin_rather_than_first_come_first_served() {
        let scheduler = LlmScheduler::new();
        scheduler.set_parallel_slots(1.0).unwrap();
        let provider = PendingProvider::new();
        let held = scheduler
            .schedule(&provider, request(), local("a"))
            .await
            .unwrap();

        // a queues twice before b and c queue once each. First-come-first-served
        // would serve a, a, b, c; the rotation must serve a, b, c, a.
        let served = Arc::new(Mutex::new(Vec::new()));
        let mut tasks = Vec::new();
        for client in ["a", "a", "b", "c"] {
            let queued_scheduler = scheduler.clone();
            let provider = provider.clone();
            let served = Arc::clone(&served);
            tasks.push(tokio::spawn(async move {
                let stream = queued_scheduler
                    .schedule(&provider, request(), local(client))
                    .await
                    .unwrap();
                lock(&served).push(client.to_string());
                // Hold the slot until the test drops it, so the order of
                // acquisition is what is recorded.
                tokio::time::sleep(Duration::from_millis(10)).await;
                drop(stream);
            }));
            // Deterministic enqueue order: each spawn reaches the queue before
            // the next is created.
            wait_for_queue(&scheduler, tasks.len() as u32).await;
        }
        drop(held);
        for task in tasks {
            task.await.unwrap();
        }
        assert_eq!(
            lock(&served).clone(),
            vec!["a".to_string(), "b".into(), "c".into(), "a".into()],
        );
    }

    #[tokio::test]
    async fn a_turn_interrupted_while_queued_never_takes_a_slot() {
        let scheduler = LlmScheduler::new();
        scheduler.set_parallel_slots(1.0).unwrap();
        let provider = PendingProvider::new();
        let held = scheduler
            .schedule(&provider, request(), local("a"))
            .await
            .unwrap();

        // b queues, then gives up. c queues behind it.
        let abandoned = {
            let scheduler = scheduler.clone();
            let provider = provider.clone();
            tokio::spawn(async move { scheduler.schedule(&provider, request(), local("b")).await })
        };
        wait_for_queue(&scheduler, 1).await;
        let (tx, rx) = oneshot::channel();
        let served = {
            let scheduler = scheduler.clone();
            let provider = provider.clone();
            tokio::spawn(async move {
                let stream = scheduler
                    .schedule(&provider, request(), local("c"))
                    .await
                    .unwrap();
                let _ = tx.send(());
                drop(stream);
            })
        };
        wait_for_queue(&scheduler, 2).await;
        abandoned.abort();
        let _ = abandoned.await;
        wait_for_queue(&scheduler, 1).await;

        drop(held);
        // c is served: the abandoned turn did not consume the freed slot.
        tokio::time::timeout(Duration::from_secs(5), rx)
            .await
            .expect("c never got the slot the abandoned turn was holding open")
            .unwrap();
        served.await.unwrap();
        wait_for(&scheduler, |depth| depth == (0, 0)).await;
    }

    #[tokio::test]
    async fn interrupting_a_queued_turn_does_not_rewind_the_rotation() {
        // The other half of the interrupt path. `a_turn_interrupted_while_queued`
        // proves the waiter is removed; this proves the rotation index is fixed
        // up when removing it shifts the client order underneath. Without the
        // fix-up every client after the removed one is skipped for a whole
        // rotation, which is invisible to every other test here because they all
        // remove from the rotation point rather than from behind it.
        let scheduler = LlmScheduler::new();
        // Two slots, both held throughout, so exactly one slot frees at a time
        // and the queue has room for six waiters (the guard refuses at slots * 4).
        scheduler.set_parallel_slots(2.0).unwrap();
        let provider = PendingProvider::new();
        let pinned = scheduler
            .schedule(&provider, request(), local("h"))
            .await
            .unwrap();
        let mut holding = Some(
            scheduler
                .schedule(&provider, request(), local("h"))
                .await
                .unwrap(),
        );

        // Two turns per client, so serving a client's first turn leaves its
        // queue non-empty and the rotation index walks forward instead of being
        // reset by a removal. Enqueue order fixes the client order to [a, b, c].
        let (served_tx, mut served_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut tasks = Vec::new();
        for client in ["a", "b", "c", "c", "a", "b"] {
            let queued_scheduler = scheduler.clone();
            let provider = provider.clone();
            let served_tx = served_tx.clone();
            tasks.push(tokio::spawn(async move {
                let stream = queued_scheduler
                    .schedule(&provider, request(), local(client))
                    .await
                    .unwrap();
                // Handing the stream back rather than dropping it lets the test
                // hold each slot and step the rotation one client at a time.
                let _ = served_tx.send((client, stream));
            }));
            wait_for_queue(&scheduler, u32::try_from(tasks.len()).unwrap()).await;
        }
        drop(served_tx);

        // Walk the rotation across all three clients. Serving c last leaves the
        // rotation point PAST c, which is the state the fix-up is about.
        for expected in ["a", "b", "c"] {
            drop(holding.take());
            let (client, stream) = next_served(&mut served_rx).await;
            assert_eq!(client, expected, "rotation order");
            holding = Some(stream);
        }

        // c's second turn is interrupted while queued, so c leaves the rotation
        // from BEHIND the rotation point.
        tasks[3].abort();
        let _ = std::mem::replace(&mut tasks[3], tokio::spawn(async {})).await;
        wait_for_queue(&scheduler, 2).await;

        drop(holding.take());
        let (client, stream) = next_served(&mut served_rx).await;
        // a is next: the rotation had wrapped past c, and pruning c must not
        // move the point off the client it was pointing at. Leaving the index
        // alone makes it land on b, which was served most recently.
        assert_eq!(
            client, "a",
            "pruning an interrupted turn skipped a full rotation",
        );

        drop(stream);
        drop(next_served(&mut served_rx).await);
        drop(pinned);
        for task in tasks {
            task.abort();
        }
    }

    #[tokio::test]
    async fn a_slot_is_freed_when_the_stream_is_dropped_mid_completion() {
        let scheduler = LlmScheduler::new();
        let provider = PendingProvider::new();
        let stream = scheduler
            .schedule(&provider, request(), local("a"))
            .await
            .unwrap();
        assert_eq!(scheduler.depth(), (1, 0));
        drop(stream);
        assert_eq!(scheduler.depth(), (0, 0));
    }

    #[tokio::test]
    async fn the_queue_depth_is_published_on_every_transition() {
        #[derive(Default)]
        struct Recorder {
            seen: Mutex<Vec<(u32, u32)>>,
        }
        impl QueueObserver for Recorder {
            fn note_llm_queue(&self, active: u32, queued: u32) {
                lock(&self.seen).push((active, queued));
            }
        }
        let recorder = Arc::new(Recorder::default());
        let scheduler = LlmScheduler::new();
        scheduler.attach_status(Arc::clone(&recorder) as Arc<dyn QueueObserver>);
        let provider = PendingProvider::new();

        let first = scheduler
            .schedule(&provider, request(), local("a"))
            .await
            .unwrap();
        let queued = {
            let scheduler = scheduler.clone();
            let provider = provider.clone();
            tokio::spawn(async move { scheduler.schedule(&provider, request(), local("b")).await })
        };
        wait_for_queue(&scheduler, 1).await;
        drop(first);
        let second = queued.await.unwrap().unwrap();
        drop(second);
        wait_for(&scheduler, |depth| depth == (0, 0)).await;

        let seen = lock(&recorder.seen).clone();
        assert!(seen.contains(&(1, 0)), "in flight: {seen:?}");
        assert!(seen.contains(&(1, 1)), "one waiting: {seen:?}");
        assert!(seen.last() == Some(&(0, 0)), "settles at idle: {seen:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn a_wedged_completion_is_aborted_and_the_handler_is_told() {
        let scheduler = LlmScheduler::new();
        let fired = Arc::new(AtomicU32::new(0));
        let counter = Arc::clone(&fired);
        scheduler.set_watchdog_handler(Some(Arc::new(move |info: WatchdogInfo| {
            assert_eq!(info.client_id, "a");
            counter.fetch_add(1, Ordering::SeqCst);
        })));
        let provider = PendingProvider::new();
        let mut stream = scheduler
            .schedule(&provider, request(), local("a"))
            .await
            .unwrap();

        let err = stream.next().await.unwrap().unwrap_err();
        assert_eq!(err.kind, ProviderErrorKind::Aborted);
        assert!(err.message.contains("120000ms"), "{}", err.message);
        assert_eq!(fired.load(Ordering::SeqCst), 1);
        assert!(stream.next().await.is_none(), "the stream is over");
        drop(stream);
        assert_eq!(scheduler.depth(), (0, 0), "the wedged slot was freed");
    }

    #[tokio::test(start_paused = true)]
    async fn a_slow_but_progressing_completion_is_never_aborted() {
        let scheduler = LlmScheduler::new();
        // Six deltas, each arriving 90s after the last: 9 minutes in total,
        // which is far past the 120s bound but never 120s of silence. Bounding
        // total duration rather than silence fails here and passes every other
        // watchdog test.
        let provider = ScriptedProvider::text("Hello world").with_gap(Duration::from_secs(90));
        let stream = scheduler
            .schedule(&provider, request(), local("a"))
            .await
            .unwrap();
        assert_eq!(drain(stream).await, "Hello world");
    }

    /// Poll until the scheduler reaches a state, so a test never sleeps a fixed
    /// guess. Bounded so a wrong expectation fails rather than hangs.
    async fn wait_for(scheduler: &LlmScheduler, predicate: impl Fn((u32, u32)) -> bool) {
        for _ in 0..2_000 {
            if predicate(scheduler.depth()) {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!(
            "scheduler never reached the expected depth (now {:?})",
            scheduler.depth()
        );
    }

    async fn wait_for_queue(scheduler: &LlmScheduler, queued: u32) {
        wait_for(scheduler, |(_, waiting)| waiting == queued).await;
    }

    /// The next turn to be handed a slot, with the slot still held. Bounded, so
    /// a rotation that serves nobody fails rather than hangs.
    async fn next_served(
        rx: &mut tokio::sync::mpsc::UnboundedReceiver<(&'static str, DeltaStream)>,
    ) -> (&'static str, DeltaStream) {
        tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("the rotation stalled: a freed slot was handed to nobody")
            .expect("the rotation ended before every queued turn was served")
    }
}
