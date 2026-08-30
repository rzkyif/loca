//! The memory store: markdown on disk, metadata in SQLite, reconciled by
//! content hash.
//!
//! The files are the source of truth. SQLite carries the id, title, provider,
//! enabled flag, and the background-generated summary and embedding; [`rescan`]
//! walks the directory and settles any disagreement, so a memory edited or
//! dropped in by hand becomes first-class on the next scan rather than being
//! ignored or clobbered.
//!
//! [`rescan`]: MemoriesStore::rescan
//!
//! Two invariants.
//!
//! **Every mutation that reads before it writes is serialised** by one
//! `tokio::sync::Mutex`, because each spans an `await` between the read and the
//! write. Only write-only bookkeeping (the summary, the embedding, `touch`, the
//! enabled flag) skips it. "It is one `UPDATE`" is not a reason to skip:
//! `rename` is one `UPDATE` and still needs the lock, because the constraint it
//! enforces (`COLLATE NOCASE` uniqueness) is not the one the schema's `UNIQUE`
//! index enforces (BINARY).
//!
//! **There is exactly one store per process**, holding that lock, the map of
//! extension provider directories and the provider gate. A second instance would
//! answer `list` from a registry nobody wired and would let two writes
//! interleave, neither of which is a compile error.
//!
//! # Providers
//!
//! `user` memories are editable and live under `<root>/memories/`. Anything else
//! is the id of the extension that ships the memory: read-only, refreshed only
//! by reinstalling that extension, and hidden from every listing while the
//! extension is downloaded but not installed (the [`ProviderGate`]). The rows
//! survive that hiding, so a later install restores them.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use indexmap::IndexMap;
use rusqlite::{Connection, OptionalExtension, Row};
use serde::Serialize;

use crate::contract::api::{Memory, MemoryKind, MemoryMeta};
use crate::db::Db;
use crate::error::{conflict, not_found, validation, Result};
use crate::paths::Paths;

use super::content::{self, SKILL_FILE};
use super::skill::parse_skill;

/// The value stored in `memories.kind`.
pub fn kind_as_str(kind: MemoryKind) -> &'static str {
    match kind {
        MemoryKind::Knowledge => "knowledge",
        MemoryKind::Skill => "skill",
    }
}

/// Anything that is not exactly `skill` reads as knowledge, so a row with a
/// corrupt `kind` is still served rather than failing the whole listing.
pub fn kind_from_row(value: &str) -> MemoryKind {
    if value == "skill" {
        MemoryKind::Skill
    } else {
        MemoryKind::Knowledge
    }
}

/// What `rescan` reports. Counts, so `i64`. Declared here rather than generated:
/// no schema describes this envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RescanResponse {
    pub added: i64,
    pub removed: i64,
    pub changed: i64,
}

/// The provider id of user-authored, editable memories. Any other value is an
/// extension id.
pub const USER_MEMORY_PROVIDER: &str = "user";

/// Decides which extension providers' memories are visible: those whose
/// extension is installed, not merely downloaded, and not drifted.
///
/// The extensions module wires this. A core without extensions (mobile) leaves
/// it unset, in which case every provider passes; `user` is always active.
///
/// A SET rather than `is_active(provider)`, because two per-provider reads can
/// straddle an uninstall and answer one listing from two different worlds. Async
/// because the extension registry is a database.
#[async_trait]
pub trait ProviderGate: Send + Sync {
    async fn active_providers(&self) -> Result<HashSet<String>>;
}

/// One memory an extension declares in its `loca.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryDecl {
    pub kind: MemoryKind,
    /// Relative to the extension's install directory.
    pub path: String,
}

/// The `summary`/`embedding` freshness state of every memory, for the indexer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexState {
    pub id: String,
    pub content_hash: String,
    pub summary_source_hash: Option<String>,
    pub embedding_source_hash: Option<String>,
}

/// A stored embedding and the hash of the text plus model it came from.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredEmbedding {
    pub vector: Vec<f32>,
    pub source_hash: Option<String>,
}

/// What an exact-edit returns: the memory plus the before and after, for the
/// diff bubble.
///
/// Not `PartialEq`: the contract's `Memory` is not, and nothing compares two of
/// these.
#[derive(Debug, Clone)]
pub struct EditOutcome {
    pub memory: Memory,
    pub before: String,
    pub after: String,
}

const META_COLUMNS: &str =
    "id, kind, title, filename, content_hash, summary, summary_source_hash, \
                            provider, enabled, suggested_tools, created_at_ms, updated_at_ms";

#[derive(Clone)]
pub struct MemoriesStore {
    inner: Arc<Inner>,
}

struct Inner {
    paths: Arc<Paths>,
    db: Db,
    /// Serialises every read-modify-write. See the module doc.
    write_lock: tokio::sync::Mutex<()>,
    /// Provider id -> base directory. `user` is not in here: it resolves to the
    /// core's memories directory, which is derived from `paths` on call.
    providers: Mutex<HashMap<String, PathBuf>>,
    gate: Mutex<Option<Arc<dyn ProviderGate>>>,
}

