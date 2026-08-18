//! Scheduled prompts: SQLite-backed CRUD plus one armed timer that opens
//! automated sessions when a schedule's occurrence arrives.
//!
//! Work package **D10** (`docs/rust-port/registry.md`). This module is that
//! package's owned path; nothing outside it writes here.
//!
//! # The arming model
//!
//! One task, always pointed at the soonest enabled `next_run_at_ms`, re-armed
//! after every CRUD and every tick and capped at [`MAX_ARM_MS`] so a long wait
//! self-heals against clock drift and the machine sleeping through it. A tick
//! fires every due row, moves its occurrence on, and re-arms.
//!
//! # Missed runs
//!
//! A row is MISSED, rather than merely late, when its occurrence is more than
//! [`MISSED_GRACE_MS`] in the past at tick time. That one definition covers both
//! ways it happens: the core was off (boot catch-up, where the first tick sees
//! an overdue row and fires immediately) and the machine slept through the
//! deadline. A missed row fires only when its `runMissed` flag is set, and at
//! most once, because the recompute that follows always moves the occurrence
//! forward from now.
//!
//! # What this module does NOT own
//!
//! Opening the turn. `runAutomatedSession` is the single backend behind both
//! greetings and scheduled prompts, so it is ported once by D8 (D-090), and so
//! is the "is the local model mid-load" probe (D-094). Both arrive through
//! [`deps`].

pub mod deps;
pub mod routes;
pub mod spec;
pub mod store;
mod timer;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod testzone;

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::contract::api::ScheduledPromptDraft;
use crate::error::{validation, AppError, ErrorCode, Result};

pub use deps::{AutomatedRunner, ChatAutomated, NeverLoading, ScheduledRun, SchedulerDeps};
pub use spec::{next_occurrence, next_occurrence_in, validate_spec};
pub use store::{Patch, ScheduledPrompt, Store};

/// Re-arm at least daily even when the next occurrence is further out.
const MAX_ARM_MS: i64 = 24 * 60 * 60 * 1000;
/// A due run older than this counts as missed (and so honours `runMissed`)
/// rather than merely late (which always fires).
const MISSED_GRACE_MS: i64 = 10 * 60 * 1000;
/// How long to hold due rows when the local model is still loading.
const SIDECAR_RETRY_MS: u64 = 10 * 1000;

/// The scheduler. One per core.
///
/// A second instance would arm a second timer over the same table, so every due
/// occurrence would fire twice. Cheap to clone; every clone shares one timer,
/// one runner and one cancellation token.
#[derive(Clone)]
pub struct Scheduler {
    inner: Arc<Inner>,
}

struct Inner {
    store: Store,
    deps: SchedulerDeps,
    /// Set once, after `Chat` exists. See [`Scheduler::attach_automated`].
    automated: OnceLock<Arc<dyn AutomatedRunner>>,
    /// Poked by every CRUD so the parked timer recomputes.
    rearm: Notify,
    cancel: CancellationToken,
    /// The delay the last tick asked for, when it asked for one. `None` means
    /// re-arm from the rows.
    retry: std::sync::Mutex<Option<Duration>>,
    /// Whether `start` has run. Read by [`Scheduler::is_armed`]; see its doc for
    /// why this exists rather than inferring it from behaviour.
    armed: std::sync::atomic::AtomicBool,
}

impl Scheduler {
    pub fn new(db: crate::db::Db, deps: SchedulerDeps) -> Self {
        Self {
            inner: Arc::new(Inner {
                store: Store::new(db),
                deps,
                automated: OnceLock::new(),
                armed: std::sync::atomic::AtomicBool::new(false),
                rearm: Notify::new(),
                cancel: CancellationToken::new(),
                retry: std::sync::Mutex::new(None),
            }),
        }
    }

    /// Give the scheduler its way to open a turn.
    ///
    /// Separate from [`Scheduler::new`] because there is a genuine construction
    /// cycle: `Chat` needs the scheduler to persist a confirmed draft, and the
    /// scheduler needs `Chat` to open a session. Set-once, following
    /// `Manager::attach_status` and `Llm::attach_status`; a second call is
    /// ignored, so a stray wiring cannot silently replace the live runner.
    ///
    /// Returns whether this call is the one that set it, which is what makes a
    /// double-attach assertable rather than invisible.
    pub fn attach_automated(&self, runner: Arc<dyn AutomatedRunner>) -> bool {
        self.inner.automated.set(runner).is_ok()
    }

    /// Arm the timer from the persisted rows.
    ///
    /// Overdue rows make the first tick fire immediately, which IS the boot
    /// catch-up: nothing else looks for runs the core was off for.
    pub fn start(&self) {
        self.inner
            .armed
            .store(true, std::sync::atomic::Ordering::Relaxed);
        tokio::spawn(timer::run(self.clone()));
    }

