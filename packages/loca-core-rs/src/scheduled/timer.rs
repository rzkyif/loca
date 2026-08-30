//! The arm loop: one task, always parked on the soonest occurrence. One task
//! cannot overlap itself, so there is no timer handle and no `ticking` flag.

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
            // Biased so a shutdown landing with a due occurrence stops rather
            // than firing one more turn.
            biased;
            () = scheduler.cancelled() => return,
            () = scheduler.rearm_signalled() => continue,
            () = sleep_for(delay) => {
                scheduler.tick_at(super::store::now_ms()).await;
            }
        }
    }
}

/// Sleep until `delay`, or park forever when nothing is scheduled.
async fn sleep_for(delay: Option<Duration>) {
    match delay {
        Some(delay) => tokio::time::sleep(delay).await,
        None => std::future::pending().await,
    }
}
