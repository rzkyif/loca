//! Which download-queue movements can change `missing`. The queue snapshots on
//! every progress tick, so recompute only on transitions that can flip a
//! requirement.

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

    /// Record `entries` and answer whether a recompute is due: a row entering a
    /// terminal state ([`is_recompute_edge`]), or a tracked row disappearing
    /// (clearing a completed download can re-expose a model file as missing).
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

/// Whether one row's status transition should trigger a recompute: model rows
/// entering a terminal state. Binary rows never do, since `Completed` means the
/// archive landed but not yet extracted, and a "still missing" broadcast would
/// race the post-extract answer; binary readiness comes from the install lifecycle.
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