    /// Whether [`Scheduler::start`] has armed the timer.
    ///
    /// Exists so boot's embedded arm is OBSERVABLE. `start` is a bare
    /// `tokio::spawn` and `shutdown` cancels a token nothing exposes, so
    /// "the arm loop did not start" had no assertion available and the mutation
    /// that removed boot's guard survived everything (M1 review, C2 skip 3). The
    /// alternative was a "nothing fired within N seconds" test, which is a slow
    /// negative assertion on a timer.
    pub fn is_armed(&self) -> bool {
        self.inner.armed.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Stop the timer. Idempotent.
    ///
    /// Cancelling is the WHOLE mechanism: the loop selects on the token, so a
    /// loop parked on a deadline a day out returns immediately. An earlier
    /// version also poked `rearm` here, which worked and was worse, because it
    /// meant shutdown had two paths out and the cancellation branch was never
    /// the one under test. A mutation that deleted that branch survived until
    /// this was reduced to one mechanism.
    pub fn shutdown(&self) {
        self.inner.cancel.cancel();
    }

    // --- CRUD, owner-scoped --------------------------------------------------

    pub async fn list(&self, owner_client_id: &str) -> Result<Vec<ScheduledPrompt>> {
        self.inner.store.list(owner_client_id).await
    }

    pub async fn get(&self, owner_client_id: &str, id: &str) -> Result<ScheduledPrompt> {
        self.inner.store.get(owner_client_id, id).await
    }

    pub async fn create(
        &self,
        owner_client_id: &str,
        draft: &ScheduledPromptDraft,
    ) -> Result<ScheduledPrompt> {
        let created = self.inner.store.create(owner_client_id, draft).await?;
        self.poke();
        Ok(created)
    }

    pub async fn update(
        &self,
        owner_client_id: &str,
        id: &str,
        patch: &Patch,
    ) -> Result<ScheduledPrompt> {
        let updated = self.inner.store.update(owner_client_id, id, patch).await?;
        self.poke();
        Ok(updated)
    }

    pub async fn delete(&self, owner_client_id: &str, id: &str) -> Result<()> {
        self.inner.store.delete(owner_client_id, id).await?;
        self.poke();
        Ok(())
    }

    /// Fire a schedule immediately: the settings "Run Now" action.
    ///
    /// Leaves the armed occurrence untouched, so a manual run is extra rather
    /// than instead of.
    pub async fn run_now(&self, owner_client_id: &str, id: &str) -> Result<String> {
        let prompt = self.get(owner_client_id, id).await?;
        if !prompt.enabled {
            return Err(validation("scheduled prompt is disabled; enable it to run"));
        }
        // The tick defers due rows while the local model loads, because a turn
        // fired then races the load and errors. A manual run refuses for the
        // same reason, and loudly, so the user retries rather than being handed
        // a session that fails on its own.
        if self.inner.deps.loading.still_loading().await {
            return Err(validation(
                "the local model is still loading; try again in a moment",
            ));
        }
        let session_id = self
            .runner()?
            .run(ScheduledRun {
                owner_client_id: owner_client_id.to_string(),
                title: prompt.title.clone(),
                instruction: prompt.instruction.clone(),
                scheduled_prompt_id: prompt.id.clone(),
            })
            .await?;
        self.inner.store.stamp_run(id, store::now_ms()).await?;
        Ok(session_id)
    }

    // --- the tick ------------------------------------------------------------

    /// Process every row due at `now`.
    ///
    /// `now` is a parameter rather than read from the clock so the whole of this
    /// behaviour is testable without a test that sleeps and hopes. The timer
    /// passes the real clock.
    pub async fn tick_at(&self, now: i64) {
        let retry = self.tick_inner(now).await;
        // Set even when a row's processing failed: a transient error must not
        // leave the scheduler permanently disarmed. The oracle does this in a
        // `finally` for the same reason (`prompt-scheduler.ts:386-392`).
        self.set_retry(retry);
    }

    async fn tick_inner(&self, now: i64) -> Option<Duration> {
        let due = match self.inner.store.due(now).await {
            Ok(due) => due,
            Err(err) => {
                tracing::error!(target: "loca::scheduled", "reading due schedules failed: {err}");
                return None;
            }
        };
        if due.is_empty() {
            return None;
        }
        // While the model is mid-load (boot, or a settings change restarting
        // it), the chat path's ensure-loaded no-ops, so a turn fired now would
        // race the load and error. Hold every due row and retry shortly.
        if self.inner.deps.loading.still_loading().await {
            tracing::debug!(
                target: "loca::scheduled",
                "local model still loading; deferring {} due schedule(s)",
                due.len(),
            );
            return Some(Duration::from_millis(SIDECAR_RETRY_MS));
        }

        for snapshot in due {
            // The loading probe awaited above is a window in which the row could
            // have been edited, advanced by a concurrent `run_now`, disabled, or
            // parked. Re-read the FULL row and act on the latest state rather
            // than on the pre-await snapshot, so a stale instruction is never
            // fired and a freshly-advanced occurrence is never clobbered.
            let row = match self.inner.store.by_id(&snapshot.id).await {
                Ok(Some(row)) => row,
                Ok(None) => continue,
                Err(err) => {
                    tracing::error!(
                        target: "loca::scheduled",
                        "re-reading scheduled prompt {} failed: {err}",
                        snapshot.id,
                    );
                    continue;
                }
            };
            if !row.enabled {
                continue;
            }
            // Advanced, parked or rescheduled in the gap: no longer due.
            let Some(due_at) = row.next_run_at_ms.filter(|at| *at <= now) else {
                continue;
            };
            let schedule = match row.schedule() {
                Ok(schedule) => schedule,
                Err(err) => {
                    // A schedule that will not parse must not wedge the whole
                    // subsystem in a throw-and-re-arm loop. Park the row, which a
                    // user edit revives, and carry on with the others.
                    tracing::error!(
                        target: "loca::scheduled",
                        "parking scheduled prompt {}: {err}",
                        row.id,
                    );
                    if let Err(err) = self.inner.store.park(&row.id).await {
                        tracing::error!(
                            target: "loca::scheduled",
                            "parking scheduled prompt {} failed: {err}",
                            row.id,
                        );
                    }
                    continue;
                }
            };

            let missed = now - due_at > MISSED_GRACE_MS;
            let next = next_occurrence(&schedule, now);
            if missed && !row.run_missed {
                tracing::info!(
                    target: "loca::scheduled",
                    "scheduled prompt \"{}\" missed its run; skipping (run missed is off)",
                    row.title,
                );
                if let Err(err) = self.inner.store.reschedule(&row.id, next).await {
                    tracing::error!(
                        target: "loca::scheduled",
                        "rescheduling {} failed: {err}",
                        row.id,
                    );
                }
                continue;
            }

            // Persist the advanced occurrence BEFORE firing (at-most-once): a
            // crash mid-run skips that occurrence instead of duplicating the
            // automated session on reboot.
            if let Err(err) = self.inner.store.advance(&row.id, now, next).await {
                tracing::error!(
                    target: "loca::scheduled",
                    "advancing {} failed, so it will not be fired: {err}",
                    row.id,
                );
                continue;
            }
            let fired = match self.runner() {
                Ok(runner) => {
                    runner
                        .run(ScheduledRun {
                            owner_client_id: row.owner_client_id.clone(),
                            title: row.title.clone(),
                            instruction: row.instruction.clone(),
                            scheduled_prompt_id: row.id.clone(),
                        })
                        .await
                }
                Err(err) => Err(err),
            };
            if let Err(err) = fired {
                tracing::error!(
                    target: "loca::scheduled",
                    "scheduled prompt \"{}\" failed to fire: {err}",
                    row.title,
                );
            }
        }
        None
    }

    // --- arming --------------------------------------------------------------

    /// How long the timer should park, or `None` when nothing is scheduled.
    pub(crate) async fn arm_delay(&self) -> Option<Duration> {
        if let Some(retry) = self.take_retry() {
            return Some(retry);
        }
        match self.inner.store.soonest().await {
            Ok(Some(soonest)) => Some(Duration::from_millis(
                soonest.saturating_sub(store::now_ms()).clamp(0, MAX_ARM_MS) as u64,
            )),
            Ok(None) => None,
            Err(err) => {
                tracing::error!(target: "loca::scheduled", "arming failed: {err}");
                None
            }
        }
    }

    /// Wake the parked timer so it recomputes its deadline.
    ///
    /// `notify_one` stores a permit when nothing is waiting, so a poke that
    /// lands while a tick is running is remembered rather than lost.
    fn poke(&self) {
        self.inner.rearm.notify_one();
    }

    pub(crate) async fn rearm_signalled(&self) {
        self.inner.rearm.notified().await;
    }

    pub(crate) async fn cancelled(&self) {
        self.inner.cancel.cancelled().await;
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.inner.cancel.is_cancelled()
    }

    fn set_retry(&self, delay: Option<Duration>) {
        *lock(&self.inner.retry) = delay;
    }

    fn take_retry(&self) -> Option<Duration> {
        lock(&self.inner.retry).take()
    }

    /// The attached runner, or a refusal.
    ///
    /// Refusing rather than quietly doing nothing: a scheduler that reports a
    /// session it never created is D-083's symptom (accepted, recorded, nothing
    /// happens) one layer up, and it is the outcome the persist-before-fire
    /// ordering exists to prevent.
    fn runner(&self) -> Result<Arc<dyn AutomatedRunner>> {
        self.inner.automated.get().cloned().ok_or_else(|| {
            AppError::new(
                ErrorCode::ServerUnavailable,
                "this core cannot open an automated session",
            )
        })
    }
}

/// Persist an agent-proposed schedule the user confirmed.
///
/// Synchronous because the caller runs it inside the schedule-confirm response
/// handler, before unblocking the tool, so a storage failure answers the tool
/// `false` and the tool never reports "scheduled" for something that was not
/// stored (`chat.ts:196-206`).
impl crate::chat::SchedulePersist for Scheduler {
    fn create(&self, client_id: &str, draft: &ScheduledPromptDraft) -> Result<()> {
        self.inner.store.create_blocking(client_id, draft)?;
        self.poke();
        Ok(())
    }
}

fn lock<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}
