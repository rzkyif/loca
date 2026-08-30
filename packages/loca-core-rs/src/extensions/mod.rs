//! The extension supervisor: loca.json bundles, the Deno permission sandbox,
//! and the PTY permission-prompt state machine.
//!
//! Extensions are DESKTOP ONLY (Android's W^X policy and App Store rule 2.5.2
//! both forbid spawning a downloaded interpreter). The whole subsystem sits
//! behind one [`Extensions`] handle that boot either constructs or does not;
//! unconstructed, the router does not merge the slices and a chat turn sees an
//! empty tool catalog, same as a desktop core with nothing installed.

pub mod broker;
pub mod hash;
pub mod install;
pub mod prompt;
/// Desktop only; gated out of the module tree so it stays out of the mobile
/// staticlib and off the binary-size budget.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub mod pty;
pub mod registry;
pub mod routes;
pub mod sandbox;
pub mod socket;
pub mod tool_filter;
pub mod wire;
pub mod worker;

use std::sync::Arc;

use async_trait::async_trait;

use crate::contract::api::{ExtensionStatus, Tool};
use crate::error::Result;
use crate::transport::frame_bus::FrameBus;

use install::runner::Installer;
use install::seed::BUILTIN_EXTENSION_ID;
use registry::Registry;
use worker::args::ArgValidators;
use worker::call::{CallController, CallEvent, ToolCallStart};
use worker::pool::WorkerPool;

/// Embeddings, for the tool-relevance routes. Declared here so `extensions::`
/// imports no `memories::` type and the module graph stays acyclic.
#[async_trait]
pub trait ToolEmbedder: Send + Sync {
    /// False when the embed model is not downloaded; the reindex route then
    /// skips cleanly rather than raising.
    async fn is_ready(&self) -> bool;
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
    /// The vector paired with its staleness hash.
    async fn embed_with_hash(&self, text: &str) -> Result<Option<(Vec<f32>, String)>>;
    /// The staleness hash for `text` under the ACTIVE model.
    fn source_hash(&self, text: &str) -> String;
}

/// Which extensions' memories are visible to the memories store.
///
/// A memory is hidden unless its extension is INSTALLED: a downloaded,
/// uninstalled, deleted or drifted extension contributes nothing, exactly as
/// its tools contribute nothing. The rows survive, so a later install brings
/// them back with the user's enable flag intact.
pub struct ExtensionProviderGate {
    registry: Registry,
}

impl ExtensionProviderGate {
    pub fn new(registry: Registry) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl crate::memories::ProviderGate for ExtensionProviderGate {
    async fn active_providers(&self) -> Result<std::collections::HashSet<String>> {
        self.registry.installed_ids().await
    }
}

/// The other tool provider's catalog, for the merged `/api/v1/tools` route.
#[async_trait]
pub trait McpTools: Send + Sync {
    async fn list_all_tools(&self) -> Result<Vec<Tool>>;
}

/// The extension subsystem. Cheap to clone; every clone drives the same
/// registry, installer and pool.
#[derive(Clone)]
pub struct Extensions {
    inner: Arc<Inner>,
}

struct Inner {
    installer: Installer,
    /// `None` on a core with no worker pool wired: no warm workers to refresh.
    pool: Option<WorkerPool>,
    embedder: Option<Arc<dyn ToolEmbedder>>,
    mcp_tools: Option<Arc<dyn McpTools>>,
    /// `None` on a core with no module broker.
    broker: Option<Arc<broker::Broker>>,
    memories: Arc<dyn install::runner::ExtensionMemories>,
    frames: Arc<FrameBus>,
    /// On the handle rather than the pool: argument validation happens BEFORE
    /// a worker is asked for.
    validators: ArgValidators,
}

/// Assembles an [`Extensions`], so the optional seams are decided ONCE at boot
/// rather than being mutable on a handle cloned into the router, the storage
/// tree and the chat orchestrator.
pub struct ExtensionsBuilder {
    inner: Inner,
}

impl ExtensionsBuilder {
    pub fn pool(mut self, pool: WorkerPool) -> Self {
        self.inner.pool = Some(pool);
        self
    }

    pub fn embedder(mut self, embedder: Arc<dyn ToolEmbedder>) -> Self {
        self.inner.embedder = Some(embedder);
        self
    }

    pub fn mcp_tools(mut self, mcp: Arc<dyn McpTools>) -> Self {
        self.inner.mcp_tools = Some(mcp);
        self
    }

    pub fn broker(mut self, broker: Arc<broker::Broker>) -> Self {
        self.inner.broker = Some(broker);
        self
    }

    pub fn build(self) -> Extensions {
        Extensions {
            inner: Arc::new(self.inner),
        }
    }
}

impl Extensions {
    pub fn builder(
        installer: Installer,
        memories: Arc<dyn install::runner::ExtensionMemories>,
        frames: Arc<FrameBus>,
    ) -> ExtensionsBuilder {
        ExtensionsBuilder {
            inner: Inner {
                installer,
                pool: None,
                embedder: None,
                mcp_tools: None,
                broker: None,
                memories,
                frames,
                validators: ArgValidators::new(),
            },
        }
    }