impl MemoriesStore {
    pub fn new(paths: Arc<Paths>, db: Db) -> Self {
        Self {
            inner: Arc::new(Inner {
                paths,
                db,
                write_lock: tokio::sync::Mutex::new(()),
                providers: Mutex::new(HashMap::new()),
                gate: Mutex::new(None),
            }),
        }
    }

    /// Register where an extension's read-only memories live, so their content
    /// resolves. Idempotent.
    pub fn register_provider(&self, provider: &str, base_dir: PathBuf) {
        lock(&self.inner.providers).insert(provider.to_string(), base_dir);
    }

    /// Install (or clear) the gate deciding which extension providers are
    /// active.
    pub fn set_provider_gate(&self, gate: Option<Arc<dyn ProviderGate>>) {
        *lock(&self.inner.gate) = gate;
    }

    /// The installed gate, cloned out of its lock.
    ///
    /// Cloned rather than borrowed because the caller awaits the gate, and a
    /// `std::sync::MutexGuard` held across an await point is not `Send`.
    fn gate(&self) -> Option<Arc<dyn ProviderGate>> {
        lock(&self.inner.gate).clone()
    }

    /// The base directory a provider's content lives under.
    fn base_dir_for(&self, provider: &str) -> Result<PathBuf> {
        if provider == USER_MEMORY_PROVIDER {
            return Ok(self.inner.paths.memories_dir());
        }
        lock(&self.inner.providers)
            .get(provider)
            .cloned()
            .ok_or_else(|| not_found(format!("memory provider \"{provider}\" is not registered")))
    }

    // --- reads ---------------------------------------------------------------

    /// Every memory whose provider is active: user memories plus those of
    /// installed extensions.
    ///
    /// This backs the management UI, chat injection and the client's
    /// autocomplete, so an extension that is downloaded but not installed
    /// contributes nothing. Reconciliation that must see hidden rows uses
    /// [`MemoriesStore::all_rows`].
    pub async fn list(&self) -> Result<Vec<MemoryMeta>> {
        let rows = self.all_rows().await?;
        // No gate means every provider passes, which is what a core with no
        // extension subsystem at all (mobile) should answer.
        let Some(gate) = self.gate() else {
            return Ok(rows);
        };
        // ONE snapshot for the whole listing. See the trait's doc for why this
        // is not a lookup per row.
        let active = gate.active_providers().await?;
        Ok(rows
            .into_iter()
            .filter(|m| m.provider == USER_MEMORY_PROVIDER || active.contains(&m.provider))
            .collect())
    }

    /// Every row, regardless of provider status.
    pub async fn all_rows(&self) -> Result<Vec<MemoryMeta>> {
        self.inner
            .db
            .call(|conn| {
                let sql = format!(
                    "SELECT {META_COLUMNS} FROM memories ORDER BY title COLLATE NOCASE ASC"
                );
                let mut stmt = conn.prepare_cached(&sql)?;
                let rows = stmt
                    .query_map([], |row| Ok(row_to_meta(row)))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            })
            .await
    }

    pub async fn get(&self, id: &str) -> Result<Memory> {
        let meta = self.meta_or_throw(id).await?;
        self.hydrate(meta).await
    }

    /// Case-insensitive lookup. Titles are unique `COLLATE NOCASE`, and tools
    /// address memories by the title they saw in the prompt, which the model may
    /// re-case.
    pub async fn get_by_title(&self, title: &str) -> Result<Option<Memory>> {
        let title = title.to_string();
        let found = self
            .inner
            .db
            .call(move |conn| {
                let sql =
                    format!("SELECT {META_COLUMNS} FROM memories WHERE title = ? COLLATE NOCASE");
                let mut stmt = conn.prepare_cached(&sql)?;
                let row = stmt
                    .query_row([&title], |row| Ok(row_to_meta(row)))
                    .optional()?;
                Ok(row)
            })
            .await?;
        match found {
            Some(meta) => Ok(Some(self.hydrate(meta).await?)),
            None => Ok(None),
        }
    }

    /// Read one bundled reference file from a skill folder.
    pub async fn get_file(&self, id: &str, name: &str) -> Result<String> {
        let path = self.skill_file_path(&self.meta_or_throw(id).await?, name)?;
        content::read_text(path)
            .await
            .map_err(|_| not_found(format!("skill file \"{name}\" not found")))
    }

    // --- mutations -----------------------------------------------------------

    /// Create or replace a bundled reference file beside a user skill's
    /// `SKILL.md`. Bumps `updated_at_ms` so the card reflects the edit.
    pub async fn write_file(&self, id: &str, name: &str, body: &str) -> Result<()> {
        let _guard = self.inner.write_lock.lock().await;
        let path = self.skill_file_path(&self.editable_or_throw(id).await?, name)?;
        crate::files::write_atomic(path, body.as_bytes().to_vec()).await?;
        self.touch(id).await
    }

