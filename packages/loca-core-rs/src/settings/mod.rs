//! Settings store and routes.
//!
//! Work package **D1** (`docs/rust-port/registry.md`). This module is that
//! package's owned path; nothing outside it writes here.
//!
//! Two stores sit behind one endpoint:
//!
//! - the **shared** core store, `<root>/settings.json`, holding the settings
//!   every client of this core sees (the model, the sidecars, the tool worker
//!   pool, the installed extensions and MCP servers), and
//! - the **per-client overlay**, the `client_settings` table, holding one
//!   client's own inference preferences (sampling, prompts, tool and memory
//!   selection), which the core must apply server-side even for a scheduled
//!   session with no client attached.
//!
//! Both are **sparse**: a key that is absent takes the schema's default, which
//! is what makes a changed default reach an existing install. Sparseness is a
//! property of what gets written, not a filter applied on write: the Client
//! PATCHes a diff and sends `null` to revert a key, and the core stores exactly
//! what it is handed.

pub mod access;
pub mod routes;
pub mod schema;
pub mod seams;
pub mod validate;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use rusqlite::OptionalExtension as _;
use serde_json::{Map, Value};
use tokio::sync::{broadcast, Mutex};

use crate::contract::api::ServerToClientFrame;
use crate::db::Db;
use crate::error::{internal, ErrorCode, Result};
use crate::files;
use crate::paths::Paths;
use crate::secrets::Vault;
use crate::transport::FrameBus;

pub use access::{bool_setting, num_setting, str_setting};
pub use schema::{schema, Destination};
pub use seams::BrokerSettingsStore;
pub use validate::validate_settings_patch;

/// The sparse on-disk shape: only the keys someone set, in no particular order.
pub type SettingsRecord = Map<String, Value>;

/// A change to the shared store, published to in-core subscribers.
///
/// `changed` names the keys whose value moved, whether set or deleted, so a
/// subscriber can decide cheaply whether it cares. `merged` is the whole sparse
/// record after the change, not a delta.
#[derive(Debug, Clone)]
pub struct CoreSettingsChange {
    pub merged: SettingsRecord,
    pub changed: BTreeSet<String>,
}

/// What one client's overlay patch actually did, split for the WS delta.
#[derive(Debug, Clone, Default)]
pub struct ClientDelta {
    pub values: SettingsRecord,
    pub deleted: Vec<String>,
}

impl ClientDelta {
    pub fn is_empty(&self) -> bool {
        self.values.is_empty() && self.deleted.is_empty()
    }
}

/// A settings change is an edge, and the subscribers that matter (the
/// requirements recompute, the sidecar restarts) resync by reading the store, so
/// a lagging one loses nothing by missing an item. Sixteen matches the vault's
/// channel.
const CHANGE_CHANNEL_CAPACITY: usize = 16;

/// The settings service. Cheap to clone; every clone shares one cache, one write
/// lock and one change broadcast.
///
/// There is exactly one of these per core, constructed by the frontend and
/// carried in the app context. Two handles over one file would each hold their
/// own cache and their own lock, which is the lost update the lock exists to
/// prevent: both read the same base, and the second write drops the first's
/// change on disk and in memory.
#[derive(Clone)]
pub struct Settings {
    inner: Arc<Inner>,
}

struct Inner {
    file: PathBuf,
    db: Db,
    frames: Arc<FrameBus>,
    /// The cached shared store, and the critical section for every
    /// read-modify-write of it. One lock rather than two because every write
    /// needs both: `load` fills it, `patch` holds it across the merge and the
    /// file write.
    ///
    /// A `tokio` mutex specifically, since the guard is held across the `.await`
    /// on that write.
    core: Mutex<Option<SettingsRecord>>,
    changes: broadcast::Sender<Arc<CoreSettingsChange>>,
    relay_spawned: AtomicBool,
}

impl std::fmt::Debug for Settings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Settings")
    }
}

impl Settings {
    pub fn new(paths: &Paths, db: Db, frames: Arc<FrameBus>) -> Self {
        Self::with_file(paths.settings_file(), db, frames)
    }

    /// An explicit file, for tests over a tempdir.
    pub fn with_file(file: impl Into<PathBuf>, db: Db, frames: Arc<FrameBus>) -> Self {
        let (changes, _) = broadcast::channel(CHANGE_CHANNEL_CAPACITY);
        Self {
            inner: Arc::new(Inner {
                file: file.into(),
                db,
                frames,
                core: Mutex::new(None),
                changes,
                relay_spawned: AtomicBool::new(false),
            }),
        }
    }

    // --- the shared store ----------------------------------------------------

