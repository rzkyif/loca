//! The seam onto the download manager, which is D7's.
//!
//! Six of this package's seventeen routes are D7-backed but mounted here, the
//! way the TypeScript mounts them under `/api/v1/models`. Rather than wait for
//! D7, each `downloadManager()` call becomes one method on a narrow trait, with
//! a stub here and the real implementation in `downloads/`. That is the pattern
//! D9 established in `storage/deps.rs`.
//!
//! # What the stub answers, and why each one
//!
//! The split is between an answer that is TRUE on a core that has downloaded
//! nothing, and an answer that would be a lie:
//!
//! | method                | unwired              | reasoning                                                          |
//! | --------------------- | -------------------- | ------------------------------------------------------------------ |
//! | `models_snapshot`     | empty                | nothing was enqueued, so an empty queue is the correct answer      |
//! | `reconcile_completed` | success              | there is nothing to reconcile                                      |
//! | `cancel`/`retry`/`remove` | no-op            | already a no-op for an unknown id in the TypeScript                |
//! | `probe`               | `server_unavailable` | a wrong "already have" is a model that silently never downloads    |
//! | `enqueue_models`      | `server_unavailable` | same, one step later                                               |
//!
//! That split is what lets thirteen of the seventeen routes carry real
//! conformance coverage before D7 exists.
//!
//! `probe` is on the seam rather than implemented here even though half of it
//! is a local presence check. The other half is `parseSource`'s URL derivation
//! and the `x-linked-size` HEAD, both in `downloads/sources.ts`. Splitting it
//! would put the `@spec` to relative-path mapping in two places, which the
//! TypeScript already does once too often (`resolveHfPath` and `parseSource`
//! derive the same path by different code).

use std::sync::Arc;

use async_trait::async_trait;

use crate::contract::api::{DownloadEntry, DownloadPlan};
use crate::error::{AppError, ErrorCode, Result};
use crate::storage::deps::RequirementsNotifier;

/// The download queue, as the model routes use it.
#[async_trait]
pub trait Downloads: Send + Sync {
    /// Presence, and a size hint for anything missing. `network` gates the
    /// outbound HEAD only: presence is always a local check, so a caller that
    /// must not reach the network (the always-on requirements snapshot) passes
    /// false and still gets a truthful `already_have`.
    async fn probe(&self, sources: &[String], network: bool) -> Result<Vec<DownloadPlan>>;

    /// Enqueue `(source, group)` pairs into the models destination. Returns one
    /// queue-row id per file, which is what cancel, retry and remove accept.
    ///
    /// Fallible where the TypeScript's is not. There it is fire-and-forget,
    /// with failures surfacing on the `downloads.snapshot` broadcast the caller
    /// is already subscribed to. Here the stub has to be able to refuse, because
    /// returning an empty `jobIds` would report that nothing needed downloading.
    fn enqueue_models(&self, items: &[(String, String)]) -> Result<Vec<String>>;

    /// Every queue row whose destination is `models`.
    fn models_snapshot(&self) -> Vec<DownloadEntry>;

    fn cancel(&self, id: &str);
    fn retry(&self, id: &str);
    fn remove(&self, id: &str);

    /// Drop completed rows whose file is no longer on disk, so a download whose
    /// model was deleted self-clears instead of lingering in the list.
    async fn reconcile_completed(&self) -> Result<()>;
}

/// Everything the model routes need from the rest of the core.
///
/// Built by `boot` and handed to the route slice, following `StorageDeps`. The
/// requirements notifier is D9's trait rather than a second one of this
/// package's own: two traits for one notification is how two subsystems end up
/// disagreeing about whether a requirement changed.
#[derive(Clone)]
pub struct ModelsDeps {
    pub downloads: Arc<dyn Downloads>,
    pub requirements: Arc<dyn RequirementsNotifier>,
}

impl ModelsDeps {
    pub fn unwired() -> Self {
        Self {
            downloads: Arc::new(Unwired),
            requirements: Arc::new(crate::storage::deps::Unwired),
        }
    }

    pub fn with_downloads(mut self, downloads: Arc<dyn Downloads>) -> Self {
        self.downloads = downloads;
        self
    }

    pub fn with_requirements(mut self, requirements: Arc<dyn RequirementsNotifier>) -> Self {
        self.requirements = requirements;
        self
    }
}

impl Default for ModelsDeps {
    fn default() -> Self {
        Self::unwired()
    }
}

/// The stub behind the download seam until D7 lands.
pub struct Unwired;

fn unavailable<T>() -> Result<T> {
    Err(AppError::new(
        ErrorCode::ServerUnavailable,
        "the download manager is not wired into this core",
    ))
}

#[async_trait]
impl Downloads for Unwired {
    async fn probe(&self, _sources: &[String], _network: bool) -> Result<Vec<DownloadPlan>> {
        unavailable()
    }

    fn enqueue_models(&self, _items: &[(String, String)]) -> Result<Vec<String>> {
        unavailable()
    }

    fn models_snapshot(&self) -> Vec<DownloadEntry> {
        Vec::new()
    }

    fn cancel(&self, _id: &str) {}
    fn retry(&self, _id: &str) {}
    fn remove(&self, _id: &str) {}

    async fn reconcile_completed(&self) -> Result<()> {
        Ok(())
    }
}
