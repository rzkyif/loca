//! The seams onto the subsystems that own what the storage view reports on.
//!
//! Each subsystem the storage view reports on becomes a narrow trait here, with
//! a stub in this module and a real implementation in the module that owns the
//! fact. Two rules keep the split honest:
//!
//! - **Facts cross the seam, copy does not.** [`ExtensionSummary`] carries
//!   `seeded` and `has_enabled_tools`; this package turns those into the
//!   tooltip a user reads. The extension supervisor does not learn the storage
//!   view's wording, and this module does not learn what "seeded" means.
//! - **Reductions live with the knowledge.** [`BinariesSource::locked_kinds`]
//!   returns "required by the current settings, or run by a live sidecar"
//!   already computed, rather than teaching this module which sidecar runs
//!   which binary.
//!
//! # Unwired seams
//!
//! [`StorageDeps::unwired`]'s enumerating seams report nothing, so their
//! categories are empty: no sessions source means no session nodes, which means
//! nothing to delete.
//!
//! The two that do not degrade quietly are deliberate. [`ModelLocks`] fails,
//! because this package builds the models category itself and listing every
//! model as unlocked would invite a delete that should have been refused.
//! [`SettingsReset`] fails, because a factory reset that silently does nothing
//! is worse than one that reports it could not run.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;

use crate::error::{internal, Result};

/// One installed helper binary, as the binaries manager knows it.
#[derive(Clone, Debug)]
pub struct InstalledBinary {
    /// The `BinaryKind` string, which is also the node's display name.
    pub kind: String,
    pub path: PathBuf,
}

/// One session, reduced to what the storage view shows.
#[derive(Clone, Debug)]
pub struct SessionSummary {
    pub id: String,
    /// Empty for an untitled session; the view supplies the placeholder.
    pub title: String,
    pub updated_at_ms: i64,
    /// The session document plus its attachments, measured by the store that
    /// owns the layout rather than re-derived here.
    pub size_bytes: u64,
}

/// One installed extension, reduced to the facts the lock rules need.
#[derive(Clone, Debug)]
pub struct ExtensionSummary {
    pub id: String,
    /// Empty falls back to the id, matching `tk.displayName || tk.id`.
    pub display_name: String,
    /// Installed by Core rather than by the user, so removing it through the
    /// storage screen would only invite it back on the next boot.
    pub seeded: bool,
    /// The built-in extension specifically, which gets its own wording.
    pub builtin: bool,
    pub has_enabled_tools: bool,
}

/// Which model files the current settings require, already resolved to
/// absolute paths on disk.
#[async_trait]
pub trait ModelLocks: Send + Sync {
    async fn required_model_paths(&self) -> Result<Vec<PathBuf>>;
}

/// The installed helper binaries and which of them are pinned in place.
#[async_trait]
pub trait BinariesSource: Send + Sync {
    async fn installed(&self) -> Result<Vec<InstalledBinary>>;
    /// Required by the current settings, or run by a sidecar that is up.
    async fn locked_kinds(&self) -> Result<HashSet<String>>;
}

/// The session store, plus the one fact the chat orchestrator owns.
#[async_trait]
pub trait SessionsSource: Send + Sync {
    async fn list(&self) -> Result<Vec<SessionSummary>>;
    /// Sessions with an agent mid-turn on them.
    async fn active_ids(&self) -> Result<HashSet<String>>;
    async fn delete(&self, session_id: &str) -> Result<()>;
}

/// The extension registry and its uninstall path.
#[async_trait]
pub trait ExtensionsSource: Send + Sync {
    async fn list(&self) -> Result<Vec<ExtensionSummary>>;
    async fn delete(&self, extension_id: &str) -> Result<()>;
}

/// The factory reset behind the `settings` category: core settings back to
/// defaults and every client's settings wiped. Clearing the secrets vault is
/// this package's own call, since the vault is a foundation rather than a
/// domain package.
#[async_trait]
pub trait SettingsReset: Send + Sync {
    async fn factory_reset(&self) -> Result<()>;
}

/// Recompute requirements after a reclaim, so a model that was just deleted
/// resurfaces as something to download.
pub trait RequirementsNotifier: Send + Sync {
    fn changed(&self);
}

