//! The extension supervisor: loca.json bundles, the Deno permission sandbox, and the PTY permission-prompt state machine.
//!
//! Work package **D12** (`docs/rust-port/registry.md`). This module is that
//! package's owned path; nothing outside it writes here.
//!
//! # The whole subsystem is one handle, and that is how it degrades
//!
//! Extensions are DESKTOP ONLY: Android's W^X policy and App Store rule 2.5.2
//! both forbid spawning a downloaded interpreter, so on mobile there is no
//! `deno`, no worker, and no extension. Rather than gating on a capability flag
//! that could be wrong at runtime, the whole subsystem is behind one
//! [`Extensions`] handle that boot either constructs or does not. Unconstructed,
//! the router simply does not merge the two slices, the storage tree keeps its
//! unwired extensions source, and a chat turn sees an empty tool catalog, which
//! is the same thing it already sees on a desktop core with nothing installed.

pub mod broker;
pub mod hash;
pub mod install;
pub mod prompt;
/// Desktop only. Android's W^X policy and App Store rule 2.5.2 both forbid
/// spawning a downloaded interpreter, so on mobile there is no `deno`, no
/// worker, and nothing to give a terminal to. Gating the module out rather than
/// gating its callers keeps it out of the staticlib the App Store review reads,
/// and off the binary-size budget (D-001).
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

/// Embeddings, for the tool-relevance routes.
///
/// Implemented by D5 over its `Embedding` service. Declared here because the
/// consumer declares the trait, so `extensions::` imports no `memories::` type
/// and the module graph stays acyclic.
#[async_trait]
pub trait ToolEmbedder: Send + Sync {
    /// Whether embeddings can be produced right now. A core whose embed model
    /// has not been downloaded answers false, and the reindex route then skips
    /// cleanly rather than raising.
    async fn is_ready(&self) -> bool;
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
    /// The vector paired with its staleness hash, so a stored row can later
    /// detect that the model changed under it.
    async fn embed_with_hash(&self, text: &str) -> Result<Option<(Vec<f32>, String)>>;
    /// The staleness hash for `text` under the ACTIVE model.
    fn source_hash(&self, text: &str) -> String;
}

/// Which extensions' memories are visible, for D5's memories store.
///
/// D5 declares the trait and this implements it, the same direction as
/// [`ToolEmbedder`] runs in reverse: the store cannot import `extensions::`, so
/// the half that knows about extension status lives here.
///
/// **A memory is hidden unless its extension is INSTALLED**, so a downloaded,
/// uninstalled, deleted or drifted extension contributes nothing to a listing or
/// to a prompt, exactly as its tools contribute nothing. The rows survive the
/// hiding, so a later install brings them back with the user's enable flag
/// intact.
///
/// This existed as a design and not as code for the whole port: the trait was
/// shaped from the TypeScript, where the registry is an in-memory map and the
/// lookup is synchronous, and the port made the registry a database. Nothing
/// could implement it, nothing did, and every extension memory stayed active
/// forever. See D-141.
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

/// The other tool provider's catalog.
///
/// Implemented by D11 over its existing inherent `Mcp::list_all_tools`. The
/// merged `/api/v1/tools` route is the one place the two providers meet.
#[async_trait]
pub trait McpTools: Send + Sync {
    async fn list_all_tools(&self) -> Result<Vec<Tool>>;
}

/// The extension subsystem.
///
/// Cheap to clone; every clone drives the same registry, installer and pool.
#[derive(Clone)]
pub struct Extensions {
    inner: Arc<Inner>,
}

struct Inner {
    installer: Installer,
    /// `None` on a core with no worker pool wired. Every use is a
    /// `refresh_permissions`, and with no pool there are no warm workers to
    /// refresh, so absent is the correct answer rather than a missing one.
    pool: Option<WorkerPool>,
    embedder: Option<Arc<dyn ToolEmbedder>>,
    mcp_tools: Option<Arc<dyn McpTools>>,
    /// `None` on a core with no module broker. The only use here is closing an
    /// extension's private SQLite handle before its data directory is removed.
    broker: Option<Arc<broker::Broker>>,
    memories: Arc<dyn install::runner::ExtensionMemories>,
    frames: Arc<FrameBus>,
    /// One compiled-schema cache per core. On the handle rather than on the
    /// pool because argument validation happens BEFORE a worker is asked for:
    /// a call whose arguments are wrong must not spawn a process to find out.
    validators: ArgValidators,
}