    pub fn registry(&self) -> &Registry {
        self.inner.installer.registry()
    }

    pub fn installer(&self) -> &Installer {
        &self.inner.installer
    }

    pub fn embedder(&self) -> Option<&Arc<dyn ToolEmbedder>> {
        self.inner.embedder.as_ref()
    }

    pub fn mcp_tools(&self) -> Option<&Arc<dyn McpTools>> {
        self.inner.mcp_tools.as_ref()
    }

    pub fn frames(&self) -> &FrameBus {
        &self.inner.frames
    }

    /// Validate and default the model's arguments against the tool's schema,
    /// before a worker is asked for.
    pub fn validate_tool_args(&self, tool: &Tool, arguments_json: &str) -> Result<String> {
        self.inner.validators.validate_and_normalize(
            &tool.id,
            &tool.name,
            &tool.parameters,
            arguments_json,
        )
    }

    /// Run one tool in the sandbox. The single execution entry point: worker
    /// key, call budgets and module fan-out are all decided here. `Err` on a
    /// core with no pool.
    pub async fn start_tool_call(
        &self,
        spec: ToolCallStart,
        events: tokio::sync::mpsc::UnboundedSender<CallEvent>,
    ) -> Result<CallController> {
        let Some(pool) = &self.inner.pool else {
            return Err(crate::error::AppError::new(
                crate::error::ErrorCode::ServerUnavailable,
                "this core has no extension worker pool",
            ));
        };
        // No broker means module requests are refused by the call rather than
        // dispatched.
        let modules = self
            .inner
            .broker
            .clone()
            .map(|broker| broker as Arc<dyn worker::call::ModuleDispatch>);
        pool.start_call(spec, events, modules).await
    }

    /// Whether a worker pool is wired in. Reads the LIVE field: boot can build
    /// a pool and still never call `.pool(...)`.
    pub fn has_worker_pool(&self) -> bool {
        self.inner.pool.is_some()
    }

    /// Tear down every warm worker of one extension. Deno bakes the permission
    /// set into the process at spawn, so a grant change must recycle workers.
    pub async fn refresh_permissions(&self, extension_id: &str) {
        if let Some(pool) = &self.inner.pool {
            pool.refresh_permissions(extension_id).await;
        }
    }

    /// Remove an extension's files and rows entirely.
    pub async fn delete_extension(&self, extension_id: &str) -> Result<()> {
        let extension = self.registry().get_or_err(extension_id).await?;
        // Workers first, so an in-flight worker drops its grants before its
        // rows vanish.
        self.refresh_permissions(extension_id).await;
        self.registry().delete(extension_id).await?;
        self.inner
            .memories
            .remove_extension_memories(extension_id)
            .await;
        // Close the SQLite handle before the directory goes, or it keeps
        // writing into an unlinked file.
        if let Some(broker) = &self.inner.broker {
            broker.close_extension_db(extension_id);
        }
        let data_dir = self.installer().paths().extension_data_dir(extension_id);
        let _ = tokio::fs::remove_dir_all(&data_dir).await;
        let _ = tokio::fs::remove_dir_all(&extension.installed_path).await;
        Ok(())
    }

    /// Revert an installed, dependency-bearing extension to `downloaded`. The
    /// SOURCE files stay; only the hash-excluded generated artefacts go.
    pub async fn uninstall_extension(&self, extension_id: &str) -> Result<()> {
        let extension = self.registry().get_or_err(extension_id).await?;
        if extension.status != ExtensionStatus::Installed || !extension.has_deps {
            return Err(crate::error::AppError::new(
                crate::error::ErrorCode::ValidationError,
                format!("extension {extension_id} has nothing to uninstall (delete it instead)"),
            ));
        }
        self.refresh_permissions(extension_id).await;
        let folder = std::path::Path::new(&extension.installed_path);
        let _ = tokio::fs::remove_dir_all(folder.join("node_modules")).await;
        let _ = tokio::fs::remove_file(folder.join("deno.lock")).await;
        self.registry().mark_downloaded(extension_id).await?;
        Ok(())
    }

    /// Install the built-in's dependencies at the user's explicit request.
    /// Answers whether the install was QUEUED (the `deno` runtime is not
    /// downloaded yet), which is not a failure. Idempotent; a no-op in `drift`,
    /// since re-installing would re-pin content the user has not reviewed.
    pub async fn request_builtin_install(&self) -> Result<bool> {
        let Some(builtin) = self.registry().get(BUILTIN_EXTENSION_ID).await? else {
            return Ok(false);
        };
        match builtin.status {
            ExtensionStatus::Installed => {
                self.enable_seeded_defaults(BUILTIN_EXTENSION_ID).await;
                Ok(false)
            }
            ExtensionStatus::Downloaded => {
                if crate::sidecars::deno::resolve_worker_deno(self.installer().paths()).is_err() {
                    return Ok(true);
                }
                self.installer().start_install_deps(BUILTIN_EXTENSION_ID)?;
                Ok(false)
            }
            ExtensionStatus::Drift => Ok(false),
        }
    }

