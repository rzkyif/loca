//! Persistence for the download queue: every read and write of the `downloads`
//! table, and the row-to-entry mapping.
//!
//! Pure SQLite. No network, no broadcast, no in-flight state: the queue owns
//! those, and keeping them out of here is what makes the queue's concurrency
//! reviewable on its own.
//!
//! # The boot pass, and why it is not a tidy-up
//!
//! [`DownloadStore::normalize_persisted_rows`] runs once at construction and
//! settles the two ways the table can disagree with the disk after a crash: a
//! `Completed` row whose file is gone, and a `Downloading` row whose worker died
//! with the process. The first is deleted, the second is flipped back to
//! `Pending` so the queue's resume loop picks it up.
//!
//! That second one is load-bearing rather than cosmetic. `Downloading` is a
//! non-terminal status, the core status bar counts non-terminal rows, and no
//! worker exists for a row left behind by a dead process. Skipping the flip
//! leaves a row nothing will ever advance and a progress bar that never
//! finishes.
//!
//! # Two `relPath` readings, both the oracle's
//!
//! [`resolve_meta`] reads `spec.rel_path` with **truthiness**, so `Some("")` is
//! absent here and the row's `rel_path` comes from the parsed source
//! (`store.ts:200`). The queue reads the same field with `??`
//! (`manager.ts:254`), where `Some("")` is present and resolves the destination
//! root itself. For `rel_path: Some("")` the two disagree about what is being
//! downloaded. Unreachable today, ported exactly as written per D-043's converse
//! rule, and pinned by a test on each side rather than harmonised.

use std::path::Path;

use rusqlite::{OptionalExtension, Row};

use crate::contract::api::{DownloadEntry, DownloadEntryDestination, DownloadStatus};
use crate::db::Db;
use crate::downloads::sources::parse_source;
use crate::downloads::EnqueueSpec;
use crate::error::Result;

const ROW_COLUMNS: &str = "id, source, destination, rel_path, abs_path, filename, group_id, \
                           size_bytes, downloaded_bytes, status, error, added_at_ms";

/// Every read and write of the `downloads` table.
#[derive(Clone)]
pub struct DownloadStore {
    db: Db,
    hf_base_url: String,
}

impl DownloadStore {
    pub fn new(db: Db, hf_base_url: &str) -> Self {
        Self {
            db,
            hf_base_url: hf_base_url.to_string(),
        }
    }

    /// Insert a `Pending` row, or reset an existing one to `Pending`.
    ///
    /// The reset clears the error and zeroes `downloaded_bytes` but leaves
    /// `size_bytes` alone unless the spec carries a new hint, which is what
    /// `COALESCE(?, size_bytes)` says: a retry that learned nothing new must not
    /// erase a size the first attempt discovered.
    pub async fn upsert_pending(
        &self,
        id: &str,
        spec: &EnqueueSpec,
        abs_path: &Path,
    ) -> Result<()> {
        let existing = self.get_row(id).await?.is_some();
        let meta = resolve_meta(spec, &self.hf_base_url)?;
        let (id, size_hint, abs_path) = (
            id.to_string(),
            spec.size_hint.map(|n| n as i64),
            abs_path.to_string_lossy().into_owned(),
        );
        let (source, destination, group_id) = (
            spec.source.clone(),
            spec.destination.to_string(),
            spec.group_id.clone(),
        );
        let now = now_ms();
        self.db
            .call(move |conn| {
                if existing {
                    conn.execute(
                        "UPDATE downloads
                            SET status = 'Pending',
                                error = NULL,
                                downloaded_bytes = 0,
                                size_bytes = COALESCE(?1, size_bytes)
                          WHERE id = ?2",
                        rusqlite::params![size_hint, id],
                    )?;
                    return Ok(());
                }
                conn.execute(
                    "INSERT INTO downloads
                       (id, source, destination, rel_path, abs_path, filename, group_id,
                        size_bytes, downloaded_bytes, status, error, added_at_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0, 'Pending', NULL, ?9)",
                    rusqlite::params![
                        id,
                        source,
                        destination,
                        meta.rel_path,
                        abs_path,
                        meta.filename,
                        group_id,
                        size_hint,
                        now,
                    ],
                )?;
                Ok(())
            })
            .await
    }

