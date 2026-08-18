//! The arm loop: one task, always parked on the soonest occurrence.
//!
//! The oracle re-arms a `setTimeout` after every CRUD and every tick
//! (`services/prompt-scheduler.ts:395-420`). One task holding the sleep is the
//! same behaviour with two pieces of its bookkeeping made unrepresentable rather
//! than guarded:
//!
//! - there is no timer handle to clear, because the sleep is a `select!` branch;
//! - there is no `ticking` flag (`:304`), because one task cannot overlap
//!   itself. `runNow` still races a tick, and that is handled where the oracle
//!   handles it: the tick re-reads each row after the loading probe.
//!
//! The cap matters more than it looks. Re-arming at least daily is what makes a
//! long wait self-heal against clock drift and the machine sleeping through the
//! deadline, neither of which a monotonic timer notices.

use std::time::Duration;

use super::Scheduler;

/// Park until the soonest occurrence, tick, repeat, until cancelled.
pub(super) async fn run(scheduler: Scheduler) {
    loop {
        if scheduler.is_cancelled() {
            return;
        }
        let delay = scheduler.arm_delay().await;
        tokio::select! {
            // Cancellation first, and biased, so a shutdown that lands together
            // with a due occurrence stops rather than firing one more turn into
            // a core that is closing its listener.
            biased;
            () = scheduler.cancelled() => return,
            () = scheduler.rearm_signalled() => continue,
            () = sleep_for(delay) => {
                scheduler.tick_at(super::store::now_ms()).await;
            }
        }
    }
}

/// Sleep until `delay`, or never when there is nothing scheduled.
///
/// `None` is `std::future::pending()`, the crate's idiom for an unarmed
/// `select!` branch (`transport/ws.rs:199-201`,
/// `extensions/worker/handle.rs:676-680`). An empty table parks the loop; it
/// does not spin, and it does not busy-wake once a day either.
async fn sleep_for(delay: Option<Duration>) {
    match delay {
        Some(delay) => tokio::time::sleep(delay).await,
        None => std::future::pending().await,
    }
}
