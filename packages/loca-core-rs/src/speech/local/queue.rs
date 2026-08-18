//! The fair-share queue in front of the single speech engine.
//!
//! One engine, one slot: speech-to-text and text-to-speech share the model's
//! threads, so a second concurrent call would thrash the same ONNX session
//! rather than doubling throughput. Everything past that slot queues, and the
//! queue is per client so one client streaming dictation cannot starve another.
//!
//! # The drop obligation is the whole reason this is not a bare semaphore
//!
//! axum drops the handler future when a client disconnects, and that is the only
//! signal an abandoned request gives. Two states have to survive it:
//!
//! - A caller **holding** the slot must release it, or the engine is idle
//!   forever with callers queued behind it. That is [`Permit`]'s `Drop`.
//! - A caller **waiting** for the slot must leave the queue, or the round robin
//!   eventually hands the slot to a receiver that has gone away, and the engine
//!   sits idle for one full rotation with work waiting. That is [`Ticket`]'s
//!   `Drop`, and it is the half that is easy to miss: the bug it prevents looks
//!   like an intermittent stall rather than a leak.
//!
//! Both are RAII rather than an explicit `release()` call, because an explicit
//! call is exactly what a dropped future does not make.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use tokio::sync::oneshot;

use crate::error::{AppError, ErrorCode, Result};

/// Concurrent local speech calls. One, and the reason is the shared engine:
/// raising it would let a transcription and a synthesis contend for the same
/// ONNX session's threads.
pub const PARALLEL_SLOTS: usize = 1;

/// Queue depth, as a multiple of [`PARALLEL_SLOTS`]. Past this a call is refused
/// immediately rather than queued, so a saturated engine answers instead of
/// growing an unbounded backlog of requests whose callers have given up.
pub const QUEUE_DEPTH_MULTIPLIER: usize = 4;

/// Told whenever the queue moves, so the aggregate core status can render it.
/// Implemented over `CoreStatusService::note_speech_queue`; a queue with no
/// observer is still a correct queue, which is what keeps the tests free of the
/// whole status subsystem.
pub trait QueueObserver: Send + Sync {
    fn note(&self, active: u32, queued: u32);
}

/// A no-op observer, for a queue nobody is watching.
pub struct Unobserved;

impl QueueObserver for Unobserved {
    fn note(&self, _active: u32, _queued: u32) {}
}

pub struct Queue {
    state: Mutex<State>,
    observer: Arc<dyn QueueObserver>,
    slots: usize,
}

struct State {
    active: usize,
    /// Waiters by client, in insertion order. An `IndexMap` rather than a
    /// `HashMap` because the rotation is positional: a hash map would reorder
    /// clients on rehash and the round robin would stop being one.
    waiting: IndexMap<String, VecDeque<Waiter>>,
    /// Which client the next dispatch starts from.
    next: usize,
    /// Hands out [`Waiter::id`]. Monotonic for the life of the queue, so an id
    /// is never reused and a withdrawal cannot hit a later waiter.
    next_id: u64,
}

/// One caller waiting for the slot.
///
/// **The `id` is what makes a withdrawal precise, and it is load-bearing.**
/// Without it the only way to remove a waiter is positionally, and a client with
/// TWO calls in flight (the client synthesising two chat segments back to back,
/// or `INTERNAL_CLIENT_ID` running two broker ops) would have the wrong one
/// removed: dropping the second would close the FIRST one's channel and answer a
/// live, still-connected caller with `server_unavailable`.
struct Waiter {
    id: u64,
    tx: oneshot::Sender<()>,
}

impl Queue {
    pub fn new(observer: Arc<dyn QueueObserver>) -> Self {
        Self {
            state: Mutex::new(State {
                active: 0,
                waiting: IndexMap::new(),
                next: 0,
                next_id: 0,
            }),
            observer,
            slots: PARALLEL_SLOTS,
        }
    }

    fn max_depth(&self) -> usize {
        self.slots * QUEUE_DEPTH_MULTIPLIER
    }