    /// Turn on every tool of a seeded extension; runtime permission prompts
    /// still gate what those tools can do.
    async fn enable_seeded_defaults(&self, extension_id: &str) {
        let Ok(tools) = self.registry().list_tools(extension_id).await else {
            return;
        };
        for tool in tools {
            if !tool.enabled {
                let _ = self
                    .registry()
                    .set_tool_enabled(extension_id, &tool.name, true)
                    .await;
            }
        }
    }

    /// Enabled tools of installed extensions, after the registry's OS gating.
    pub async fn enabled_tools(&self) -> Result<Vec<Tool>> {
        Ok(self
            .registry()
            .list_all_tools()
            .await?
            .into_iter()
            .filter(|tool| tool.enabled)
            .collect())
    }
}

/// The storage screen's view of the installed extensions.
#[async_trait]
impl crate::storage::deps::ExtensionsSource for Extensions {
    async fn list(&self) -> Result<Vec<crate::storage::deps::ExtensionSummary>> {
        let mut out = Vec::new();
        for extension in self.registry().list().await? {
            // A read failure for one extension must not blank the whole view;
            // it is reported as having no tools, which only makes it MORE
            // deletable.
            let has_enabled_tools = self
                .registry()
                .list_tools(&extension.id)
                .await
                .unwrap_or_default()
                .iter()
                .any(|tool| tool.enabled);
            out.push(crate::storage::deps::ExtensionSummary {
                builtin: extension.id == BUILTIN_EXTENSION_ID,
                seeded: extension.source == crate::contract::api::ExtensionSource::Seeded,
                display_name: extension.display_name,
                id: extension.id,
                has_enabled_tools,
            });
        }
        Ok(out)
    }

    /// The full removal, not the dependency-revert: the storage screen offers
    /// to reclaim the disk.
    async fn delete(&self, extension_id: &str) -> Result<()> {
        self.delete_extension(extension_id).await
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::sync::Arc;

    use crate::contract::api::{ExtensionStatus, MemoryKind};
    use crate::db::Db;
    use crate::memories::{MemoriesStore, MemoryDecl};
    use crate::paths::Paths;

    use super::registry::{ExtensionInsert, Registry};
    use super::ExtensionProviderGate;

    /// Drives the REAL registry through the REAL gate (a fake-driven test
    /// passed while nothing in production installed a gate), and live rather
    /// than as a boot snapshot, since `verify_all_on_boot` flips extensions to
    /// `drift` from a spawned task.
    #[tokio::test]
    async fn an_uninstalled_extensions_memories_leave_every_listing() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Arc::new(Paths::rooted(tmp.path()));
        paths.ensure_dirs().unwrap();
        let db = Db::open_in_memory().unwrap();
        db.call_blocking(|conn| crate::db::migrate(conn)).unwrap();

        let registry = Registry::new(db.clone(), (*paths).clone());
        let store = MemoriesStore::new(Arc::clone(&paths), db.clone());
        store.set_provider_gate(Some(Arc::new(ExtensionProviderGate::new(registry.clone()))));

        // A user memory, as the control.
        store
            .create(MemoryKind::Knowledge, "Mine", "notes")
            .await
            .unwrap();

        // Downloaded and not installed, shipping one memory.
        let install_dir = paths.extensions_dir().join("ext-a");
        std::fs::create_dir_all(&install_dir).unwrap();
        std::fs::write(install_dir.join("guide.md"), "# Guide").unwrap();
        registry
            .upsert_extension(ExtensionInsert {
                id: "ext-a".to_string(),
                source: crate::contract::api::ExtensionSource::Npm,
                display_name: "Ext A".to_string(),
                description: None,
                version: "1.0.0".to_string(),
                installed_path: install_dir.to_string_lossy().into_owned(),
                manifest_hash: String::new(),
                content_hash: String::new(),
                status: ExtensionStatus::Downloaded,
                has_deps: false,
                has_database: false,
            })
            .await
            .unwrap();
        store
            .register_extension_memories(
                "ext-a",
                paths.extensions_dir(),
                &[MemoryDecl {
                    kind: MemoryKind::Knowledge,
                    path: "guide.md".to_string(),
                }],
            )
            .await
            .unwrap();

        let titles = |rows: Vec<crate::contract::api::MemoryMeta>| {
            rows.into_iter().map(|m| m.title).collect::<Vec<_>>()
        };

        // Hidden, and the row survives.
        assert_eq!(titles(store.list().await.unwrap()), ["Mine"]);
        assert_eq!(store.all_rows().await.unwrap().len(), 2);

        // Installed: appears with no invalidation step.
        registry
            .set_status("ext-a", ExtensionStatus::Installed)
            .await
            .unwrap();
        let mut visible = titles(store.list().await.unwrap());
        visible.sort();
        assert_eq!(visible, ["Guide", "Mine"]);

        registry.mark_drift("ext-a").await.unwrap();
        assert_eq!(
            titles(store.list().await.unwrap()),
            ["Mine"],
            "a drifted extension's memories were still being listed, which means \
             they were still being injected into prompts"
        );

        registry.delete("ext-a").await.unwrap();
        assert_eq!(titles(store.list().await.unwrap()), ["Mine"]);
    }
}
