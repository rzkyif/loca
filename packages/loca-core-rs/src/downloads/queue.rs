//! The queue: one transfer at a time, the in-flight set, the waiter fanout, and
//! the change broadcast. Persistence is [`DownloadStore`]'s, bytes are
//! [`Transfer`]'s.
//!
//! Invariant: every non-terminal row has a live worker. The worker no-ops
//! without an in-flight entry, so each of the three row-creating paths
//! (`enqueue`, `retry`, `resume_pending`) arms the entry BEFORE spawning.
//!
//! A subscriber that sees `RecvError::Lagged` must call [`DownloadQueue::snapshot`]
//! and diff: progress frames are droppable, the terminal frame is not.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{broadcast, oneshot, Semaphore};

use crate::contain::is_within;
use crate::contract::api::{DownloadEntry, DownloadEntryDestination, DownloadStatus};
use crate::downloads::cancel::CancellationToken;
use crate::downloads::net;
use crate::downloads::sources::parse_source;
use crate::downloads::store::DownloadStore;
use crate::downloads::transfer::{tmp_path_for, Transfer, TransferOpts};
use crate::downloads::EnqueueSpec;
use crate::error::{validation, AppError, ErrorCode, Result};
use crate::paths::Paths;

/// Breathing room between successive downloads so HuggingFace does not
/// burst-throttle. Injectable so a test can prove the wait on a real clock.
pub const INTER_DOWNLOAD_DELAY: Duration = Duration::from_secs(1);

/// How many snapshots the change channel buffers before a slow receiver lags.
const BROADCAST_CAPACITY: usize = 64;

/// One download that is queued or running, and everyone waiting on it.
struct InFlight {
    cancel: CancellationToken,
    /// Every caller that asked for this file; two callers share one transfer.
    waiters: Vec<oneshot::Sender<std::result::Result<PathBuf, AppError>>>,
}

impl InFlight {
    fn new(waiters: Vec<oneshot::Sender<std::result::Result<PathBuf, AppError>>>) -> Self {
        Self {
            cancel: CancellationToken::new(),
            waiters,
        }
    }
}

/// The download queue.
#[derive(Clone)]
pub struct DownloadQueue {
    inner: Arc<Inner>,
}

struct Inner {
    store: DownloadStore,
    transfer: Arc<Transfer>,
    paths: Paths,
    hf_base_url: String,
    in_flight: Mutex<HashMap<String, InFlight>>,
    /// Serialises the three operations that decide who owns an id (`enqueue`'s
    /// arming half, `retry`, `remove`): each awaits SQLite between its guard
    /// and its mutation. Never held across a transfer.
    ops: tokio::sync::Mutex<()>,
    #[cfg(test)]
    panic_next: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    widen_race: std::sync::atomic::AtomicBool,
    /// One permit: at most one transfer at a time.
    permits: Semaphore,
    changes: broadcast::Sender<Arc<Vec<DownloadEntry>>>,
    inter_download_delay: Mutex<Duration>,
    /// The last snapshot, so a SYNC caller (`models_snapshot`) can read the
    /// queue without blocking on SQLite. Refreshed on boot, every broadcast
    /// and every reconcile; never lazily on read.
    cached: Mutex<Arc<Vec<DownloadEntry>>>,
}

impl DownloadQueue {
    pub fn new(store: DownloadStore, transfer: Arc<Transfer>, paths: Paths, hf: &str) -> Self {
        let (changes, _) = broadcast::channel(BROADCAST_CAPACITY);
        Self {
            inner: Arc::new(Inner {
                store,
                transfer,
                paths,
                hf_base_url: hf.to_string(),
                in_flight: Mutex::new(HashMap::new()),
                ops: tokio::sync::Mutex::new(()),
                #[cfg(test)]
                panic_next: std::sync::atomic::AtomicBool::new(false),
                #[cfg(test)]
                widen_race: std::sync::atomic::AtomicBool::new(false),
                permits: Semaphore::new(1),
                changes,
                inter_download_delay: Mutex::new(INTER_DOWNLOAD_DELAY),
                cached: Mutex::new(Arc::new(Vec::new())),
            }),
        }
    }

    /// Shorten the inter-download throttle. See [`INTER_DOWNLOAD_DELAY`].
    pub fn with_delay(self, delay: Duration) -> Self {
        *lock(&self.inner.inter_download_delay) = delay;
        self
    }

    /// Boot: settle the table against the disk, then put a worker behind every
    /// row that still needs one. Both halves, in this order: `normalize` alone
    /// flips `Downloading` rows to `Pending` with nothing to advance them.
    /// `binaries` rows are left for the installer's own reconcile.
    pub async fn start(&self) -> Result<()> {
        self.normalize().await?;
        self.resume_pending().await
    }

    /// Settle the table against the disk and prime the cached snapshot.
    pub async fn normalize(&self) -> Result<()> {
        self.inner.store.normalize_persisted_rows().await?;
        self.refresh_cache().await;
        Ok(())
    }

    #[cfg(test)]
    pub fn store_for_test(&self) -> &DownloadStore {
        &self.inner.store
    }