    /// Wait for the engine slot on behalf of `client_id`.
    ///
    /// Refuses with `server_busy` when the queue is already at its bound, and
    /// that check happens BEFORE the caller is enqueued: a request that is going
    /// to be refused should be refused now, while the client is still listening,
    /// rather than after a wait.
    pub async fn acquire(self: &Arc<Self>, client_id: &str) -> Result<Permit> {
        let ticket = {
            let mut state = lock(&self.state);
            let queued = depth(&state);
            if queued >= self.max_depth() {
                return Err(AppError::new(
                    ErrorCode::ServerBusy,
                    format!("speech queue full ({queued} waiting, {} slots)", self.slots),
                ));
            }
            if state.active < self.slots {
                state.active += 1;
                self.observe(&state);
                return Ok(Permit {
                    queue: Arc::clone(self),
                });
            }
            let (tx, rx) = oneshot::channel();
            let id = state.next_id;
            state.next_id += 1;
            state
                .waiting
                .entry(client_id.to_string())
                .or_default()
                .push_back(Waiter { id, tx });
            self.observe(&state);
            Ticket {
                queue: Arc::clone(self),
                client_id: client_id.to_string(),
                id,
                rx,
                admitted: false,
            }
        };
        ticket.wait().await
    }

    /// Dispatch to the next client in rotation, if the slot is free and anyone
    /// is waiting.
    ///
    /// The rotation point is kept stable across the removal of an emptied
    /// client, which is the part that is easy to get subtly wrong: advancing
    /// past the removed index would skip whichever client shifted into it, and
    /// with three clients that reads as "mostly fair" rather than as a bug.
    fn dispatch(&self, state: &mut State) {
        // **The outer loop exists because the map can SHRINK inside the inner
        // one**, and, like the guard that drives it, it is unreachable while
        // `abandon` removes by id under this lock. It is here for the same
        // reason: the state it handles is one `abandon` bug away, and that bug
        // has existed once. Admitting a waiter that turns out to be gone removes its client
        // from `waiting`, which invalidates the `len` the inner scan was written
        // against: continuing with a stale `len` walks off the end, finds
        // `None`, and falls out of the loop having admitted nobody, leaving the
        // engine idle with callers still queued. Re-reading `len` per pass is
        // what keeps "the slot is free" and "somebody is waiting" from being
        // true at the same time.
        'rescan: while state.active < self.slots && !state.waiting.is_empty() {
            let len = state.waiting.len();
            let start = state.next % len;
            for offset in 0..len {
                let idx = (start + offset) % len;
                let Some((_, waiters)) = state.waiting.get_index_mut(idx) else {
                    continue;
                };
                let Some(waiter) = waiters.pop_front() else {
                    continue;
                };
                let emptied = waiters.is_empty();
                if emptied {
                    state.waiting.shift_remove_index(idx);
                    // The client that shifted into this index has not had its
                    // turn, so the rotation resumes AT it rather than after it.
                    state.next = idx;
                } else {
                    state.next = idx + 1;
                }
                state.active += 1;
                // A receiver that has gone away means the waiter was dropped
                // between being chosen and being woken. The slot it was just
                // granted goes back, and the scan restarts because the map may
                // have just shrunk.
                //
                // **Whether this branch is reachable depends on `abandon`, and
                // that dependency is the whole reason it stays.** It is
                // unreachable ONLY while `abandon` removes this ticket's own
                // waiter, by id, under this same lock. Both halves matter: when
                // `abandon` removed positionally instead, a client with two
                // calls in flight left its own sender in the deque to be
                // granted here, and deleting this guard turned that into a
                // permanent leak of the only slot.
                //
                // That is not hypothetical. It is what the code did until the
                // review found it, and `a_client_with_two_calls_in_flight_can_abandon_one`
                // is the test that drives it: stage positional removal and it
                // fails; stage positional removal AND delete this guard and it
                // fails with the engine wedged rather than merely stalled.
                //
                // So its mutation surviving today is a statement about
                // `abandon`, not about this branch being dead code.
                if waiter.tx.send(()).is_err() {
                    state.active -= 1;
                    continue 'rescan;
                }
                return;
            }
            // A full pass over every client found no waiter to admit.
            return;
        }
    }

    fn release(&self) {
        let mut state = lock(&self.state);
        state.active = state.active.saturating_sub(1);
        self.dispatch(&mut state);
        self.observe(&state);
    }

    /// Undo a ticket that is being dropped before it became a [`Permit`].
    ///
    /// **The two cases must be distinguished under the lock, and getting that
    /// wrong wedges the engine permanently.** A caller that disconnects can be
    /// dropped at either of two moments:
    ///
    /// - still queued, in which case its waiter has to leave the queue;
    /// - already CHOSEN by [`Self::dispatch`], which incremented `active` and
    ///   sent the grant, but dropped before it could receive that grant and turn
    ///   it into a permit.
    ///
    /// The second case sends successfully, so the "receiver went away" guard in
    /// `dispatch` does not see it, and the slot is charged to a caller that will
    /// never release it. With one slot that is the whole engine, lost for the
    /// life of the process, and every later call queues behind it forever.
    ///
    /// `try_recv` under the state lock is what separates them: `dispatch` can
    /// only send while holding this same lock, so an empty channel here means no
    /// grant is in flight and cannot become one before the waiter is removed.
    fn abandon(&self, client_id: &str, id: u64, rx: &mut oneshot::Receiver<()>) {
        let mut state = lock(&self.state);
        match rx.try_recv() {
            // Granted, and nobody is going to release it. Hand it back.
            Ok(()) => {
                state.active = state.active.saturating_sub(1);
                self.dispatch(&mut state);
            }
            // Still queued: take OUR waiter out, found by id.
            //
            // **By id, never positionally.** One client can have several calls
            // in flight, so the front of its deque is somebody else's waiter,
            // and removing that one closes a live caller's channel: it is
            // answered `server_unavailable` while still connected, and this
            // ticket's own sender stays in the queue to be granted a slot
            // nobody will ever release.
            Err(oneshot::error::TryRecvError::Empty) => {
                if let Some(waiters) = state.waiting.get_mut(client_id) {
                    waiters.retain(|waiter| waiter.id != id);
                    if waiters.is_empty() {
                        state.waiting.shift_remove(client_id);
                    }
                }
            }
            // The queue itself went away; there is nothing to give back.
            Err(oneshot::error::TryRecvError::Closed) => {}
        }
        self.observe(&state);
    }

    fn observe(&self, state: &State) {
        self.observer.note(state.active as u32, depth(state) as u32);
    }

    #[cfg(test)]
    fn snapshot(&self) -> (usize, usize) {
        let state = lock(&self.state);
        (state.active, depth(&state))
    }
}