    /// Delete a bundled reference file from a user skill's folder.
    pub async fn delete_file(&self, id: &str, name: &str) -> Result<()> {
        let _guard = self.inner.write_lock.lock().await;
        let path = self.skill_file_path(&self.editable_or_throw(id).await?, name)?;
        // The existence check preserves the not_found a plain remove would not
        // give: `content::remove` is idempotent and would succeed silently.
        if !content::exists(path.clone()).await {
            return Err(not_found(format!("skill file \"{name}\" not found")));
        }
        content::remove(path, false).await?;
        self.touch(id).await
    }

    pub async fn create(&self, kind: MemoryKind, title: &str, body: &str) -> Result<Memory> {
        let _guard = self.inner.write_lock.lock().await;
        let clean_title = title.trim().to_string();
        if clean_title.is_empty() {
            return Err(validation("memory title required"));
        }
        // NOCASE: "Notes" beside "notes" would be two rows a title-keyed tool
        // API cannot tell apart.
        if self.title_exists(&clean_title, None).await? {
            return Err(conflict(format!("memory \"{clean_title}\" already exists")));
        }
        let id = new_memory_id();
        let filename = self.unique_name(kind, &clean_title).await?;
        let hash = content::content_hash(body);
        let now = now_ms();
        let skill = (kind == MemoryKind::Skill).then(|| parse_skill(body));
        let summary = skill
            .as_ref()
            .map(|s| s.description.clone())
            .filter(|d| !d.is_empty());
        let tools = suggested_tools_json(skill.as_ref().map(|s| s.suggested_tools.as_slice()));

        // The file is written BEFORE the row, so a failed write cannot leave a
        // row pointing at missing content; `rescan` tolerates an orphaned file,
        // so this is the recoverable order.
        let base = self.inner.paths.memories_dir();
        content::write_memory_content(&base, kind, &filename, body).await?;

        let inserted = {
            let id = id.clone();
            let filename = filename.clone();
            let hash = hash.clone();
            let summary = summary.clone();
            self.inner
                .db
                .call(move |conn| {
                    insert_memory(
                        conn,
                        &NewRow {
                            id: &id,
                            kind,
                            title: &clean_title,
                            filename: &filename,
                            content_hash: &hash,
                            summary: summary.as_deref(),
                            provider: USER_MEMORY_PROVIDER,
                            enabled: true,
                            suggested_tools: tools.as_deref(),
                            now,
                        },
                    )
                })
                .await
        };
        if let Err(err) = inserted {
            // The same path either way: a skill's folder IS `filename`, and a
            // knowledge file IS `filename`. Only the recursion differs.
            let _ = content::remove(base.join(&filename), kind == MemoryKind::Skill).await;
            return Err(err);
        }
        self.get(&id).await
    }

    pub async fn replace_content(&self, id: &str, body: &str) -> Result<Memory> {
        let _guard = self.inner.write_lock.lock().await;
        let meta = self.editable_or_throw(id).await?;
        self.write_content_inner(&meta, body).await?;
        self.get(id).await
    }

    /// Exact single-occurrence find and replace.
    ///
    /// Zero matches or more than one is an error rather than a guess, so the
    /// caller corrects its anchor instead of silently editing the wrong spot.
    pub async fn edit_content(&self, id: &str, find: &str, replace: &str) -> Result<EditOutcome> {
        let _guard = self.inner.write_lock.lock().await;
        if find.is_empty() {
            return Err(validation("find text must not be empty"));
        }
        let meta = self.editable_or_throw(id).await?;
        let before = self.read_content(&meta).await?;
        let Some(first) = before.find(find) else {
            return Err(validation("find text not found in memory"));
        };
        // One CHARACTER past the first match, not past its end, so
        // self-overlapping occurrences still count as ambiguous: `"\n\n"` in
        // `"a\n\n\nb"` matches at two offsets and must be rejected.
        let second_from = before[first..]
            .char_indices()
            .nth(1)
            .map_or(before.len(), |(offset, _)| first + offset);
        if before[second_from..].contains(find) {
            return Err(validation(
                "find text matches more than once; include more surrounding context",
            ));
        }
        let after = format!(
            "{}{replace}{}",
            &before[..first],
            &before[first + find.len()..]
        );
        self.write_content_inner(&meta, &after).await?;
        Ok(EditOutcome {
            memory: self.get(id).await?,
            before,
            after,
        })
    }

    pub async fn rename(&self, id: &str, title: &str) -> Result<MemoryMeta> {
        // Held for the whole check-then-act: the `UNIQUE` index on `title` is
        // BINARY, so it does not catch "Notes" beside "notes", and without the
        // lock two concurrent renames both pass the check and both commit.
        let _guard = self.inner.write_lock.lock().await;
        let clean_title = title.trim().to_string();
        if clean_title.is_empty() {
            return Err(validation("memory title required"));
        }
        let meta = self.editable_or_throw(id).await?;
        if clean_title == meta.title {
            return Ok(meta);
        }
        if self.title_exists(&clean_title, Some(id)).await? {
            return Err(conflict(format!("memory \"{clean_title}\" already exists")));
        }
        // The on-disk name stays put: names only have to be unique, and a rename
        // that moved files would break external references for no gain.
        let now = now_ms();
        let id_owned = id.to_string();
        self.inner
            .db
            .call(move |conn| {
                conn.execute(
                    "UPDATE memories SET title = ?1, updated_at_ms = ?2 WHERE id = ?3",
                    rusqlite::params![clean_title, now, id_owned],
                )?;
                Ok(())
            })
            .await?;
        self.meta_or_throw(id).await
    }

