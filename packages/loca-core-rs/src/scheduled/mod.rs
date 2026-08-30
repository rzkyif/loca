//! Scheduled prompts: SQLite-backed CRUD plus one armed timer that opens
//! automated sessions when a schedule's occurrence arrives.
//!
//! One task is always parked on the soonest enabled `next_run_at_ms`, re-armed
//! after every CRUD and tick and capped at [`MAX_ARM_MS`] so a long wait
//! self-heals against clock drift and sleep. A row more than [`MISSED_GRACE_MS`]
//! overdue at tick time is missed (fires only with `runMissed`, at most once);
//! anything less is merely late and always fires. Opening the turn and the
//! model-loading probe are shared with greetings and arrive through [`deps`].

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

/// The scheduler. One per core: a second instance would fire every due
/// occurrence twice. Clones share one timer, runner and cancellation token.
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
    /// Whether `start` has run.
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

    /// Give the scheduler its way to open a turn. Separate from `new` because
    /// `Chat` needs the scheduler and the scheduler needs `Chat`. Set-once;
    /// returns whether this call set it.
    pub fn attach_automated(&self, runner: Arc<dyn AutomatedRunner>) -> bool {
        self.inner.automated.set(runner).is_ok()
    }

    /// Arm the timer from the persisted rows. Overdue rows make the first tick
    /// fire immediately, which is the boot catch-up.
    pub fn start(&self) {
        self.inner
            .armed
            .store(true, std::sync::atomic::Ordering::Relaxed);
        tokio::spawn(timer::run(self.clone()));
    }

    /// Whether [`Scheduler::start`] has armed the timer; makes boot's arm
    /// observable without a "nothing fired within N seconds" test.
    pub fn is_armed(&self) -> bool {
        self.inner.armed.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Stop the timer. Idempotent; cancelling the token is the whole mechanism.
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

    /// Fire a schedule immediately ("Run Now"). Leaves the armed occurrence
    /// untouched, so a manual run is extra rather than instead of.
    pub async fn run_now(&self, owner_client_id: &str, id: &str) -> Result<String> {
        let prompt = self.get(owner_client_id, id).await?;
        if !prompt.enabled {
            return Err(validation("scheduled prompt is disabled; enable it to run"));
        }
        // A turn fired mid-load races the load and errors; refuse loudly.
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

    /// Process every row due at `now` (a parameter so tests need not sleep).
    pub async fn tick_at(&self, now: i64) {
        let retry = self.tick_inner(now).await;
        // Set even when a row failed: a transient error must not disarm us.
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
        // A turn fired mid-load races the load and errors; hold and retry.
        if self.inner.deps.loading.still_loading().await {
            tracing::debug!(
                target: "loca::scheduled",
                "local model still loading; deferring {} due schedule(s)",
                due.len(),
            );
            return Some(Duration::from_millis(SIDECAR_RETRY_MS));
        }

        for snapshot in due {
            // The row may have changed during the await above; act on the
            // latest state, never the snapshot.
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
                    // Park an unparseable row (a user edit revives it) rather
                    // than wedge the loop.
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

            // Advance BEFORE firing (at-most-once): a crash mid-run skips the
            // occurrence instead of duplicating it on reboot.
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

    /// Wake the parked timer. `notify_one` stores a permit when nothing waits,
    /// so a poke during a tick is not lost.
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

    /// The attached runner, or a refusal (never a silent no-op).
    fn runner(&self) -> Result<Arc<dyn AutomatedRunner>> {
        self.inner.automated.get().cloned().ok_or_else(|| {
            AppError::new(
                ErrorCode::ServerUnavailable,
                "this core cannot open an automated session",
            )
        })
    }
}

/// Persist an agent-proposed schedule the user confirmed. Synchronous so a
/// storage failure answers the tool `false` before it reports "scheduled".
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