fn depth(state: &State) -> usize {
    state.waiting.values().map(VecDeque::len).sum()
}

/// The engine slot. Releasing it is [`Drop`], never a method, because the caller
/// that most needs to release it is one that will never run another line.
pub struct Permit {
    queue: Arc<Queue>,
}

impl Drop for Permit {
    fn drop(&mut self) {
        self.queue.release();
    }
}

/// A place in the queue, held only while awaiting admission.
struct Ticket {
    queue: Arc<Queue>,
    client_id: String,
    /// Identifies THIS ticket's waiter in its client's deque.
    id: u64,
    rx: oneshot::Receiver<()>,
    admitted: bool,
}

impl Ticket {
    async fn wait(mut self) -> Result<Permit> {
        match (&mut self.rx).await {
            Ok(()) => {
                self.admitted = true;
                Ok(Permit {
                    queue: Arc::clone(&self.queue),
                })
            }
            // The queue was dropped out from under the waiter. Nothing is going
            // to admit it, so it says so rather than hanging.
            Err(_) => Err(AppError::new(
                ErrorCode::ServerUnavailable,
                "local speech engine went away",
            )),
        }
    }
}

impl Drop for Ticket {
    fn drop(&mut self) {
        if !self.admitted {
            let client_id = std::mem::take(&mut self.client_id);
            self.queue.abandon(&client_id, self.id, &mut self.rx);
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::future::Future;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use super::*;

    /// Await with a deadline, and EVERY admission wait in this module goes
    /// through it.
    ///
    /// The failure mode of every dispatch bug in this queue is a wedge, not a
    /// wrong value, so an unbounded `.await` turns a caught mutation into a hung
    /// test binary that holds the cargo build lock against the whole fleet
    /// (D-070) rather than into a failure. This package has produced that
    /// outcome three times: the `Ticket::drop` fix, the runtime-blocking test,
    /// and the depth-bound mutation, which parked here for eleven minutes before
    /// anyone looked.
    async fn bounded<T>(what: &str, fut: impl Future<Output = T>) -> T {
        match tokio::time::timeout(Duration::from_secs(5), fut).await {
            Ok(value) => value,
            Err(_) => panic!("{what}: nothing was admitted within 5s, so the queue wedged"),
        }
    }

    /// Spin until the queue's `(active, queued)` satisfies `want`, with a
    /// deadline.
    ///
    /// **The same rule as [`bounded`], one level down, and missing it is how
    /// D-149 recurred inside the fix for it.** Every `while queue.snapshot()...`
    /// loop is a wait, and a state that never arrives is the normal failure mode
    /// of every bug in this module, so a bare spin wedges the binary where a
    /// deadline would name the state it got stuck at. The F1 test parked for 30
    /// seconds under a neighbouring mutation for exactly this reason: its two
    /// admission awaits were bounded and its state waits were not.
    async fn until(what: &str, queue: &Arc<Queue>, want: impl Fn((usize, usize)) -> bool) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let snapshot = queue.snapshot();
            if want(snapshot) {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "{what}: the queue stayed at (active {}, queued {}) for 5s",
                snapshot.0,
                snapshot.1,
            );
            tokio::task::yield_now().await;
        }
    }

