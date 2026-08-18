//! Cooperative cancellation for a transfer.
//!
//! One re-export. `tokio-util` is a direct dependency now, so the hand-rolled
//! `watch::<bool>` stand-in that stood here while the crate was queued is gone:
//! it existed only to keep the call surface (`cancel`, `is_cancelled`,
//! `cancelled().await`, `Clone`, `Default`) identical so that the swap would be
//! this line and no call site would move. It was, and none did.
//!
//! The module stays rather than collapsing into a `use` at each call site,
//! because the two properties below are what this package actually relies on,
//! and they are worth stating and testing where the type enters the package.

pub use tokio_util::sync::CancellationToken;

/// These characterize a DEPENDENCY rather than code in this package, which is
/// normally a smell. They are kept for one reason: the queue's correctness rests
/// on the token being level-triggered, not edge-triggered, and that is a
/// property a minor version bump could quietly change. A worker that acquires
/// the permit after its row was already cancelled has to see the trip that
/// happened while it waited; an edge-only primitive loses it and the worker
/// downloads a file the user stopped.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// Every awaiting test in this package is bounded, so a regression fails
    /// rather than hanging (D-036). The message names the package: a bounded
    /// failure in a shared tree has to be attributable without opening the file.
    async fn bounded<T>(fut: impl std::future::Future<Output = T>) -> T {
        tokio::time::timeout(Duration::from_secs(5), fut)
            .await
            .expect("d7: test exceeded its bound")
    }

    #[tokio::test]
    async fn a_clone_observes_the_original_being_tripped() {
        let token = CancellationToken::new();
        let clone = token.clone();
        assert!(!clone.is_cancelled());
        token.cancel();
        assert!(clone.is_cancelled());
        bounded(clone.cancelled()).await;
    }

    #[tokio::test]
    async fn a_waiter_that_arrives_after_the_trip_still_resolves() {
        // The queue's shape: a worker blocked on the permit, cancelled while it
        // waited, has to see the trip rather than wait for a second one.
        let token = CancellationToken::new();
        token.cancel();
        bounded(token.cancelled()).await;
    }

    #[tokio::test]
    async fn cancelled_resolves_only_once_the_switch_trips() {
        let token = CancellationToken::new();
        let waiter = token.clone();
        let handle = tokio::spawn(async move { waiter.cancelled().await });
        // Not yet: the future must still be pending after a real tick.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!handle.is_finished());
        token.cancel();
        bounded(handle).await.unwrap();
    }
}
