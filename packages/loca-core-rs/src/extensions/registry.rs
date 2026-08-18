//! The extensions, tools, grants and embeddings registry.
//!
//! Ports `packages/loca-core/src/extensions/registry.ts`. Four tables, already
//! present in `schema.sql`, plus the lifecycle rules over them.
//!
//! # The lifecycle, and which transitions are gates
//!
//! | from         | to           | where                                          |
//! | ------------ | ------------ | ---------------------------------------------- |
//! | none         | `downloaded` | [`Registry::upsert_extension`]                 |
//! | `downloaded` | `installed`  | [`Registry::mark_installed`], the ONLY hash pin |
//! | `installed`  | `drift`      | [`Registry::verify_all_on_boot`], [`Registry::verify_hash_fresh`] |
//! | `drift`      | `installed`  | [`Registry::repin_and_clear`], user-confirmed only |
//! | `installed`  | `downloaded` | [`Registry::mark_downloaded`], unpins the hash |
//!
//! Two of those are security gates rather than bookkeeping. **Drift never
//! auto-clears**: leaving it requires the user pressing confirm-reenable, which
//! re-pins whatever is on disk now. And **flagging drift always disables every
//! tool in the same breath**, which is why the two steps are one private
//! function here rather than two calls each caller has to remember.
//!
//! # There is no singleton
//!
//! `extensionsRegistry()` and its `__resetForTesting` do not port (D-010): the
//! handle is constructed at boot and threaded, so `cargo test` can run a
//! database test per thread instead of forcing every one onto a shared handle.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use async_trait::async_trait;
use rusqlite::{Connection, OptionalExtension};
use serde_json::{Map, Value};

use crate::contract::api::{
    Extension, ExtensionSource, ExtensionStatus, Grant, GrantState, PermissionDecl, PermissionKind,
    Tool, ToolProviderKind, UndeclaredPolicy,
};
use crate::db::Db;
use crate::error::{AppError, ErrorCode, Result};
use crate::paths::Paths;

use super::broker::{GateFacts, RegistryFacts};
use super::hash::{hash_extension, stat_signature};
use super::sandbox::permission_key;

/// A tool's id is derived, not generated: `<extension id>::<tool name>`.
///
/// That is load-bearing in [`Registry::replace_tools`], which deletes and
/// re-inserts every row: a re-inserted tool keeps the id its grant rows already
/// reference, so grants can be carried across without a rewrite.
pub fn tool_id(extension_id: &str, tool_name: &str) -> String {
    format!("{extension_id}::{tool_name}")
}

/// What an install writes about one extension.
#[derive(Debug, Clone)]
pub struct ExtensionInsert {
    pub id: String,
    pub source: ExtensionSource,
    pub display_name: String,
    pub description: Option<String>,
    pub version: String,
    pub installed_path: String,
    pub manifest_hash: String,
    /// Empty until the extension is installed. Download leaves it unpinned.
    pub content_hash: String,
    pub status: ExtensionStatus,
    pub has_deps: bool,
    pub has_database: bool,
}

/// What a register writes about one tool.
#[derive(Debug, Clone)]
pub struct ToolInsert {
    pub name: String,
    pub description: String,
    pub parameters: Map<String, Value>,
    pub triggers: Vec<String>,
    pub fn_export: String,
    pub always_available: bool,
    pub platforms: Vec<String>,
    pub required_permissions: Vec<PermissionDecl>,
}

/// One grant the user decided on, as the grants route supplies it.
#[derive(Debug, Clone)]
pub struct GrantEntry {
    pub key: String,
    pub kind: PermissionKind,
    pub state: GrantState,
}

/// A stored tool embedding.
#[derive(Debug, Clone)]
pub struct StoredEmbedding {
    pub vector: Vec<f32>,
    pub source_hash: String,
}

/// The per-call re-hash skip key: the stat signature that was seen, and the
/// hash it verified against.
///
/// BOTH halves are compared. The hash matters because a re-pin (an update, a
/// confirm-reenable) changes what "verified" means, and a cache keyed on the
/// signature alone would keep answering for the old pin.
#[derive(Debug, Clone)]
struct Verified {
    signature: String,
    hash: String,
}

/// Cheap to clone, and the clones SHARE the verification cache rather than each
/// getting their own. That is the load-bearing half: `forget_verified` is what
/// invalidates an entry when a grant, an enable or a reinstall changes what
/// "verified" means, and a second independent cache would keep answering from a
/// map that no mutation ever reaches. Boot hands one to the installer and the
/// same one to the module broker's `RegistryFacts`.
#[derive(Clone)]
pub struct Registry {
    db: Db,
    paths: Paths,
    /// The host's tool-platform tokens, resolved once at construction.
    host_platforms: Vec<String>,
    verified: Arc<Mutex<HashMap<String, Verified>>>,
}

impl Registry {
    pub fn new(db: Db, paths: Paths) -> Self {
        Self::with_platforms(db, paths, host_platforms())
    }

