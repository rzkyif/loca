//! Requirements snapshot, greetings, and automated sessions.
//!
//! Work package **D8** (`docs/rust-port/registry.md`). This module is that
//! package's owned path; nothing outside it writes here, with one carve-out:
//! `chat/automated.rs` is D8's file living in D4's module, per D-090 and D-094.
//!
//! # What this module is for
//!
//! One question, asked from several places: **what does the current
//! configuration need on disk that is not there yet?** The client renders the
//! answer as a single pending-downloads popup and gates the app on it, so
//! everything that can change the answer has to converge on one computation.
//! The things that can change it are a settings write, a model download
//! finishing or failing, a binary install finishing or failing, and a reclaim
//! through the storage screen.
//!
//! Greetings live here because the greeting route is the other half of "is this
//! core ready to be used yet": it is the request a client makes on the same edge
//! that starts the model load.
//!
//! # The recompute is coalesced, and the reason is a settings burst
//!
//! Applying a preset writes many keys, and each write is a change edge. Without
//! coalescing that is one full recompute and one broadcast per key. [`Requirements::changed`]
//! therefore never computes inline: it pokes a one-slot channel, so a burst of N
//! notifications costs at most two recomputes (the one running and one trailing
//! rerun), which is the bound `services/requirements.ts:150-172` states.

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
    /// One slot. A full buffer means a rerun is already queued, so a dropped
    /// send IS the coalescing rather than a lost notification.
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
    /// Build the service and start its recompute worker.
    ///
    /// The worker holds a `Weak`, and the only `Sender` lives in `Inner`, so
    /// dropping the last handle closes the channel and the task ends. That
    /// matters for tests more than for production: a strong reference here would
    /// leak one live task per test.
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

    /// The snapshot for the current configuration.
    ///
    /// `probe_sizes` is the ONLY thing that reaches the network here, and it is
    /// set only by `?probe=1`, which the client sends only when the user opens
    /// the download confirmation modal.
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

/// Recompute and broadcast, from any of the change triggers.
///
/// Deliberately infallible and non-blocking: every caller is a fire-and-forget
/// notification on a mutation path that has already succeeded, and making them
/// wait on a model probe would put a filesystem walk in the middle of a settings
/// PATCH.
impl RequirementsNotifier for Requirements {
    fn changed(&self) {
        // A full channel is the coalescing working, and a closed one means the
        // worker is gone, which only happens at shutdown. Neither is worth a log
        // line on a path this hot.
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

/// One pass of the worker. A failed recompute logs and skips the broadcast
/// rather than propagating: there is nobody to propagate to, and broadcasting a
/// partial snapshot would tell every client the requirements changed to
/// something wrong.
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
