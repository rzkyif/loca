//! Which download-queue movements can change `missing`.
//!
//! Ports `ws/hub.ts:50-64` and `:453-465`. The queue emits a whole snapshot on
//! every progress tick, and recomputing on each one would rebuild the model and
//! binary sets and broadcast to every client several times a second for a bar
//! that moved. So the boot subscription keeps the previous status per row and
//! recomputes only on the transitions that can actually flip a requirement.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::contract::api::{DownloadEntry, DownloadEntryDestination, DownloadStatus};

/// Per-row status, so a snapshot can be compared with the one before it.
#[derive(Default)]
pub struct DownloadEdges {
    last: Mutex<HashMap<String, DownloadStatus>>,
}

impl DownloadEdges {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record `entries` and answer whether anything in it should trigger a
    /// requirements recompute.
    ///
    /// Two things count. A row ENTERING a terminal state, per
    /// [`is_recompute_edge`]. And a tracked row DISAPPEARING: the user clearing
    /// a completed download can re-expose a model file as missing, and no
    /// per-row transition describes a row that is simply gone.
    pub fn note(&self, entries: &[DownloadEntry]) -> bool {
        let mut last = lock(&self.last);
        let mut changed = false;
        for entry in entries {
            let previous = last.get(&entry.id).copied();
            if is_recompute_edge(entry.destination, previous, entry.status) {
                changed = true;
            }
            last.insert(entry.id.clone(), entry.status);
        }
        let before = last.len();
        last.retain(|id, _| entries.iter().any(|e| &e.id == id));
        changed || last.len() != before
    }
}

/// Whether one row's status transition should trigger a recompute.
///
/// MODEL rows recompute on entering a terminal state: `Completed` means the
/// file is now present, and `Error` means the snapshot should surface the
/// failure as retryable.
///
/// **BINARY rows never recompute here, and dropping that guard reintroduces a
/// race.** A binaries row reaches `Completed` when its ARCHIVE has landed,
/// which is before extraction and before the installed version is recorded
/// (`downloads/binaries/installer.rs`, where `finish` extracts and records after
/// the queue hands the archive back). Recomputing there would broadcast a
/// "still missing" snapshot that races the real, post-extract answer, and
/// whichever arrived last would win. Binary readiness comes only from the
/// install lifecycle.
pub fn is_recompute_edge(
    destination: DownloadEntryDestination,
    previous: Option<DownloadStatus>,
    next: DownloadStatus,
) -> bool {
    if destination == DownloadEntryDestination::Binaries {
        return false;
    }
    !previous.is_some_and(terminal) && terminal(next)
}

fn terminal(status: DownloadStatus) -> bool {
    matches!(status, DownloadStatus::Completed | DownloadStatus::Error)
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}