    /// Records every `(active, queued)` the queue announced, so a test can
    /// assert what the core status bar would have shown rather than only what
    /// the queue's own snapshot says.
    #[derive(Default)]
    struct Recorder {
        seen: Mutex<Vec<(u32, u32)>>,
        calls: AtomicUsize,
    }

    impl QueueObserver for Recorder {
        fn note(&self, active: u32, queued: u32) {
            self.calls.fetch_add(1, Ordering::SeqCst);
            lock(&self.seen).push((active, queued));
        }
    }

    fn queue() -> (Arc<Queue>, Arc<Recorder>) {
        let recorder = Arc::new(Recorder::default());
        let queue = Arc::new(Queue::new(Arc::clone(&recorder) as Arc<dyn QueueObserver>));
        (queue, recorder)
    }

    #[tokio::test]
    async fn one_call_runs_and_the_next_waits_for_it() {
        let (queue, _) = queue();
        let first = queue.acquire("a").await.unwrap();
        assert_eq!(queue.snapshot(), (1, 0));

        let waiting = {
            let queue = Arc::clone(&queue);
            tokio::spawn(async move { queue.acquire("b").await.map(|_| ()) })
        };
        // Yield until the spawned task has actually enqueued, rather than
        // sleeping: a timing sleep here would pass with the queue deleted.
        until("the second caller queues", &queue, |s| s.1 > 0).await;
        assert_eq!(queue.snapshot(), (1, 1));

        drop(first);
        // The waiter was admitted (its acquire returned), ran, and dropped its
        // own permit on the way out, so the engine is idle again.
        bounded("the queued caller", waiting)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(queue.snapshot(), (0, 0));
    }

    #[tokio::test]
    async fn waiting_clients_are_admitted_round_robin_not_first_come_first_served() {
        let (queue, _) = queue();
        let held = queue.acquire("holder").await.unwrap();

        // Client "a" queues twice before "b" queues once. First-come order would
        // admit a, a, b; round robin admits a, b, a.
        let order = Arc::new(Mutex::new(Vec::new()));
        let mut handles = Vec::new();
        for client in ["a", "a", "b"] {
            let spawned = Arc::clone(&queue);
            let order = Arc::clone(&order);
            let want = queue.snapshot().1 + 1;
            handles.push(tokio::spawn(async move {
                let permit = spawned.acquire(client).await.unwrap();
                lock(&order).push(client);
                drop(permit);
            }));
            until("a waiter queues", &queue, |s| s.1 >= want).await;
        }
        assert_eq!(queue.snapshot(), (1, 3));

        drop(held);
        for handle in handles {
            bounded("a round-robin waiter", handle).await.unwrap();
        }
        assert_eq!(*lock(&order), vec!["a", "b", "a"]);
    }

