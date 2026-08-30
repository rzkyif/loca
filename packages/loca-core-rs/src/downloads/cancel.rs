//! Cooperative cancellation for a transfer.

pub use tokio_util::sync::CancellationToken;

/// The queue rests on the token being level-triggered: a worker that acquires
/// the permit after its row was cancelled must still see the trip. These pin
/// that against a dependency bump quietly changing it.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// Bounds every await so a regression fails rather than hangs.
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
        let token = CancellationToken::new();
        token.cancel();
        bounded(token.cancelled()).await;
    }

    #[tokio::test]
    async fn cancelled_resolves_only_once_the_switch_trips() {
        let token = CancellationToken::new();
        let waiter = token.clone();
        let handle = tokio::spawn(async move { waiter.cancelled().await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!handle.is_finished());
        token.cancel();
        bounded(handle).await.unwrap();
    }
}