    /// The sparse shared store: only the keys someone set.
    pub async fn load(&self) -> Result<SettingsRecord> {
        let mut cache = self.inner.core.lock().await;
        Ok(fill(&mut cache, &self.inner.file).await?.clone())
    }

    /// The shared store with the schema defaults underneath it, and NO
    /// per-client overlay.
    ///
    /// For core-global consumers only: requirement computation, model ensure,
    /// storage. The default model path and `llm.supportImages` are not persisted
    /// while they are unchanged, so requirement computation would see neither.
    ///
    /// **Not for anything on a chat turn.** This is the method that looks like
    /// the fix and is not: it silently drops the owning client's own inference
    /// preferences. A turn wants [`Self::load_effective_resolved`].
    pub async fn load_resolved(&self) -> Result<SettingsRecord> {
        let sparse = self.load().await?;
        let mut resolved = schema().defaults().clone();
        for (key, value) in sparse {
            resolved.insert(key, value);
        }
        Ok(resolved)
    }

    /// Apply a sparse patch to the shared store. A `null` deletes a key,
    /// reverting it to the schema default. Returns the merged record.
    ///
    /// Load, merge, write and cache are one critical section: a concurrent patch
    /// must not read the same base and clobber this write.
    pub async fn patch(&self, patch: SettingsRecord) -> Result<SettingsRecord> {
        let (merged, changed) = {
            let mut cache = self.inner.core.lock().await;
            let mut merged = fill(&mut cache, &self.inner.file).await?.clone();
            let mut changed = BTreeSet::new();
            for (key, value) in patch {
                if value.is_null() {
                    if merged.remove(&key).is_some() {
                        changed.insert(key);
                    }
                } else if !merged
                    .get(&key)
                    .is_some_and(|current| validate::same_scalar(current, &value))
                {
                    merged.insert(key.clone(), value);
                    changed.insert(key);
                }
            }
            // Nothing to write when nothing moved. The bytes would be identical
            // and no consumer reads this file's mtime.
            if !changed.is_empty() {
                files::write_json_atomic(self.inner.file.clone(), &merged).await?;
                *cache = Some(merged.clone());
            }
            (merged, changed)
        };
        if !changed.is_empty() {
            self.announce(&merged, &changed);
        }
        Ok(merged)
    }

    /// Reset every setting to its schema default: the sparse file becomes empty,
    /// and every previously set key is broadcast as deleted.
    ///
    /// The storage view's factory reset. Secrets and the per-client overlays are
    /// cleared alongside this by the same caller.
    pub async fn reset(&self) -> Result<()> {
        let changed = {
            let mut cache = self.inner.core.lock().await;
            let previous: BTreeSet<String> = fill(&mut cache, &self.inner.file)
                .await?
                .keys()
                .cloned()
                .collect();
            // Written unconditionally: a factory reset should leave the file
            // definitively empty, including when it was unreadable junk.
            files::write_json_atomic(self.inner.file.clone(), &SettingsRecord::new()).await?;
            *cache = Some(SettingsRecord::new());
            previous
        };
        if !changed.is_empty() {
            self.announce(&SettingsRecord::new(), &changed);
        }
        Ok(())
    }

    /// Subscribe to shared-store changes.
    ///
    /// For in-core consumers: the requirements recompute, the sidecar restarts,
    /// the LLM endpoint resolver. A subscriber that lags gets `Lagged` and
    /// resyncs with [`Settings::load`]; it never stalls a patch.
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<CoreSettingsChange>> {
        self.inner.changes.subscribe()
    }

    // --- the per-client overlay ----------------------------------------------

    /// The SPARSE settings for work owned by `client_id`: the shared values with
    /// that client's own overrides on top.
    ///
    /// Sparse on purpose, and the settings routes are the only correct consumer:
    /// absence is how the wire says "unset", which is what lets
    /// `GET /api/v1/settings` answer `{}` for a fresh core and what stops a
    /// client round-tripping a read into a PATCH that persists every default.
    ///
    /// Anything that ACTS on a setting wants [`Self::load_effective_resolved`].
    pub async fn load_effective(&self, client_id: &str) -> Result<SettingsRecord> {
        let mut effective = self.load().await?;
        for (key, value) in self.load_overlay(client_id).await? {
            effective.insert(key, value);
        }
        Ok(effective)
    }