    /// Record a file that is already on disk as `Completed`.
    ///
    /// On an existing row `downloaded_bytes` is filled from `size_bytes`
    /// (`COALESCE(size_bytes, downloaded_bytes)`), because a row completing
    /// through the on-disk fast path never streamed a byte and would otherwise
    /// report a full-size file as 0 of N downloaded.
    pub async fn upsert_completed(
        &self,
        id: &str,
        spec: &EnqueueSpec,
        abs_path: &Path,
    ) -> Result<()> {
        let existing = self.get_row(id).await?.is_some();
        let abs_path_owned = abs_path.to_string_lossy().into_owned();
        if existing {
            let id = id.to_string();
            return self
                .db
                .call(move |conn| {
                    conn.execute(
                        "UPDATE downloads
                            SET status = 'Completed',
                                error = NULL,
                                abs_path = ?1,
                                downloaded_bytes = COALESCE(size_bytes, downloaded_bytes)
                          WHERE id = ?2",
                        rusqlite::params![abs_path_owned, id],
                    )?;
                    Ok(())
                })
                .await;
        }
        // Best-effort: a file that cannot be stat'd is recorded with no size
        // rather than refused, because the row is the record that it exists.
        let size_on_disk = tokio::fs::metadata(abs_path)
            .await
            .ok()
            .map(|m| m.len() as i64);
        let meta = resolve_meta(spec, &self.hf_base_url)?;
        let (id, source, destination, group_id) = (
            id.to_string(),
            spec.source.clone(),
            spec.destination.to_string(),
            spec.group_id.clone(),
        );
        let now = now_ms();
        self.db
            .call(move |conn| {
                conn.execute(
                    "INSERT INTO downloads
                       (id, source, destination, rel_path, abs_path, filename, group_id,
                        size_bytes, downloaded_bytes, status, error, added_at_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'Completed', NULL, ?10)",
                    rusqlite::params![
                        id,
                        source,
                        destination,
                        meta.rel_path,
                        abs_path_owned,
                        meta.filename,
                        group_id,
                        size_on_disk,
                        size_on_disk.unwrap_or(0),
                        now,
                    ],
                )?;
                Ok(())
            })
            .await
    }

    /// Set a row's status, and optionally its byte count in the same statement.
    ///
    /// The two-statement split is the oracle's: passing no byte count must leave
    /// `downloaded_bytes` untouched rather than zero it, which is what keeps an
    /// errored row's progress visible for the resume that follows.
    pub async fn set_status(
        &self,
        id: &str,
        status: DownloadStatus,
        error: Option<String>,
        downloaded_bytes: Option<u64>,
    ) -> Result<()> {
        let (id, status) = (id.to_string(), status.to_string());
        self.db
            .call(move |conn| {
                match downloaded_bytes {
                    Some(bytes) => conn.execute(
                        "UPDATE downloads SET status = ?1, error = ?2, downloaded_bytes = ?3
                          WHERE id = ?4",
                        rusqlite::params![status, error, bytes as i64, id],
                    )?,
                    None => conn.execute(
                        "UPDATE downloads SET status = ?1, error = ?2 WHERE id = ?3",
                        rusqlite::params![status, error, id],
                    )?,
                };
                Ok(())
            })
            .await
    }

    pub async fn update_progress(&self, id: &str, downloaded_bytes: u64) -> Result<()> {
        let id = id.to_string();
        self.db
            .call(move |conn| {
                conn.execute(
                    "UPDATE downloads SET downloaded_bytes = ?1 WHERE id = ?2",
                    rusqlite::params![downloaded_bytes as i64, id],
                )?;
                Ok(())
            })
            .await
    }

    pub async fn update_size(&self, id: &str, size_bytes: u64) -> Result<()> {
        let id = id.to_string();
        self.db
            .call(move |conn| {
                conn.execute(
                    "UPDATE downloads SET size_bytes = ?1 WHERE id = ?2",
                    rusqlite::params![size_bytes as i64, id],
                )?;
                Ok(())
            })
            .await
    }

    pub async fn get_row(&self, id: &str) -> Result<Option<DownloadEntry>> {
        let id = id.to_string();
        self.db
            .call(move |conn| {
                let sql = format!("SELECT {ROW_COLUMNS} FROM downloads WHERE id = ?1");
                conn.query_row(&sql, rusqlite::params![id], row_to_entry)
                    .optional()
                    .map_err(Into::into)
            })
            .await
    }

