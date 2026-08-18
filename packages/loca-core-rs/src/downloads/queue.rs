//! The queue: one transfer at a time, the in-flight set, the waiter fanout, and
//! the change broadcast.
//!
//! Persistence is [`DownloadStore`]'s and the bytes are [`Transfer`]'s. This
//! module orchestrates: it decides what runs, what joins, what stops, and who
//! hears about it.
//!
//! # One at a time, and the throttle is inside the critical section
//!
//! A single permit, because HuggingFace rate-limits a burst of parallel pulls.
//! The inter-download delay is awaited **while the permit is still held**
//! (`manager.ts:356-363` releases it from inside the timer, not before it), so
//! the throttle is part of the critical section rather than a gap after it.
//! Releasing first and then sleeping would let the next transfer start
//! immediately and delete the throttle entirely, which is why it has its own
//! test.
//!
//! `pendingTimers` and `shutdown()` do not survive the port. Their whole job is
//! cancelling stray `setTimeout` handles at process exit, and a dropped tokio
//! task cancels its own sleep. Recorded so the missing `shutdown()` reads as a
//! structural drop rather than an omission.
//!
//! # The invariant: every non-terminal row has a live worker
//!
//! The worker no-ops when it finds no in-flight entry, so a row that reaches
//! `Pending` without one stays `Pending` forever with nothing to advance it, and
//! the core status bar (which counts non-terminal rows) sticks on "Downloading".
//! Every one of the three paths that creates a row therefore **arms the
//! in-flight entry before spawning**: [`DownloadQueue::enqueue`],
//! [`DownloadQueue::retry`], and [`DownloadQueue::resume_pending`].
//!
//! # A lagging subscriber must pull, not wait
//!
//! Progress broadcasts fire every 250 ms, so a slow consumer can fall behind and
//! `broadcast` drops the oldest frames. Dropping a progress frame is harmless.
//! Dropping the **terminal** frame is not: the sidecar-boot hook and the hub's
//! requirements edge both watch for a transition rather than a state. So a
//! receiver that sees `RecvError::Lagged` must call [`DownloadQueue::snapshot`]
//! and diff against what it last saw, rather than waiting for the next frame.

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
/// burst-throttle. Injectable for the same reason the stall window is: a test
/// that proves the second download waits has to run on a real clock, and the
/// shipping RATIO is what it must keep.
pub const INTER_DOWNLOAD_DELAY: Duration = Duration::from_secs(1);

/// How many snapshots the change channel buffers before a slow receiver lags.
const BROADCAST_CAPACITY: usize = 64;

/// One download that is queued or running, and everyone waiting on it.
struct InFlight {
    cancel: CancellationToken,
    /// Each caller that asked for this file gets the final result. Two callers
    /// for one file share one transfer.
    waiters: Vec<oneshot::Sender<std::result::Result<PathBuf, AppError>>>,
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
    /// Serialises the three operations that decide who owns an id: `enqueue`'s
    /// arming half, `retry` and `remove`.
    ///
    /// The oracle needs nothing like this because all three are synchronous
    /// between their guard and their mutation (`manager.ts:152-165, 189-199,
    /// 216-224`), and JavaScript cannot interleave a synchronous block. Here
    /// each of them awaits a SQLite call in the middle, so the guard and the
    /// act are not atomic without it. Never held across a transfer: only the
    /// deciding half.
    ops: tokio::sync::Mutex<()>,
    /// Makes the next worker panic. See [`DownloadQueue::panic_next_worker`].
    #[cfg(test)]
    panic_next: std::sync::atomic::AtomicBool,
    /// Holds `remove` open mid-operation. See [`DownloadQueue::widen_ops_race`].
    #[cfg(test)]
    widen_race: std::sync::atomic::AtomicBool,
    /// One permit: at most one transfer at a time.
    permits: Semaphore,
    changes: broadcast::Sender<Arc<Vec<DownloadEntry>>>,
    inter_download_delay: Mutex<Duration>,
    /// The last snapshot, kept current so a SYNCHRONOUS caller can read the
    /// queue without blocking a tokio worker on SQLite.
    ///
    /// `models::deps::Downloads::models_snapshot` is sync (the TypeScript's is,
    /// because its SQLite call is), while every read here goes through
    /// `Db::call` and a blocking pool. Refreshed on the same three edges that
    /// can change what a snapshot would say: the boot pass, every broadcast, and
    /// every reconcile. Never refreshed lazily on read, because a lazy refresh
    /// from a sync method is exactly the blocking call this avoids.
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

    /// The boot sequence: settle the table against the disk, then put a worker
    /// behind every row that still needs one.
    ///
    /// **Both halves, and in this order.** [`Self::normalize`] alone is what
    /// boot used to do, and it flips every `Downloading` row to `Pending`
    /// without spawning anything, so a core restarted mid-download came up with
    /// a row nothing would ever advance: the transfer never resumes, the status
    /// bar sticks on "Downloading" (it counts non-terminal rows), and cancel is
    /// the only way out. Normalising after resuming would be worse still, since
    /// it would reset rows that had just been picked up.
    ///
    /// `binaries` rows are deliberately left for
    /// [`crate::downloads::binaries::installer::BinariesInstaller::reconcile_interrupted_installs`],
    /// which boot must call after this. See [`Self::resume_pending`].
    pub async fn start(&self) -> Result<()> {
        self.normalize().await?;
        self.resume_pending().await
    }