    #[tokio::test]
    async fn the_rotation_resumes_at_the_client_that_shifted_into_a_vacated_slot() {
        // Three clients, one waiter each. Removing the admitted client's entry
        // shifts the others down, and a rotation that advanced past the removed
        // index would skip whoever moved into it.
        let (queue, _) = queue();
        let held = queue.acquire("holder").await.unwrap();

        let order = Arc::new(Mutex::new(Vec::new()));
        let mut handles = Vec::new();
        for client in ["a", "b", "c"] {
            let spawned = Arc::clone(&queue);
            let order = Arc::clone(&order);
            let want = queue.snapshot().1 + 1;
            handles.push(tokio::spawn(async move {
                let permit = spawned.acquire(client).await.unwrap();
                lock(&order).push(client);
                drop(permit);
            }));
            until("a waiter queues", &queue, |s| s.1 >= want).await;
        }

        drop(held);
        for handle in handles {
            bounded("a rotation waiter", handle).await.unwrap();
        }
        assert_eq!(*lock(&order), vec!["a", "b", "c"]);
    }

    #[tokio::test]
    async fn a_saturated_queue_refuses_before_it_enqueues() {
        let (queue, _) = queue();
        let held = queue.acquire("holder").await.unwrap();

        let mut handles = Vec::new();
        for i in 0..PARALLEL_SLOTS * QUEUE_DEPTH_MULTIPLIER {
            let spawned = Arc::clone(&queue);
            let client = format!("c{i}");
            let want = queue.snapshot().1 + 1;
            handles.push(tokio::spawn(async move {
                spawned.acquire(&client).await.map(|_| ())
            }));
            until("a waiter queues", &queue, |s| s.1 >= want).await;
        }
        assert_eq!(queue.snapshot().1, PARALLEL_SLOTS * QUEUE_DEPTH_MULTIPLIER);

        // Bounded, because the mutation this test exists to catch (relaxing
        // the bound from `>=` to `>`) does not admit the caller: it ENQUEUES it,
        // and an unbounded await then parks forever instead of failing.
        let err = match bounded("the refused caller", queue.acquire("late")).await {
            Ok(_) => panic!("a saturated queue admitted another caller"),
            Err(err) => err,
        };
        assert_eq!(err.code, ErrorCode::ServerBusy);
        assert_eq!(err.message, "speech queue full (4 waiting, 1 slots)");
        // Refused, not enqueued: the depth is unchanged.
        assert_eq!(queue.snapshot().1, PARALLEL_SLOTS * QUEUE_DEPTH_MULTIPLIER);

        drop(held);
        for handle in handles {
            bounded("a queued caller", handle).await.unwrap().unwrap();
        }
    }

    #[tokio::test]
    async fn a_caller_that_gives_up_while_queued_leaves_the_queue() {
        let (queue, _) = queue();
        let held = queue.acquire("holder").await.unwrap();

        let abandoned = {
            let queue = Arc::clone(&queue);
            tokio::spawn(async move { queue.acquire("gone").await.map(|_| ()) })
        };
        until("the abandoned caller queues", &queue, |s| s.1 > 0).await;
        abandoned.abort();
        let _ = abandoned.await;
        // The abort drops the future, which drops the ticket, which withdraws.
        until("the withdrawal", &queue, |s| s.1 == 0).await;

        // The engine is still reachable: the slot the abandoned caller would
        // have been handed is not lost.
        drop(held);
        let after = bounded("the next caller", queue.acquire("next"))
            .await
            .unwrap();
        assert_eq!(queue.snapshot(), (1, 0));
        drop(after);
    }

    #[tokio::test]
    async fn a_caller_that_gives_up_after_being_chosen_hands_its_slot_back() {
        // The narrow race the plain version misses: the waiter is dropped
        // between `dispatch` choosing it and the oneshot being received, so the
        // send fails with the slot already counted as active.
        let (queue, _) = queue();
        let held = queue.acquire("holder").await.unwrap();

        let abandoned = {
            let queue = Arc::clone(&queue);
            tokio::spawn(async move { queue.acquire("gone").await.map(|_| ()) })
        };
        until("the abandoned caller queues", &queue, |s| s.1 > 0).await;
        let live = {
            let queue = Arc::clone(&queue);
            tokio::spawn(async move { queue.acquire("live").await.map(|_| ()) })
        };
        until("the live waiter queues", &queue, |s| s.1 >= 2).await;

        // Abort without letting the task run again, so the grant is SENT to a
        // receiver that is alive at send time and gone by receive time. This is
        // the case `dispatch`'s send-failed guard cannot see.
        abandoned.abort();
        drop(held);

        // The live waiter still gets in, which is only true if the slot granted
        // to the abandoned caller was handed back. Bounded rather than a bare
        // await: the regression here is a permanent wedge, and an unbounded
        // await would turn it into a hung test binary that blocks the whole
        // fleet's build lock rather than a failure (D-070).
        let admitted = bounded(
            "the live waiter (the abandoned grant leaked the slot)",
            live,
        )
        .await;
        admitted.unwrap().unwrap();
        let _ = abandoned.await;
    }