    /// Make the next worker panic, so the panic guard can be watched working.
    #[cfg(test)]
    pub fn panic_next_worker(&self) {
        self.inner
            .panic_next
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// Hold `remove` open between its in-flight guard and its delete, so a
    /// test can hit the one-SQLite-round-trip window deterministically.
    #[cfg(test)]
    pub fn widen_ops_race(&self) {
        self.inner
            .widen_race
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    #[cfg(test)]
    async fn widen_race_point(&self) {
        if self
            .inner
            .widen_race
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            tokio::time::sleep(Duration::from_millis(80)).await;
        }
    }

    /// The last known snapshot, without touching the database.
    pub fn cached_snapshot(&self) -> Arc<Vec<DownloadEntry>> {
        Arc::clone(&lock(&self.inner.cached))
    }

    async fn refresh_cache(&self) -> Arc<Vec<DownloadEntry>> {
        match self.inner.store.snapshot().await {
            Ok(snapshot) => {
                let snapshot = Arc::new(snapshot);
                *lock(&self.inner.cached) = Arc::clone(&snapshot);
                snapshot
            }
            Err(err) => {
                tracing::warn!(target: "loca::downloads", "snapshot refresh failed: {err}");
                self.cached_snapshot()
            }
        }
    }

    /// Subscribe to queue changes. On `Lagged`, pull a [`Self::snapshot`].
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<Vec<DownloadEntry>>> {
        self.inner.changes.subscribe()
    }

    pub async fn snapshot(&self) -> Result<Vec<DownloadEntry>> {
        self.inner.store.snapshot().await
    }

    /// Rows filtered to one destination.
    pub async fn snapshot_for(
        &self,
        destination: DownloadEntryDestination,
    ) -> Result<Vec<DownloadEntry>> {
        Ok(self
            .snapshot()
            .await?
            .into_iter()
            .filter(|row| row.destination == destination)
            .collect())
    }

    /// The queue-row id `enqueue` would use for this spec, without enqueuing.
    /// Cancel, retry and remove match on it; any other id silently no-ops.
    pub fn id_for(&self, spec: &EnqueueSpec) -> Result<String> {
        let abs = self.resolve_abs_path(spec)?;
        Ok(download_id(spec.destination, &abs))
    }

    /// Start or join a download. Resolves when the file is on disk.
    pub async fn enqueue(&self, spec: &EnqueueSpec) -> Result<PathBuf> {
        let abs_path = self.resolve_abs_path(spec)?;
        let id = download_id(spec.destination, &abs_path);
        // Held for the arming half only, never across `rx.await`.
        let ops = self.inner.ops.lock().await;

        if already_on_disk(&abs_path).await {
            self.inner
                .store
                .upsert_completed(&id, spec, &abs_path)
                .await?;
            self.broadcast().await;
            return Ok(abs_path);
        }

        let (tx, rx) = oneshot::channel();
        // Only the caller that created the entry starts a worker; the other
        // becomes a waiter on it.
        let is_owner = {
            let mut in_flight = lock(&self.inner.in_flight);
            match in_flight.get_mut(&id) {
                Some(existing) => {
                    existing.waiters.push(tx);
                    false
                }
                None => {
                    in_flight.insert(id.clone(), InFlight::new(vec![tx]));
                    true
                }
            }
        };
        if is_owner {
            self.inner
                .store
                .upsert_pending(&id, spec, &abs_path)
                .await?;
            self.broadcast().await;
            self.spawn(id.clone(), spec.clone(), abs_path.clone());
        }
        drop(ops);

        rx.await.unwrap_or_else(|_| {
            // Sender dropped without settling: a distinguishable error beats a hang.
            Err(AppError::new(
                ErrorCode::InternalError,
                format!("download {id} ended without a result"),
            ))
        })
    }

    /// Stop a download. In flight: trip the token and let the worker settle
    /// the row. Not in flight and `Pending`: flip straight to `Cancelled`.
    /// Otherwise nothing. Never deletes the row; [`Self::remove`] does.
    pub async fn cancel(&self, id: &str) -> Result<()> {
        let token = lock(&self.inner.in_flight)
            .get(id)
            .map(|entry| entry.cancel.clone());
        if let Some(token) = token {
            token.cancel();
            return Ok(());
        }
        let row = self.inner.store.get_row(id).await?;
        if row.is_some_and(|row| row.status == DownloadStatus::Pending) {
            self.inner
                .store
                .set_status(
                    id,
                    DownloadStatus::Cancelled,
                    Some("cancelled".into()),
                    None,
                )
                .await?;
            self.broadcast().await;
        }
        Ok(())
    }

    /// Re-queue a failed download. A no-op when the row is missing, in flight,
    /// or non-terminal. Zeroes the byte count on the ROW only: the `.tmp` stays,
    /// which is what makes the retry resume rather than restart.
    pub async fn retry(&self, id: &str) -> Result<()> {
        let _ops = self.inner.ops.lock().await;
        let Some(row) = self.inner.store.get_row(id).await? else {
            return Ok(());
        };
        if matches!(
            row.status,
            DownloadStatus::Downloading | DownloadStatus::Pending
        ) {
            return Ok(());
        }
        // Guard and arm in ONE critical section so nothing can interleave with
        // `enqueue`'s arm.
        let armed = {
            let mut in_flight = lock(&self.inner.in_flight);
            if in_flight.contains_key(id) {
                false
            } else {
                in_flight.insert(id.to_string(), InFlight::new(Vec::new()));
                true
            }
        };
        if !armed {
            return Ok(());
        }
        if let Err(err) = self
            .inner
            .store
            .set_status(id, DownloadStatus::Pending, None, Some(0))
            .await
        {
            // Nothing will run, so the id must not stay claimed.
            lock(&self.inner.in_flight).remove(id);
            return Err(err);
        }
        self.broadcast().await;
        self.spawn(
            id.to_string(),
            spec_from_row(&row),
            PathBuf::from(&row.abs_path),
        );
        Ok(())
    }

    /// Drop a row and its partial. A no-op while in flight. This is where an
    /// `Error` row's `.tmp` stops being wanted.
    pub async fn remove(&self, id: &str) -> Result<()> {
        let _ops = self.inner.ops.lock().await;
        let Some(row) = self.inner.store.get_row(id).await? else {
            return Ok(());
        };
        if lock(&self.inner.in_flight).contains_key(id) {
            return Ok(());
        }
        #[cfg(test)]
        self.widen_race_point().await;
        self.inner.store.delete(id).await?;
        let _ = tokio::fs::remove_file(tmp_path_for(Path::new(&row.abs_path))).await;
        self.broadcast().await;
        Ok(())
    }

    /// Drop `Completed` rows whose file vanished, broadcasting if any did.
    pub async fn reconcile_completed(&self) -> Result<()> {
        if self.inner.store.reconcile_completed().await? {
            self.broadcast().await;
        } else {
            // The sync seam reconciles and then reads immediately, so the
            // cache is refreshed even when nothing was dropped.
            self.refresh_cache().await;
        }
        Ok(())
    }

    /// Re-spawn rows left `Pending` when the process last exited.
    ///
    /// `binaries` rows are skipped: a generic resume re-runs only the byte
    /// transfer, without the extract-and-record step or the sha256 re-check,
    /// so it would land an unverified archive in staging. The installer's boot
    /// reconcile finishes those.
    pub async fn resume_pending(&self) -> Result<()> {
        let rows = self.snapshot().await?;
        for row in rows {
            if row.status != DownloadStatus::Pending
                || row.destination == DownloadEntryDestination::Binaries
            {
                continue;
            }
            {
                let mut in_flight = lock(&self.inner.in_flight);
                if in_flight.contains_key(&row.id) {
                    continue;
                }
                in_flight.insert(row.id.clone(), InFlight::new(Vec::new()));
            }
            self.spawn(
                row.id.clone(),
                spec_from_row(&row),
                PathBuf::from(&row.abs_path),
            );
        }
        Ok(())
    }

    /// Start a worker for `id` and release its entry whatever happens to it.
    /// A panic inside [`Self::run`] would otherwise leak the waiter's sender
    /// inside the surviving entry and wedge the id for cancel, retry and remove.
    fn spawn(&self, id: String, spec: EnqueueSpec, abs_path: PathBuf) {
        let queue = self.clone();
        tokio::spawn(async move {
            let worker = tokio::spawn({
                let (queue, id, abs_path) = (queue.clone(), id.clone(), abs_path.clone());
                async move { queue.run(&id, &spec, &abs_path).await }
            });
            match worker.await {
                Ok(Ok(())) => {}
                Ok(Err(err)) => {
                    tracing::warn!(target: "loca::downloads", "download {id}: {err}");
                }
                Err(join) => queue.settle_after_worker_died(&id, &join.to_string()).await,
            }
        });
    }

    /// Same three steps as `run`'s failing path, in the same order: terminal
    /// row, waiters, entry.
    async fn settle_after_worker_died(&self, id: &str, reason: &str) {
        tracing::error!(target: "loca::downloads", "download worker for {id} died: {reason}");
        let message = format!("download worker stopped unexpectedly: {reason}");
        let _ = self
            .inner
            .store
            .set_status(id, DownloadStatus::Error, Some(message.clone()), None)
            .await;
        self.broadcast().await;
        self.fanout(id, Err(message));
        lock(&self.inner.in_flight).remove(id);
    }

    async fn run(&self, id: &str, spec: &EnqueueSpec, abs_path: &Path) -> Result<()> {
        #[cfg(test)]
        if self
            .inner
            .panic_next
            .swap(false, std::sync::atomic::Ordering::Relaxed)
        {
            injected_panic();
        }
        let _permit = self
            .inner
            .permits
            .acquire()
            .await
            .map_err(|_| AppError::new(ErrorCode::InternalError, "download queue closed"))?;

        let cancel = {
            let in_flight = lock(&self.inner.in_flight);
            match in_flight.get(id) {
                None => return Ok(()),
                Some(entry) => entry.cancel.clone(),
            }
        };

        if cancel.is_cancelled() {
            // Cancelled while queued. Dropping the entry is load-bearing:
            // without it the id is wedged forever.
            self.inner
                .store
                .set_status(id, DownloadStatus::Cancelled, None, None)
                .await?;
            self.fanout(id, Err(net::CANCELLED_MESSAGE.to_string()));
            lock(&self.inner.in_flight).remove(id);
            self.broadcast().await;
            return Ok(());
        }

        self.inner
            .store
            .set_status(id, DownloadStatus::Downloading, None, Some(0))
            .await?;
        self.broadcast().await;

        let result = self.transfer_bytes(id, spec, abs_path, &cancel).await;

        match &result {
            Ok(final_bytes) => {
                self.inner.store.update_progress(id, *final_bytes).await?;
                // No byte count here: passing one would re-zero a finished row.
                self.inner
                    .store
                    .set_status(id, DownloadStatus::Completed, None, None)
                    .await?;
            }
            Err(err) if cancel.is_cancelled() => {
                // Classified on the token, not the message, so a cancel racing
                // a genuine failure is still a cancel. The partial goes with it.
                self.inner
                    .store
                    .set_status(
                        id,
                        DownloadStatus::Cancelled,
                        Some(err.message.clone()),
                        None,
                    )
                    .await?;
                let _ = tokio::fs::remove_file(tmp_path_for(abs_path)).await;
            }
            Err(err) => {
                // A transient failure KEEPS the partial so a retry resumes by
                // Range; the terminal-bad outcomes already deleted it inside
                // the transfer.
                self.inner
                    .store
                    .set_status(id, DownloadStatus::Error, Some(err.message.clone()), None)
                    .await?;
            }
        }
        self.broadcast().await;
        self.fanout(
            id,
            result
                .map(|_| abs_path.to_path_buf())
                .map_err(|err| err.message),
        );
        lock(&self.inner.in_flight).remove(id);

        // The throttle is inside the critical section: the permit is still
        // held, so the next transfer cannot start during it.
        let delay = *lock(&self.inner.inter_download_delay);
        tokio::time::sleep(delay).await;
        drop(_permit);
        Ok(())
    }

    async fn transfer_bytes(
        &self,
        id: &str,
        spec: &EnqueueSpec,
        abs_path: &Path,
        cancel: &CancellationToken,
    ) -> Result<u64> {
        let (queue, progress_id) = (self.clone(), id.to_string());
        let on_progress = move |downloaded: u64| {
            let queue = queue.clone();
            let id = progress_id.clone();
            // Each tick is its own task; ordering does not matter because every
            // tick carries an absolute byte count.
            tokio::spawn(async move {
                let _ = queue.inner.store.update_progress(&id, downloaded).await;
                queue.broadcast().await;
            });
        };
        let (queue, size_id) = (self.clone(), id.to_string());
        let on_size_known = move |total: u64| {
            let queue = queue.clone();
            let id = size_id.clone();
            tokio::spawn(async move {
                let _ = queue.inner.store.update_size(&id, total).await;
            });
        };

        self.inner
            .transfer
            .stream_transfer(TransferOpts {
                spec,
                abs_path,
                cancel,
                on_progress: &on_progress,
                on_size_known: &on_size_known,
            })
            .await
    }

    /// Settle everyone waiting on this download. A failure reaches each waiter
    /// as a fresh `provider_error` carrying the message, not the original error.
    fn fanout(&self, id: &str, result: std::result::Result<PathBuf, String>) {
        let waiters = {
            let mut in_flight = lock(&self.inner.in_flight);
            match in_flight.get_mut(id) {
                Some(entry) => std::mem::take(&mut entry.waiters),
                None => return,
            }
        };
        for waiter in waiters {
            let _ = waiter.send(match &result {
                Ok(path) => Ok(path.clone()),
                Err(message) => Err(AppError::new(ErrorCode::ProviderError, message.clone())),
            });
        }
    }

    async fn broadcast(&self) {
        // Refreshed first and unconditionally: the sync seam reads the cache
        // even when no client is subscribed.
        let snapshot = self.refresh_cache().await;
        if self.inner.changes.receiver_count() == 0 {
            return;
        }
        let _ = self.inner.changes.send(snapshot);
    }

    /// Where this spec's bytes land. `rel_path: Some("")` resolves the
    /// destination root itself.
    fn resolve_abs_path(&self, spec: &EnqueueSpec) -> Result<PathBuf> {
        let rel_path = match spec.rel_path.clone() {
            Some(rel) => rel,
            None => parse_source(&spec.source, &self.inner.hf_base_url)?.rel_path,
        };
        let root = self.destination_root(spec.destination);
        // Normalised because the id is what cancel, retry and remove match on:
        // `Path::join` would otherwise keep a literal `..` or trailing `/`.
        let abs = crate::contain::normalize(&root.join(&rel_path));
        // A caller-supplied `rel_path` bypasses the source parser's
        // per-component check, so the JOINED path is checked here.
        if !is_within(&root, &abs) {
            return Err(validation(format!(
                "download path escapes {} root",
                spec.destination
            )));
        }
        Ok(abs)
    }

    fn destination_root(&self, destination: DownloadEntryDestination) -> PathBuf {
        match destination {
            DownloadEntryDestination::Models => self.inner.paths.models_dir().to_path_buf(),
            DownloadEntryDestination::Binaries => self.inner.paths.bin_dir(),
            DownloadEntryDestination::Extensions => self.inner.paths.extensions_dir(),
        }
    }
}

/// A zero-byte file counts as absent: it is an aborted prior download.
async fn already_on_disk(abs_path: &Path) -> bool {
    match tokio::fs::metadata(abs_path).await {
        Ok(meta) => meta.is_file() && meta.len() > 0,
        Err(_) => false,
    }
}

fn download_id(destination: DownloadEntryDestination, abs_path: &Path) -> String {
    format!("{}:{}", destination, abs_path.display())
}

/// The spec a resumed or retried row implies. A row carries no hash, URL or
/// `rel_path`, which is why a `binaries` row cannot be resumed this way.
fn spec_from_row(row: &DownloadEntry) -> EnqueueSpec {
    EnqueueSpec {
        source: row.source.clone(),
        destination: row.destination,
        group_id: row.group_id.clone(),
        size_hint: row.size_bytes.map(|n| n as u64),
        sha256: None,
        url: None,
        rel_path: None,
        filename: None,
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|err| err.into_inner())
}

#[cfg(test)]
#[allow(clippy::panic)]
fn injected_panic() -> ! {
    panic!("d7: injected worker panic (see the panic-guard test)");
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::db::migrate::migrate;
    use crate::db::Db;
    use crate::downloads::deps::NoCatalog;
    use crate::downloads::net::Net;
    use crate::downloads::testing::{bounded, Serve, Upstream};

    /// Every spec here carries an explicit URL, so nothing reaches this host.
    const HF: &str = "https://huggingface.co";
    const BODY: &[u8] = b"0123456789abcdef";
    const DEAD: &str = "http://127.0.0.1:1/f";

    struct Fixture {
        queue: DownloadQueue,
        store: DownloadStore,
        dir: tempfile::TempDir,
    }

    impl Fixture {
        fn new() -> Self {
            Self::build(HF, Duration::from_millis(0))
        }

        fn with_delay(delay: Duration) -> Self {
            Self::build(HF, delay)
        }

        /// Needed by every test that goes through a RESUMED or RETRIED row:
        /// the row carries no URL, so its spec is resolved against this host.
        fn with_hf(hf: &str) -> Self {
            Self::build(hf, Duration::from_millis(0))
        }

        fn build(hf: &str, delay: Duration) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let db = Db::open_in_memory().unwrap();
            db.call_blocking(|conn| migrate(conn)).unwrap();
            let store = DownloadStore::new(db, hf);
            let net = Arc::new(Net::new().unwrap());
            let transfer = Arc::new(
                Transfer::new(net, Arc::new(NoCatalog), hf)
                    .with_intervals(Duration::from_secs(5), Duration::from_millis(1)),
            );
            let paths = Paths::rooted(dir.path());
            let queue = DownloadQueue::new(store.clone(), transfer, paths, hf).with_delay(delay);
            Self { queue, store, dir }
        }

        fn models_root(&self) -> PathBuf {
            self.dir.path().join("models")
        }

        fn dest(&self, name: &str) -> PathBuf {
            self.models_root().join(format!("u/r/{name}"))
        }

        async fn row(&self, id: &str) -> DownloadEntry {
            self.store.get_row(id).await.unwrap().unwrap()
        }

        async fn status(&self, id: &str) -> DownloadStatus {
            self.row(id).await.status
        }

        /// Stage a `Pending` row for `s` at `dest`, then optionally move it to
        /// `status`. Returns the id.
        async fn stage(
            &self,
            s: &EnqueueSpec,
            dest: &Path,
            status: Option<(DownloadStatus, Option<u64>)>,
        ) -> String {
            let id = self.queue.id_for(s).unwrap();
            self.store.upsert_pending(&id, s, dest).await.unwrap();
            if let Some((status, bytes)) = status {
                let error = (status == DownloadStatus::Error).then(|| "boom".into());
                self.store
                    .set_status(&id, status, error, bytes)
                    .await
                    .unwrap();
            }
            id
        }

        fn spawn_enqueue(&self, s: &EnqueueSpec) -> tokio::task::JoinHandle<Result<PathBuf>> {
            let (queue, s) = (self.queue.clone(), s.clone());
            tokio::spawn(async move { queue.enqueue(&s).await })
        }

        /// Poll the row until `done`, failing with `msg` after five seconds.
        async fn wait_until(&self, id: &str, done: impl Fn(&DownloadEntry) -> bool, msg: &str) {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let row = self.row(id).await;
                if done(&row) {
                    return;
                }
                assert!(Instant::now() < deadline, "{msg}: {:?}", row.status);
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
    }

    fn spec(url: &str, name: &str) -> EnqueueSpec {
        EnqueueSpec {
            source: format!("@u/r/main/{name}"),
            destination: DownloadEntryDestination::Models,
            group_id: "g".into(),
            size_hint: None,
            sha256: None,
            url: Some(url.to_string()),
            rel_path: None,
            filename: None,
        }
    }

    /// The common setup: an upstream serving `serve`, a default fixture, and a
    /// spec for `/f`.
    async fn served(serve: Serve) -> (Upstream, Fixture, EnqueueSpec) {
        let server = Upstream::start(serve).await;
        let s = spec(&server.url("/f"), "model.gguf");
        (server, Fixture::new(), s)
    }

    fn trickle(count: usize) -> Serve {
        Serve::Trickle {
            chunk: b"abcd".to_vec(),
            count,
            gap: Duration::from_millis(5),
            declare_length: true,
        }
    }

    fn write(path: &Path, bytes: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    fn is_terminal(row: &DownloadEntry) -> bool {
        row.status != DownloadStatus::Pending && row.status != DownloadStatus::Downloading
    }

    #[tokio::test]
    async fn a_download_lands_its_bytes_and_completes_the_row() {
        let (_server, fx, s) = served(Serve::Ranged(BODY.to_vec())).await;

        let path = bounded(fx.queue.enqueue(&s)).await.unwrap();

        assert_eq!(path, fx.dest("model.gguf"));
        assert_eq!(std::fs::read(&path).unwrap(), BODY);
        let row = fx.row(&fx.queue.id_for(&s).unwrap()).await;
        assert_eq!(row.status, DownloadStatus::Completed);
        assert_eq!(row.downloaded_bytes, BODY.len() as i64);
    }

    #[tokio::test]
    async fn id_for_matches_the_row_enqueue_creates() {
        let (_server, fx, s) = served(Serve::Ranged(BODY.to_vec())).await;

        let expected = fx.queue.id_for(&s).unwrap();
        bounded(fx.queue.enqueue(&s)).await.unwrap();

        let rows = fx.queue.snapshot().await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, expected);
        assert_eq!(
            expected,
            format!("models:{}", fx.dest("model.gguf").display())
        );
    }

    #[tokio::test]
    async fn a_file_already_on_disk_completes_without_a_transfer() {
        let server = Upstream::start(Serve::Ranged(BODY.to_vec())).await;
        let fx = Fixture::new();
        let dest = fx.dest("model.gguf");
        write(&dest, b"already here");

        bounded(fx.queue.enqueue(&spec(&server.url("/f"), "model.gguf")))
            .await
            .unwrap();

        assert!(
            server.calls().is_empty(),
            "the fast path reached the network"
        );
        assert_eq!(std::fs::read(&dest).unwrap(), b"already here");
    }

    #[tokio::test]
    async fn a_zero_byte_file_is_treated_as_absent() {
        let server = Upstream::start(Serve::Ranged(BODY.to_vec())).await;
        let fx = Fixture::new();
        let dest = fx.dest("model.gguf");
        write(&dest, b"");

        bounded(fx.queue.enqueue(&spec(&server.url("/f"), "model.gguf")))
            .await
            .unwrap();

        assert_eq!(
            std::fs::read(&dest).unwrap(),
            BODY,
            "it did not re-download"
        );
        assert!(!server.calls().is_empty());
    }

    #[tokio::test]
    async fn two_callers_for_one_file_share_one_transfer() {
        let (server, fx, s) = served(trickle(4)).await;

        let (first, second) =
            bounded(async { tokio::join!(fx.queue.enqueue(&s), fx.queue.enqueue(&s)) }).await;

        assert_eq!(first.unwrap(), fx.dest("model.gguf"));
        assert_eq!(second.unwrap(), fx.dest("model.gguf"));
        let gets = server
            .calls()
            .into_iter()
            .filter(|c| c.method == "GET")
            .count();
        assert_eq!(gets, 1, "the second caller started its own transfer");
    }

    #[tokio::test]
    async fn only_one_transfer_runs_at_a_time() {
        let server = Upstream::start(trickle(6)).await;
        let fx = Fixture::new();

        let (a, b, c) = (
            spec(&server.url("/a"), "a.gguf"),
            spec(&server.url("/b"), "b.gguf"),
            spec(&server.url("/c"), "c.gguf"),
        );
        let results = bounded(async {
            tokio::join!(
                fx.queue.enqueue(&a),
                fx.queue.enqueue(&b),
                fx.queue.enqueue(&c),
            )
        })
        .await;

        // A gauge of 1 is also what three failed transfers would report.
        assert!(results.0.is_ok(), "{:?}", results.0);
        assert!(results.1.is_ok(), "{:?}", results.1);
        assert!(results.2.is_ok(), "{:?}", results.2);
        assert_eq!(
            server.max_concurrent(),
            1,
            "two transfers were in flight together"
        );
    }

    #[tokio::test]
    async fn the_inter_download_delay_is_inside_the_critical_section() {
        let server = Upstream::start(Serve::Ranged(BODY.to_vec())).await;
        let delay = Duration::from_millis(300);
        let fx = Fixture::with_delay(delay);

        let started = Instant::now();
        bounded(fx.queue.enqueue(&spec(&server.url("/a"), "a.gguf")))
            .await
            .unwrap();
        bounded(fx.queue.enqueue(&spec(&server.url("/b"), "b.gguf")))
            .await
            .unwrap();
        let elapsed = started.elapsed();

        assert!(
            elapsed >= delay,
            "the second download started during the throttle ({elapsed:?} < {delay:?})"
        );
    }

    #[tokio::test]
    async fn cancelling_a_running_download_drops_the_partial() {
        let (_server, fx, s) = served(trickle(200)).await;
        let id = fx.queue.id_for(&s).unwrap();

        let queue = fx.queue.clone();
        let cancel_id = id.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(60)).await;
            queue.cancel(&cancel_id).await.unwrap();
        });
        let err = bounded(fx.queue.enqueue(&s)).await.unwrap_err();

        assert_eq!(err.code, ErrorCode::ProviderError, "the fanout's own code");
        assert_eq!(fx.status(&id).await, DownloadStatus::Cancelled);
        assert!(
            !tmp_path_for(&fx.dest("model.gguf")).exists(),
            "a cancel abandons the partial"
        );
    }

    #[tokio::test]
    async fn a_transient_error_keeps_the_partial_for_a_later_resume() {
        let fx = Fixture::new();
        let dest = fx.dest("model.gguf");
        write(&tmp_path_for(&dest), b"partial");
        let server = Upstream::start(Serve::Status(503)).await;
        let s = spec(&server.url("/f"), "model.gguf");

        let err = bounded(fx.queue.enqueue(&s)).await.unwrap_err();

        assert_eq!(err.code, ErrorCode::ProviderError);
        assert_eq!(
            fx.status(&fx.queue.id_for(&s).unwrap()).await,
            DownloadStatus::Error
        );
        assert!(
            tmp_path_for(&dest).exists(),
            "a transient failure destroyed the partial a retry needs"
        );
    }

    #[tokio::test]
    async fn cancelling_a_pending_row_that_is_not_in_flight_flips_it_directly() {
        let fx = Fixture::new();
        let s = spec(DEAD, "model.gguf");
        let id = fx.stage(&s, &fx.dest("model.gguf"), None).await;

        fx.queue.cancel(&id).await.unwrap();

        let row = fx.row(&id).await;
        assert_eq!(row.status, DownloadStatus::Cancelled);
        assert_eq!(row.error.as_deref(), Some("cancelled"));
    }

    #[tokio::test]
    async fn cancelling_a_completed_row_does_nothing() {
        let fx = Fixture::new();
        let dest = fx.dest("model.gguf");
        let s = spec(DEAD, "model.gguf");
        let id = fx.queue.id_for(&s).unwrap();
        fx.store.upsert_completed(&id, &s, &dest).await.unwrap();

        fx.queue.cancel(&id).await.unwrap();

        assert_eq!(fx.status(&id).await, DownloadStatus::Completed);
    }

    #[tokio::test]
    async fn a_download_cancelled_while_queued_can_be_enqueued_again() {
        let (_server, fx, s) = served(Serve::Ranged(BODY.to_vec())).await;
        let id = fx.queue.id_for(&s).unwrap();

        // Arm and cancel before the worker can take the permit, by holding it.
        let held = fx.queue.inner.permits.acquire().await.unwrap();
        let joined = fx.spawn_enqueue(&s);
        tokio::time::sleep(Duration::from_millis(30)).await;
        fx.queue.cancel(&id).await.unwrap();
        drop(held);
        let first = bounded(joined).await.unwrap();

        assert!(first.is_err(), "the cancelled caller was not told");
        assert!(
            !lock(&fx.queue.inner.in_flight).contains_key(&id),
            "the in-flight entry leaked, wedging this id forever"
        );

        let path = bounded(fx.queue.enqueue(&s)).await.unwrap();
        assert_eq!(std::fs::read(path).unwrap(), BODY);
    }

    #[tokio::test]
    async fn retry_zeroes_the_row_and_keeps_the_partial() {
        let fx = Fixture::new();
        let dest = fx.dest("model.gguf");
        write(&tmp_path_for(&dest), b"partial");
        let s = spec(DEAD, "model.gguf");
        let id = fx
            .stage(&s, &dest, Some((DownloadStatus::Error, Some(7))))
            .await;

        fx.queue.retry(&id).await.unwrap();

        let row = fx.row(&id).await;
        assert_eq!(row.downloaded_bytes, 0);
        assert_eq!(row.error, None);
        assert!(
            tmp_path_for(&dest).exists(),
            "retry deleted the partial it exists to resume"
        );
    }

    #[tokio::test]
    async fn retry_refuses_while_the_row_is_downloading() {
        let (_server, fx, s) = served(trickle(200)).await;
        let id = fx.queue.id_for(&s).unwrap();
        let running = fx.spawn_enqueue(&s);
        tokio::time::sleep(Duration::from_millis(50)).await;

        fx.queue.retry(&id).await.unwrap();

        assert_eq!(
            fx.status(&id).await,
            DownloadStatus::Downloading,
            "retry reset a row that was mid-transfer"
        );
        fx.queue.cancel(&id).await.unwrap();
        let _ = bounded(running).await;
    }

    #[tokio::test]
    async fn retry_refuses_while_an_entry_is_live_for_an_already_terminal_row() {
        // The worker writes the terminal status and only THEN drops its entry;
        // a retry landing in between passes the status check, so only the
        // in-flight guard catches it. Staged directly rather than raced.
        let fx = Fixture::new();
        let s = spec(DEAD, "model.gguf");
        let id = fx
            .stage(
                &s,
                &fx.dest("model.gguf"),
                Some((DownloadStatus::Error, Some(7))),
            )
            .await;
        lock(&fx.queue.inner.in_flight).insert(id.clone(), InFlight::new(Vec::new()));

        fx.queue.retry(&id).await.unwrap();

        assert_eq!(
            fx.status(&id).await,
            DownloadStatus::Error,
            "retry re-armed a row whose worker had not finished with it"
        );
    }

    #[tokio::test]
    async fn remove_refuses_while_the_row_is_in_flight() {
        let (_server, fx, s) = served(trickle(200)).await;
        let id = fx.queue.id_for(&s).unwrap();
        let running = fx.spawn_enqueue(&s);
        tokio::time::sleep(Duration::from_millis(50)).await;

        fx.queue.remove(&id).await.unwrap();

        assert!(
            fx.store.get_row(&id).await.unwrap().is_some(),
            "remove deleted a row that was mid-transfer"
        );
        fx.queue.cancel(&id).await.unwrap();
        let _ = bounded(running).await;
    }

    #[tokio::test]
    async fn removing_a_row_drops_its_partial() {
        let fx = Fixture::new();
        let dest = fx.dest("model.gguf");
        write(&tmp_path_for(&dest), b"partial");
        let id = fx.stage(&spec(DEAD, "model.gguf"), &dest, None).await;

        fx.queue.remove(&id).await.unwrap();

        assert!(fx.store.get_row(&id).await.unwrap().is_none());
        assert!(!tmp_path_for(&dest).exists(), "the .tmp was orphaned");
    }

    #[tokio::test]
    async fn removing_a_missing_row_is_a_no_op() {
        let fx = Fixture::new();
        fx.queue.remove("models:/nowhere").await.unwrap();
    }

    #[tokio::test]
    async fn every_non_terminal_row_has_a_live_worker_after_resume() {
        let fx = Fixture::with_hf("http://127.0.0.1:1");
        let id = fx
            .stage(&spec(DEAD, "model.gguf"), &fx.dest("model.gguf"), None)
            .await;

        fx.queue.resume_pending().await.unwrap();

        fx.wait_until(&id, is_terminal, "the resumed row never advanced")
            .await;
        assert_eq!(
            fx.status(&id).await,
            DownloadStatus::Error,
            "no URL on a resumed row"
        );
    }

    #[tokio::test]
    async fn a_binaries_row_is_left_for_the_installer_to_finish() {
        let fx = Fixture::new();
        let mut s = spec(DEAD, "deno.zip");
        s.destination = DownloadEntryDestination::Binaries;
        s.rel_path = Some("staging/deno.zip".into());
        let id = fx
            .stage(&s, &fx.dir.path().join("bin/staging/deno.zip"), None)
            .await;

        fx.queue.resume_pending().await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        assert_eq!(
            fx.status(&id).await,
            DownloadStatus::Pending,
            "a binaries row was resumed by the generic queue"
        );
    }

    #[tokio::test]
    async fn start_normalises_and_then_resumes_in_one_call() {
        // Staged as the state a kill actually leaves: `Downloading`, which
        // `resume_pending` alone would skip.
        let server = Upstream::start(Serve::Ranged(BODY.to_vec())).await;
        let fx = Fixture::with_hf(server.base());
        let s = spec(&server.url("/f"), "model.gguf");
        let id = fx
            .stage(
                &s,
                &fx.dest("model.gguf"),
                Some((DownloadStatus::Downloading, Some(4))),
            )
            .await;

        fx.queue.start().await.unwrap();

        fx.wait_until(
            &id,
            |row| row.status == DownloadStatus::Completed,
            "a row left mid-download was never picked up again",
        )
        .await;
        assert_eq!(std::fs::read(fx.dest("model.gguf")).unwrap(), BODY);
    }

    #[tokio::test]
    async fn a_worker_that_panics_releases_its_id_instead_of_wedging_it() {
        let server = Upstream::start(Serve::Ranged(BODY.to_vec())).await;
        let fx = Fixture::with_hf(server.base());
        let s = spec(&server.url("/f"), "model.gguf");
        let id = fx.queue.id_for(&s).unwrap();
        fx.queue.panic_next_worker();

        let err = bounded(fx.queue.enqueue(&s)).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::ProviderError, "{err:?}");
        assert_eq!(fx.status(&id).await, DownloadStatus::Error);

        fx.queue.retry(&id).await.unwrap();
        fx.wait_until(
            &id,
            |row| row.status == DownloadStatus::Completed,
            "the id stayed wedged after a panic",
        )
        .await;
    }

    #[tokio::test]
    async fn a_retry_racing_an_enqueue_never_strands_the_caller() {
        // Looped: one round proves nothing about the rounds it did not run.
        for round in 0..24 {
            let server = Upstream::start(Serve::Ranged(BODY.to_vec())).await;
            let fx = Fixture::with_hf(server.base());
            let name = format!("model-{round}.gguf");
            let s = spec(&server.url("/f"), &name);
            let id = fx
                .stage(&s, &fx.dest(&name), Some((DownloadStatus::Error, None)))
                .await;

            let (retried, enqueued) = tokio::join!(
                {
                    let queue = fx.queue.clone();
                    let id = id.clone();
                    async move { queue.retry(&id).await }
                },
                bounded(fx.queue.enqueue(&s)),
            );

            retried.unwrap();
            let path = enqueued.unwrap_or_else(|err| {
                panic!("round {round}: the caller was stranded by a racing retry: {err:?}")
            });
            assert_eq!(std::fs::read(&path).unwrap(), BODY, "round {round}");
        }
    }

    #[tokio::test]
    async fn a_remove_racing_an_enqueue_does_not_delete_the_live_row() {
        // Fewer rounds than the retry race: the window is held open, not raced for.
        for round in 0..6 {
            let server = Upstream::start(Serve::Ranged(BODY.to_vec())).await;
            let fx = Fixture::with_hf(server.base());
            fx.queue.widen_ops_race();
            let name = format!("model-{round}.gguf");
            let s = spec(&server.url("/f"), &name);
            let id = fx
                .stage(&s, &fx.dest(&name), Some((DownloadStatus::Error, None)))
                .await;

            let (removed, enqueued) = tokio::join!(
                {
                    let queue = fx.queue.clone();
                    let id = id.clone();
                    async move { queue.remove(&id).await }
                },
                bounded(fx.queue.enqueue(&s)),
            );

            removed.unwrap();
            let path = enqueued.unwrap_or_else(|err| {
                panic!("round {round}: a racing remove killed the download: {err:?}")
            });
            assert_eq!(std::fs::read(&path).unwrap(), BODY, "round {round}");
            // The bytes arrive either way; the row is what the race destroys.
            let row = fx
                .store
                .get_row(&id)
                .await
                .unwrap()
                .unwrap_or_else(|| panic!("round {round}: a racing remove deleted a live row"));
            assert_eq!(row.status, DownloadStatus::Completed, "round {round}");
        }
    }

    #[tokio::test]
    async fn a_caller_supplied_rel_path_cannot_escape_its_root() {
        let fx = Fixture::new();
        let mut s = spec(DEAD, "model.gguf");
        s.rel_path = Some("../../../etc/passwd".into());

        let err = fx.queue.id_for(&s).unwrap_err();

        assert_eq!(err.code, ErrorCode::ValidationError);
        assert!(err.message.contains("escapes"), "{}", err.message);
        assert!(bounded(fx.queue.enqueue(&s)).await.is_err());
    }

    #[tokio::test]
    async fn an_empty_rel_path_resolves_the_destination_root_itself() {
        let fx = Fixture::new();
        let mut s = spec(DEAD, "model.gguf");
        s.rel_path = Some(String::new());

        let id = fx.queue.id_for(&s).unwrap();

        assert_eq!(id, format!("models:{}", fx.models_root().display()));
    }

    #[tokio::test]
    async fn a_contained_rel_path_is_normalised_before_it_becomes_an_id() {
        let fx = Fixture::new();
        let mut s = spec(DEAD, "model.gguf");
        s.rel_path = Some("u/../r/model.gguf".into());

        let id = fx.queue.id_for(&s).unwrap();

        assert_eq!(
            id,
            format!("models:{}", fx.models_root().join("r/model.gguf").display())
        );
        assert!(!id.contains(".."), "a traversal survived into the id");
    }

    #[tokio::test]
    async fn a_lagging_subscriber_still_learns_the_terminal_state_by_pulling() {
        let server = Upstream::start(Serve::Ranged(BODY.to_vec())).await;
        let fx = Fixture::new();
        let mut rx = fx.queue.subscribe();
        let s = spec(&server.url("/f"), "model.gguf");

        for _ in 0..(BROADCAST_CAPACITY * 2) {
            fx.queue.broadcast().await;
        }
        bounded(fx.queue.enqueue(&s)).await.unwrap();

        assert!(
            matches!(
                rx.try_recv(),
                Err(broadcast::error::TryRecvError::Lagged(_))
            ),
            "the fixture failed to make the receiver lag"
        );
        let rows = fx.queue.snapshot().await.unwrap();
        assert_eq!(rows[0].status, DownloadStatus::Completed);
    }
}
