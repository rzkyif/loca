//! Requirements snapshot (what the current configuration needs on disk that is
//! not there yet) and greetings. Every change trigger (settings write, model or
//! binary download ending, storage reclaim) converges on one coalesced recompute:
//! [`Requirements::changed`] pokes a one-slot channel, so a burst of N
//! notifications costs at most two recomputes.

pub mod compute;
pub mod deps;
pub mod edges;
pub mod greetings;
pub mod routes;

#[cfg(test)]
mod tests;

use std::sync::{Arc, Weak};

use tokio::sync::mpsc;

use crate::contract::api::{RequirementsSnapshot, ServerToClientFrame};
use crate::error::Result;
use crate::models::Models;
use crate::settings::Settings;
use crate::storage::deps::RequirementsNotifier;
use crate::transport::FrameBus;

pub use compute::{binary_source, binary_source_to_kind, required_binary_kinds};
pub use edges::DownloadEdges;
pub use greetings::Greetings;

struct Inner {
    settings: Settings,
    models: Models,
    binaries: Arc<dyn deps::Binaries>,
    frames: Arc<FrameBus>,
    /// One slot: a full buffer means a rerun is already queued.
    wake: mpsc::Sender<()>,
}

/// The requirements service. Cheap to clone; every clone shares one recompute
/// worker.
#[derive(Clone)]
pub struct Requirements {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Requirements {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Requirements").finish_non_exhaustive()
    }
}

impl Requirements {
    /// Build the service and start its recompute worker. The worker holds a
    /// `Weak` and the only `Sender` lives in `Inner`, so dropping the last
    /// handle ends the task.
    pub fn new(
        settings: Settings,
        models: Models,
        binaries: Arc<dyn deps::Binaries>,
        frames: Arc<FrameBus>,
    ) -> Self {
        let (wake, mut rx) = mpsc::channel(1);
        let inner = Arc::new(Inner {
            settings,
            models,
            binaries,
            frames,
            wake,
        });
        let weak: Weak<Inner> = Arc::downgrade(&inner);
        tokio::spawn(async move {
            while rx.recv().await.is_some() {
                let Some(inner) = weak.upgrade() else { break };
                recompute(&inner).await;
            }
        });
        Self { inner }
    }

    /// The snapshot for the current configuration. `probe_sizes` is the only
    /// thing that reaches the network, and only `?probe=1` (the confirm modal)
    /// sets it.
    pub async fn snapshot(&self, probe_sizes: bool) -> Result<RequirementsSnapshot> {
        snapshot(&self.inner, probe_sizes).await
    }

    pub(crate) fn models(&self) -> &Models {
        &self.inner.models
    }

    pub(crate) fn binaries(&self) -> &dyn deps::Binaries {
        self.inner.binaries.as_ref()
    }
}

/// Infallible and non-blocking: callers are fire-and-forget notifications on
/// mutation paths that already succeeded.
impl RequirementsNotifier for Requirements {
    fn changed(&self) {
        // Full = coalesced; closed = shutdown. Neither is worth a log line.
        let _ = self.inner.wake.try_send(());
    }
}

async fn snapshot(inner: &Inner, probe_sizes: bool) -> Result<RequirementsSnapshot> {
    let settings = inner.settings.load_resolved().await?;
    compute::compute(
        compute::Sources {
            settings: &settings,
            models: &inner.models,
            binaries: inner.binaries.as_ref(),
        },
        probe_sizes,
    )
    .await
}

/// One worker pass. A failed recompute logs and skips the broadcast rather than
/// pushing a partial snapshot to every client.
async fn recompute(inner: &Inner) {
    match snapshot(inner, false).await {
        Ok(snap) => inner
            .frames
            .broadcast_all(&ServerToClientFrame::RequirementsSnapshot {
                missing: snap.missing,
                required: snap.required,
            }),
        Err(err) => tracing::warn!(
            target: "loca::requirements",
            "requirements recompute failed: {err}",
        ),
    }
}