    /// [`Self::load_effective`] with the schema defaults underneath: what every
    /// consumer that ACTS on a setting reads, chat turns above all.
    ///
    /// The two halves existed separately and neither was correct for a turn.
    /// [`Self::load_resolved`] applies the defaults but drops the overlay, losing
    /// the client's own inference preferences. [`Self::load_effective`] applies
    /// the overlay but stays sparse, and the doc on it used to claim readers
    /// applied the schema defaults afterwards. They never did: `str_setting` /
    /// `num_setting` / `bool_setting` apply the fallback their CALLER hardcodes,
    /// which differs from the schema wherever the two disagree and supplies
    /// nothing at all where the caller passes `0` or `""`.
    ///
    /// That is why `prompts.titleGenerationThinkingBudget` (schema 128) and
    /// `prompts.memorySummaryThinkingBudget` (schema 256) resolved to 0 on every
    /// install, and thinking was off everywhere the panel said it was on. The
    /// blast radius is wider than those two: resolving underneath makes the
    /// schema authoritative for every `*_setting` read on the turn path at once,
    /// which is the ruling. See D-037 / D-069 / D-072.
    pub async fn load_effective_resolved(&self, client_id: &str) -> Result<SettingsRecord> {
        let mut resolved = schema().defaults().clone();
        for (key, value) in self.load_effective(client_id).await? {
            resolved.insert(key, value);
        }
        Ok(resolved)
    }