/// Everything the storage routes need from the rest of the core.
///
/// Built by `boot` and handed to the route slice. Each field starts unwired and
/// is replaced by one builder call as its owning package lands, so no later
/// package has to edit this one.
#[derive(Clone)]
pub struct StorageDeps {
    pub models: Arc<dyn ModelLocks>,
    pub binaries: Arc<dyn BinariesSource>,
    pub sessions: Arc<dyn SessionsSource>,
    pub extensions: Arc<dyn ExtensionsSource>,
    pub settings: Arc<dyn SettingsReset>,
    pub requirements: Arc<dyn RequirementsNotifier>,
}

impl StorageDeps {
    /// Every seam stubbed. See the module docs for what each stub reports and
    /// why two of them fail rather than returning nothing.
    pub fn unwired() -> Self {
        Self {
            models: Arc::new(Unwired),
            binaries: Arc::new(Unwired),
            sessions: Arc::new(Unwired),
            extensions: Arc::new(Unwired),
            settings: Arc::new(Unwired),
            requirements: Arc::new(Unwired),
        }
    }

    pub fn with_models(mut self, source: Arc<dyn ModelLocks>) -> Self {
        self.models = source;
        self
    }

    pub fn with_binaries(mut self, source: Arc<dyn BinariesSource>) -> Self {
        self.binaries = source;
        self
    }

    pub fn with_sessions(mut self, source: Arc<dyn SessionsSource>) -> Self {
        self.sessions = source;
        self
    }

    pub fn with_extensions(mut self, source: Arc<dyn ExtensionsSource>) -> Self {
        self.extensions = source;
        self
    }

    pub fn with_settings(mut self, source: Arc<dyn SettingsReset>) -> Self {
        self.settings = source;
        self
    }

    pub fn with_requirements(mut self, source: Arc<dyn RequirementsNotifier>) -> Self {
        self.requirements = source;
        self
    }
}

impl Default for StorageDeps {
    fn default() -> Self {
        Self::unwired()
    }
}

/// The stub behind every seam a core has not wired yet.
pub struct Unwired;

fn unwired<T>(what: &str) -> Result<T> {
    Err(internal(format!("{what} is not wired into this core")))
}

#[async_trait]
impl ModelLocks for Unwired {
    /// Fails rather than reporting an empty set. An empty set would mean "no
    /// model is required by anything", which reads as "every model is safe to
    /// delete" and is the one place an unwired seam could cost a user data.
    async fn required_model_paths(&self) -> Result<Vec<PathBuf>> {
        unwired("the model lock source")
    }
}

#[async_trait]
impl BinariesSource for Unwired {
    async fn installed(&self) -> Result<Vec<InstalledBinary>> {
        Ok(Vec::new())
    }

    async fn locked_kinds(&self) -> Result<HashSet<String>> {
        Ok(HashSet::new())
    }
}

#[async_trait]
impl SessionsSource for Unwired {
    async fn list(&self) -> Result<Vec<SessionSummary>> {
        Ok(Vec::new())
    }

    async fn active_ids(&self) -> Result<HashSet<String>> {
        Ok(HashSet::new())
    }

    /// Unreachable while `list` is empty: with no nodes there is nothing in the
    /// delete index to resolve to a session.
    async fn delete(&self, _session_id: &str) -> Result<()> {
        unwired("the sessions store")
    }
}

#[async_trait]
impl ExtensionsSource for Unwired {
    async fn list(&self) -> Result<Vec<ExtensionSummary>> {
        Ok(Vec::new())
    }

    async fn delete(&self, _extension_id: &str) -> Result<()> {
        unwired("the extension registry")
    }
}

#[async_trait]
impl SettingsReset for Unwired {
    /// Reachable, unlike the other two: this package builds the settings
    /// category itself from a pair of file sizes, so the node exists and can be
    /// asked to clear. Reporting success without resetting anything would be a
    /// lie the user cannot see.
    async fn factory_reset(&self) -> Result<()> {
        unwired("the settings store")
    }
}

impl RequirementsNotifier for Unwired {
    fn changed(&self) {}
}
