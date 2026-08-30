//! The extensions, tools, grants and embeddings registry.
//!
//! Four tables, already present in `schema.sql`, plus the lifecycle rules over
//! them.
//!
//! Two status transitions are security gates rather than bookkeeping. **Drift
//! never auto-clears**: leaving it requires the user pressing confirm-reenable,
//! which re-pins whatever is on disk now. And **flagging drift always disables
//! every tool in the same breath**, which is why the two steps are one private
//! function here rather than two calls each caller has to remember.

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

/// A tool's id is derived, not generated: `<extension id>::<tool name>`, so a
/// tool re-inserted by [`Registry::replace_tools`] keeps the id its grant rows
/// already reference.
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

/// The per-call re-hash skip key. BOTH halves are compared: a re-pin changes
/// what "verified" means, so a cache keyed on the signature alone would keep
/// answering for the old pin.
#[derive(Debug, Clone)]
struct Verified {
    signature: String,
    hash: String,
}

/// Cheap to clone, and the clones SHARE the verification cache: a second
/// independent cache would keep answering from a map `forget_verified` never
/// reaches.
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

    /// Platform tokens are injected, so OS gating can be tested for a host the
    /// test is not running on.
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

    /// The ids of every INSTALLED extension: one query and one snapshot, for
    /// [`super::ExtensionProviderGate`]. `installed` reaches the SQL through
    /// [`enum_str`], so the status vocabulary has exactly one spelling.
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
                // One statement rather than a read then a branch, because two
                // downloads of the same id can be queued at once.
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
        // Paired with the hash comparison in `verify_fresh`'s cache lookup.
        self.forget_verified(id);
        Ok(())
    }

    /// Out-of-band content change: flip to `drift`, keeping the pinned hash for
    /// reference. Prefer [`Registry::flag_drift`], which also takes the tools
    /// out of exposure.
    pub async fn mark_drift(&self, id: &str) -> Result<()> {
        self.update_extension(id, "status = 'drift'", Vec::new())
            .await?;
        self.forget_verified(id);
        Ok(())
    }

    /// Flag drift AND disable every tool. One function because a drifted
    /// extension whose tools stayed enabled is still offered to the model.
    async fn flag_drift(&self, id: &str) -> Result<()> {
        self.mark_drift(id).await?;
        self.disable_all_tools(id).await
    }

    /// Confirm-reenable: re-pin what is on disk NOW and clear drift. Never
    /// automatic, or tampered content could run once and put itself back.
    pub async fn repin_and_clear(&self, id: &str, content_hash: &str) -> Result<()> {
        self.mark_installed(id, content_hash).await
    }

    /// Uninstall (revert): drop back to `downloaded` and unpin the hash. Tool
    /// `enabled` flags are deliberately KEPT, so a re-install restores them.
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
        // Hygiene, not a correctness guard: every path back to `verify_fresh`
        // goes through `mark_installed`, which invalidates anyway.
        self.forget_verified(id);
        Ok(())
    }

    /// One `UPDATE`, with `updated_at_ms` appended here rather than at each
    /// call site. `set` is a literal from this module and every value is bound.
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

    /// Boot-time integrity check over every INSTALLED extension, returning the
    /// ids that drifted. A `downloaded` extension has no pinned hash and a
    /// `drift` one is already flagged, so both are skipped.
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
                // Drift, not a pass: failing open would make "delete a file
                // the walk needs" a way to skip verification.
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

    /// Per-call integrity check, run before a tool call is dispatched, since
    /// boot verification alone lets post-boot tampering run until restart. On a
    /// mismatch it flags drift, disables every tool, and raises
    /// `extension_hash_drift`; there is no auto-clear. The skip cache makes it
    /// affordable, with the limitation documented on [`stat_signature`].
    pub async fn verify_fresh(&self, extension_id: &str) -> Result<()> {
        // An unknown extension is the tool lookup's problem, and it has already
        // answered `tool_not_found`.
        let Some(extension) = self.get(extension_id).await? else {
            return Ok(());
        };
        // Defence in depth: reaching here in any other state means a caller
        // skipped the exposure gate.
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
            // The hash comparison is redundant with `forget_verified` and kept
            // anyway: it covers a `content_hash` changed without going through
            // this handle, and a future write path that forgets to invalidate.
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

    /// Atomically replace an extension's tools. `enabled` and
    /// `always_available` carry across BY NAME, but **grants survive only when
    /// the canonical permission-key SET is unchanged**: a changed set drops
    /// every grant so the user re-reviews.
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
                            // A brand-new tool is OFF: an update must not enable
                            // a tool the user never agreed to.
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

    /// The tools of one extension, with their grants attached. Host-unsupported
    /// tools are filtered out HERE, so an OS-gated tool is invisible to chat
    /// selection, the routes and the Tools UI alike.
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
    /// provider's display name. A `downloaded` or `drift` one contributes
    /// nothing.
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

    /// One tool by id, or `None` for a tool this host does not support. The
    /// platform filter is repeated here because dispatch takes an id directly.
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
                // `column` is a `&'static str` from this module, never input.
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

    /// Every stored embedding in one query, since relevance runs over every
    /// enabled tool on every chat turn.
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
        // Through `list_tools`, so an OS-gated tool has no facts at all: an
        // empty `required` reads as "undeclared" to the gate.
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
    // `COLLATE NOCASE`, or every capitalised name sorts first.
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
            // Derived at read time: `missing_required` depends on current grant
            // state, so a row cannot answer it.
            missing_required: Vec::new(),
            grants: Vec::new(),
            provider_name: None,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Drop the tools this host does not support, then attach each survivor's
/// grants. In that order, or dropped tools cost a query each.
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

/// An empty declaration means every platform: most tools are pure TypeScript
/// and run anywhere, so declaring nothing is the common case.
pub fn tool_platform_supported(declared: &[String], host: &[String]) -> bool {
    declared.is_empty() || declared.iter().any(|p| host.iter().any(|h| h == p))
}

/// The host's platform tokens. Only Linux's display server needs runtime
/// detection: an X11 window can be positioned from the command line and a
/// Wayland one cannot.
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

/// The canonical, order-independent signature of a required-permission set, so
/// a cosmetic reorder does not throw away every grant the user gave.
fn key_set(declarations: &[PermissionDecl]) -> String {
    let mut keys: Vec<String> = declarations.iter().map(permission_key).collect();
    keys.sort();
    keys.join("|")
}

/// A stored JSON column, or the type's default when it will not parse, so one
/// corrupted row leaves the rest of the extension list readable in the UI.
fn json_of<T: serde::de::DeserializeOwned + Default>(raw: &str) -> T {
    serde_json::from_str(raw).unwrap_or_default()
}

/// The wire spelling of a contract enum, via serde rather than a hand-written
/// match, so a new variant cannot be persisted under an invented name.
fn enum_str<T: serde::Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(Value::String(text)) => text,
        _ => String::new(),
    }
}