    /// Toggle whether a memory takes part in chat. Allowed for any provider: the
    /// user may silence an extension's memory without uninstalling it.
    pub async fn set_enabled(&self, id: &str, enabled: bool) -> Result<MemoryMeta> {
        self.meta_or_throw(id).await?;
        let now = now_ms();
        let id_owned = id.to_string();
        self.inner
            .db
            .call(move |conn| {
                conn.execute(
                    "UPDATE memories SET enabled = ?1, updated_at_ms = ?2 WHERE id = ?3",
                    rusqlite::params![i64::from(enabled), now, id_owned],
                )?;
                Ok(())
            })
            .await?;
        self.meta_or_throw(id).await
    }

    pub async fn delete(&self, id: &str) -> Result<()> {
        let _guard = self.inner.write_lock.lock().await;
        let meta = self.editable_or_throw(id).await?;
        let id_owned = id.to_string();
        self.inner
            .db
            .call(move |conn| {
                conn.execute("DELETE FROM memories WHERE id = ?1", [&id_owned])?;
                Ok(())
            })
            .await?;
        content::remove(
            self.inner.paths.memories_dir().join(&meta.filename),
            meta.kind == MemoryKind::Skill,
        )
        .await
    }

    /// Reconcile user rows against the files on disk.
    ///
    /// New `<slug>.md` files become knowledge and new `<slug>/SKILL.md` folders
    /// become skills, titled from the stem; rows whose content vanished are
    /// dropped; rows whose body drifted get their hash bumped so the indexer
    /// refreshes. An extension's memories are reconciled at extension load, not
    /// here.
    pub async fn rescan(&self) -> Result<RescanResponse> {
        let _guard = self.inner.write_lock.lock().await;
        let dir = self.inner.paths.memories_dir();

        // Every disk read happens before the transaction, so the transaction
        // itself is one synchronous critical section.
        let mut on_disk: IndexMap<String, (MemoryKind, String)> = IndexMap::new();
        if content::exists(dir.clone()).await {
            for entry in content::read_dir(dir.clone()).await {
                if !entry.is_dir && entry.name.ends_with(".md") {
                    if let Ok(body) = content::read_text(dir.join(&entry.name)).await {
                        on_disk.insert(entry.name, (MemoryKind::Knowledge, body));
                    }
                } else if entry.is_dir {
                    let body = dir.join(&entry.name).join(SKILL_FILE);
                    if let Ok(body) = content::read_text(body).await {
                        on_disk.insert(entry.name, (MemoryKind::Skill, body));
                    }
                }
                // A vanished entry, or a folder with no SKILL.md, is skipped;
                // the next rescan settles it.
            }
        }

        let now = now_ms();
        let counts = self
            .inner
            .db
            .call(move |conn| {
                let tx = conn.transaction()?;
                let mut added = 0i64;
                let mut removed = 0i64;
                let mut changed = 0i64;
                let mut on_disk = on_disk;

                let existing: Vec<MemoryMeta> = {
                    let sql = format!(
                        "SELECT {META_COLUMNS} FROM memories WHERE provider = ?1 \
                         ORDER BY title COLLATE NOCASE ASC"
                    );
                    let mut stmt = tx.prepare(&sql)?;
                    let rows = stmt
                        .query_map([USER_MEMORY_PROVIDER], |row| Ok(row_to_meta(row)))?
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    rows
                };

                for row in existing {
                    // Consumed only when it still describes the same sort of
                    // thing. A name whose kind flipped drops the row AND leaves
                    // the entry, so the adoption pass below re-adds it with the
                    // right kind in this same rescan rather than a later one.
                    let same_kind =
                        matches!(on_disk.get(&row.filename), Some((kind, _)) if *kind == row.kind);
                    if !same_kind {
                        tx.execute("DELETE FROM memories WHERE id = ?1", [&row.id])?;
                        removed += 1;
                        continue;
                    }
                    let Some((kind, body)) = on_disk.shift_remove(&row.filename) else {
                        continue;
                    };
                    let hash = content::content_hash(&body);
                    if hash != row.content_hash {
                        let skill = (kind == MemoryKind::Skill).then(|| parse_skill(&body));
                        let tools = suggested_tools_json(
                            skill.as_ref().map(|s| s.suggested_tools.as_slice()),
                        );
                        tx.execute(
                            "UPDATE memories SET content_hash = ?1, suggested_tools = ?2, \
                             updated_at_ms = ?3 WHERE id = ?4",
                            rusqlite::params![hash, tools, now, row.id],
                        )?;
                        changed += 1;
                    }
                }

                for (filename, (kind, body)) in on_disk {
                    let stem = filename.strip_suffix(".md").unwrap_or(&filename);
                    let title = unique_title(&tx, stem)?;
                    let skill = (kind == MemoryKind::Skill).then(|| parse_skill(&body));
                    let summary = skill
                        .as_ref()
                        .map(|s| s.description.clone())
                        .filter(|d| !d.is_empty());
                    let tools =
                        suggested_tools_json(skill.as_ref().map(|s| s.suggested_tools.as_slice()));
                    let hash = content::content_hash(&body);
                    insert_memory(
                        &tx,
                        &NewRow {
                            id: &new_memory_id(),
                            kind,
                            title: &title,
                            filename: &filename,
                            content_hash: &hash,
                            summary: summary.as_deref(),
                            provider: USER_MEMORY_PROVIDER,
                            enabled: true,
                            suggested_tools: tools.as_deref(),
                            now,
                        },
                    )?;
                    added += 1;
                }

                tx.commit()?;
                Ok(RescanResponse {
                    added,
                    removed,
                    changed,
                })
            })
            .await?;

        if counts.added != 0 || counts.removed != 0 || counts.changed != 0 {
            tracing::info!(
                target: "loca::memories",
                "rescan: {} added, {} removed, {} changed",
                counts.added, counts.removed, counts.changed,
            );
        }
        Ok(counts)
    }