    /// One client's overlay: only the `client-on-core` keys it set.
    pub async fn load_overlay(&self, client_id: &str) -> Result<SettingsRecord> {
        let client_id = client_id.to_string();
        self.inner
            .db
            .call(move |conn| {
                let mut stmt = conn.prepare_cached(
                    "SELECT key, value_json FROM client_settings WHERE client_id = ?1",
                )?;
                let rows = stmt.query_map([&client_id], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?;
                let mut overlay = SettingsRecord::new();
                for row in rows {
                    let (key, json) = row?;
                    match serde_json::from_str(&json) {
                        Ok(value) => {
                            overlay.insert(key, value);
                        }
                        Err(err) => tracing::warn!(
                            target: "loca::core-settings",
                            "dropping corrupt client_settings value for {key}: {err}",
                        ),
                    }
                }
                Ok(overlay)
            })
            .await
    }

    /// Apply a sparse patch to one client's overlay. A `null` deletes a key,
    /// reverting it to the shared value.
    ///
    /// All or nothing: the per-key upserts and deletes run in one transaction,
    /// so a failure partway cannot leave the overlay half applied.
    pub async fn patch_client(
        &self,
        client_id: &str,
        patch: SettingsRecord,
    ) -> Result<ClientDelta> {
        let owner = client_id.to_string();
        let delta = self
            .inner
            .db
            .call(move |conn| {
                let tx = conn.transaction()?;
                let mut delta = ClientDelta::default();
                {
                    let mut current = tx.prepare_cached(
                        "SELECT value_json FROM client_settings WHERE client_id = ?1 AND key = ?2",
                    )?;
                    let mut upsert = tx.prepare_cached(
                        "INSERT INTO client_settings (client_id, key, value_json) VALUES (?1, ?2, ?3) \
                         ON CONFLICT(client_id, key) DO UPDATE SET value_json = excluded.value_json",
                    )?;
                    let mut delete = tx.prepare_cached(
                        "DELETE FROM client_settings WHERE client_id = ?1 AND key = ?2",
                    )?;
                    for (key, value) in patch {
                        let stored: Option<String> = current
                            .query_row((&owner, &key), |row| row.get(0))
                            .optional()?;
                        let stored: Option<Value> =
                            stored.and_then(|json| serde_json::from_str(&json).ok());
                        if value.is_null() {
                            if stored.is_some() {
                                delete.execute((&owner, &key))?;
                                delta.deleted.push(key);
                            }
                        } else if !stored
                            .is_some_and(|current| validate::same_scalar(&current, &value))
                        {
                            upsert.execute((&owner, &key, &value.to_string()))?;
                            delta.values.insert(key, value);
                        }
                    }
                }
                tx.commit()?;
                Ok(delta)
            })
            .await?;
        if !delta.is_empty() {
            // To the owning client alone. Another client must never see, or
            // converge onto, someone else's preferences.
            let (values, deleted) = wire_delta(
                &delta.values,
                delta
                    .values
                    .keys()
                    .chain(delta.deleted.iter())
                    .cloned()
                    .collect::<BTreeSet<_>>(),
            );
            if !values.is_empty() || !deleted.is_empty() {
                self.inner.frames.broadcast_to_client(
                    client_id,
                    &ServerToClientFrame::SettingsUpdated {
                        values,
                        deleted,
                        secret_names: None,
                    },
                );
            }
        }
        Ok(delta)
    }

    /// Wipe every client's overlay, and tell each affected client which of its
    /// keys went.
    ///
    /// The other half of the factory reset: the shared store and the vault are
    /// cleared alongside this, so leaving the per-client inference overrides
    /// would keep each client's agent personality intact through a "clear
    /// settings".
    pub async fn reset_all_clients(&self) -> Result<()> {
        let rows: Vec<(String, String)> = self
            .inner
            .db
            .call(|conn| {
                let tx = conn.transaction()?;
                let rows = {
                    let mut stmt = tx.prepare("SELECT client_id, key FROM client_settings")?;
                    let mapped =
                        stmt.query_map([], |row| Ok((row.get(0)?, row.get::<_, String>(1)?)))?;
                    mapped.collect::<std::result::Result<Vec<_>, _>>()?
                };
                tx.execute("DELETE FROM client_settings", [])?;
                tx.commit()?;
                Ok(rows)
            })
            .await?;
        let mut by_client: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for (client_id, key) in rows {
            by_client.entry(client_id).or_default().insert(key);
        }
        for (client_id, keys) in by_client {
            let (values, deleted) = wire_delta(&SettingsRecord::new(), keys);
            if !deleted.is_empty() {
                self.inner.frames.broadcast_to_client(
                    &client_id,
                    &ServerToClientFrame::SettingsUpdated {
                        values,
                        deleted,
                        secret_names: None,
                    },
                );
            }
        }
        Ok(())
    }

    // --- broadcasts ----------------------------------------------------------

    /// Relay the vault's name list onto the settings frame, so every client's
    /// "saved" placeholders stay fresh.
    ///
    /// One task per core, guarded rather than global: two cores in one process
    /// is exactly what the conformance harness wants. Spawned by the route slice
    /// because the vault is a mutation surface for several packages (the factory
    /// reset, MCP tokens, uninstall), and subscribing once here covers all of
    /// them where a per-route broadcast would cover only this one.
    pub fn spawn_secret_name_relay(&self, vault: &Vault) {
        if self.inner.relay_spawned.swap(true, Ordering::SeqCst) {
            return;
        }
        let mut names = vault.subscribe();
        let frames = Arc::clone(&self.inner.frames);
        tokio::spawn(async move {
            loop {
                match names.recv().await {
                    Ok(names) => frames.broadcast_all(&ServerToClientFrame::SettingsUpdated {
                        values: SettingsRecord::new(),
                        deleted: Vec::new(),
                        secret_names: Some(names.to_vec()),
                    }),
                    Err(broadcast::error::RecvError::Lagged(missed)) => tracing::warn!(
                        target: "loca::core-settings",
                        "missed {missed} vault changes; clients resync on their next settings load",
                    ),
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    /// Broadcast a shared-store change to every client, and publish it in-core.
    fn announce(&self, merged: &SettingsRecord, changed: &BTreeSet<String>) {
        let (values, deleted) = wire_delta(merged, changed.clone());
        if !values.is_empty() || !deleted.is_empty() {
            self.inner
                .frames
                .broadcast_all(&ServerToClientFrame::SettingsUpdated {
                    values,
                    deleted,
                    secret_names: None,
                });
        }
        // No subscriber is not an error: the send fails when nobody is
        // listening, which is the normal state of a core with no sidecars up.
        let _ = self.inner.changes.send(Arc::new(CoreSettingsChange {
            merged: merged.clone(),
            changed: changed.clone(),
        }));
    }
}

/// Split a changed-key set into the frame's `values` and `deleted`.
///
/// Defence in depth: a key that is not in the schema, or is secret-typed, never
/// crosses the wire whatever ended up in the store.
fn wire_delta(merged: &SettingsRecord, changed: BTreeSet<String>) -> (SettingsRecord, Vec<String>) {
    let schema = schema();
    let mut values = SettingsRecord::new();
    let mut deleted = Vec::new();
    for key in changed {
        if !schema.is_valid_key(&key) || schema.is_secret_key(&key) {
            continue;
        }
        match merged.get(&key) {
            Some(value) => {
                values.insert(key, value.clone());
            }
            None => deleted.push(key),
        }
    }
    (values, deleted)
}

/// The cached record, read from disk on a miss.
async fn fill<'a>(
    cache: &'a mut Option<SettingsRecord>,
    file: &Path,
) -> Result<&'a mut SettingsRecord> {
    if cache.is_none() {
        *cache = Some(read_file(file).await?);
    }
    cache
        .as_mut()
        .ok_or_else(|| internal("settings cache was not filled"))
}

/// An absent file is an empty record: every value is its default.
async fn read_file(file: &Path) -> Result<SettingsRecord> {
    match files::read_json::<Value>(file.to_path_buf()).await {
        Ok(Value::Object(map)) => Ok(map),
        Ok(_) => Err(internal("settings file is not an object")),
        Err(err) if err.code == ErrorCode::NotFound => Ok(SettingsRecord::new()),
        Err(err) => Err(err),
    }
}

#[cfg(test)]
mod tests;