    /// Settle the table against the disk. See [`Self::start`], which is what
    /// boot calls: this half on its own leaves rows with no worker.
    ///
    /// Primes the cached snapshot too, so a core that boots with persisted rows
    /// answers a sync read correctly before anything has changed. Without that,
    /// the first `models_snapshot` after a restart reports an empty queue and
    /// the client shows nothing until the next broadcast.
    pub async fn normalize(&self) -> Result<()> {
        self.inner.store.normalize_persisted_rows().await?;
        self.refresh_cache().await;
        Ok(())
    }

    /// The store, for tests that stage rows the queue would otherwise have to
    /// download to create.
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

    /// Hold `remove` open between its in-flight guard and its delete.
    ///
    /// The window is one SQLite round-trip wide, so a test that merely starts
    /// two futures at once loses the race essentially every time and reports a
    /// green it has not earned. Widening it is not staging a fake bug: it is
    /// making an existing one observable.
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
    ///
    /// See [`Inner::cached`]: this is what the sync seam reads.
    pub fn cached_snapshot(&self) -> Arc<Vec<DownloadEntry>> {
        Arc::clone(&lock(&self.inner.cached))
    }

    /// Re-read the queue and update the cache, returning what it now holds.
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

    /// Subscribe to queue changes. See the module docs: on `Lagged`, pull a
    /// [`Self::snapshot`] and diff rather than waiting for the next frame.
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<Vec<DownloadEntry>>> {
        self.inner.changes.subscribe()
    }

    pub async fn snapshot(&self) -> Result<Vec<DownloadEntry>> {
        self.inner.store.snapshot().await
    }

    /// Rows filtered to one destination, which is what the models route serves.
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
    ///
    /// Callers that hand ids to clients must use this: cancel, retry and remove
    /// match on it, so any other id silently no-ops.
    pub fn id_for(&self, spec: &EnqueueSpec) -> Result<String> {
        let abs = self.resolve_abs_path(spec)?;
        Ok(download_id(spec.destination, &abs))
    }

    /// Start or join a download. Resolves when the file is on disk.
    pub async fn enqueue(&self, spec: &EnqueueSpec) -> Result<PathBuf> {
        let abs_path = self.resolve_abs_path(spec)?;
        let id = download_id(spec.destination, &abs_path);
        // Held for the arming half only, never across `rx.await`: this
        // serialises the three operations that decide who owns an id, and
        // holding it while a multi-gigabyte transfer runs would serialise the
        // whole API instead.
        let ops = self.inner.ops.lock().await;

        // Fast path: a usable file is already there.
        if already_on_disk(&abs_path).await {
            self.inner
                .store
                .upsert_completed(&id, spec, &abs_path)
                .await?;
            self.broadcast().await;
            return Ok(abs_path);
        }

        let (tx, rx) = oneshot::channel();
        // Armed here, under the lock, so two concurrent callers cannot both
        // decide they own the transfer. Only the one that created the entry
        // starts a worker; the other becomes a waiter on it.
        let is_owner = {
            let mut in_flight = lock(&self.inner.in_flight);
            match in_flight.get_mut(&id) {
                Some(existing) => {
                    // Two callers for one file share one transfer.
                    existing.waiters.push(tx);
                    false
                }
                None => {
                    in_flight.insert(
                        id.clone(),
                        InFlight {
                            cancel: CancellationToken::new(),
                            waiters: vec![tx],
                        },
                    );
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
            // The sender was dropped without settling. The oracle's equivalent
            // is a promise that never resolves; a distinguishable error beats a
            // hang, and the in-flight delete that causes it is tested.
            Err(AppError::new(
                ErrorCode::InternalError,
                format!("download {id} ended without a result"),
            ))
        })
    }

    /// Stop a download, in whichever of its three states it is found.
    ///
    /// Running or queued: trip the token and let the worker settle the row.
    /// Not in flight and `Pending`: flip the row straight to `Cancelled`.
    /// Not in flight and anything else: nothing.
    ///
    /// `cancel` never deletes the row. [`Self::remove`] does.
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

    /// Re-queue a failed download.
    ///
    /// A no-op when the row is missing, already in flight, or in a non-terminal
    /// status. The byte count is zeroed on the ROW only: the `.tmp` on disk is
    /// left alone, which is exactly what makes the retry resume rather than
    /// restart. Between the retry and the first progress tick the row therefore
    /// says 0 while the file says N, and the number jumps back up.
    pub async fn retry(&self, id: &str) -> Result<()> {
        // Serialised against `enqueue` and `remove`. The oracle is synchronous
        // from its `getRow` to its `inFlight.set` (`manager.ts:189-199`), so no
        // window exists there; here there are three awaits in between, and the
        // insert at the end is UNCONDITIONAL. A retry racing an enqueue for the
        // same id therefore replaced the entry the enqueue had just armed,
        // dropping its waiter's sender and spawning a second worker for one
        // file.
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
        // Guard and arm in ONE critical section, so even a caller that reaches
        // this without the ops lock cannot interleave with `enqueue`'s arm.
        let armed = {
            let mut in_flight = lock(&self.inner.in_flight);
            if in_flight.contains_key(id) {
                false
            } else {
                in_flight.insert(
                    id.to_string(),
                    InFlight {
                        cancel: CancellationToken::new(),
                        waiters: Vec::new(),
                    },
                );
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
            // Nothing will run, so the id must not stay claimed: leaving the
            // entry behind is exactly the wedge this guard exists to avoid.
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

    /// Drop a row and its partial. A no-op while in flight.
    ///
    /// The `.tmp` unlink is the cleanup point: an `Error` row keeps its partial
    /// so a retry can resume, so discarding the row is where that file stops
    /// being wanted.
    pub async fn remove(&self, id: &str) -> Result<()> {
        // Same window as `retry`, with a worse outcome: an `enqueue` that armed
        // between the guard and the delete has its row deleted and its `.tmp`
        // unlinked out from under a live transfer, which then fails at the
        // publishing rename with an id that no longer has a row.
        let _ops = self.inner.ops.lock().await;
        let Some(row) = self.inner.store.get_row(id).await? else {
            return Ok(());
        };
        if lock(&self.inner.in_flight).contains_key(id) {
            return Ok(());
        }
        // Widens the window between the guard above and the delete below, so a
        // test can open it deterministically instead of racing a SQLite
        // round-trip for it. Inert unless a test asks; compiled out otherwise.
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
            // Refreshed even when nothing was dropped, because the sync seam's
            // one caller reconciles and then reads immediately, and "nothing
            // vanished" does not mean "nothing changed since the last edge".
            self.refresh_cache().await;
        }
        Ok(())
    }

    /// Re-spawn rows left `Pending` when the process last exited.
    ///
    /// **`binaries` rows are skipped deliberately.** A generic resume re-runs
    /// only the byte transfer, but a binary still needs the extract-and-record
    /// step that lives in the installer, and its sha256 is not re-verified here.
    /// Resuming one would land an unverified archive in staging and never
    /// install it. The installer's own boot reconcile finishes those instead.
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
                // Armed BEFORE the spawn, like the other two paths. See the
                // module docs: the worker no-ops without an entry, so a row
                // spawned without one would never advance.
                in_flight.insert(
                    row.id.clone(),
                    InFlight {
                        cancel: CancellationToken::new(),
                        waiters: Vec::new(),
                    },
                );
            }
            self.spawn(
                row.id.clone(),
                spec_from_row(&row),
                PathBuf::from(&row.abs_path),
            );
        }
        Ok(())
    }