    /// Reconcile the read-only memories an extension ships.
    ///
    /// `extensions_base_dir` is the **parent** of the install directory, so a
    /// row's `filename` (`<extensionId>/<decl.path>`) stays globally unique
    /// while resolving to `<installDir>/<decl.path>`. Declarations that vanished
    /// are removed; ones already present are left alone, so a reinstall does not
    /// churn ids or lose the user's enabled toggle.
    pub async fn register_extension_memories(
        &self,
        extension_id: &str,
        extensions_base_dir: PathBuf,
        decls: &[MemoryDecl],
    ) -> Result<()> {
        let _guard = self.inner.write_lock.lock().await;
        self.register_provider(extension_id, extensions_base_dir.clone());
        let now = now_ms();

        // Defence in depth: the manifest schema already rejects `..` and absolute
        // paths, and containment is re-checked before any path reaches the
        // filesystem.
        let install_dir = extensions_base_dir.join(extension_id);
        let mut want: IndexMap<String, MemoryDecl> = IndexMap::new();
        for decl in decls {
            if is_within(&install_dir, &install_dir.join(&decl.path)) {
                want.insert(format!("{extension_id}/{}", decl.path), decl.clone());
            } else {
                tracing::warn!(
                    target: "loca::memories",
                    "extension \"{extension_id}\" memory path escapes install dir; skipped: {}",
                    decl.path,
                );
            }
        }

        // Content is resolved up front so the transaction runs without an await.
        // A declared path missing on disk is skipped rather than failing the
        // install.
        let mut resolved: IndexMap<String, (MemoryDecl, String)> = IndexMap::new();
        for (filename, decl) in &want {
            let path = content::memory_path(&extensions_base_dir, decl.kind, filename);
            if let Ok(body) = content::read_text(path).await {
                resolved.insert(filename.clone(), (decl.clone(), body));
            }
        }

        let extension_id = extension_id.to_string();
        self.inner
            .db
            .call(move |conn| {
                let tx = conn.transaction()?;
                let mut want = want;

                let existing: Vec<(String, String)> = {
                    let mut stmt =
                        tx.prepare("SELECT id, filename FROM memories WHERE provider = ?1")?;
                    let rows = stmt
                        .query_map([&extension_id], |row| Ok((row.get(0)?, row.get(1)?)))?
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    rows
                };
                for (id, filename) in existing {
                    if want.shift_remove(&filename).is_none() {
                        tx.execute("DELETE FROM memories WHERE id = ?1", [&id])?;
                    }
                    // Present already: left exactly as it is, including whether
                    // the user disabled it.
                }

                for (filename, (decl, body)) in resolved {
                    if !want.contains_key(&filename) {
                        continue; // already present, left alone above
                    }
                    let hash = content::content_hash(&body);
                    let skill = (decl.kind == MemoryKind::Skill).then(|| parse_skill(&body));
                    let summary = skill
                        .as_ref()
                        .map(|s| s.description.clone())
                        .filter(|d| !d.is_empty());
                    let tools =
                        suggested_tools_json(skill.as_ref().map(|s| s.suggested_tools.as_slice()));
                    let title = unique_title(&tx, &content::pretty_title(&decl.path))?;
                    insert_memory(
                        &tx,
                        &NewRow {
                            id: &new_memory_id(),
                            kind: decl.kind,
                            title: &title,
                            filename: &filename,
                            content_hash: &hash,
                            summary: summary.as_deref(),
                            provider: &extension_id,
                            // Opt-in: an extension's memory arrives disabled.
                            enabled: false,
                            suggested_tools: tools.as_deref(),
                            now,
                        },
                    )?;
                }
                tx.commit()?;
                Ok(())
            })
            .await
    }