    /// The platform tokens are injected rather than read inside, so the OS
    /// gating can be tested for a host this test is not running on. Reading
    /// them from the process would make every gating test assert whatever the
    /// developer's laptop happens to be.
    pub fn with_platforms(db: Db, paths: Paths, host_platforms: Vec<String>) -> Self {
        Self {
            db,
            paths,
            host_platforms,
            verified: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Where an extension's files live.
    pub fn install_path(&self, extension_id: &str) -> PathBuf {
        self.paths.extension_dir(extension_id)
    }

    // --- extension level ------------------------------------------------------

    pub async fn list(&self) -> Result<Vec<Extension>> {
        self.db.call(|conn| select_extensions(conn, None)).await
    }

    pub async fn get(&self, id: &str) -> Result<Option<Extension>> {
        let id = id.to_string();
        Ok(self
            .db
            .call(move |conn| select_extensions(conn, Some(&id)))
            .await?
            .into_iter()
            .next())
    }

    /// The ids of every INSTALLED extension.
    ///
    /// One query and one snapshot, for [`super::ExtensionProviderGate`]: a
    /// memory listing asks once rather than once per row, and cannot answer from
    /// two different worlds because an uninstall landed between two of its
    /// lookups.
    ///
    /// `installed` reaches the SQL through [`enum_str`] rather than as a
    /// literal, so the status vocabulary has exactly one spelling. A literal
    /// here would be a string that no compiler and no test relates to
    /// `ExtensionStatus`, which is the failure mode this whole seam exists to
    /// avoid re-introducing (D-141).
    pub async fn installed_ids(&self) -> Result<HashSet<String>> {
        let installed = enum_str(&ExtensionStatus::Installed);
        self.db
            .call(move |conn| {
                let mut stmt = conn.prepare("SELECT id FROM extensions WHERE status = ?1")?;
                let ids = stmt
                    .query_map([&installed], |row| row.get::<_, String>(0))?
                    .collect::<rusqlite::Result<HashSet<String>>>()?;
                Ok(ids)
            })
            .await
    }

    pub async fn get_or_err(&self, id: &str) -> Result<Extension> {
        self.get(id).await?.ok_or_else(|| {
            AppError::new(
                ErrorCode::ExtensionNotFound,
                format!("extension {id} not found"),
            )
        })
    }

    pub async fn upsert_extension(&self, input: ExtensionInsert) -> Result<()> {
        let now = now_ms();
        self.db
            .call(move |conn| {
                // One statement rather than a read followed by a branch: the
                // TypeScript's get-then-insert-or-update races itself, and the
                // race is reachable because two downloads of the same id can be
                // queued at once.
                //
                // `installed_at_ms` is set only on the INSERT, so a re-download
                // keeps the date the user first added the extension.
                conn.execute(
                    "INSERT INTO extensions
                       (id, source, display_name, description, version, installed_path,
                        manifest_hash, content_hash, status, has_deps, has_database,
                        installed_at_ms, updated_at_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?12)
                     ON CONFLICT(id) DO UPDATE SET
                       source = excluded.source,
                       display_name = excluded.display_name,
                       description = excluded.description,
                       version = excluded.version,
                       installed_path = excluded.installed_path,
                       manifest_hash = excluded.manifest_hash,
                       content_hash = excluded.content_hash,
                       status = excluded.status,
                       has_deps = excluded.has_deps,
                       has_database = excluded.has_database,
                       updated_at_ms = excluded.updated_at_ms",
                    rusqlite::params![
                        input.id,
                        enum_str(&input.source),
                        input.display_name,
                        input.description,
                        input.version,
                        input.installed_path,
                        input.manifest_hash,
                        input.content_hash,
                        enum_str(&input.status),
                        i64::from(input.has_deps),
                        i64::from(input.has_database),
                        now,
                    ],
                )?;
                Ok(())
            })
            .await
    }

    pub async fn set_status(&self, id: &str, status: ExtensionStatus) -> Result<()> {
        self.update_extension(id, "status = ?1", vec![enum_str(&status)])
            .await
    }

    pub async fn set_undeclared_policy(&self, id: &str, policy: UndeclaredPolicy) -> Result<()> {
        self.update_extension(id, "undeclared_policy = ?1", vec![enum_str(&policy)])
            .await
    }

    /// Install finished: pin the content hash and flip to `installed`.
    ///
    /// The ONLY place a hash is pinned outside [`Registry::repin_and_clear`].
    pub async fn mark_installed(&self, id: &str, content_hash: &str) -> Result<()> {
        self.update_extension(
            id,
            "status = 'installed', content_hash = ?1",
            vec![content_hash.to_string()],
        )
        .await?;
        // Paired with the hash comparison in `verify_fresh`'s cache lookup: see
        // the comment there. Each masks the other, so a mutation of either
        // alone survives and only removing both is observable.
        self.forget_verified(id);
        Ok(())
    }

    /// Out-of-band content change: flip to `drift`, keeping the pinned hash for
    /// reference, and leave it for the user.
    ///
    /// Prefer [`Registry::flag_drift`], which also takes the tools out of
    /// exposure. This is public only because confirm-reenable's inverse is.
    pub async fn mark_drift(&self, id: &str) -> Result<()> {
        self.update_extension(id, "status = 'drift'", Vec::new())
            .await?;
        self.forget_verified(id);
        Ok(())
    }

    /// Flag drift AND disable every tool.
    ///
    /// The two halves are one function because every drift detection has to do
    /// both: a drifted extension whose tools stayed enabled is still offered to
    /// the model, which is the whole failure the hash gate exists to prevent.
    async fn flag_drift(&self, id: &str) -> Result<()> {
        self.mark_drift(id).await?;
        self.disable_all_tools(id).await
    }

    /// Confirm-reenable: re-pin what is on disk NOW as trusted and clear drift.
    ///
    /// Reached only from `POST /:id/confirm-reenable`. Never automatic: an
    /// auto-clear on a later match would let an attacker restore the original
    /// bytes after running once and leave no trace.
    pub async fn repin_and_clear(&self, id: &str, content_hash: &str) -> Result<()> {
        self.mark_installed(id, content_hash).await
    }

    /// Uninstall (revert): drop back to `downloaded` and unpin the hash.
    ///
    /// Tool `enabled` flags are deliberately KEPT, so a later re-install
    /// restores the user's prior selection rather than making them redo it.
    pub async fn mark_downloaded(&self, id: &str) -> Result<()> {
        self.update_extension(id, "status = 'downloaded', content_hash = ''", Vec::new())
            .await?;
        self.forget_verified(id);
        Ok(())
    }

    pub async fn delete(&self, id: &str) -> Result<()> {
        let owned = id.to_string();
        self.db
            .call(move |conn| {
                // CASCADE drops tools, tool_embeddings and grants.
                conn.execute("DELETE FROM extensions WHERE id = ?1", [owned.as_str()])?;
                Ok(())
            })
            .await?;
        // Hygiene rather than a correctness guard, and it is worth saying which:
        // every path back to `verify_fresh` for this id has to go through
        // `mark_installed` first, which invalidates anyway. What this stops is a
        // map entry per deleted extension living for the process's lifetime.
        // The mutation that removes it SURVIVES, correctly.
        self.forget_verified(id);
        Ok(())
    }

    /// One `UPDATE`, with `updated_at_ms` appended.
    ///
    /// `set` is a literal from this module and every value is bound, so the
    /// interpolation carries no caller input. Bumping `updated_at_ms` lives here
    /// rather than at each call site because a write that forgets it leaves the
    /// UI showing a stale timestamp with nothing to notice it.
    async fn update_extension(&self, id: &str, set: &str, args: Vec<String>) -> Result<()> {
        let id = id.to_string();
        let sql = format!(
            "UPDATE extensions SET {set}, updated_at_ms = ?{} WHERE id = ?{}",
            args.len() + 1,
            args.len() + 2
        );
        let now = now_ms();
        self.db
            .call(move |conn| {
                let mut binds: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
                for arg in args {
                    binds.push(Box::new(arg));
                }
                binds.push(Box::new(now));
                binds.push(Box::new(id));
                conn.execute(&sql, rusqlite::params_from_iter(binds.iter()))?;
                Ok(())
            })
            .await
    }

    fn forget_verified(&self, id: &str) {
        lock(&self.verified).remove(id);
    }

    // --- the hash gate --------------------------------------------------------

    /// Boot-time integrity check over every INSTALLED extension.
    ///
    /// Returns the ids that drifted, for boot logging. A `downloaded` extension
    /// has no pinned hash to check and a `drift` one is already flagged, so
    /// both are skipped rather than re-examined.
    pub async fn verify_all_on_boot(&self) -> Result<Vec<String>> {
        let mut drifted = Vec::new();
        for extension in self.list().await? {
            if extension.status != ExtensionStatus::Installed {
                continue;
            }
            let root = PathBuf::from(&extension.installed_path);
            let actual = tokio::task::spawn_blocking(move || hash_extension(&root))
                .await
                .map_err(|err| {
                    AppError::new(ErrorCode::InternalError, format!("hash task failed: {err}"))
                })?;
            let drift_reason = match actual {
                // A hash that cannot be computed is treated as drift, not as a
                // pass. Failing open here would make "delete a file the walk
                // needs" a way to skip verification entirely.
                Err(err) => Some(format!("hash failed: {}", err.message)),
                Ok(actual) if actual != extension.content_hash => Some(format!(
                    "content drift: stored={} actual={}",
                    short(&extension.content_hash),
                    short(&actual)
                )),
                Ok(_) => None,
            };
            if let Some(reason) = drift_reason {
                tracing::warn!(
                    target: "loca::extensions",
                    "[{}] {reason}; disabling tools", extension.id,
                );
                self.flag_drift(&extension.id).await?;
                drifted.push(extension.id);
            }
        }
        Ok(drifted)
    }

    /// Per-call integrity check, run before a tool call is dispatched.
    ///
    /// [`Registry::verify_all_on_boot`] only runs at startup, so without this
    /// an extension tampered with at any point afterwards executes freely until
    /// the next restart. On a mismatch it flags drift, disables every tool, and
    /// raises `extension_hash_drift`; there is no auto-clear.
    ///
    /// The skip cache is what makes this affordable. Its limitation is real and
    /// documented on [`stat_signature`]: a modification preserving the count,
    /// the summed size and the newest mtime is invisible to it until the next
    /// boot.
    pub async fn verify_fresh(&self, extension_id: &str) -> Result<()> {
        // An unknown extension is the tool lookup's problem, and it has already
        // answered `tool_not_found`. Returning Ok here rather than inventing a
        // second error for it matches the specification.
        let Some(extension) = self.get(extension_id).await? else {
            return Ok(());
        };
        // Defence in depth: only an installed extension is ever exposed to the
        // model, so reaching here in any other state means a caller skipped the
        // exposure gate.
        if extension.status != ExtensionStatus::Installed {
            return Err(drift_error(format!(
                "extension {extension_id} is not installed (status {})",
                enum_str(&extension.status)
            )));
        }

        let root = PathBuf::from(&extension.installed_path);
        let signature = {
            let root = root.clone();
            tokio::task::spawn_blocking(move || stat_signature(&root))
                .await
                .map_err(|err| {
                    AppError::new(ErrorCode::InternalError, format!("stat task failed: {err}"))
                })?
        };
        if let Some(cached) = lock(&self.verified).get(extension_id) {
            // The hash comparison is REDUNDANT with `forget_verified`, which
            // every write path that changes `content_hash` calls, and it is
            // kept anyway. Neither guard is individually observable: mutate
            // either one alone and the other covers it, which is exactly what
            // makes the pair worth naming rather than simplifying. What it
            // defends is a `content_hash` that changed without going through
            // this handle at all, and a future write path whose author forgets
            // the invalidation call. Removing BOTH is the mutation that fails.
            if cached.signature == signature && cached.hash == extension.content_hash {
                return Ok(());
            }
        }

        let actual = tokio::task::spawn_blocking(move || hash_extension(&root))
            .await
            .map_err(|err| {
                AppError::new(ErrorCode::InternalError, format!("hash task failed: {err}"))
            })?;
        let actual = match actual {
            Ok(actual) => actual,
            Err(err) => {
                self.flag_drift(extension_id).await?;
                return Err(drift_error(format!(
                    "extension {extension_id} hash failed: {}",
                    err.message
                )));
            }
        };
        if actual == extension.content_hash {
            lock(&self.verified).insert(
                extension_id.to_string(),
                Verified {
                    signature,
                    hash: actual,
                },
            );
            return Ok(());
        }
        self.flag_drift(extension_id).await?;
        Err(drift_error(format!(
            "extension {extension_id} content changed since it was trusted"
        )))
    }

    // --- tool level -----------------------------------------------------------

    /// Atomically replace an extension's tools.
    ///
    /// Two user choices survive a re-download, and one deliberately does not:
    ///
    /// - `enabled` and `always_available` are carried across BY NAME, so an
    ///   update does not silently switch a tool the user turned on back off.
    /// - **Grants survive only when the canonical permission-key SET is
    ///   unchanged.** A changed set drops every grant so the user re-reviews.
    ///   Carrying grants across by key would keep an approval the user gave for
    ///   a different declaration, which is the whole point of the permission
    ///   review at install time.
    pub async fn replace_tools(&self, extension_id: &str, tools: Vec<ToolInsert>) -> Result<()> {
        let extension_id = extension_id.to_string();
        self.db
            .call(move |conn| {
                let tx = conn.transaction()?;
                let prior = prior_tools(&tx, &extension_id)?;
                tx.execute("DELETE FROM tools WHERE extension_id = ?1", [&extension_id])?;

                for tool in &tools {
                    let id = tool_id(&extension_id, &tool.name);
                    let previous = prior.get(&tool.name);
                    tx.execute(
                        "INSERT INTO tools
                           (id, extension_id, name, description, parameters_json, triggers_json,
                            fn_export, always_available, platforms_json, enabled,
                            required_permissions_json)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                        rusqlite::params![
                            id,
                            extension_id,
                            tool.name,
                            tool.description,
                            serde_json::to_string(&tool.parameters)?,
                            serde_json::to_string(&tool.triggers)?,
                            tool.fn_export,
                            previous.map_or(i64::from(tool.always_available), |p| {
                                i64::from(p.always_available)
                            }),
                            serde_json::to_string(&tool.platforms)?,
                            // A brand-new tool is OFF. An extension that adds a
                            // tool in an update must not have it enabled by an
                            // upgrade the user only agreed to as an upgrade.
                            previous.map_or(0, |p| i64::from(p.enabled)),
                            serde_json::to_string(&tool.required_permissions)?,
                        ],
                    )?;

                    let Some(previous) = previous else { continue };
                    if previous.permission_key_set != key_set(&tool.required_permissions) {
                        continue;
                    }
                    for grant in &previous.grants {
                        tx.execute(
                            "INSERT INTO grants
                               (tool_id, permission_key, permission_kind, state, granted_at_ms)
                             VALUES (?1, ?2, ?3, ?4, ?5)",
                            rusqlite::params![
                                id,
                                grant.permission_key,
                                enum_str(&grant.permission_kind),
                                enum_str(&grant.state),
                                grant.granted_at_ms,
                            ],
                        )?;
                    }
                }
                tx.commit()?;
                Ok(())
            })
            .await
    }

    /// The tools of one extension, with their grants attached.
    ///
    /// Tools the host OS does not support are filtered out HERE rather than at
    /// each consumer, so an OS-gated tool is invisible to chat selection, the
    /// routes and the Tools UI alike.
    pub async fn list_tools(&self, extension_id: &str) -> Result<Vec<Tool>> {
        let extension_id = extension_id.to_string();
        let host = self.host_platforms.clone();
        self.db
            .call(move |conn| {
                let tools = select_tools(
                    conn,
                    "extension_id = ?1 ORDER BY name ASC",
                    &[&extension_id],
                )?;
                keep_supported(conn, tools, &host)
            })
            .await
    }

    /// Every tool of every INSTALLED extension, flat, tagged with its
    /// provider's display name.
    ///
    /// A `downloaded` or `drift` extension contributes nothing, exactly as it
    /// contributes nothing to chat exposure.
    pub async fn list_all_tools(&self) -> Result<Vec<Tool>> {
        let host = self.host_platforms.clone();
        self.db
            .call(move |conn| {
                let extensions = select_extensions(conn, None)?;
                let mut out = Vec::new();
                for extension in extensions {
                    if extension.status != ExtensionStatus::Installed {
                        continue;
                    }
                    let tools = select_tools(
                        conn,
                        "extension_id = ?1 ORDER BY name ASC",
                        &[&extension.id],
                    )?;
                    for mut tool in keep_supported(conn, tools, &host)? {
                        tool.provider_name = Some(extension.display_name.clone());
                        out.push(tool);
                    }
                }
                Ok(out)
            })
            .await
    }

    /// One tool by id, or `None` for a tool this host does not support.
    ///
    /// The platform filter is repeated here on purpose: dispatch takes a tool
    /// id directly, so filtering only in the listing would let an OS-gated tool
    /// run whenever its id was already known.
    pub async fn get_tool(&self, id: &str) -> Result<Option<Tool>> {
        let id = id.to_string();
        let host = self.host_platforms.clone();
        self.db
            .call(move |conn| {
                let tools = select_tools(conn, "id = ?1", &[&id])?;
                Ok(keep_supported(conn, tools, &host)?.into_iter().next())
            })
            .await
    }

    pub async fn set_tool_enabled(&self, extension_id: &str, name: &str, on: bool) -> Result<()> {
        self.set_tool_flag("enabled", extension_id, name, on).await
    }

    pub async fn set_tool_always_available(
        &self,
        extension_id: &str,
        name: &str,
        on: bool,
    ) -> Result<()> {
        self.set_tool_flag("always_available", extension_id, name, on)
            .await
    }

    async fn set_tool_flag(
        &self,
        column: &'static str,
        extension_id: &str,
        name: &str,
        on: bool,
    ) -> Result<()> {
        let extension_id = extension_id.to_string();
        let name = name.to_string();
        self.db
            .call(move |conn| {
                // `column` is a `&'static str` from this module, never user
                // input, which is what makes the interpolation safe.
                conn.execute(
                    &format!(
                        "UPDATE tools SET {column} = ?1 WHERE extension_id = ?2 AND name = ?3"
                    ),
                    rusqlite::params![i64::from(on), extension_id, name],
                )?;
                Ok(())
            })
            .await
    }

    /// Take every tool of one extension out of exposure in a single statement.
    pub async fn disable_all_tools(&self, extension_id: &str) -> Result<()> {
        let extension_id = extension_id.to_string();
        self.db
            .call(move |conn| {
                conn.execute(
                    "UPDATE tools SET enabled = 0 WHERE extension_id = ?1",
                    [&extension_id],
                )?;
                Ok(())
            })
            .await
    }

    // --- grants ---------------------------------------------------------------

    pub async fn set_grants(&self, tool: &str, entries: Vec<GrantEntry>) -> Result<Vec<Grant>> {
        let tool = tool.to_string();
        let now = now_ms();
        self.db
            .call(move |conn| {
                let tx = conn.transaction()?;
                for entry in entries {
                    tx.execute(
                        "INSERT INTO grants
                           (tool_id, permission_key, permission_kind, state, granted_at_ms)
                         VALUES (?1, ?2, ?3, ?4, ?5)
                         ON CONFLICT(tool_id, permission_key) DO UPDATE SET
                           state = excluded.state,
                           granted_at_ms = excluded.granted_at_ms",
                        rusqlite::params![
                            tool,
                            entry.key,
                            enum_str(&entry.kind),
                            enum_str(&entry.state),
                            now,
                        ],
                    )?;
                }
                tx.commit()?;
                select_grants(conn, &tool)
            })
            .await
    }

    pub async fn list_grants_for_tool(&self, tool: &str) -> Result<Vec<Grant>> {
        let tool = tool.to_string();
        self.db.call(move |conn| select_grants(conn, &tool)).await
    }

    // --- embeddings -----------------------------------------------------------

    pub async fn store_embedding(
        &self,
        tool: &str,
        vector: Vec<f32>,
        source_hash: &str,
    ) -> Result<()> {
        let tool = tool.to_string();
        let source_hash = source_hash.to_string();
        self.db
            .call(move |conn| {
                conn.execute(
                    "INSERT INTO tool_embeddings (tool_id, dim, vector, source_hash)
                     VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(tool_id) DO UPDATE SET
                       dim = excluded.dim,
                       vector = excluded.vector,
                       source_hash = excluded.source_hash",
                    rusqlite::params![
                        tool,
                        i64::try_from(vector.len()).unwrap_or(i64::MAX),
                        encode_vector(&vector),
                        source_hash,
                    ],
                )?;
                Ok(())
            })
            .await
    }

    pub async fn load_embedding(&self, tool: &str) -> Result<Option<StoredEmbedding>> {
        let tool = tool.to_string();
        self.db
            .call(move |conn| {
                conn.query_row(
                    "SELECT vector, source_hash FROM tool_embeddings WHERE tool_id = ?1",
                    [&tool],
                    |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()?
                .map(|(bytes, source_hash)| decode_embedding(&bytes, source_hash))
                .transpose()
            })
            .await
    }

    /// Every stored embedding in one query.
    ///
    /// Relevance runs on every chat turn over every enabled tool, and one
    /// prepared statement per tool was N round trips per turn.
    pub async fn load_all_embeddings(&self) -> Result<HashMap<String, StoredEmbedding>> {
        self.db
            .call(|conn| {
                let mut stmt =
                    conn.prepare("SELECT tool_id, vector, source_hash FROM tool_embeddings")?;
                let rows = stmt.query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Vec<u8>>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })?;
                let mut out = HashMap::new();
                for row in rows {
                    let (id, bytes, source_hash) = row?;
                    out.insert(id, decode_embedding(&bytes, source_hash)?);
                }
                Ok(out)
            })
            .await
    }
}

#[async_trait]
impl RegistryFacts for Registry {
    async fn gate_facts(&self, extension_id: &str, tool_name: &str) -> Result<GateFacts> {
        let extension = self.get_or_err(extension_id).await?;
        // The tool lookup goes through `list_tools`, so an OS-gated tool has no
        // facts at all rather than facts with an empty permission set: an empty
        // `required` reads as "undeclared" to the gate, which the `ask` policy
        // would then offer to the user for a tool that cannot run here.
        let tool = self
            .list_tools(extension_id)
            .await?
            .into_iter()
            .find(|tool| tool.name == tool_name)
            .ok_or_else(|| {
                AppError::new(
                    ErrorCode::ToolNotFound,
                    format!("tool {tool_name} not found in extension {extension_id}"),
                )
            })?;
        Ok(GateFacts {
            has_database: extension.has_database,
            undeclared_policy: extension.undeclared_policy,
            required: tool.required_permissions,
            grants: tool.grants,
        })
    }
}

// --- row mapping ------------------------------------------------------------

const EXTENSION_COLUMNS: &str = "id, source, display_name, description, version, installed_path,
     manifest_hash, content_hash, status, has_deps, has_database, undeclared_policy,
     (SELECT COUNT(*) FROM tools WHERE extension_id = extensions.id) AS tool_count,
     (SELECT COUNT(*) FROM tools WHERE extension_id = extensions.id AND enabled = 1)
       AS enabled_tool_count,
     installed_at_ms, updated_at_ms";

fn select_extensions(conn: &Connection, id: Option<&str>) -> Result<Vec<Extension>> {
    // `COLLATE NOCASE` on the sort, so the list reads the way a user expects
    // rather than putting every capitalised name first.
    let sql = match id {
        Some(_) => format!("SELECT {EXTENSION_COLUMNS} FROM extensions WHERE id = ?1"),
        None => format!(
            "SELECT {EXTENSION_COLUMNS} FROM extensions ORDER BY display_name COLLATE NOCASE ASC"
        ),
    };
    let mut stmt = conn.prepare(&sql)?;
    let map = |row: &rusqlite::Row<'_>| -> rusqlite::Result<Extension> {
        Ok(Extension {
            id: row.get(0)?,
            source: parse_enum(1, &row.get::<_, String>(1)?)?,
            display_name: row.get(2)?,
            description: row.get(3)?,
            version: row.get(4)?,
            installed_path: row.get(5)?,
            manifest_hash: row.get(6)?,
            content_hash: row.get(7)?,
            status: parse_enum(8, &row.get::<_, String>(8)?)?,
            has_deps: row.get::<_, i64>(9)? == 1,
            has_database: row.get::<_, i64>(10)? == 1,
            undeclared_policy: parse_enum(11, &row.get::<_, String>(11)?)?,
            tool_count: row.get(12)?,
            enabled_tool_count: row.get(13)?,
            installed_at_ms: row.get(14)?,
            updated_at_ms: row.get(15)?,
            // Populated by the route that asks for it, never by the row.
            tools: None,
        })
    };
    let rows = match id {
        Some(id) => stmt
            .query_map([id], map)?
            .collect::<rusqlite::Result<_>>()?,
        None => stmt.query_map([], map)?.collect::<rusqlite::Result<_>>()?,
    };
    Ok(rows)
}

