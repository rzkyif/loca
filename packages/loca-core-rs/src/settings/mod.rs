//! Settings store and routes.
//!
//! Two sparse stores behind one endpoint: the shared `<root>/settings.json`
//! every client sees, and the per-client overlay in `client_settings` holding
//! one client's inference preferences (applied server-side even for a scheduled
//! session). An absent key takes the schema default; the core stores exactly
//! what it is handed and `null` reverts a key.

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

/// The sparse on-disk shape: only the keys someone set.
pub type SettingsRecord = Map<String, Value>;

/// A change to the shared store: `changed` names the keys that moved (set or
/// deleted), `merged` is the whole sparse record afterwards.
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

/// Subscribers resync by reading the store, so a lagging one loses nothing.
const CHANGE_CHANNEL_CAPACITY: usize = 16;

/// The settings service. Cheap to clone; every clone shares one cache, one
/// write lock and one change broadcast. Exactly one per core: two handles over
/// one file would each hold their own lock, which is the lost update it exists
/// to prevent.
#[derive(Clone)]
pub struct Settings {
    inner: Arc<Inner>,
}

struct Inner {
    file: PathBuf,
    db: Db,
    frames: Arc<FrameBus>,
    /// The cached shared store and the critical section for every
    /// read-modify-write of it. A `tokio` mutex: the guard is held across the
    /// `.await` on the file write.
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

    /// The shared store over the schema defaults, with NO per-client overlay.
    /// For core-global consumers only (requirements, model ensure, storage);
    /// a chat turn wants [`Self::load_effective_resolved`].
    pub async fn load_resolved(&self) -> Result<SettingsRecord> {
        let sparse = self.load().await?;
        let mut resolved = schema().defaults().clone();
        for (key, value) in sparse {
            resolved.insert(key, value);
        }
        Ok(resolved)
    }

    /// Apply a sparse patch to the shared store. A `null` deletes a key.
    /// Load, merge, write and cache are one critical section so a concurrent
    /// patch cannot read the same base and clobber this write.
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

    /// Factory reset of the shared store: the file becomes empty (written
    /// unconditionally, even when it was unreadable junk) and every previously
    /// set key is broadcast as deleted.
    pub async fn reset(&self) -> Result<()> {
        let changed = {
            let mut cache = self.inner.core.lock().await;
            let previous: BTreeSet<String> = fill(&mut cache, &self.inner.file)
                .await?
                .keys()
                .cloned()
                .collect();
            files::write_json_atomic(self.inner.file.clone(), &SettingsRecord::new()).await?;
            *cache = Some(SettingsRecord::new());
            previous
        };
        if !changed.is_empty() {
            self.announce(&SettingsRecord::new(), &changed);
        }
        Ok(())
    }

    /// Subscribe to shared-store changes. A lagging subscriber gets `Lagged`
    /// and resyncs with [`Settings::load`]; it never stalls a patch.
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<CoreSettingsChange>> {
        self.inner.changes.subscribe()
    }

    // --- the per-client overlay ----------------------------------------------

    /// The SPARSE effective settings for `client_id`: shared values with its own
    /// overrides on top. For the settings routes only, where absence means
    /// "unset" on the wire; anything that ACTS on a setting wants
    /// [`Self::load_effective_resolved`].
    pub async fn load_effective(&self, client_id: &str) -> Result<SettingsRecord> {
        let mut effective = self.load().await?;
        for (key, value) in self.load_overlay(client_id).await? {
            effective.insert(key, value);
        }
        Ok(effective)
    }

    /// [`Self::load_effective`] over the schema defaults: what every consumer
    /// that ACTS on a setting reads. The `*_setting` readers fall back to their
    /// caller's default, not the schema's, so resolving underneath is what makes
    /// the schema authoritative on the turn path.
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

    /// Apply a sparse patch to one client's overlay in one transaction. A
    /// `null` deletes a key, reverting it to the shared value.
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
            // To the owning client alone: another client must never see, or
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

    /// Wipe every client's overlay (the other half of the factory reset) and
    /// tell each affected client which of its keys went.
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

    /// Relay the vault's name list onto the settings frame so every client's
    /// "saved" placeholders stay fresh. One task per core (guarded, not global);
    /// subscribing once here covers every vault mutation surface.
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
        // No subscriber is the normal state of a core with no sidecars up.
        let _ = self.inner.changes.send(Arc::new(CoreSettingsChange {
            merged: merged.clone(),
            changed: changed.clone(),
        }));
    }
}

/// Split a changed-key set into the frame's `values` and `deleted`. A key that
/// is not in the schema, or is secret-typed, never crosses the wire.
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

/// The storage screen's factory reset: both stores. The vault is cleared by
/// the storage package alongside this call.
#[async_trait::async_trait]
impl crate::storage::deps::SettingsReset for Settings {
    async fn factory_reset(&self) -> Result<()> {
        self.reset().await?;
        self.reset_all_clients().await
    }
}

#[cfg(test)]
mod tests;