    /// Drop every memory an extension provided, when it is removed.
    pub async fn remove_extension_memories(&self, extension_id: &str) -> Result<()> {
        let extension_id = extension_id.to_string();
        self.inner
            .db
            .call(move |conn| {
                conn.execute("DELETE FROM memories WHERE provider = ?1", [&extension_id])?;
                Ok(())
            })
            .await
    }

    // --- index bookkeeping ---------------------------------------------------

    pub async fn set_summary(&self, id: &str, summary: &str, source_hash: &str) -> Result<()> {
        let (id, summary, source_hash) =
            (id.to_string(), summary.to_string(), source_hash.to_string());
        self.inner
            .db
            .call(move |conn| {
                conn.execute(
                    "UPDATE memories SET summary = ?1, summary_source_hash = ?2 WHERE id = ?3",
                    rusqlite::params![summary, source_hash, id],
                )?;
                Ok(())
            })
            .await
    }

    /// Store an embedding as raw little-endian `f32` bytes plus its width.
    ///
    /// The layout is what an existing install already has on disk, and every
    /// target this ships to is little-endian.
    pub async fn set_embedding(&self, id: &str, vector: &[f32], source_hash: &str) -> Result<()> {
        let blob: Vec<u8> = vector.iter().flat_map(|x| x.to_le_bytes()).collect();
        let dim = vector.len() as i64;
        let (id, source_hash) = (id.to_string(), source_hash.to_string());
        self.inner
            .db
            .call(move |conn| {
                conn.execute(
                    "UPDATE memories SET embedding = ?1, embedding_dim = ?2, \
                     embedding_source_hash = ?3 WHERE id = ?4",
                    rusqlite::params![blob, dim, source_hash, id],
                )?;
                Ok(())
            })
            .await
    }