/// Assembles an [`Extensions`], so the four optional seams are decided ONCE at
/// boot rather than being mutable on a shared handle.
///
/// Not a stylistic preference: `Extensions` is cloned into the router, the
/// storage tree and the chat orchestrator, and a setter on the handle would
/// mean one of those clones could change what the others see, halfway through a
/// request.
pub struct ExtensionsBuilder {
    inner: Inner,
}

impl ExtensionsBuilder {
    /// Attach the warm-worker pool. Absent means no warm workers, so nothing to
    /// refresh, which is the right answer rather than a missing one.
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

    /// Validate and default the model's arguments against the tool's schema.
    ///
    /// Before a worker is asked for, so arguments that cannot be right never
    /// cost a process start.
    pub fn validate_tool_args(&self, tool: &Tool, arguments_json: &str) -> Result<String> {
        self.inner.validators.validate_and_normalize(
            &tool.id,
            &tool.name,
            &tool.parameters,
            arguments_json,
        )
    }

    /// Run one tool in the sandbox.
    ///
    /// The single execution entry point, so the adapter that presents this
    /// subsystem to the chat orchestrator carries no policy of its own: the
    /// worker key, the call budgets and the module fan-out are all decided
    /// here. `Err` on a core with no pool, which is a core that offered no
    /// extension tools in the first place.
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
        // The broker is what a tool reaches memories, the llm, speech and its
        // own database through. Absent means those requests are refused by the
        // call rather than dispatched, which is what a core without those
        // subsystems should do.
        let modules = self
            .inner
            .broker
            .clone()
            .map(|broker| broker as Arc<dyn worker::call::ModuleDispatch>);
        pool.start_call(spec, events, modules).await
    }

    /// Whether a worker pool is wired into this subsystem.
    ///
    /// Exists for the boot-seam suite (D-165), and it reads the LIVE field
    /// rather than restating what boot intended. Distinct from
    /// `Booted::extension_pool` being populated, which is what the first
    /// version of that test asserted and why it was vacuous: boot can build a
    /// pool, hand it back on `Booted`, and still never call `.pool(...)`. The
    /// consequence of that gap is that `start_tool_call` refuses every call
    /// with `ServerUnavailable` while the frontend holds a pool of warm workers
    /// nothing can reach.
    pub fn has_worker_pool(&self) -> bool {
        self.inner.pool.is_some()
    }

    /// Tear down every warm worker of one extension.
    ///
    /// Called by anything that changes what a worker is allowed to do, because
    /// Deno bakes the permission set into the process at spawn: a grant change
    /// that left the worker running would take effect at the next spawn, which
    /// could be minutes away or never.
    pub async fn refresh_permissions(&self, extension_id: &str) {
        if let Some(pool) = &self.inner.pool {
            pool.refresh_permissions(extension_id).await;
        }
    }

    /// Remove an extension's files and rows entirely.
    pub async fn delete_extension(&self, extension_id: &str) -> Result<()> {
        let extension = self.registry().get_or_err(extension_id).await?;
        // The worker's permission view is refreshed FIRST, so an in-flight
        // worker drops this extension's grants before its rows vanish.
        self.refresh_permissions(extension_id).await;
        self.registry().delete(extension_id).await?;
        self.inner
            .memories
            .remove_extension_memories(extension_id)
            .await;
        // The handle is closed before the directory goes: removing a directory
        // out from under an open SQLite handle leaves it writing into an
        // unlinked file.
        if let Some(broker) = &self.inner.broker {
            broker.close_extension_db(extension_id);
        }
        let data_dir = self.installer().paths().extension_data_dir(extension_id);
        let _ = tokio::fs::remove_dir_all(&data_dir).await;
        let _ = tokio::fs::remove_dir_all(&extension.installed_path).await;
        Ok(())
    }

    /// Revert an installed, dependency-bearing extension to `downloaded`.
    ///
    /// The SOURCE files stay, so a re-install needs no re-download. Only the
    /// generated artefacts go, and both are hash-excluded, so the re-install
    /// re-pins the identical content hash.
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
    ///
    /// Answers whether the install was QUEUED rather than started, which
    /// happens when the `deno` runtime has not been downloaded yet. That is not
    /// a failure and must not read as one: the user pressed a button, the
    /// runtime download is a separate consent-gated step, and the client says
    /// so instead of showing an error.
    ///
    /// Idempotent, and a no-op in `drift`: re-installing over a drifted
    /// extension would re-pin content the user has not reviewed.
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

    /// Turn on every tool of a seeded extension.
    ///
    /// The starter set is default-on; runtime permission prompts still gate
    /// what any of those tools can actually do.
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

    /// Enabled tools of installed extensions.
    ///
    /// The registry's own OS gating applies, so a tool declaring platforms this
    /// host is not in never reaches the relevance filter either.
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

    /// **The test the defect it covers could survive without.**
    ///
    /// D5 already had a provider-gate test, and it passed throughout the whole
    /// period when nothing in production installed a gate at all, because it
    /// drives an injected fake. A seam tested only through a double reports on
    /// the seam rather than on the behaviour, so an unwired seam looks covered
    /// (D-141). This one drives the REAL registry through the REAL
    /// implementation, so unwiring the gate turns it red.
    ///
    /// It also has to be live rather than a boot snapshot: `verify_all_on_boot`
    /// flips an extension to `drift` from a spawned task, minutes after any
    /// snapshot was taken, and a drifted extension's memories reaching a prompt
    /// is the case the gate exists for.
    #[tokio::test]
    async fn an_uninstalled_extensions_memories_leave_every_listing() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Arc::new(Paths::rooted(tmp.path()));
        paths.ensure_dirs().unwrap();
        let db = Db::open_in_memory().unwrap();
        db.call_blocking(|conn| crate::db::migrate(conn)).unwrap();

        // One database, both halves over it: the same shape boot builds.
        let registry = Registry::new(db.clone(), (*paths).clone());
        let store = MemoriesStore::new(Arc::clone(&paths), db.clone());
        store.set_provider_gate(Some(Arc::new(ExtensionProviderGate::new(registry.clone()))));

        // A user memory, as the control. Nothing the gate does may touch it.
        store
            .create(MemoryKind::Knowledge, "Mine", "notes")
            .await
            .unwrap();

        // An extension that has been DOWNLOADED and not installed, shipping one
        // memory. This is what a first boot leaves behind after seeding.
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

        // Hidden, and the row survives. Both halves matter: hidden means it
        // never reaches a prompt, and surviving means a later install restores
        // it with the user's enable flag intact.
        assert_eq!(titles(store.list().await.unwrap()), ["Mine"]);
        assert_eq!(store.all_rows().await.unwrap().len(), 2);

        // Installed: it appears, with no other call and no invalidation step.
        // A cached view would need one, and this is the assertion that would
        // catch a cache that forgot.
        registry
            .set_status("ext-a", ExtensionStatus::Installed)
            .await
            .unwrap();
        let mut visible = titles(store.list().await.unwrap());
        visible.sort();
        assert_eq!(visible, ["Guide", "Mine"]);

        // Drifted: gone again. This is the transition `verify_all_on_boot`
        // makes from a background task, and the one a boot-time snapshot would
        // miss entirely.
        registry.mark_drift("ext-a").await.unwrap();
        assert_eq!(
            titles(store.list().await.unwrap()),
            ["Mine"],
            "a drifted extension's memories were still being listed, which means \
             they were still being injected into prompts"
        );

        // Deleted: still gone, and now for a second reason. An extension the
        // user removed leaves rows behind until the memories store is told, and
        // the gate is what stops those rows being served in the meantime.
        registry.delete("ext-a").await.unwrap();
        assert_eq!(titles(store.list().await.unwrap()), ["Mine"]);
    }
}
