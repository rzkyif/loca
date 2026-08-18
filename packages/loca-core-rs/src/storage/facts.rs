//! The snapshot of everything the tree build needs from other subsystems,
//! gathered once before the walk starts.
//!
//! This is the half of the module that awaits. Once [`Facts::gather`] returns,
//! building the tree is a pure synchronous function over the filesystem, which
//! is what lets every walk and lock rule be tested with literal facts over a
//! tempdir and no trait objects at all.
//!
//! A seam that fails is logged once and degrades to nothing, matching the
//! TypeScript's `safeBuildNodes`, whose comment is the rule this follows: a
//! single broken category can never blank the whole storage view.

use std::collections::HashSet;

use super::deps::{ExtensionSummary, InstalledBinary, SessionSummary, StorageDeps};
use super::safety;

/// What the other subsystems answered, at one moment.
#[derive(Clone, Debug, Default)]
pub struct Facts {
    /// Normalised absolute paths of the model files the current settings need.
    ///
    /// `None` means the lock source could not be reached, which takes the
    /// models category out of the view entirely rather than showing every model
    /// as unlocked.
    pub required_model_paths: Option<HashSet<String>>,
    pub binaries: Vec<InstalledBinary>,
    pub locked_binary_kinds: HashSet<String>,
    pub sessions: Vec<SessionSummary>,
    pub active_session_ids: HashSet<String>,
    pub extensions: Vec<ExtensionSummary>,
}

impl Facts {
    pub async fn gather(deps: &StorageDeps) -> Self {
        Self {
            required_model_paths: match deps.models.required_model_paths().await {
                Ok(paths) => Some(
                    paths
                        .iter()
                        .map(|path| safety::normalize(&path.to_string_lossy()))
                        .collect(),
                ),
                Err(err) => {
                    warn("model locks", &err);
                    None
                }
            },
            binaries: or_empty("installed binaries", deps.binaries.installed().await),
            locked_binary_kinds: or_empty("binary locks", deps.binaries.locked_kinds().await),
            sessions: or_empty("sessions", deps.sessions.list().await),
            active_session_ids: or_empty("active sessions", deps.sessions.active_ids().await),
            extensions: or_empty("extensions", deps.extensions.list().await),
        }
    }
}

fn or_empty<T: Default>(what: &str, result: crate::error::Result<T>) -> T {
    match result {
        Ok(value) => value,
        Err(err) => {
            warn(what, &err);
            T::default()
        }
    }
}

fn warn(what: &str, err: &crate::error::AppError) {
    tracing::warn!(target: "loca::storage", "storage {what} unavailable: {}", err.message);
}