/// The inverse, for a column this module itself wrote. A value that does not
/// parse is an ERROR rather than a fallback: there is no least-privileged
/// answer for `source` or `permission_kind`, and a value no branch matches is a
/// tool that is neither exposed nor reported.
fn parse_enum<T: serde::de::DeserializeOwned>(column: usize, raw: &str) -> rusqlite::Result<T> {
    serde_json::from_value(Value::String(raw.to_string())).map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(column, rusqlite::types::Type::Text, err.into())
    })
}

/// The embedding, as explicitly little-endian `f32` bytes: the database file
/// gets copied between machines, and a native-endian blob would decode to noise
/// rather than fail loudly.
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

    /// `PermissionDecl` carries no `PartialEq`, and the wire form is what the
    /// column stores anyway.
    fn same(left: &[PermissionDecl], right: &[PermissionDecl]) -> bool {
        serde_json::to_value(left).ok() == serde_json::to_value(right).ok()
    }

    /// A tiny extension on disk, so the hash gate has something real to hash.
    fn write_extension(root: &std::path::Path, body: &str) {
        std::fs::create_dir_all(root).unwrap();
        std::fs::write(root.join("main.ts"), body).unwrap();
    }

    /// A fresh registry with extension `a` installed at the temp root.
    async fn installed_a() -> (tempfile::TempDir, Registry) {
        let dir = tempfile::tempdir().unwrap();
        let reg = registry(&dir);
        reg.upsert_extension(insert("a", ExtensionStatus::Installed, dir.path()))
            .await
            .unwrap();
        (dir, reg)
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
        // The capital must be in the MIDDLE of the alphabet, or a byte-wise
        // sort happens to agree.
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
        assert_eq!(names, vec!["apple", "Banana", "cherry"]);
    }

    #[tokio::test]
    async fn an_uninstall_unpins_the_hash_and_keeps_the_users_tool_switches() {
        // A surviving hash would verify clean on re-install without ever
        // re-pinning what is on disk now.
        let (_dir, reg) = installed_a().await;
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
        // The clock has millisecond resolution and these writes are faster.
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
        // KEPT, so the user can see what it was.
        assert_eq!(after.content_hash, hash);

        // Restoring the bytes does NOT clear drift on its own.
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
        // An unknown extension is the tool lookup's problem; inventing a
        // second error would report a drift that did not happen.
        reg.delete("a").await.unwrap();
        reg.verify_fresh("a").await.unwrap();
    }

    #[tokio::test]
    async fn a_hash_that_cannot_be_computed_is_drift_rather_than_a_pass() {
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

        // Files untouched, so the stat signature is identical.
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
        let (_dir, reg) = installed_a().await;
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

        // `keep` re-declares the same key, `changed` a different host, `gone`
        // disappears.
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
        let (_dir, reg) = installed_a().await;
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
        let (_dir, reg) = installed_a().await;
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
        // The model names the tool, so its id is always already known.
        let (_dir, reg) = installed_a().await;
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

        // But the detail view still shows tools you cannot yet enable.
        assert_eq!(reg.list_tools("staged").await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn the_flat_catalog_tags_each_tool_with_its_provider() {
        let (_dir, reg) = installed_a().await;
        reg.replace_tools("a", vec![tool("t", vec![])])
            .await
            .unwrap();
        let tools = reg.list_all_tools().await.unwrap();
        assert_eq!(tools[0].provider_name.as_deref(), Some("The a"));
        assert_eq!(tools[0].provider_kind, ToolProviderKind::Extension);
    }

    #[tokio::test]
    async fn deleting_an_extension_cascades_to_its_tools_grants_and_embeddings() {
        let (_dir, reg) = installed_a().await;
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
        let (_dir, reg) = installed_a().await;
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
        let (_dir, reg) = installed_a().await;
        reg.replace_tools("a", vec![tool("t", vec![]), tool("u", vec![])])
            .await
            .unwrap();
        let t = tool_id("a", "t");
        let vector = vec![0.5f32, -0.25, 1.0e-8, f32::MIN_POSITIVE];
        reg.store_embedding(&t, vector.clone(), "src-1")
            .await
            .unwrap();

        let loaded = reg.load_embedding(&t).await.unwrap().unwrap();
        // Bit-exact: a lossy round trip skews every cosine score silently.
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
        // as undeclared to the gate.
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
        // Nothing in the crate constructs the pair yet, so the two halves would
        // otherwise meet for the first time at boot.
        fn takes_the_seam(_: std::sync::Arc<dyn RegistryFacts>) {}
        let dir = tempfile::tempdir().unwrap();
        takes_the_seam(std::sync::Arc::new(registry(&dir)));
    }

    #[test]
    fn a_tool_id_is_derived_from_its_names() {
        // A generated id would orphan every grant on every `replace_tools`.
        assert_eq!(tool_id("ext", "do-thing"), "ext::do-thing");
    }

    #[test]
    fn a_corrupted_enum_column_is_an_error_rather_than_a_guess() {
        assert!(parse_enum::<ExtensionStatus>(8, "nonsense").is_err());
        assert!(parse_enum::<GrantState>(3, "nonsense").is_err());
        assert!(parse_enum::<UndeclaredPolicy>(11, "nonsense").is_err());
        assert_eq!(
            parse_enum::<ExtensionStatus>(8, "drift").unwrap(),
            ExtensionStatus::Drift
        );
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
        // A truncated blob is an error, not a silently shorter vector.
        assert!(decode_embedding(&[0, 0, 0], "h".to_string()).is_err());
    }

    #[test]
    fn a_memories_declaration_keys_by_access() {
        // `replace_tools` compares key SETS, so read becoming write must read
        // as changed.
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
        assert_eq!(
            key_set(&[read.clone(), write.clone()]),
            key_set(&[write, read])
        );
    }
}