const TOOL_COLUMNS: &str = "id, extension_id, name, description, parameters_json, triggers_json,
     fn_export, always_available, platforms_json, enabled, required_permissions_json";

fn select_tools(
    conn: &Connection,
    where_clause: &str,
    params: &[&dyn rusqlite::ToSql],
) -> Result<Vec<Tool>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {TOOL_COLUMNS} FROM tools WHERE {where_clause}"
    ))?;
    let rows = stmt.query_map(params, |row| {
        Ok(Tool {
            id: row.get(0)?,
            extension_id: row.get(1)?,
            provider_kind: ToolProviderKind::Extension,
            name: row.get(2)?,
            description: row.get(3)?,
            parameters: json_of(&row.get::<_, String>(4)?),
            triggers: json_of(&row.get::<_, String>(5)?),
            fn_export: row.get(6)?,
            always_available: row.get::<_, i64>(7)? == 1,
            platforms: json_of(&row.get::<_, String>(8)?),
            enabled: row.get::<_, i64>(9)? == 1,
            required_permissions: json_of(&row.get::<_, String>(10)?),
            // Both are derived at read time by the layer that has the grants:
            // `missing_required` depends on current grant state, so a row
            // cannot answer it.
            missing_required: Vec::new(),
            grants: Vec::new(),
            provider_name: None,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Drop the tools this host does not support, then attach each survivor's
/// grants.
///
/// In that order: attaching grants to a tool that is about to be dropped is a
/// query per tool for nothing.
fn keep_supported(conn: &Connection, tools: Vec<Tool>, host: &[String]) -> Result<Vec<Tool>> {
    tools
        .into_iter()
        .filter(|tool| tool_platform_supported(&tool.platforms, host))
        .map(|mut tool| {
            tool.grants = select_grants(conn, &tool.id)?;
            Ok(tool)
        })
        .collect()
}

/// An empty declaration means every platform.
///
/// The empty case is not an accident of the data model: most tools are pure
/// TypeScript and run anywhere, so declaring nothing is the common case and
/// must not read as "runs nowhere".
pub fn tool_platform_supported(declared: &[String], host: &[String]) -> bool {
    declared.is_empty() || declared.iter().any(|p| host.iter().any(|h| h == p))
}

/// The host's platform tokens.
///
/// The only one needing runtime detection is Linux's display server: an X11
/// window can be positioned from the command line and a Wayland one cannot, so
/// a window-managing tool targets `linux_x11` specifically.
pub fn host_platforms() -> Vec<String> {
    if cfg!(target_os = "macos") {
        return vec!["darwin".to_string()];
    }
    if cfg!(target_os = "windows") {
        return vec!["windows".to_string()];
    }
    let wayland = !std::env::var("WAYLAND_DISPLAY")
        .unwrap_or_default()
        .is_empty()
        || std::env::var("XDG_SESSION_TYPE")
            .unwrap_or_default()
            .eq_ignore_ascii_case("wayland");
    vec![
        "linux".to_string(),
        if wayland {
            "linux_wayland"
        } else {
            "linux_x11"
        }
        .to_string(),
    ]
}

fn select_grants(conn: &Connection, tool: &str) -> Result<Vec<Grant>> {
    let mut stmt = conn.prepare(
        "SELECT tool_id, permission_key, permission_kind, state, granted_at_ms
         FROM grants WHERE tool_id = ?1",
    )?;
    let rows = stmt.query_map([tool], |row| {
        Ok(Grant {
            tool_id: row.get(0)?,
            permission_key: row.get(1)?,
            permission_kind: parse_enum(2, &row.get::<_, String>(2)?)?,
            state: parse_enum(3, &row.get::<_, String>(3)?)?,
            granted_at_ms: row.get(4)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// What [`Registry::replace_tools`] needs to know about the row it is replacing.
struct PriorTool {
    enabled: bool,
    always_available: bool,
    permission_key_set: String,
    grants: Vec<Grant>,
}

fn prior_tools(conn: &Connection, extension_id: &str) -> Result<HashMap<String, PriorTool>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, enabled, always_available, required_permissions_json
         FROM tools WHERE extension_id = ?1",
    )?;
    let rows: Vec<(String, String, i64, i64, String)> = stmt
        .query_map([extension_id], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;

    let mut out = HashMap::new();
    for (id, name, enabled, always_available, required_json) in rows {
        let grants = select_grants(conn, &id)?;
        let declarations: Vec<PermissionDecl> = json_of(&required_json);
        out.insert(
            name,
            PriorTool {
                permission_key_set: key_set(&declarations),
                enabled: enabled == 1,
                always_available: always_available == 1,
                grants,
            },
        );
    }
    Ok(out)
}

/// The canonical, order-independent signature of a required-permission set.
///
/// Sorted and joined so two manifests that declare the same permissions in a
/// different order count as unchanged, which is what stops a cosmetic reorder
/// from throwing away every grant the user gave.
fn key_set(declarations: &[PermissionDecl]) -> String {
    let mut keys: Vec<String> = declarations.iter().map(permission_key).collect();
    keys.sort();
    keys.join("|")
}

/// A stored JSON column, or the type's default when it will not parse.
///
/// Defaulting rather than failing is deliberate and narrow: every one of these
/// columns is written by this module from a typed value, so an unparseable one
/// means the file was edited by hand or corrupted, and answering with an empty
/// list keeps the rest of the extension list readable in the UI where the user
/// can remove it.
fn json_of<T: serde::de::DeserializeOwned + Default>(raw: &str) -> T {
    serde_json::from_str(raw).unwrap_or_default()
}

/// The wire spelling of a contract enum.
///
/// Via serde rather than a hand-written match, so a variant added to the
/// contract cannot be persisted under a name this module invented.
fn enum_str<T: serde::Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(Value::String(text)) => text,
        _ => String::new(),
    }
}

/// The inverse, for a column this module itself wrote.
///
/// A value that does not parse is an ERROR rather than a fallback, and that is
/// the deliberate half. Every one of these columns is written here from a typed
/// value, so an unreadable one means the database was edited by hand or
/// corrupted, and there is no least-privileged answer for `source` or
/// `permission_kind` to fall back to. The specification casts the string
/// blindly (`row.status as ExtensionStatus`), which produces a value no branch
/// matches and a tool that is neither exposed nor reported.
fn parse_enum<T: serde::de::DeserializeOwned>(column: usize, raw: &str) -> rusqlite::Result<T> {
    serde_json::from_value(Value::String(raw.to_string())).map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(column, rusqlite::types::Type::Text, err.into())
    })
}

/// The embedding, as little-endian `f32` bytes.
///
/// Explicitly little-endian rather than `to_ne_bytes`: the database file is
/// copied between machines by the storage browser and by a user moving
/// `~/.loca`, and a native-endian blob would decode to noise on the other side
/// rather than failing loudly.
fn encode_vector(vector: &[f32]) -> Vec<u8> {
    vector.iter().flat_map(|v| v.to_le_bytes()).collect()
}

fn decode_embedding(bytes: &[u8], source_hash: String) -> Result<StoredEmbedding> {
    if !bytes.len().is_multiple_of(4) {
        return Err(AppError::new(
            ErrorCode::InternalError,
            "tool embedding blob is not a whole number of f32 values",
        ));
    }
    Ok(StoredEmbedding {
        vector: bytes
            .chunks_exact(4)
            .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
            .collect(),
        source_hash,
    })
}

fn drift_error(message: impl Into<String>) -> AppError {
    AppError::new(ErrorCode::ExtensionHashDrift, message)
}

fn short(hash: &str) -> &str {
    &hash[..hash.len().min(12)]
}

fn lock<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
    value.lock().unwrap_or_else(PoisonError::into_inner)
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registry")
            .field("host_platforms", &self.host_platforms)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::contract::api::PermissionDeclAccess;

    fn registry(dir: &tempfile::TempDir) -> Registry {
        let db = Db::open_in_memory().unwrap();
        db.call_blocking(|conn| crate::db::migrate(conn)).unwrap();
        Registry::with_platforms(db, Paths::rooted(dir.path()), vec!["darwin".to_string()])
    }

    fn insert(id: &str, status: ExtensionStatus, path: &std::path::Path) -> ExtensionInsert {
        ExtensionInsert {
            id: id.to_string(),
            source: ExtensionSource::Local,
            display_name: format!("The {id}"),
            description: None,
            version: "1.0.0".to_string(),
            installed_path: path.to_string_lossy().into_owned(),
            manifest_hash: "mh".to_string(),
            content_hash: String::new(),
            status,
            has_deps: false,
            has_database: false,
        }
    }

    fn tool(name: &str, permissions: Vec<PermissionDecl>) -> ToolInsert {
        ToolInsert {
            name: name.to_string(),
            description: format!("does {name}"),
            parameters: Map::new(),
            triggers: vec![name.to_string()],
            fn_export: "default".to_string(),
            always_available: false,
            platforms: Vec::new(),
            required_permissions: permissions,
        }
    }

    fn net(host: &str) -> PermissionDecl {
        PermissionDecl::Net {
            host: host.to_string(),
            optional: None,
            ports: Vec::new(),
            reason: "to fetch".to_string(),
        }
    }

    /// `PermissionDecl` carries no `PartialEq`, and comparing the wire form is
    /// the truer comparison anyway: it is what the column stores.
    fn same(left: &[PermissionDecl], right: &[PermissionDecl]) -> bool {
        serde_json::to_value(left).ok() == serde_json::to_value(right).ok()
    }

    /// A tiny extension on disk, so the hash gate has something real to hash.
    fn write_extension(root: &std::path::Path, body: &str) {
        std::fs::create_dir_all(root).unwrap();
        std::fs::write(root.join("main.ts"), body).unwrap();
    }

    #[tokio::test]
    async fn an_extension_round_trips_through_the_row_and_back() {
        let dir = tempfile::tempdir().unwrap();
        let reg = registry(&dir);
        reg.upsert_extension(insert("a", ExtensionStatus::Downloaded, dir.path()))
            .await
            .unwrap();

        let got = reg.get_or_err("a").await.unwrap();
        assert_eq!(got.id, "a");
        assert_eq!(got.status, ExtensionStatus::Downloaded);
        assert_eq!(got.content_hash, "");
        assert_eq!(got.undeclared_policy, UndeclaredPolicy::Deny);
        assert_eq!(got.tool_count, 0);

        assert_eq!(
            reg.get_or_err("missing").await.unwrap_err().code,
            ErrorCode::ExtensionNotFound
        );
        assert!(reg.get("missing").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_re_download_keeps_the_date_the_user_first_added_it() {
        // `installedAtMs` is what the UI sorts and labels by. Resetting it on
        // every update would make an extension the user has had for a year look
        // brand new after a patch release.
        let dir = tempfile::tempdir().unwrap();
        let reg = registry(&dir);
        reg.upsert_extension(insert("a", ExtensionStatus::Downloaded, dir.path()))
            .await
            .unwrap();
        let first = reg.get_or_err("a").await.unwrap().installed_at_ms;

        let mut again = insert("a", ExtensionStatus::Downloaded, dir.path());
        again.version = "2.0.0".to_string();
        reg.upsert_extension(again).await.unwrap();

        let after = reg.get_or_err("a").await.unwrap();
        assert_eq!(after.version, "2.0.0");
        assert_eq!(after.installed_at_ms, first, "the install date was reset");
    }

    #[tokio::test]
    async fn the_extension_list_is_ordered_case_insensitively() {
        let dir = tempfile::tempdir().unwrap();
        let reg = registry(&dir);
        // The capital must be in the MIDDLE of the alphabet for this to
        // distinguish the two collations: with "Apple" first, a byte-wise sort
        // happens to agree and the test passes either way.
        for (id, name) in [("b", "Banana"), ("a", "apple"), ("c", "cherry")] {
            let mut row = insert(id, ExtensionStatus::Downloaded, dir.path());
            row.display_name = name.to_string();
            reg.upsert_extension(row).await.unwrap();
        }
        let names: Vec<String> = reg
            .list()
            .await
            .unwrap()
            .into_iter()
            .map(|e| e.display_name)
            .collect();
        // A byte-wise sort puts every capitalised name ahead of every
        // lowercase one, so it would answer Banana, apple, cherry.
        assert_eq!(names, vec!["apple", "Banana", "cherry"]);
    }

    #[tokio::test]
    async fn an_uninstall_unpins_the_hash_and_keeps_the_users_tool_switches() {
        // Unpinning is the point: a `downloaded` extension whose content hash
        // survived would verify clean the moment it was re-installed, without
        // the install ever re-pinning what is actually on disk now.
        let dir = tempfile::tempdir().unwrap();
        let reg = registry(&dir);
        reg.upsert_extension(insert("a", ExtensionStatus::Installed, dir.path()))
            .await
            .unwrap();
        reg.replace_tools("a", vec![tool("t", vec![])])
            .await
            .unwrap();
        reg.set_tool_enabled("a", "t", true).await.unwrap();
        reg.mark_installed("a", &"a".repeat(64)).await.unwrap();

        reg.mark_downloaded("a").await.unwrap();
        let after = reg.get_or_err("a").await.unwrap();
        assert_eq!(after.status, ExtensionStatus::Downloaded);
        assert_eq!(
            after.content_hash, "",
            "the pinned hash outlived the uninstall"
        );
        // The enable flag is deliberately kept, so re-installing restores the
        // user's selection rather than making them redo it.
        assert!(reg.list_tools("a").await.unwrap()[0].enabled);
    }

    #[tokio::test]
    async fn a_write_bumps_the_timestamp_the_ui_sorts_by() {
        let dir = tempfile::tempdir().unwrap();
        let reg = registry(&dir);
        reg.upsert_extension(insert("a", ExtensionStatus::Downloaded, dir.path()))
            .await
            .unwrap();
        let before = reg.get_or_err("a").await.unwrap().updated_at_ms;
        // The clock has millisecond resolution and these writes are faster than
        // that, so the write is staged behind a real gap rather than asserted
        // to be strictly greater by luck.
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;

        reg.set_undeclared_policy("a", UndeclaredPolicy::Ask)
            .await
            .unwrap();
        let after = reg.get_or_err("a").await.unwrap();
        assert_eq!(after.undeclared_policy, UndeclaredPolicy::Ask);
        assert!(
            after.updated_at_ms > before,
            "the policy changed without moving updated_at_ms"
        );

        reg.set_status("a", ExtensionStatus::Installed)
            .await
            .unwrap();
        assert_eq!(
            reg.get_or_err("a").await.unwrap().status,
            ExtensionStatus::Installed
        );
    }

    // --- the lifecycle --------------------------------------------------------

    #[tokio::test]
    async fn drift_disables_every_tool_and_never_clears_itself() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("ext");
        write_extension(&root, "export const x = 1;\n");
        let reg = registry(&dir);
        reg.upsert_extension(insert("a", ExtensionStatus::Installed, &root))
            .await
            .unwrap();
        reg.replace_tools("a", vec![tool("one", vec![]), tool("two", vec![])])
            .await
            .unwrap();
        let hash = hash_extension(&root).unwrap();
        reg.mark_installed("a", &hash).await.unwrap();
        reg.set_tool_enabled("a", "one", true).await.unwrap();
        reg.set_tool_enabled("a", "two", true).await.unwrap();
        assert_eq!(reg.get_or_err("a").await.unwrap().enabled_tool_count, 2);

        std::fs::write(root.join("main.ts"), "export const x = 2;\n").unwrap();
        assert_eq!(
            reg.verify_fresh("a").await.unwrap_err().code,
            ErrorCode::ExtensionHashDrift
        );

        let after = reg.get_or_err("a").await.unwrap();
        assert_eq!(after.status, ExtensionStatus::Drift);
        assert_eq!(
            after.enabled_tool_count, 0,
            "a drifted extension kept tools exposed to the model"
        );
        // The pinned hash is KEPT, so the user can see what it was.
        assert_eq!(after.content_hash, hash);

        // Restoring the bytes does NOT clear drift on its own. Otherwise an
        // extension could run tampered once and put itself back.
        std::fs::write(root.join("main.ts"), "export const x = 1;\n").unwrap();
        assert_eq!(
            reg.verify_fresh("a").await.unwrap_err().code,
            ErrorCode::ExtensionHashDrift,
            "drift cleared itself once the content matched again"
        );
        assert_eq!(
            reg.get_or_err("a").await.unwrap().status,
            ExtensionStatus::Drift
        );

        // Only the user's confirm-reenable clears it.
        reg.repin_and_clear("a", &hash_extension(&root).unwrap())
            .await
            .unwrap();
        assert_eq!(
            reg.get_or_err("a").await.unwrap().status,
            ExtensionStatus::Installed
        );
        reg.verify_fresh("a").await.unwrap();
    }

    #[tokio::test]
    async fn a_verify_of_anything_but_an_installed_extension_is_refused() {
        // Defence in depth: the exposure gate should already have stopped this,
        // so reaching here means a caller skipped it, and answering "fine" would
        // dispatch a tool call against unpinned content.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("ext");
        write_extension(&root, "x");
        let reg = registry(&dir);
        for status in [ExtensionStatus::Downloaded, ExtensionStatus::Drift] {
            reg.upsert_extension(insert("a", status, &root))
                .await
                .unwrap();
            assert_eq!(
                reg.verify_fresh("a").await.unwrap_err().code,
                ErrorCode::ExtensionHashDrift,
                "{status:?} verified clean"
            );
        }
        // An extension nobody has heard of is the tool lookup's problem, and it
        // has already answered. Inventing a second error here would report a
        // drift that did not happen.
        reg.delete("a").await.unwrap();
        reg.verify_fresh("a").await.unwrap();
    }

    #[tokio::test]
    async fn a_hash_that_cannot_be_computed_is_drift_rather_than_a_pass() {
        // Failing open here would make "delete a file the walk needs" a way to
        // skip verification entirely.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("ext");
        write_extension(&root, "x");
        let reg = registry(&dir);
        reg.upsert_extension(insert("a", ExtensionStatus::Installed, &root))
            .await
            .unwrap();
        reg.mark_installed("a", &hash_extension(&root).unwrap())
            .await
            .unwrap();
        reg.verify_fresh("a").await.unwrap();

        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(
            reg.verify_fresh("a").await.unwrap_err().code,
            ErrorCode::ExtensionHashDrift
        );
        assert_eq!(
            reg.get_or_err("a").await.unwrap().status,
            ExtensionStatus::Drift
        );
    }

    #[tokio::test]
    async fn boot_verification_touches_only_installed_extensions() {
        let dir = tempfile::tempdir().unwrap();
        let reg = registry(&dir);
        for (id, status) in [
            ("clean", ExtensionStatus::Installed),
            ("tampered", ExtensionStatus::Installed),
            ("vanished", ExtensionStatus::Installed),
            ("downloaded", ExtensionStatus::Downloaded),
            ("already", ExtensionStatus::Drift),
        ] {
            let root = dir.path().join(id);
            write_extension(&root, "export const x = 1;\n");
            reg.upsert_extension(insert(id, status, &root))
                .await
                .unwrap();
            if status == ExtensionStatus::Installed {
                reg.mark_installed(id, &hash_extension(&root).unwrap())
                    .await
                    .unwrap();
            }
        }
        std::fs::write(
            dir.path().join("tampered").join("main.ts"),
            "export const x = 9;\n",
        )
        .unwrap();
        // A hash that cannot be computed at all is drift too, not a pass:
        // otherwise "delete a file the walk needs" is a way to skip
        // verification entirely.
        std::fs::remove_dir_all(dir.path().join("vanished")).unwrap();

        let mut drifted = reg.verify_all_on_boot().await.unwrap();
        drifted.sort();
        assert_eq!(
            drifted,
            vec!["tampered".to_string(), "vanished".to_string()]
        );
        assert_eq!(
            reg.get_or_err("vanished").await.unwrap().status,
            ExtensionStatus::Drift
        );
        // A downloaded extension has no pinned hash, so hashing it would flag
        // every freshly-downloaded extension as drifted.
        assert_eq!(
            reg.get_or_err("downloaded").await.unwrap().status,
            ExtensionStatus::Downloaded
        );
        assert_eq!(
            reg.get_or_err("clean").await.unwrap().status,
            ExtensionStatus::Installed
        );
    }

    #[tokio::test]
    async fn the_skip_cache_is_invalidated_by_a_re_pin_and_not_only_by_the_signature() {
        // Keyed on the signature alone, the cache would keep answering "clean"
        // against the OLD pin after an update re-pinned a different hash.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("ext");
        write_extension(&root, "export const x = 1;\n");
        let reg = registry(&dir);
        reg.upsert_extension(insert("a", ExtensionStatus::Installed, &root))
            .await
            .unwrap();
        reg.mark_installed("a", &hash_extension(&root).unwrap())
            .await
            .unwrap();
        reg.verify_fresh("a").await.unwrap();
        assert!(lock(&reg.verified).contains_key("a"), "nothing was cached");

        // Re-pin to a hash the content does NOT have, with the files untouched
        // so the stat signature is identical.
        reg.mark_installed("a", &"0".repeat(64)).await.unwrap();
        assert_eq!(
            reg.verify_fresh("a").await.unwrap_err().code,
            ErrorCode::ExtensionHashDrift,
            "the cache answered against a stale pin"
        );
    }

    // --- tools ----------------------------------------------------------------

    #[tokio::test]
    async fn a_re_register_keeps_the_users_switches_but_not_a_changed_permission_set() {
        let dir = tempfile::tempdir().unwrap();
        let reg = registry(&dir);
        reg.upsert_extension(insert("a", ExtensionStatus::Installed, dir.path()))
            .await
            .unwrap();
        reg.replace_tools(
            "a",
            vec![
                tool("keep", vec![net("api.example.com")]),
                tool("changed", vec![net("api.example.com")]),
                tool("gone", vec![]),
            ],
        )
        .await
        .unwrap();
        reg.set_tool_enabled("a", "keep", true).await.unwrap();
        reg.set_tool_always_available("a", "keep", true)
            .await
            .unwrap();
        reg.set_tool_enabled("a", "changed", true).await.unwrap();

        let keep_id = tool_id("a", "keep");
        let changed_id = tool_id("a", "changed");
        for id in [&keep_id, &changed_id] {
            reg.set_grants(
                id,
                vec![GrantEntry {
                    key: permission_key(&net("api.example.com")),
                    kind: PermissionKind::Net,
                    state: GrantState::Granted,
                }],
            )
            .await
            .unwrap();
        }

        // The update: `keep` declares the same permission in a different shape
        // that produces the same key, `changed` declares a different host, and
        // `gone` disappears.
        reg.replace_tools(
            "a",
            vec![
                tool("keep", vec![net("api.example.com")]),
                tool("changed", vec![net("evil.example.com")]),
                tool("added", vec![]),
            ],
        )
        .await
        .unwrap();

        let tools = reg.list_tools("a").await.unwrap();
        let by_name = |name: &str| tools.iter().find(|t| t.name == name).unwrap().clone();

        let keep = by_name("keep");
        assert!(keep.enabled, "the user's enable was lost across an update");
        assert!(keep.always_available, "the always-available flag was lost");
        assert_eq!(
            keep.grants.len(),
            1,
            "an unchanged permission set lost its grant"
        );
        assert_eq!(keep.grants[0].state, GrantState::Granted);

        let changed = by_name("changed");
        assert!(
            changed.enabled,
            "the enable flag is per tool, not per grant"
        );
        assert!(
            changed.grants.is_empty(),
            "a grant survived a CHANGED permission set, so the user was never re-asked"
        );

        let added = by_name("added");
        assert!(
            !added.enabled,
            "a tool added by an update was enabled by it"
        );

        assert!(
            tools.iter().all(|t| t.name != "gone"),
            "a removed tool survived the replace"
        );
    }

    #[tokio::test]
    async fn a_reordered_permission_list_is_not_a_changed_one() {
        // Otherwise a cosmetic manifest edit throws away every grant and the
        // user is re-asked for nothing.
        let dir = tempfile::tempdir().unwrap();
        let reg = registry(&dir);
        reg.upsert_extension(insert("a", ExtensionStatus::Installed, dir.path()))
            .await
            .unwrap();
        let first = vec![net("a.example.com"), net("b.example.com")];
        let reordered = vec![net("b.example.com"), net("a.example.com")];
        reg.replace_tools("a", vec![tool("t", first.clone())])
            .await
            .unwrap();
        reg.set_grants(
            &tool_id("a", "t"),
            first
                .iter()
                .map(|decl| GrantEntry {
                    key: permission_key(decl),
                    kind: PermissionKind::Net,
                    state: GrantState::Granted,
                })
                .collect(),
        )
        .await
        .unwrap();

        reg.replace_tools("a", vec![tool("t", reordered)])
            .await
            .unwrap();
        assert_eq!(
            reg.list_tools("a").await.unwrap()[0].grants.len(),
            2,
            "reordering the declarations dropped the grants"
        );
    }

    #[tokio::test]
    async fn the_required_permissions_are_persisted_and_read_back() {
        // The specification's comment says they are not persisted. The column
        // exists and is written; the comment is simply wrong, and a port that
        // believed it would re-parse `loca.json` on every read.
        let dir = tempfile::tempdir().unwrap();
        let reg = registry(&dir);
        reg.upsert_extension(insert("a", ExtensionStatus::Installed, dir.path()))
            .await
            .unwrap();
        reg.replace_tools("a", vec![tool("t", vec![net("api.example.com")])])
            .await
            .unwrap();
        let tools = reg.list_tools("a").await.unwrap();
        assert!(same(
            &tools[0].required_permissions,
            &[net("api.example.com")]
        ));
    }

    #[tokio::test]
    async fn a_tool_this_host_does_not_support_is_invisible_by_id_as_well_as_in_the_list() {
        // Filtering only in the listing would let dispatch run an OS-gated tool
        // whenever its id was already known, which is exactly how it arrives:
        // the model names the tool.
        let dir = tempfile::tempdir().unwrap();
        let reg = registry(&dir);
        reg.upsert_extension(insert("a", ExtensionStatus::Installed, dir.path()))
            .await
            .unwrap();
        let mut windows_only = tool("winonly", vec![]);
        windows_only.platforms = vec!["windows".to_string()];
        let mut anywhere = tool("anywhere", vec![]);
        anywhere.platforms = vec![];
        let mut here = tool("here", vec![]);
        here.platforms = vec!["darwin".to_string(), "linux".to_string()];
        reg.replace_tools("a", vec![windows_only, anywhere, here])
            .await
            .unwrap();

        let names: Vec<String> = reg
            .list_tools("a")
            .await
            .unwrap()
            .into_iter()
            .map(|t| t.name)
            .collect();
        assert_eq!(names, vec!["anywhere".to_string(), "here".to_string()]);
        assert!(reg
            .get_tool(&tool_id("a", "winonly"))
            .await
            .unwrap()
            .is_none());
        assert!(reg.get_tool(&tool_id("a", "here")).await.unwrap().is_some());
    }

    #[test]
    fn an_empty_platform_list_means_every_platform() {
        assert!(tool_platform_supported(&[], &["darwin".to_string()]));
        assert!(tool_platform_supported(
            &["linux".to_string(), "darwin".to_string()],
            &["darwin".to_string()]
        ));
        assert!(!tool_platform_supported(
            &["windows".to_string()],
            &["darwin".to_string()]
        ));
    }

    #[tokio::test]
    async fn only_installed_extensions_contribute_to_the_flat_tool_catalog() {
        // The flat catalog is what the model is offered. A downloaded or
        // drifted extension being in it is the failure the whole lifecycle
        // exists to prevent.
        let dir = tempfile::tempdir().unwrap();
        let reg = registry(&dir);
        for (id, status) in [
            ("live", ExtensionStatus::Installed),
            ("staged", ExtensionStatus::Downloaded),
            ("bad", ExtensionStatus::Drift),
        ] {
            reg.upsert_extension(insert(id, status, dir.path()))
                .await
                .unwrap();
            reg.replace_tools(id, vec![tool(&format!("{id}-tool"), vec![])])
                .await
                .unwrap();
        }
        let names: Vec<String> = reg
            .list_all_tools()
            .await
            .unwrap()
            .into_iter()
            .map(|t| t.name)
            .collect();
        assert_eq!(names, vec!["live-tool".to_string()]);

        // But `list_tools` for one extension still answers, because the
        // extension detail view has to show tools you cannot yet enable.
        assert_eq!(reg.list_tools("staged").await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn the_flat_catalog_tags_each_tool_with_its_provider() {
        let dir = tempfile::tempdir().unwrap();
        let reg = registry(&dir);
        reg.upsert_extension(insert("a", ExtensionStatus::Installed, dir.path()))
            .await
            .unwrap();
        reg.replace_tools("a", vec![tool("t", vec![])])
            .await
            .unwrap();
        let tools = reg.list_all_tools().await.unwrap();
        assert_eq!(tools[0].provider_name.as_deref(), Some("The a"));
        assert_eq!(tools[0].provider_kind, ToolProviderKind::Extension);
    }

    #[tokio::test]
    async fn deleting_an_extension_cascades_to_its_tools_grants_and_embeddings() {
        let dir = tempfile::tempdir().unwrap();
        let reg = registry(&dir);
        reg.upsert_extension(insert("a", ExtensionStatus::Installed, dir.path()))
            .await
            .unwrap();
        reg.replace_tools("a", vec![tool("t", vec![net("h")])])
            .await
            .unwrap();
        let id = tool_id("a", "t");
        reg.set_grants(
            &id,
            vec![GrantEntry {
                key: permission_key(&net("h")),
                kind: PermissionKind::Net,
                state: GrantState::Granted,
            }],
        )
        .await
        .unwrap();
        reg.store_embedding(&id, vec![0.5, -0.25], "src")
            .await
            .unwrap();

        reg.delete("a").await.unwrap();
        assert!(reg.get_tool(&id).await.unwrap().is_none());
        assert!(reg.list_grants_for_tool(&id).await.unwrap().is_empty());
        assert!(reg.load_embedding(&id).await.unwrap().is_none());
    }

    // --- grants ---------------------------------------------------------------

    #[tokio::test]
    async fn setting_a_grant_twice_updates_it_rather_than_duplicating_it() {
        let dir = tempfile::tempdir().unwrap();
        let reg = registry(&dir);
        reg.upsert_extension(insert("a", ExtensionStatus::Installed, dir.path()))
            .await
            .unwrap();
        reg.replace_tools("a", vec![tool("t", vec![net("h")])])
            .await
            .unwrap();
        let id = tool_id("a", "t");
        let key = permission_key(&net("h"));

        let granted = reg
            .set_grants(
                &id,
                vec![GrantEntry {
                    key: key.clone(),
                    kind: PermissionKind::Net,
                    state: GrantState::Granted,
                }],
            )
            .await
            .unwrap();
        assert_eq!(granted.len(), 1);
        assert_eq!(granted[0].state, GrantState::Granted);

        let denied = reg
            .set_grants(
                &id,
                vec![GrantEntry {
                    key: key.clone(),
                    kind: PermissionKind::Net,
                    state: GrantState::Denied,
                }],
            )
            .await
            .unwrap();
        assert_eq!(denied.len(), 1, "the second decision inserted a second row");
        assert_eq!(denied[0].state, GrantState::Denied);
        assert_eq!(denied[0].permission_kind, PermissionKind::Net);
        assert_eq!(denied[0].tool_id, id);
    }

    // --- embeddings -----------------------------------------------------------

    #[tokio::test]
    async fn an_embedding_round_trips_exactly() {
        let dir = tempfile::tempdir().unwrap();
        let reg = registry(&dir);
        reg.upsert_extension(insert("a", ExtensionStatus::Installed, dir.path()))
            .await
            .unwrap();
        reg.replace_tools("a", vec![tool("t", vec![]), tool("u", vec![])])
            .await
            .unwrap();
        let t = tool_id("a", "t");
        let vector = vec![0.5f32, -0.25, 1.0e-8, f32::MIN_POSITIVE];
        reg.store_embedding(&t, vector.clone(), "src-1")
            .await
            .unwrap();

        let loaded = reg.load_embedding(&t).await.unwrap().unwrap();
        // Bit-exact, not approximately: a lossy round trip changes every cosine
        // score by a little and nothing ever fails.
        assert_eq!(loaded.vector, vector);
        assert_eq!(loaded.source_hash, "src-1");

        // Re-storing replaces rather than conflicting.
        reg.store_embedding(&t, vec![1.0], "src-2").await.unwrap();
        let replaced = reg.load_embedding(&t).await.unwrap().unwrap();
        assert_eq!(replaced.vector, vec![1.0]);
        assert_eq!(replaced.source_hash, "src-2");

        let all = reg.load_all_embeddings().await.unwrap();
        assert_eq!(all.len(), 1, "a tool with no embedding invented one");
        assert_eq!(all[&t].vector, vec![1.0]);
        assert!(reg
            .load_embedding(&tool_id("a", "u"))
            .await
            .unwrap()
            .is_none());
    }

    // --- the gate seam --------------------------------------------------------

    #[tokio::test]
    async fn the_gate_facts_come_from_the_invoked_tool_and_not_from_its_siblings() {
        // The broker gates on ONE tool's declarations. Answering with the
        // extension's union would let a benign tool use a sibling's grant.
        let dir = tempfile::tempdir().unwrap();
        let reg = registry(&dir);
        let mut row = insert("a", ExtensionStatus::Installed, dir.path());
        row.has_database = true;
        reg.upsert_extension(row).await.unwrap();
        reg.replace_tools(
            "a",
            vec![
                tool("mine", vec![net("mine.example.com")]),
                tool("theirs", vec![net("theirs.example.com")]),
            ],
        )
        .await
        .unwrap();
        reg.set_grants(
            &tool_id("a", "theirs"),
            vec![GrantEntry {
                key: permission_key(&net("theirs.example.com")),
                kind: PermissionKind::Net,
                state: GrantState::Granted,
            }],
        )
        .await
        .unwrap();

        let facts = reg.gate_facts("a", "mine").await.unwrap();
        assert!(facts.has_database);
        assert_eq!(facts.undeclared_policy, UndeclaredPolicy::Deny);
        assert!(same(&facts.required, &[net("mine.example.com")]));
        assert!(
            facts.grants.is_empty(),
            "a sibling tool's grant reached this tool's gate"
        );

        assert_eq!(
            reg.gate_facts("a", "nope").await.unwrap_err().code,
            ErrorCode::ToolNotFound
        );
        assert_eq!(
            reg.gate_facts("nope", "mine").await.unwrap_err().code,
            ErrorCode::ExtensionNotFound
        );
    }

    #[tokio::test]
    async fn an_os_gated_tool_has_no_gate_facts_at_all() {
        // Not "facts with an empty permission set": an empty `required` reads
        // as undeclared to the gate, which an `ask` policy would then offer to
        // the user for a tool that cannot run on this host.
        let dir = tempfile::tempdir().unwrap();
        let reg = registry(&dir);
        let mut row = insert("a", ExtensionStatus::Installed, dir.path());
        row.has_database = true;
        reg.upsert_extension(row).await.unwrap();
        let mut windows_only = tool("winonly", vec![]);
        windows_only.platforms = vec!["windows".to_string()];
        reg.replace_tools("a", vec![windows_only]).await.unwrap();

        assert_eq!(
            reg.gate_facts("a", "winonly").await.unwrap_err().code,
            ErrorCode::ToolNotFound
        );
    }

    #[test]
    fn a_registry_can_be_handed_to_the_broker_as_its_facts_source() {
        // The seam is a trait object and nothing in the crate constructs the
        // pair yet, so without this the two halves compile independently and
        // meet for the first time at a boot wiring that is somebody else's.
        fn takes_the_seam(_: std::sync::Arc<dyn RegistryFacts>) {}
        let dir = tempfile::tempdir().unwrap();
        takes_the_seam(std::sync::Arc::new(registry(&dir)));
    }

    #[test]
    fn a_tool_id_is_derived_from_its_names() {
        // Grants reference it, and `replace_tools` deletes and re-inserts the
        // row, so a generated id would orphan every grant on every update.
        assert_eq!(tool_id("ext", "do-thing"), "ext::do-thing");
    }

    #[test]
    fn a_corrupted_enum_column_is_an_error_rather_than_a_guess() {
        // The specification casts the string blindly, which produces a value no
        // branch matches: a tool that is neither exposed nor reported. There is
        // also no least-privileged answer for `source` to fall back to.
        assert!(parse_enum::<ExtensionStatus>(8, "nonsense").is_err());
        assert!(parse_enum::<GrantState>(3, "nonsense").is_err());
        assert!(parse_enum::<UndeclaredPolicy>(11, "nonsense").is_err());
        assert_eq!(
            parse_enum::<ExtensionStatus>(8, "drift").unwrap(),
            ExtensionStatus::Drift
        );
        // And the round trip through the wire spelling is exact.
        assert_eq!(enum_str(&ExtensionStatus::Drift), "drift");
        assert_eq!(enum_str(&GrantState::Granted), "granted");
        assert_eq!(enum_str(&PermissionKind::Memories), "memories");
        assert_eq!(enum_str(&ExtensionSource::Seeded), "seeded");
    }

    #[test]
    fn an_embedding_blob_is_little_endian_so_it_survives_moving_machines() {
        let bytes = encode_vector(&[1.0f32]);
        assert_eq!(bytes, vec![0x00, 0x00, 0x80, 0x3f]);
        assert_eq!(
            decode_embedding(&bytes, "h".to_string()).unwrap().vector,
            vec![1.0f32]
        );
        // A truncated blob is an error rather than a silently shorter vector,
        // which would compare against every other embedding at the wrong
        // dimension.
        assert!(decode_embedding(&[0, 0, 0], "h".to_string()).is_err());
    }

    #[test]
    fn a_memories_declaration_keys_by_access() {
        // `permission_key` is the sandbox's, and `replace_tools` compares key
        // SETS, so a read declaration becoming a write must read as changed.
        let read = PermissionDecl::Memories {
            access: PermissionDeclAccess::Read,
            optional: None,
            reason: "r".to_string(),
        };
        let write = PermissionDecl::Memories {
            access: PermissionDeclAccess::Write,
            optional: None,
            reason: "r".to_string(),
        };
        assert_ne!(
            key_set(std::slice::from_ref(&read)),
            key_set(std::slice::from_ref(&write))
        );
        // And order does not matter.
        assert_eq!(
            key_set(&[read.clone(), write.clone()]),
            key_set(&[write, read])
        );
    }
}