    /// Content hash plus the hashes the stored summary and embedding were
    /// derived from, which is all the indexer needs to decide what is stale.
    pub async fn list_index_states(&self) -> Result<Vec<IndexState>> {
        self.inner
            .db
            .call(|conn| {
                let mut stmt = conn.prepare_cached(
                    "SELECT id, content_hash, summary_source_hash, embedding_source_hash \
                     FROM memories",
                )?;
                let rows = stmt
                    .query_map([], |row| {
                        Ok(IndexState {
                            id: row.get(0)?,
                            content_hash: row.get(1)?,
                            summary_source_hash: row.get(2)?,
                            embedding_source_hash: row.get(3)?,
                        })
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            })
            .await
    }

    /// Every stored embedding, paired with the source hash it was derived from,
    /// for scoring at prompt time.
    ///
    /// A `Vec` rather than a map, because the order is observable: equal scores
    /// keep SQLite's row order in the top-K.
    ///
    /// A row whose blob is shorter than its declared width is skipped with a
    /// warning rather than failing the load, so one corrupt row does not disable
    /// memory injection for the turn.
    pub async fn load_all_embeddings(&self) -> Result<Vec<(String, StoredEmbedding)>> {
        self.inner
            .db
            .call(|conn| {
                let mut stmt = conn.prepare_cached(
                    "SELECT id, embedding, embedding_dim, embedding_source_hash \
                     FROM memories WHERE embedding IS NOT NULL",
                )?;
                let rows = stmt
                    .query_map([], |row| {
                        let id: String = row.get(0)?;
                        let blob: Vec<u8> = row.get(1)?;
                        let dim: Option<i64> = row.get(2)?;
                        let source_hash: Option<String> = row.get(3)?;
                        Ok((id, blob, dim, source_hash))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                let mut out = Vec::with_capacity(rows.len());
                for (id, blob, dim, source_hash) in rows {
                    let dim = dim.unwrap_or(0).max(0) as usize;
                    if blob.len() < dim * 4 {
                        tracing::warn!(
                            target: "loca::memories",
                            "memory {id} has a {}-byte embedding blob for a declared width of {dim}; skipped",
                            blob.len(),
                        );
                        continue;
                    }
                    let vector = blob[..dim * 4]
                        .chunks_exact(4)
                        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                        .collect();
                    out.push((
                        id,
                        StoredEmbedding {
                            vector,
                            source_hash,
                        },
                    ));
                }
                Ok(out)
            })
            .await
    }

    // --- internals -----------------------------------------------------------

    /// A memory's metadata, plus its content and (for a skill) its bundled file
    /// names.
    async fn hydrate(&self, meta: MemoryMeta) -> Result<Memory> {
        let body = self.read_content(&meta).await?;
        let files = self.skill_files(&meta).await;
        Ok(Memory {
            id: meta.id,
            kind: meta.kind,
            title: meta.title,
            filename: meta.filename,
            content_hash: meta.content_hash,
            summary: meta.summary,
            summary_stale: meta.summary_stale,
            provider: meta.provider,
            enabled: meta.enabled,
            suggested_tools: meta.suggested_tools,
            files,
            created_at_ms: meta.created_at_ms,
            updated_at_ms: meta.updated_at_ms,
            content: body,
        })
    }

    /// A memory's content.
    ///
    /// A row whose file vanished is `not_found`, never empty content: surfacing
    /// the loss beats letting a tool "read", or overwrite, an empty memory it
    /// believes is real.
    async fn read_content(&self, meta: &MemoryMeta) -> Result<String> {
        let base = self.base_dir_for(&meta.provider)?;
        let path = content::memory_path(&base, meta.kind, &meta.filename);
        content::read_text(path).await.map_err(|_| {
            not_found(format!(
                "memory content for \"{}\" is missing on disk; run a memories rescan",
                meta.filename
            ))
        })
    }

    /// Bundled file names beside a skill's `SKILL.md`, or `None` for knowledge.
    /// Best-effort: an unreadable folder yields an empty list.
    async fn skill_files(&self, meta: &MemoryMeta) -> Option<Vec<String>> {
        if meta.kind != MemoryKind::Skill {
            return None;
        }
        let base = self.base_dir_for(&meta.provider).ok()?;
        let mut names: Vec<String> = content::read_dir(base.join(&meta.filename))
            .await
            .into_iter()
            .filter(|e| !e.is_dir && e.name != SKILL_FILE)
            .map(|e| e.name)
            .collect();
        names.sort();
        Some(names)
    }

    /// Bump the row's content hash and suggested tools, then write the file.
    /// Runs only inside a locked section.
    async fn write_content_inner(&self, meta: &MemoryMeta, body: &str) -> Result<()> {
        let hash = content::content_hash(body);
        let skill = (meta.kind == MemoryKind::Skill).then(|| parse_skill(body));
        let tools = suggested_tools_json(skill.as_ref().map(|s| s.suggested_tools.as_slice()));
        let now = now_ms();
        let id = meta.id.clone();
        self.inner
            .db
            .call(move |conn| {
                conn.execute(
                    "UPDATE memories SET content_hash = ?1, suggested_tools = ?2, \
                     updated_at_ms = ?3 WHERE id = ?4",
                    rusqlite::params![hash, tools, now, id],
                )?;
                Ok(())
            })
            .await?;
        content::write_memory_content(
            &self.inner.paths.memories_dir(),
            meta.kind,
            &meta.filename,
            body,
        )
        .await
    }

    async fn meta_or_throw(&self, id: &str) -> Result<MemoryMeta> {
        let id_owned = id.to_string();
        let found = self
            .inner
            .db
            .call(move |conn| {
                let sql = format!("SELECT {META_COLUMNS} FROM memories WHERE id = ?1");
                let mut stmt = conn.prepare_cached(&sql)?;
                Ok(stmt
                    .query_row([&id_owned], |row| Ok(row_to_meta(row)))
                    .optional()?)
            })
            .await?;
        found.ok_or_else(|| not_found(format!("memory {id} not found")))
    }

    /// Like [`MemoriesStore::meta_or_throw`], but refuses an extension-provided
    /// memory: those are read-only and are refreshed by reinstalling.
    async fn editable_or_throw(&self, id: &str) -> Result<MemoryMeta> {
        let meta = self.meta_or_throw(id).await?;
        if meta.provider != USER_MEMORY_PROVIDER {
            return Err(conflict(format!(
                "memory {id} is provided by an extension and is read-only"
            )));
        }
        Ok(meta)
    }

    async fn touch(&self, id: &str) -> Result<()> {
        let now = now_ms();
        let id = id.to_string();
        self.inner
            .db
            .call(move |conn| {
                conn.execute(
                    "UPDATE memories SET updated_at_ms = ?1 WHERE id = ?2",
                    rusqlite::params![now, id],
                )?;
                Ok(())
            })
            .await
    }

    /// Whether some **other** row already holds this title, case-insensitively.
    ///
    /// The exclusion is applied to the first matching row rather than in SQL:
    /// renaming a memory to the title it already has is not a conflict, and
    /// renaming it to another row's is.
    async fn title_exists(&self, title: &str, exclude_id: Option<&str>) -> Result<bool> {
        let title = title.to_string();
        let exclude = exclude_id.map(str::to_string);
        self.inner
            .db
            .call(move |conn| {
                let mut stmt =
                    conn.prepare_cached("SELECT id FROM memories WHERE title = ?1 COLLATE NOCASE")?;
                let found: Option<String> =
                    stmt.query_row([&title], |row| row.get(0)).optional()?;
                Ok(match (found, exclude) {
                    (None, _) => false,
                    (Some(id), Some(exclude)) => id != exclude,
                    (Some(_), None) => true,
                })
            })
            .await
    }

    /// A filename that collides with neither a row nor a stray entry on disk.
    async fn unique_name(&self, kind: MemoryKind, title: &str) -> Result<String> {
        let slug = content::slugify(title);
        let dir = self.inner.paths.memories_dir();
        for i in 1u32.. {
            let stem = if i == 1 {
                slug.clone()
            } else {
                format!("{slug}-{i}")
            };
            let name = match kind {
                MemoryKind::Skill => stem,
                MemoryKind::Knowledge => format!("{stem}.md"),
            };
            let probe = name.clone();
            let in_db = self
                .inner
                .db
                .call(move |conn| {
                    let mut stmt =
                        conn.prepare_cached("SELECT 1 FROM memories WHERE filename = ?1")?;
                    Ok(stmt.query_row([&probe], |_| Ok(())).optional()?.is_some())
                })
                .await?;
            if in_db {
                continue;
            }
            if !content::exists(dir.join(&name)).await {
                return Ok(name);
            }
        }
        // `1u32..` is unbounded; this is unreachable and exists only because the
        // loop is not `loop`.
        Err(crate::error::internal(
            "could not allocate a memory filename",
        ))
    }

    fn skill_file_path(&self, meta: &MemoryMeta, name: &str) -> Result<PathBuf> {
        if meta.kind != MemoryKind::Skill {
            return Err(validation("memory is not a skill"));
        }
        content::check_skill_file_name(name)?;
        Ok(self
            .base_dir_for(&meta.provider)?
            .join(&meta.filename)
            .join(name))
    }
}

// --- free helpers ------------------------------------------------------------

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

pub fn new_memory_id() -> String {
    ulid::Ulid::new().to_string()
}

/// True when `candidate` resolves inside `root`, or is `root` itself. Pure path
/// maths, no filesystem access, so a path that does not exist yet is still
/// checked.
use crate::contain::is_within;

/// The `suggested_tools` column: a JSON array, or NULL when there are none.
/// Absent and empty are the same thing here, which is what the column already
/// holds in every existing install.
fn suggested_tools_json(tools: Option<&[String]>) -> Option<String> {
    let tools = tools?;
    if tools.is_empty() {
        return None;
    }
    serde_json::to_string(tools).ok()
}

struct NewRow<'a> {
    id: &'a str,
    kind: MemoryKind,
    title: &'a str,
    filename: &'a str,
    content_hash: &'a str,
    summary: Option<&'a str>,
    provider: &'a str,
    enabled: bool,
    suggested_tools: Option<&'a str>,
    now: i64,
}

/// One insert, shared by create, rescan adoption and extension registration, so
/// the column list and the summary-pinning rule exist once.
///
/// `summary_source_hash` is set only when a summary is present: a skill's
/// frontmatter description seeds the summary and is by definition derived from
/// the content being inserted, so it is already fresh.
fn insert_memory(conn: &Connection, row: &NewRow<'_>) -> Result<()> {
    conn.execute(
        "INSERT INTO memories (id, kind, title, filename, content_hash, summary, \
         summary_source_hash, provider, enabled, suggested_tools, created_at_ms, updated_at_ms) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        rusqlite::params![
            row.id,
            kind_as_str(row.kind),
            row.title,
            row.filename,
            row.content_hash,
            row.summary,
            row.summary.map(|_| row.content_hash),
            row.provider,
            i64::from(row.enabled),
            row.suggested_tools,
            row.now,
            row.now,
        ],
    )?;
    Ok(())
}

/// A title that no row holds yet, suffixed `" (2)"`, `" (3)"`, and so on.
fn unique_title(conn: &Connection, stem: &str) -> rusqlite::Result<String> {
    let base = {
        let trimmed = stem.trim();
        if trimmed.is_empty() {
            "Memory"
        } else {
            trimmed
        }
    };
    for i in 1u32.. {
        let title = if i == 1 {
            base.to_string()
        } else {
            format!("{base} ({i})")
        };
        let mut stmt =
            conn.prepare_cached("SELECT 1 FROM memories WHERE title = ?1 COLLATE NOCASE")?;
        if stmt.query_row([&title], |_| Ok(())).optional()?.is_none() {
            return Ok(title);
        }
    }
    Ok(base.to_string())
}

fn row_to_meta(row: &Row<'_>) -> MemoryMeta {
    let kind = kind_from_row(&row.get::<_, String>(1).unwrap_or_default());
    let content_hash: String = row.get(4).unwrap_or_default();
    let summary_source_hash: Option<String> = row.get(6).unwrap_or_default();
    let suggested: Option<String> = row.get(9).unwrap_or_default();
    // A malformed cache is ignored rather than failing the read.
    let suggested_tools = suggested
        .and_then(|raw| serde_json::from_str::<Vec<serde_json::Value>>(&raw).ok())
        .map(|values| {
            values
                .into_iter()
                .map(|v| match v {
                    serde_json::Value::String(s) => s,
                    other => other.to_string(),
                })
                .collect::<Vec<_>>()
        });
    MemoryMeta {
        id: row.get(0).unwrap_or_default(),
        kind,
        title: row.get(2).unwrap_or_default(),
        filename: row.get(3).unwrap_or_default(),
        // Stale when never indexed, or edited since. The contract types it
        // optional, but it is computed on every row and is always `Some`.
        summary_stale: Some(summary_source_hash.as_deref() != Some(content_hash.as_str())),
        content_hash,
        summary: row.get(5).unwrap_or_default(),
        provider: row.get(7).unwrap_or_default(),
        enabled: row.get::<_, i64>(8).unwrap_or(0) != 0,
        suggested_tools: if kind == MemoryKind::Skill {
            suggested_tools
        } else {
            None
        },
        files: None,
        created_at_ms: row.get(10).unwrap_or_default(),
        updated_at_ms: row.get(11).unwrap_or_default(),
    }
}