    /// The case the rest of the suite could not see: **one client with two calls
    /// in flight**, abandoning the second.
    ///
    /// Every other drop test queues at most one waiter per client, which is
    /// exactly the shape where removing "the front of the client's deque"
    /// happens to be correct. It is not correct in general, and the two failures
    /// it produced are different in kind:
    ///
    /// 1. the client's OTHER call, still waiting and still connected, had its
    ///    channel closed and was answered `server_unavailable`;
    /// 2. the abandoned sender survived to be granted the slot, `dispatch` fell
    ///    off a stale `len`, and the engine idled with another client queued.
    ///
    /// It is also the input that reaches the dead-receiver guard in `dispatch`,
    /// which was previously documented as unreachable. With that guard removed
    /// the stall below becomes a permanent leak of the only slot.
    #[tokio::test]
    async fn a_client_with_two_calls_in_flight_can_abandon_one() {
        let (queue, _) = queue();
        let held = queue.acquire("holder").await.unwrap();

        // Client "a" queues twice; client "b" once, behind them.
        let first = {
            let spawned = Arc::clone(&queue);
            tokio::spawn(async move { spawned.acquire("a").await.map(|_| ()) })
        };
        until("client a's first call queues", &queue, |s| s.1 >= 1).await;
        let second = {
            let spawned = Arc::clone(&queue);
            tokio::spawn(async move { spawned.acquire("a").await.map(|_| ()) })
        };
        until("client a's second call queues", &queue, |s| s.1 >= 2).await;
        let other_client = {
            let spawned = Arc::clone(&queue);
            tokio::spawn(async move { spawned.acquire("b").await.map(|_| ()) })
        };
        until("client b queues", &queue, |s| s.1 >= 3).await;

        // The user cancels the SECOND of client a's calls.
        second.abort();
        let _ = second.await;
        until("the abandoned call to leave the queue", &queue, |s| {
            s.1 == 2
        })
        .await;

        // Client a's FIRST call is untouched: it is still queued, and when the
        // engine frees up it is served rather than told the engine went away.
        drop(held);
        bounded("client a's surviving call", first)
            .await
            .unwrap()
            .expect("client a's other call was answered with an error");

        // And client b is still reachable: the slot was not lost to the
        // abandoned waiter.
        bounded("client b", other_client).await.unwrap().unwrap();
        assert_eq!(queue.snapshot(), (0, 0));
    }

    #[tokio::test]
    async fn every_transition_is_announced_to_the_observer() {
        let (queue, recorder) = queue();
        let first = queue.acquire("a").await.unwrap();
        let waiting = {
            let queue = Arc::clone(&queue);
            tokio::spawn(async move { queue.acquire("b").await.map(|_| ()) })
        };
        until("the second caller queues", &queue, |s| s.1 > 0).await;
        drop(first);
        bounded("the announced waiter", waiting)
            .await
            .unwrap()
            .unwrap();

        let seen = lock(&recorder.seen).clone();
        // Acquire, enqueue, and release are all announced, and the queue depth
        // is visible in at least one of them: a status bar that only ever saw
        // (1, 0) could not render a backlog.
        assert!(seen.contains(&(1, 0)), "{seen:?}");
        assert!(seen.contains(&(1, 1)), "{seen:?}");
        assert!(recorder.calls.load(Ordering::SeqCst) >= 3, "{seen:?}");
    }

    #[tokio::test]
    async fn the_slot_is_released_even_when_the_holder_panics_its_way_out() {
        // Not a panic test: the property is that release is Drop rather than an
        // explicit call, so any unwind past the permit still frees the engine.
        let (queue, _) = queue();
        {
            let _permit = queue.acquire("a").await.unwrap();
            assert_eq!(queue.snapshot(), (1, 0));
        }
        assert_eq!(queue.snapshot(), (0, 0));
        let _again = queue.acquire("a").await.unwrap();
        assert_eq!(queue.snapshot(), (1, 0));
    }
}