    // --- internals ---------------------------------------------------------

    /// Start a worker for `id`, and make sure that whatever happens to it, the
    /// entry it owns is released.
    ///
    /// The worker runs in a task of its own so this one can observe it dying.
    /// A panic anywhere inside [`Self::run`] unwinds past the `in_flight`
    /// removal and the fanout, and the consequences are not "one download
    /// failed": the `oneshot::Sender` leaks inside the surviving map entry, so
    /// `enqueue`'s `rx.await` never resolves, and the in-flight guards then
    /// wedge that id forever, with cancel, retry and remove all no-opping on
    /// it. One arithmetic overflow on a `content-length` header was enough to
    /// reach that state, which is why the guard is structural rather than a
    /// promise to be careful.
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

    /// Release an id whose worker never got to release it itself.
    ///
    /// Writes the terminal row, settles the waiters, drops the entry: the same
    /// three things `run` does on its failing path, in the same order, so a
    /// dead worker leaves the queue in a state a retry can act on.
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
        // The only way to exercise the panic guard is to stage a panic, and the
        // guard protects an invariant (every non-terminal row has a live
        // worker) that no other failure mode can break. Compiled out of every
        // production build.
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
                // No entry: nothing armed this, so there is nothing to run. The
                // invariant in the module docs exists to make this unreachable.
                None => return Ok(()),
                Some(entry) => entry.cancel.clone(),
            }
        };

        if cancel.is_cancelled() {
            // Cancelled while it sat in the queue.
            self.inner
                .store
                .set_status(id, DownloadStatus::Cancelled, None, None)
                .await?;
            self.fanout(id, Err(net::CANCELLED_MESSAGE.to_string()));
            // Dropping the entry here is load-bearing. Without it a download
            // cancelled while queued leaks its entry, and the in-flight guards
            // then wedge that id forever: retry and remove no-op, and a fresh
            // enqueue joins a dead entry whose waiter never settles.
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
                // No byte count: the progress write above already set it, and
                // passing one here would re-zero a row that just finished.
                self.inner
                    .store
                    .set_status(id, DownloadStatus::Completed, None, None)
                    .await?;
            }
            Err(err) if cancel.is_cancelled() => {
                // The USER stopped this, so the partial is abandoned with it.
                // The classification is the token, not the message, so a cancel
                // racing a genuine failure is still a cancel.
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
                // Range. The terminal-bad outcomes (checksum mismatch, HTTP 416)
                // already deleted it inside the transfer, so both halves have to
                // be read together to see that nothing leaks.
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

        // The permit is still held. See the module docs: the throttle is inside
        // the critical section, so the next transfer cannot start during it.
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
            // The store write and the broadcast are async and the callback is
            // not, so each tick is its own task. Ordering between ticks is not
            // required: every tick carries an absolute byte count, so a late one
            // is stale rather than wrong, and the terminal write is separate.
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

    /// Settle everyone waiting on this download.
    ///
    /// A failure reaches each waiter as a **fresh** `provider_error` carrying
    /// the message, not as the original error. That is the oracle
    /// (`manager.ts:390`), and it is why a cancelled download's waiter sees
    /// `provider_error: cancelled` rather than the `conflict` the transfer
    /// returned: the queue classifies on the token, and the waiter is told what
    /// happened rather than which mechanism said so.
    ///
    /// The waiters are taken out of the entry rather than borrowed, because a
    /// `oneshot::Sender` sends by value; the entry itself is removed by the
    /// caller once the row is written.
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
        // The cache is refreshed FIRST and unconditionally. Skipping it when
        // nobody is subscribed would leave the sync seam reading a stale queue
        // on exactly the common case: a core with no client attached.
        let snapshot = self.refresh_cache().await;
        // `send` erroring on no receivers is not a failure worth surfacing.
        if self.inner.changes.receiver_count() == 0 {
            return;
        }
        let _ = self.inner.changes.send(snapshot);
    }

    /// Where this spec's bytes land.
    ///
    /// `spec.rel_path` is read with "present", so `Some("")` resolves the
    /// destination root itself. That is the oracle (`manager.ts:254`), and it
    /// disagrees with the store, which reads the same field with truthiness. See
    /// the store's module docs.
    fn resolve_abs_path(&self, spec: &EnqueueSpec) -> Result<PathBuf> {
        let rel_path = match spec.rel_path.clone() {
            Some(rel) => rel,
            None => parse_source(&spec.source, &self.inner.hf_base_url)?.rel_path,
        };
        let root = self.destination_root(spec.destination);
        // Normalised, because `@std/path`'s `join` normalises its result and
        // Rust's `Path::join` does not. Without this the two cores mint
        // DIFFERENT ids for the same spec: `rel_path: ""` gives `<root>/` here
        // against `<root>` there, and `a/../b` persists a literal `..` in the
        // id and the row's `abs_path`. The id is what cancel, retry and remove
        // match on and what is written to disk, so a cosmetic-looking difference
        // in the string is a functional one.
        let abs = crate::contain::normalize(&root.join(&rel_path));
        // A caller-supplied `rel_path` bypasses the source parser's
        // per-component check, so the JOINED path is checked here. Shared with
        // the memories store, the storage walker and the sandbox (D-049): a
        // containment control with four copies is a containment control that
        // drifts, and three of them already had.
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

/// Whether a usable file is already there.
///
/// **A zero-byte file counts as absent.** It is an aborted prior download rather
/// than a usable artifact, so treating it as present would complete a row for a
/// file with nothing in it.
async fn already_on_disk(abs_path: &Path) -> bool {
    match tokio::fs::metadata(abs_path).await {
        Ok(meta) => meta.is_file() && meta.len() > 0,
        Err(_) => false,
    }
}

fn download_id(destination: DownloadEntryDestination, abs_path: &Path) -> String {
    format!("{}:{}", destination, abs_path.display())
}

/// The spec a resumed or retried row implies. A row carries no hash, no URL and
/// no explicit `rel_path`, which is why a `binaries` row cannot be resumed this
/// way: it would lose the pinned sha256 that makes it verifiable.
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

/// The staged panic. A function of its own so the `panic!` sits behind
/// `#[cfg(test)]` rather than inside a production one.
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

    /// Far from the HuggingFace host, so no test accidentally triggers the etag
    /// lookup: every spec here carries an explicit URL instead.
    const HF: &str = "https://huggingface.co";
    const BODY: &[u8] = b"0123456789abcdef";

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

        /// A fixture whose HuggingFace host is `hf`.
        ///
        /// Needed by every test that goes through a RESUMED or RETRIED row: the
        /// row carries no URL (that is what makes a `binaries` row unresumable),
        /// so the spec is rebuilt from `source` and resolved against this host.
        /// With the real `huggingface.co` here, those tests make a genuine
        /// outbound request and pass or fail on what the internet says.
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
                    // The shipping ratio kept: a stall window far above the
                    // inter-chunk interval of every fixture below.
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

    // -- enqueue ------------------------------------------------------------

    #[tokio::test]
    async fn a_download_lands_its_bytes_and_completes_the_row() {
        let server = Upstream::start(Serve::Ranged(BODY.to_vec())).await;
        let fx = Fixture::new();

        let path = bounded(fx.queue.enqueue(&spec(&server.url("/f"), "model.gguf")))
            .await
            .unwrap();

        assert_eq!(path, fx.dest("model.gguf"));
        assert_eq!(std::fs::read(&path).unwrap(), BODY);
        let row = fx
            .store
            .get_row(
                &fx.queue
                    .id_for(&spec(&server.url("/f"), "model.gguf"))
                    .unwrap(),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.status, DownloadStatus::Completed);
        assert_eq!(row.downloaded_bytes, BODY.len() as i64);
    }

    #[tokio::test]
    async fn id_for_matches_the_row_enqueue_creates() {
        // Cancel, retry and remove all match on this id, so an id that does not
        // match the row silently no-ops rather than failing loudly.
        let server = Upstream::start(Serve::Ranged(BODY.to_vec())).await;
        let fx = Fixture::new();
        let s = spec(&server.url("/f"), "model.gguf");

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
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::write(&dest, b"already here").unwrap();

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
        // An aborted prior download, not a usable artifact: completing a row for
        // it would leave the user with an empty file the UI calls done.
        let server = Upstream::start(Serve::Ranged(BODY.to_vec())).await;
        let fx = Fixture::new();
        let dest = fx.dest("model.gguf");
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::write(&dest, b"").unwrap();

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
        let server = Upstream::start(Serve::Trickle {
            chunk: b"abcd".to_vec(),
            count: 4,
            gap: Duration::from_millis(5),
            declare_length: true,
        })
        .await;
        let fx = Fixture::new();
        let s = spec(&server.url("/f"), "model.gguf");

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

    // -- one at a time ------------------------------------------------------

    #[tokio::test]
    async fn only_one_transfer_runs_at_a_time() {
        // Counting calls cannot see an overlap; the server's live-GET gauge can.
        let server = Upstream::start(Serve::Trickle {
            chunk: b"abcd".to_vec(),
            count: 6,
            gap: Duration::from_millis(5),
            declare_length: true,
        })
        .await;
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

        // All three must actually have run: a gauge of 1 is also what three
        // failed transfers would report, so the serialisation claim only means
        // something once they are known to have happened.
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
        // The permit is held ACROSS the delay, so the next transfer cannot begin
        // during it. Releasing first and then sleeping would delete the throttle
        // while leaving the sleep in place, which is the mutation this catches.
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

    // -- cancel -------------------------------------------------------------

    #[tokio::test]
    async fn cancelling_a_running_download_drops_the_partial() {
        let server = Upstream::start(Serve::Trickle {
            chunk: b"abcd".to_vec(),
            count: 200,
            gap: Duration::from_millis(5),
            declare_length: true,
        })
        .await;
        let fx = Fixture::new();
        let s = spec(&server.url("/f"), "model.gguf");
        let id = fx.queue.id_for(&s).unwrap();

        let queue = fx.queue.clone();
        let cancel_id = id.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(60)).await;
            queue.cancel(&cancel_id).await.unwrap();
        });
        let err = bounded(fx.queue.enqueue(&s)).await.unwrap_err();

        assert_eq!(err.code, ErrorCode::ProviderError, "the fanout's own code");
        let row = fx.store.get_row(&id).await.unwrap().unwrap();
        assert_eq!(row.status, DownloadStatus::Cancelled);
        assert!(
            !tmp_path_for(&fx.dest("model.gguf")).exists(),
            "a cancel abandons the partial"
        );
    }

    #[tokio::test]
    async fn a_transient_error_keeps_the_partial_for_a_later_resume() {
        // The other half of the pair above: same shape, different signal. A 503
        // mid-life is not the user abandoning anything, so the bytes stay.
        let fx = Fixture::new();
        let dest = fx.dest("model.gguf");
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::write(tmp_path_for(&dest), b"partial").unwrap();
        let server = Upstream::start(Serve::Status(503)).await;
        let s = spec(&server.url("/f"), "model.gguf");

        let err = bounded(fx.queue.enqueue(&s)).await.unwrap_err();

        assert_eq!(err.code, ErrorCode::ProviderError);
        let row = fx
            .store
            .get_row(&fx.queue.id_for(&s).unwrap())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.status, DownloadStatus::Error);
        assert!(
            tmp_path_for(&dest).exists(),
            "a transient failure destroyed the partial a retry needs"
        );
    }

    #[tokio::test]
    async fn cancelling_a_pending_row_that_is_not_in_flight_flips_it_directly() {
        let fx = Fixture::new();
        let dest = fx.dest("model.gguf");
        let s = spec("http://127.0.0.1:1/f", "model.gguf");
        let id = fx.queue.id_for(&s).unwrap();
        fx.store.upsert_pending(&id, &s, &dest).await.unwrap();

        fx.queue.cancel(&id).await.unwrap();

        let row = fx.store.get_row(&id).await.unwrap().unwrap();
        assert_eq!(row.status, DownloadStatus::Cancelled);
        assert_eq!(row.error.as_deref(), Some("cancelled"));
    }

    #[tokio::test]
    async fn cancelling_a_completed_row_does_nothing() {
        let fx = Fixture::new();
        let dest = fx.dest("model.gguf");
        let s = spec("http://127.0.0.1:1/f", "model.gguf");
        let id = fx.queue.id_for(&s).unwrap();
        fx.store.upsert_completed(&id, &s, &dest).await.unwrap();

        fx.queue.cancel(&id).await.unwrap();

        assert_eq!(
            fx.store.get_row(&id).await.unwrap().unwrap().status,
            DownloadStatus::Completed
        );
    }

    #[tokio::test]
    async fn a_download_cancelled_while_queued_can_be_enqueued_again() {
        // The in-flight delete at manager.ts:303. Without it the id is wedged
        // forever: retry and remove no-op, and a fresh enqueue joins a dead
        // entry whose waiter never settles.
        let server = Upstream::start(Serve::Ranged(BODY.to_vec())).await;
        let fx = Fixture::new();
        let s = spec(&server.url("/f"), "model.gguf");
        let id = fx.queue.id_for(&s).unwrap();

        // Arm and cancel before the worker can take the permit, by holding it.
        let held = fx.queue.inner.permits.acquire().await.unwrap();
        let queue = fx.queue.clone();
        let joined = tokio::spawn({
            let s = s.clone();
            async move { queue.enqueue(&s).await }
        });
        tokio::time::sleep(Duration::from_millis(30)).await;
        fx.queue.cancel(&id).await.unwrap();
        drop(held);
        let first = bounded(joined).await.unwrap();

        assert!(first.is_err(), "the cancelled caller was not told");
        assert!(
            !lock(&fx.queue.inner.in_flight).contains_key(&id),
            "the in-flight entry leaked, wedging this id forever"
        );

        // The whole point: the same spec can be enqueued again and completes.
        let path = bounded(fx.queue.enqueue(&s)).await.unwrap();
        assert_eq!(std::fs::read(path).unwrap(), BODY);
    }

    // -- retry and remove ---------------------------------------------------

    #[tokio::test]
    async fn retry_zeroes_the_row_and_keeps_the_partial() {
        // The zeroing is row-only, which is what makes a retry RESUME. Between
        // the retry and the first progress tick the row says 0 while the file
        // says N, and the number jumps back up. That is intended.
        let fx = Fixture::new();
        let dest = fx.dest("model.gguf");
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::write(tmp_path_for(&dest), b"partial").unwrap();
        let s = spec("http://127.0.0.1:1/f", "model.gguf");
        let id = fx.queue.id_for(&s).unwrap();
        fx.store.upsert_pending(&id, &s, &dest).await.unwrap();
        fx.store
            .set_status(&id, DownloadStatus::Error, Some("boom".into()), Some(7))
            .await
            .unwrap();

        fx.queue.retry(&id).await.unwrap();

        let row = fx.store.get_row(&id).await.unwrap().unwrap();
        assert_eq!(row.downloaded_bytes, 0);
        assert_eq!(row.error, None);
        assert!(
            tmp_path_for(&dest).exists(),
            "retry deleted the partial it exists to resume"
        );
    }

    #[tokio::test]
    async fn retry_refuses_while_the_row_is_downloading() {
        let server = Upstream::start(Serve::Trickle {
            chunk: b"abcd".to_vec(),
            count: 200,
            gap: Duration::from_millis(5),
            declare_length: true,
        })
        .await;
        let fx = Fixture::new();
        let s = spec(&server.url("/f"), "model.gguf");
        let id = fx.queue.id_for(&s).unwrap();
        let queue = fx.queue.clone();
        let running = tokio::spawn({
            let s = s.clone();
            async move { queue.enqueue(&s).await }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;

        fx.queue.retry(&id).await.unwrap();

        assert_eq!(
            fx.store.get_row(&id).await.unwrap().unwrap().status,
            DownloadStatus::Downloading,
            "retry reset a row that was mid-transfer"
        );
        fx.queue.cancel(&id).await.unwrap();
        let _ = bounded(running).await;
    }

    #[tokio::test]
    async fn retry_refuses_while_an_entry_is_live_for_an_already_terminal_row() {
        // The window the status check cannot see, and the only thing the
        // in-flight guard catches on its own. The worker writes the terminal
        // status, broadcasts, fans out, and only THEN drops its entry; a retry
        // landing in between passes the status check because the row is already
        // Error. Without the in-flight guard it would arm a second entry over
        // the first and spawn a second worker, and the first worker's own
        // `remove` would then drop the new entry, leaving the row Pending with
        // nothing alive to advance it. That is the invariant inverted.
        let fx = Fixture::new();
        let dest = fx.dest("model.gguf");
        let s = spec("http://127.0.0.1:1/f", "model.gguf");
        let id = fx.queue.id_for(&s).unwrap();
        fx.store.upsert_pending(&id, &s, &dest).await.unwrap();
        fx.store
            .set_status(&id, DownloadStatus::Error, Some("boom".into()), Some(7))
            .await
            .unwrap();
        // Stage the window directly: constructing it by racing a real worker
        // would be a flaky test for a deterministic guard.
        lock(&fx.queue.inner.in_flight).insert(
            id.clone(),
            InFlight {
                cancel: CancellationToken::new(),
                waiters: Vec::new(),
            },
        );

        fx.queue.retry(&id).await.unwrap();

        assert_eq!(
            fx.store.get_row(&id).await.unwrap().unwrap().status,
            DownloadStatus::Error,
            "retry re-armed a row whose worker had not finished with it"
        );
    }

    #[tokio::test]
    async fn remove_refuses_while_the_row_is_in_flight() {
        let server = Upstream::start(Serve::Trickle {
            chunk: b"abcd".to_vec(),
            count: 200,
            gap: Duration::from_millis(5),
            declare_length: true,
        })
        .await;
        let fx = Fixture::new();
        let s = spec(&server.url("/f"), "model.gguf");
        let id = fx.queue.id_for(&s).unwrap();
        let queue = fx.queue.clone();
        let running = tokio::spawn({
            let s = s.clone();
            async move { queue.enqueue(&s).await }
        });
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
        // An Error row keeps its .tmp so a retry can resume, so removing the row
        // is the point at which that file stops being wanted.
        let fx = Fixture::new();
        let dest = fx.dest("model.gguf");
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::write(tmp_path_for(&dest), b"partial").unwrap();
        let s = spec("http://127.0.0.1:1/f", "model.gguf");
        let id = fx.queue.id_for(&s).unwrap();
        fx.store.upsert_pending(&id, &s, &dest).await.unwrap();

        fx.queue.remove(&id).await.unwrap();

        assert!(fx.store.get_row(&id).await.unwrap().is_none());
        assert!(!tmp_path_for(&dest).exists(), "the .tmp was orphaned");
    }

    #[tokio::test]
    async fn removing_a_missing_row_is_a_no_op() {
        let fx = Fixture::new();
        fx.queue.remove("models:/nowhere").await.unwrap();
    }

    // -- resume -------------------------------------------------------------

    #[tokio::test]
    async fn every_non_terminal_row_has_a_live_worker_after_resume() {
        // The invariant: the worker no-ops without an in-flight entry, so a row
        // resumed without one would sit Pending forever with the core status bar
        // stuck on "Downloading".
        // A dead local host rather than the real HuggingFace: a resumed row
        // carries no URL, so its spec is rebuilt from `source` and resolved
        // against the configured host, and this test used to make a genuine
        // outbound request and assert on whatever the internet answered.
        let fx = Fixture::with_hf("http://127.0.0.1:1");
        let s = spec("http://127.0.0.1:1/f", "model.gguf");
        let id = fx.queue.id_for(&s).unwrap();
        fx.store
            .upsert_pending(&id, &s, &fx.dest("model.gguf"))
            .await
            .unwrap();

        fx.queue.resume_pending().await.unwrap();

        // The row must reach a terminal state on its own.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let row = fx.store.get_row(&id).await.unwrap().unwrap();
            if row.status != DownloadStatus::Pending && row.status != DownloadStatus::Downloading {
                assert_eq!(row.status, DownloadStatus::Error, "no URL on a resumed row");
                break;
            }
            assert!(Instant::now() < deadline, "the resumed row never advanced");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[tokio::test]
    async fn a_binaries_row_is_left_for_the_installer_to_finish() {
        // A generic resume re-runs only the byte transfer, but a binary still
        // needs the extract-and-record step and its sha256 is not re-verified
        // here, so resuming one would stage an unverified archive and stop.
        let fx = Fixture::new();
        let mut s = spec("http://127.0.0.1:1/f", "deno.zip");
        s.destination = DownloadEntryDestination::Binaries;
        s.rel_path = Some("staging/deno.zip".into());
        let id = fx.queue.id_for(&s).unwrap();
        fx.store
            .upsert_pending(&id, &s, &fx.dir.path().join("bin/staging/deno.zip"))
            .await
            .unwrap();

        fx.queue.resume_pending().await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        assert_eq!(
            fx.store.get_row(&id).await.unwrap().unwrap().status,
            DownloadStatus::Pending,
            "a binaries row was resumed by the generic queue"
        );
    }

    #[tokio::test]
    async fn start_normalises_and_then_resumes_in_one_call() {
        // The finding: `resume_pending` had no production call site anywhere.
        // Boot called `normalize()` and stopped, which flips a `Downloading` row
        // to `Pending` and spawns NOTHING, so a core restarted mid-download came
        // up with a row nothing would ever advance and a status bar stuck on
        // "Downloading" until the user cancelled it.
        //
        // Staged as the state a kill actually leaves: `Downloading`, which
        // `resume_pending` alone would skip.
        // The HuggingFace host points at the mock, because a resumed row
        // carries no URL and its spec is rebuilt from `source`.
        let server = Upstream::start(Serve::Ranged(BODY.to_vec())).await;
        let fx = Fixture::with_hf(server.base());
        let s = spec(&server.url("/f"), "model.gguf");
        let id = fx.queue.id_for(&s).unwrap();
        fx.store
            .upsert_pending(&id, &s, &fx.dest("model.gguf"))
            .await
            .unwrap();
        fx.store
            .set_status(&id, DownloadStatus::Downloading, None, Some(4))
            .await
            .unwrap();

        fx.queue.start().await.unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let row = fx.store.get_row(&id).await.unwrap().unwrap();
            if row.status == DownloadStatus::Completed {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "a row left mid-download was never picked up again: {:?}",
                row.status
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(std::fs::read(fx.dest("model.gguf")).unwrap(), BODY);
    }

    #[tokio::test]
    async fn a_worker_that_panics_releases_its_id_instead_of_wedging_it() {
        // The structural half of the content-length overflow: `run` unwinds
        // past its `in_flight.remove`, so the waiter's sender leaks inside the
        // surviving entry, `enqueue` never resolves, and cancel, retry and
        // remove all no-op on that id forever.
        let server = Upstream::start(Serve::Ranged(BODY.to_vec())).await;
        // Retrying rebuilds the spec from `source`, so the HuggingFace host has
        // to be the mock for step 3 to mean anything.
        let fx = Fixture::with_hf(server.base());
        let s = spec(&server.url("/f"), "model.gguf");
        let id = fx.queue.id_for(&s).unwrap();
        fx.queue.panic_next_worker();

        // 1. The caller is told, rather than hanging forever.
        let err = bounded(fx.queue.enqueue(&s)).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::ProviderError, "{err:?}");

        // 2. The row is terminal, so the status bar stops counting it.
        assert_eq!(
            fx.store.get_row(&id).await.unwrap().unwrap().status,
            DownloadStatus::Error
        );

        // 3. And the id is usable again: a retry gets a live worker and the
        //    file lands, which is the property the leaked entry destroyed.
        fx.queue.retry(&id).await.unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if fx.store.get_row(&id).await.unwrap().unwrap().status == DownloadStatus::Completed {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the id stayed wedged after a panic"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[tokio::test]
    async fn a_retry_racing_an_enqueue_never_strands_the_caller() {
        // `retry` awaits three times between its in-flight guard and its
        // UNCONDITIONAL insert, where the oracle is synchronous throughout
        // (`manager.ts:189-199`). A retry landing inside that window replaced
        // the entry `enqueue` had just armed, dropping the waiter's sender and
        // leaving a second worker on one file.
        //
        // Looped, because a race is what is being staged: one round proves
        // nothing about the rounds it did not run.
        for round in 0..24 {
            let server = Upstream::start(Serve::Ranged(BODY.to_vec())).await;
            let fx = Fixture::with_hf(server.base());
            let name = format!("model-{round}.gguf");
            let s = spec(&server.url("/f"), &name);
            let id = fx.queue.id_for(&s).unwrap();
            // An Error row, so `retry` gets past its status check.
            fx.store
                .upsert_pending(&id, &s, &fx.dest(&name))
                .await
                .unwrap();
            fx.store
                .set_status(&id, DownloadStatus::Error, Some("boom".into()), None)
                .await
                .unwrap();

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
        // The same window in `remove`, with a worse outcome: the row is deleted
        // and the `.tmp` unlinked out from under a transfer that just started.
        // Six rounds rather than the retry test's twenty-four: the window here
        // is held open deliberately, so it is hit every time rather than raced
        // for.
        for round in 0..6 {
            let server = Upstream::start(Serve::Ranged(BODY.to_vec())).await;
            let fx = Fixture::with_hf(server.base());
            fx.queue.widen_ops_race();
            let name = format!("model-{round}.gguf");
            let s = spec(&server.url("/f"), &name);
            let id = fx.queue.id_for(&s).unwrap();
            fx.store
                .upsert_pending(&id, &s, &fx.dest(&name))
                .await
                .unwrap();
            fx.store
                .set_status(&id, DownloadStatus::Error, Some("boom".into()), None)
                .await
                .unwrap();

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
            // The row is what makes this visible, and it is what the race
            // destroys: a `delete` landing after the enqueue's `upsert_pending`
            // wipes the row of a transfer that is already running, so the
            // client watches the download disappear from the queue and a file
            // appear from nowhere. The bytes arrive either way, which is why
            // asserting only on them cannot see this.
            let row = fx
                .store
                .get_row(&id)
                .await
                .unwrap()
                .unwrap_or_else(|| panic!("round {round}: a racing remove deleted a live row"));
            assert_eq!(row.status, DownloadStatus::Completed, "round {round}");
        }
    }

    // -- containment --------------------------------------------------------

    #[tokio::test]
    async fn a_caller_supplied_rel_path_cannot_escape_its_root() {
        // `rel_path` bypasses the source parser's per-component check, so the
        // JOINED path is what gets checked. Shared with the sandbox, the storage
        // walker and the memories store (D-049).
        let fx = Fixture::new();
        let mut s = spec("http://127.0.0.1:1/f", "model.gguf");
        s.rel_path = Some("../../../etc/passwd".into());

        let err = fx.queue.id_for(&s).unwrap_err();

        assert_eq!(err.code, ErrorCode::ValidationError);
        assert!(err.message.contains("escapes"), "{}", err.message);
        assert!(bounded(fx.queue.enqueue(&s)).await.is_err());
    }

    #[tokio::test]
    async fn an_empty_rel_path_resolves_the_destination_root_itself() {
        // The queue's half of the inconsistency the store's docs record: `??`
        // here, so "" is PRESENT and joins to nothing. The store reads the same
        // field with truthiness and takes the parsed source instead. Unreachable
        // today; pinned on both sides rather than harmonised.
        let fx = Fixture::new();
        let mut s = spec("http://127.0.0.1:1/f", "model.gguf");
        s.rel_path = Some(String::new());

        let id = fx.queue.id_for(&s).unwrap();

        assert_eq!(id, format!("models:{}", fx.models_root().display()));
    }

    #[tokio::test]
    async fn a_contained_rel_path_is_normalised_before_it_becomes_an_id() {
        // `@std/path`'s join normalises and Rust's does not, so without an
        // explicit normalise the two cores mint different ids for one spec and
        // the row's abs_path keeps a literal "..". Contained either way, which
        // is exactly why only the STRING shows the difference.
        let fx = Fixture::new();
        let mut s = spec("http://127.0.0.1:1/f", "model.gguf");
        s.rel_path = Some("u/../r/model.gguf".into());

        let id = fx.queue.id_for(&s).unwrap();

        assert_eq!(
            id,
            format!("models:{}", fx.models_root().join("r/model.gguf").display())
        );
        assert!(!id.contains(".."), "a traversal survived into the id");
    }

    // -- broadcast ----------------------------------------------------------

    #[tokio::test]
    async fn a_lagging_subscriber_still_learns_the_terminal_state_by_pulling() {
        // Progress frames are droppable; the terminal frame is not, because the
        // sidecar-boot hook and the requirements edge watch for a TRANSITION.
        // The contract is that a lagged receiver pulls a snapshot.
        let server = Upstream::start(Serve::Ranged(BODY.to_vec())).await;
        let fx = Fixture::new();
        let mut rx = fx.queue.subscribe();
        let s = spec(&server.url("/f"), "model.gguf");

        // Overrun the channel without reading it.
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