    /// Every row, newest first. The order is the wire order: the client renders
    /// the list as it arrives.
    pub async fn snapshot(&self) -> Result<Vec<DownloadEntry>> {
        self.db
            .call(move |conn| {
                let sql = format!("SELECT {ROW_COLUMNS} FROM downloads ORDER BY added_at_ms DESC");
                let mut stmt = conn.prepare(&sql)?;
                let rows = stmt
                    .query_map([], row_to_entry)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            })
            .await
    }

    pub async fn delete(&self, id: &str) -> Result<()> {
        let id = id.to_string();
        self.db
            .call(move |conn| {
                conn.execute("DELETE FROM downloads WHERE id = ?1", rusqlite::params![id])?;
                Ok(())
            })
            .await
    }

    /// Drop `Completed` rows whose file is no longer on disk, so a stale "done"
    /// entry self-clears. Returns whether anything was dropped.
    ///
    /// **Only a definite NotFound deletes.** Any other stat failure (a
    /// permission change, a path whose parent stopped being a directory, an
    /// unmounted volume) leaves the row alone, because those say "cannot answer"
    /// rather than "gone", and deleting on them turns a temporarily unreadable
    /// file into a download the user has to notice and redo.
    pub async fn reconcile_completed(&self) -> Result<bool> {
        let completed = self
            .db
            .call(move |conn| {
                let mut stmt =
                    conn.prepare("SELECT id, abs_path FROM downloads WHERE status = 'Completed'")?;
                let rows = stmt
                    .query_map([], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            })
            .await?;

        let mut vanished = Vec::new();
        for (id, abs_path) in completed {
            if let Err(err) = tokio::fs::metadata(&abs_path).await {
                if err.kind() == std::io::ErrorKind::NotFound {
                    vanished.push(id);
                }
            }
        }
        if vanished.is_empty() {
            return Ok(false);
        }
        self.db
            .call(move |conn| {
                for id in &vanished {
                    conn.execute("DELETE FROM downloads WHERE id = ?1", rusqlite::params![id])?;
                }
                Ok(true)
            })
            .await
    }

    /// The boot pass. See the module docs: this is recovery, not tidy-up.
    ///
    /// Unlike [`Self::reconcile_completed`], a vanished `Completed` row is
    /// dropped on ANY stat error here, which is the oracle's bare `catch`
    /// (`store.ts:184`). The two are deliberately different and the difference
    /// is not obviously right; it is preserved because narrowing it at boot
    /// would be a behaviour change dressed as a consistency fix.
    pub async fn normalize_persisted_rows(&self) -> Result<()> {
        let all = self
            .db
            .call(move |conn| {
                let mut stmt = conn.prepare("SELECT id, abs_path, status FROM downloads")?;
                let rows = stmt
                    .query_map([], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            })
            .await?;

        let mut drop_ids = Vec::new();
        let mut reset_ids = Vec::new();
        for (id, abs_path, status) in all {
            match status.as_str() {
                "Completed" if tokio::fs::metadata(&abs_path).await.is_err() => drop_ids.push(id),
                "Downloading" => reset_ids.push(id),
                _ => {}
            }
        }
        if drop_ids.is_empty() && reset_ids.is_empty() {
            return Ok(());
        }
        self.db
            .call(move |conn| {
                for id in &drop_ids {
                    conn.execute("DELETE FROM downloads WHERE id = ?1", rusqlite::params![id])?;
                }
                for id in &reset_ids {
                    conn.execute(
                        "UPDATE downloads SET status = 'Pending', downloaded_bytes = 0
                          WHERE id = ?1",
                        rusqlite::params![id],
                    )?;
                }
                Ok(())
            })
            .await
    }
}

struct Meta {
    rel_path: String,
    filename: String,
}

/// The row's `rel_path` and `filename`.
///
/// `spec.rel_path` is read with truthiness, so `Some("")` falls through to the
/// parsed source. See the module docs: the queue reads the same field the other
/// way, and both readings are the oracle's.
fn resolve_meta(spec: &EnqueueSpec, hf_base_url: &str) -> Result<Meta> {
    if let Some(rel_path) = spec.rel_path.as_ref().filter(|p| !p.is_empty()) {
        let filename = spec
            .filename
            .clone()
            .unwrap_or_else(|| rel_path.rsplit('/').next().unwrap_or(rel_path).to_string());
        return Ok(Meta {
            rel_path: rel_path.clone(),
            filename,
        });
    }
    let parsed = parse_source(&spec.source, hf_base_url)?;
    Ok(Meta {
        rel_path: parsed.rel_path,
        filename: spec.filename.clone().unwrap_or(parsed.filename),
    })
}

fn row_to_entry(row: &Row<'_>) -> rusqlite::Result<DownloadEntry> {
    let destination: String = row.get("destination")?;
    let status: String = row.get("status")?;
    Ok(DownloadEntry {
        id: row.get("id")?,
        source: row.get("source")?,
        // A row whose destination or status is not on the contract cannot be
        // rendered, and the table is ours alone, so an unknown value is a bug
        // here rather than input to validate. Falling back keeps the snapshot
        // servable instead of failing every list because one row is odd.
        destination: destination
            .parse()
            .unwrap_or(DownloadEntryDestination::Models),
        rel_path: row.get("rel_path")?,
        abs_path: row.get("abs_path")?,
        filename: row.get("filename")?,
        group_id: row.get("group_id")?,
        size_bytes: row.get("size_bytes")?,
        downloaded_bytes: row.get::<_, Option<i64>>("downloaded_bytes")?.unwrap_or(0),
        status: status.parse().unwrap_or(DownloadStatus::Pending),
        error: row.get("error")?,
        added_at_ms: row.get("added_at_ms")?,
    })
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::db::migrate::migrate;

    const HF: &str = "https://huggingface.co";

    fn spec(source: &str) -> EnqueueSpec {
        EnqueueSpec {
            source: source.into(),
            destination: DownloadEntryDestination::Models,
            group_id: "g".into(),
            size_hint: None,
            sha256: None,
            url: None,
            rel_path: None,
            filename: None,
        }
    }

    async fn store() -> (DownloadStore, tempfile::TempDir) {
        let db = Db::open_in_memory().unwrap();
        db.call_blocking(|conn| migrate(conn)).unwrap();
        (DownloadStore::new(db, HF), tempfile::tempdir().unwrap())
    }

    /// Force a row's `added_at_ms`, because two rows inserted in the same
    /// millisecond tie and a tie cannot demonstrate an order.
    async fn set_added_at(store: &DownloadStore, id: &str, ms: i64) {
        let id = id.to_string();
        store
            .db
            .call(move |conn| {
                conn.execute(
                    "UPDATE downloads SET added_at_ms = ?1 WHERE id = ?2",
                    rusqlite::params![ms, id],
                )?;
                Ok(())
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_pending_row_round_trips_with_its_parsed_path_and_filename() {
        let (store, dir) = store().await;
        let abs = dir.path().join("u/r/model.gguf");
        store
            .upsert_pending("models:x", &spec("@u/r/main/model.gguf"), &abs)
            .await
            .unwrap();

        let row = store.get_row("models:x").await.unwrap().unwrap();
        assert_eq!(row.status, DownloadStatus::Pending);
        assert_eq!(row.rel_path, "u/r/model.gguf");
        assert_eq!(row.filename, "model.gguf");
        assert_eq!(row.downloaded_bytes, 0);
        assert_eq!(row.size_bytes, None);
    }

    #[tokio::test]
    async fn re_pending_an_existing_row_clears_the_error_and_keeps_a_known_size() {
        let (store, dir) = store().await;
        let abs = dir.path().join("u/r/model.gguf");
        let mut s = spec("@u/r/main/model.gguf");
        s.size_hint = Some(4096);
        store.upsert_pending("id", &s, &abs).await.unwrap();
        store
            .set_status("id", DownloadStatus::Error, Some("boom".into()), Some(900))
            .await
            .unwrap();

        // A retry that learned nothing new: no hint this time.
        store
            .upsert_pending("id", &spec("@u/r/main/model.gguf"), &abs)
            .await
            .unwrap();

        let row = store.get_row("id").await.unwrap().unwrap();
        assert_eq!(row.status, DownloadStatus::Pending);
        assert_eq!(row.error, None);
        assert_eq!(row.downloaded_bytes, 0, "the row zeroes; the .tmp does not");
        assert_eq!(
            row.size_bytes,
            Some(4096),
            "COALESCE keeps the size the first attempt discovered"
        );
    }

    #[tokio::test]
    async fn completing_an_existing_row_fills_downloaded_from_size() {
        // The fast path: a file found on disk never streams a byte, so without
        // COALESCE(size_bytes, downloaded_bytes) a finished row reads 0 of N.
        let (store, dir) = store().await;
        let abs = dir.path().join("u/r/model.gguf");
        let mut s = spec("@u/r/main/model.gguf");
        s.size_hint = Some(2048);
        store.upsert_pending("id", &s, &abs).await.unwrap();

        store.upsert_completed("id", &s, &abs).await.unwrap();

        let row = store.get_row("id").await.unwrap().unwrap();
        assert_eq!(row.status, DownloadStatus::Completed);
        assert_eq!(row.downloaded_bytes, 2048);
        assert_eq!(row.error, None);
    }

    #[tokio::test]
    async fn completing_a_new_row_takes_its_size_from_the_file_on_disk() {
        let (store, dir) = store().await;
        let abs = dir.path().join("u/r/model.gguf");
        std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
        std::fs::write(&abs, b"0123456789").unwrap();

        store
            .upsert_completed("id", &spec("@u/r/main/model.gguf"), &abs)
            .await
            .unwrap();

        let row = store.get_row("id").await.unwrap().unwrap();
        assert_eq!(row.size_bytes, Some(10));
        assert_eq!(row.downloaded_bytes, 10);
    }

    #[tokio::test]
    async fn a_status_change_without_a_byte_count_leaves_the_count_alone() {
        // What keeps an errored row's progress visible for the resume after it.
        let (store, dir) = store().await;
        let abs = dir.path().join("u/r/model.gguf");
        store
            .upsert_pending("id", &spec("@u/r/main/model.gguf"), &abs)
            .await
            .unwrap();
        store.update_progress("id", 512).await.unwrap();

        store
            .set_status("id", DownloadStatus::Error, Some("network".into()), None)
            .await
            .unwrap();

        let row = store.get_row("id").await.unwrap().unwrap();
        assert_eq!(row.downloaded_bytes, 512);
        assert_eq!(row.error.as_deref(), Some("network"));
    }

    #[tokio::test]
    async fn the_snapshot_is_newest_first() {
        let (store, dir) = store().await;
        for (id, ms) in [("old", 1_000), ("newest", 3_000), ("middle", 2_000)] {
            let abs = dir.path().join(format!("u/r/{id}.gguf"));
            store
                .upsert_pending(id, &spec(&format!("@u/r/main/{id}.gguf")), &abs)
                .await
                .unwrap();
            set_added_at(&store, id, ms).await;
        }

        let ids: Vec<String> = store
            .snapshot()
            .await
            .unwrap()
            .into_iter()
            .map(|e| e.id)
            .collect();
        assert_eq!(ids, vec!["newest", "middle", "old"]);
    }

    #[tokio::test]
    async fn a_completed_row_whose_file_vanished_self_clears() {
        let (store, dir) = store().await;
        let abs = dir.path().join("u/r/model.gguf");
        std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
        std::fs::write(&abs, b"x").unwrap();
        store
            .upsert_completed("id", &spec("@u/r/main/model.gguf"), &abs)
            .await
            .unwrap();
        assert!(!store.reconcile_completed().await.unwrap());

        std::fs::remove_file(&abs).unwrap();

        assert!(store.reconcile_completed().await.unwrap());
        assert!(store.get_row("id").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_stat_error_that_is_not_not_found_leaves_the_row() {
        // ENOTDIR rather than a permission error: stat a path whose PARENT is a
        // regular file. A permission fixture is not portable across the three CI
        // platforms, and a test that only runs on one is D-015's shape.
        let (store, dir) = store().await;
        let blocker = dir.path().join("u/r/model.gguf");
        std::fs::create_dir_all(blocker.parent().unwrap()).unwrap();
        std::fs::write(&blocker, b"x").unwrap();
        let abs = blocker.join("child.gguf");

        store
            .upsert_completed("id", &spec("@u/r/main/model.gguf"), &abs)
            .await
            .unwrap();

        let err = std::fs::metadata(&abs).unwrap_err();
        assert_ne!(
            err.kind(),
            std::io::ErrorKind::NotFound,
            "the fixture must not stage a NotFound, or it proves nothing"
        );
        assert!(!store.reconcile_completed().await.unwrap());
        assert!(
            store.get_row("id").await.unwrap().is_some(),
            "a cannot-answer stat deleted the row"
        );
    }

    #[tokio::test]
    async fn a_persisted_downloading_row_is_reset_at_boot() {
        // Downloading is non-terminal and no worker survived the process, so a
        // row left this way is one nothing will ever advance.
        let (store, dir) = store().await;
        let abs = dir.path().join("u/r/model.gguf");
        store
            .upsert_pending("id", &spec("@u/r/main/model.gguf"), &abs)
            .await
            .unwrap();
        store
            .set_status("id", DownloadStatus::Downloading, None, Some(700))
            .await
            .unwrap();

        store.normalize_persisted_rows().await.unwrap();

        let row = store.get_row("id").await.unwrap().unwrap();
        assert_eq!(row.status, DownloadStatus::Pending);
        assert_eq!(row.downloaded_bytes, 0);
    }

    #[tokio::test]
    async fn the_boot_pass_drops_a_completed_row_whose_file_is_gone() {
        let (store, dir) = store().await;
        let abs = dir.path().join("u/r/model.gguf");
        store
            .upsert_completed("gone", &spec("@u/r/main/model.gguf"), &abs)
            .await
            .unwrap();
        let kept = dir.path().join("u/r/kept.gguf");
        std::fs::create_dir_all(kept.parent().unwrap()).unwrap();
        std::fs::write(&kept, b"x").unwrap();
        store
            .upsert_completed("kept", &spec("@u/r/main/kept.gguf"), &kept)
            .await
            .unwrap();

        store.normalize_persisted_rows().await.unwrap();

        assert!(store.get_row("gone").await.unwrap().is_none());
        assert!(store.get_row("kept").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn a_pending_row_is_left_alone_by_the_boot_pass() {
        let (store, dir) = store().await;
        let abs = dir.path().join("u/r/model.gguf");
        store
            .upsert_pending("id", &spec("@u/r/main/model.gguf"), &abs)
            .await
            .unwrap();

        store.normalize_persisted_rows().await.unwrap();

        assert_eq!(
            store.get_row("id").await.unwrap().unwrap().status,
            DownloadStatus::Pending
        );
    }

    #[tokio::test]
    async fn an_empty_rel_path_is_absent_here_and_the_parsed_source_wins() {
        // The store's half of the inconsistency in the module docs: truthiness,
        // so "" falls through to parseSource. The queue's half reads the same
        // field with `??` and resolves the destination root instead.
        let (store, dir) = store().await;
        let mut s = spec("@u/r/main/model.gguf");
        s.rel_path = Some(String::new());
        store
            .upsert_pending("id", &s, &dir.path().join("anywhere"))
            .await
            .unwrap();

        let row = store.get_row("id").await.unwrap().unwrap();
        assert_eq!(row.rel_path, "u/r/model.gguf");
    }

    #[tokio::test]
    async fn a_supplied_rel_path_and_filename_are_used_verbatim() {
        let (store, dir) = store().await;
        let mut s = spec("@u/r/main/model.gguf");
        s.rel_path = Some("staging/deno.zip".into());
        store
            .upsert_pending("id", &s, &dir.path().join("staging/deno.zip"))
            .await
            .unwrap();

        let row = store.get_row("id").await.unwrap().unwrap();
        assert_eq!(row.rel_path, "staging/deno.zip");
        assert_eq!(row.filename, "deno.zip", "the basename of the rel path");
    }

    #[tokio::test]
    async fn deleting_a_row_removes_it_from_the_snapshot() {
        let (store, dir) = store().await;
        let abs = dir.path().join("u/r/model.gguf");
        store
            .upsert_pending("id", &spec("@u/r/main/model.gguf"), &abs)
            .await
            .unwrap();

        store.delete("id").await.unwrap();

        assert!(store.get_row("id").await.unwrap().is_none());
        assert!(store.snapshot().await.unwrap().is_empty());
    }
}
