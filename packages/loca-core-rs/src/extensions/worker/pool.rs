//! The warm-worker pool: one process per (extension, tool), an LRU cap, and an
//! idle timer.
//!
//! # Why the key is a pair
//!
//! Workers are keyed per **(extension, tool)**, not per extension, and that is
//! least privilege: the spawn flags come from ONLY the invoked tool's grants,
//! so a benign tool never runs in a process holding a sibling tool's `run` or
//! `ffi` grant.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{Duration, Instant};

use crate::error::{AppError, ErrorCode, Result};
use crate::settings::access::num_setting;

use super::call::{
    start_call, CallBudgets, CallController, CallEvent, ModuleDispatch, PoolCallbacks,
    ToolCallStart,
};
use super::handle::WorkerHandle;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolConfig {
    /// How many worker processes may be warm at once.
    pub max_warm_workers: usize,
    /// How long a worker with no calls survives before it is evicted.
    pub worker_idle: Duration,
    /// The compute budget for one call. Pausable: time spent waiting on a human
    /// does not burn it.
    pub call_timeout: Duration,
    /// How long a `shutdown` frame gets before the process is killed.
    pub drain_timeout: Duration,
}

pub const DEFAULT_POOL_CONFIG: PoolConfig = PoolConfig {
    max_warm_workers: 8,
    worker_idle: Duration::from_secs(300),
    call_timeout: Duration::from_secs(60),
    drain_timeout: Duration::from_secs(2),
};

/// One warm worker's identity in the pool.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct WorkerKey {
    pub extension_id: String,
    pub tool_name: String,
}

impl WorkerKey {
    pub fn new(extension_id: impl Into<String>, tool_name: impl Into<String>) -> Self {
        Self {
            extension_id: extension_id.into(),
            tool_name: tool_name.into(),
        }
    }
}

/// How a worker comes into existence. A seam, because the real implementation
/// needs the registry, the sandbox derivation and a `deno` on disk, and the
/// pool's own keying, eviction and retirement need none of them.
#[async_trait]
pub trait WorkerSpawner: Send + Sync {
    async fn spawn(&self, key: &WorkerKey) -> Result<WorkerHandle>;
}

struct Inner {
    workers: BTreeMap<WorkerKey, WorkerHandle>,
    /// Least recently used first.
    lru: Vec<WorkerKey>,
    idle_timers: BTreeMap<WorkerKey, JoinHandle<()>>,
}

#[derive(Clone)]
pub struct WorkerPool {
    /// Live rather than fixed at construction (three of its four fields are
    /// user settings) and shared across clones, since `Accounting` holds one
    /// and arms idle timers from it.
    config: Arc<Mutex<PoolConfig>>,
    spawner: Arc<dyn WorkerSpawner>,
    inner: Arc<Mutex<Inner>>,
}

/// The settings that tune the pool, paired with a test asserting every key
/// [`pool_config_from`] reads is in it: a watch set that omits one means the
/// user changes a setting and nothing happens. `drain_timeout` is not a setting.
pub const POOL_KEYS: &[&str] = &[
    "extensions.maxWarmWorkers",
    "extensions.workerIdleMs",
    "extensions.callTimeoutMs",
];

/// Read the pool's tunables from a RESOLVED settings record. Every fallback is
/// [`DEFAULT_POOL_CONFIG`]'s value, which is also the schema default. A zero or
/// negative is refused: zero warm workers means no tool can ever run. The
/// panel's `min` does not survive into the generated type, and the record can
/// be hand-edited, so the bound is re-asserted here.
pub fn pool_config_from(settings: &crate::settings::SettingsRecord) -> PoolConfig {
    fn positive_ms(
        settings: &crate::settings::SettingsRecord,
        key: &str,
        fallback: Duration,
    ) -> Duration {
        let ms = num_setting(settings, key, fallback.as_millis() as f64);
        if ms > 0.0 {
            Duration::from_millis(ms as u64)
        } else {
            fallback
        }
    }
    let warm = num_setting(
        settings,
        "extensions.maxWarmWorkers",
        DEFAULT_POOL_CONFIG.max_warm_workers as f64,
    );
    PoolConfig {
        max_warm_workers: if warm >= 1.0 {
            warm as usize
        } else {
            DEFAULT_POOL_CONFIG.max_warm_workers
        },
        worker_idle: positive_ms(
            settings,
            "extensions.workerIdleMs",
            DEFAULT_POOL_CONFIG.worker_idle,
        ),
        call_timeout: positive_ms(
            settings,
            "extensions.callTimeoutMs",
            DEFAULT_POOL_CONFIG.call_timeout,
        ),
        drain_timeout: DEFAULT_POOL_CONFIG.drain_timeout,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolStats {
    pub warm_workers: usize,
    pub max_warm_workers: usize,
    pub in_flight_calls: usize,
}

impl WorkerPool {
    pub fn new(spawner: Arc<dyn WorkerSpawner>, config: PoolConfig) -> Self {
        Self {
            config: Arc::new(Mutex::new(config)),
            spawner,
            inner: Arc::new(Mutex::new(Inner {
                workers: BTreeMap::new(),
                lru: Vec::new(),
                idle_timers: BTreeMap::new(),
            })),
        }
    }

    pub fn config(&self) -> PoolConfig {
        *lock(&self.config)
    }

    /// Replace the tunables. Applies to the next call and the next idle timer;
    /// a call already running keeps the budget it started with, which is the
    /// only answer that does not move a deadline under a tool mid-execution.
    pub fn set_config(&self, config: PoolConfig) {
        *lock(&self.config) = config;
    }

    /// True when a settings change touches anything [`pool_config_from`] reads.
    pub fn watches(changed: &std::collections::BTreeSet<String>) -> bool {
        changed.iter().any(|key| POOL_KEYS.contains(&key.as_str()))
    }

    /// Apply the stored settings now, then on every change that touches
    /// [`POOL_KEYS`]. Owns its own subscription so boot carries no key list; a
    /// lagged receiver re-reads rather than breaking out, which would freeze
    /// the tunables for the life of the process.
    pub fn spawn_settings_watch(&self, settings: crate::settings::Settings) {
        let pool = self.clone();
        let mut changes = settings.subscribe();
        tokio::spawn(async move {
            pool.apply_settings(&settings).await;
            loop {
                match changes.recv().await {
                    Ok(change) => {
                        if Self::watches(&change.changed) {
                            pool.apply_settings(&settings).await;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(target: "loca::extensions", "settings feed lagged {n}");
                        pool.apply_settings(&settings).await;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    async fn apply_settings(&self, settings: &crate::settings::Settings) {
        match settings.load_resolved().await {
            Ok(record) => self.set_config(pool_config_from(&record)),
            Err(err) => {
                tracing::warn!(target: "loca::extensions", "pool settings unreadable: {err}");
            }
        }
    }

    /// Run one tool call, on a warm worker or on a fresh one. The pool owns the
    /// accounting and nothing else: [`super::call`] reports back through
    /// [`PoolCallbacks`], so `in_flight_calls` moves in exactly one place.
    pub async fn start_call(
        &self,
        spec: ToolCallStart,
        events: mpsc::UnboundedSender<CallEvent>,
        modules: Option<Arc<dyn ModuleDispatch>>,
    ) -> Result<CallController> {
        let key = WorkerKey::new(spec.extension_id.clone(), spec.tool_name.clone());
        let worker = self.get_or_spawn(&key).await?;
        let callbacks = Arc::new(Accounting {
            pool: self.clone(),
            key,
            worker: worker.clone(),
        });
        Ok(start_call(
            ulid::Ulid::new().to_string(),
            Arc::new(worker),
            spec,
            events,
            callbacks,
            modules,
            CallBudgets {
                call_timeout: self.config().call_timeout,
                drain_timeout: self.config().drain_timeout,
                boot_timeout: super::BOOT_TIMEOUT,
            },
        ))
    }

    /// The warm worker for `key`, spawning one if there is room.
    pub async fn get_or_spawn(&self, key: &WorkerKey) -> Result<WorkerHandle> {
        {
            let mut inner = lock(&self.inner);
            if let Some(worker) = inner.workers.get(key).cloned() {
                inner.touch_lru(key);
                return Ok(worker);
            }
            if inner.workers.len() >= self.config().max_warm_workers
                && !self.evict_least_recent(&mut inner)
            {
                // Exceeding the cap silently is how a runaway tool turns into
                // N processes.
                return Err(AppError::new(
                    ErrorCode::ServerBusy,
                    format!(
                        "all {} warm extension workers are busy; retry after a tool call drains",
                        self.config().max_warm_workers
                    ),
                ));
            }
        }

        // The lock is released across the spawn: a `std::Mutex` cannot be held
        // over an await, and holding one would block every other tool call.
        let worker = self.spawner.spawn(key).await?;

        let mut inner = lock(&self.inner);
        // Another call may have spawned the same key meanwhile. Keep the first
        // and discard ours rather than orphaning one.
        if let Some(existing) = inner.workers.get(key).cloned() {
            drop(inner);
            let drain = self.config().drain_timeout;
            tokio::spawn(async move { worker.terminate(drain).await });
            return Ok(existing);
        }
        inner.workers.insert(key.clone(), worker.clone());
        inner.touch_lru(key);
        Ok(worker)
    }

    /// The normal settle path: return the worker to the warm set, UNLESS a
    /// user-answered permission prompt happened during its lifetime. Deno
    /// caches the verdict for the PROCESS while the answer was scoped to ONE
    /// CALL, so reuse is an invisible privilege leak.
    pub fn retire_or_idle(&self, key: &WorkerKey, worker: &WorkerHandle) {
        if worker.prompt_answered_by_user() {
            tracing::info!(
                target: "loca::extensions",
                "retiring worker after a user-answered permission prompt ({})",
                key.extension_id,
            );
            self.kill_worker(key, worker);
            return;
        }
        self.bump_idle_timer(key);
    }

    /// Force-kill one worker and drop it from the pool, for a call that timed
    /// out or a cancel that went unacked.
    pub fn kill_worker(&self, key: &WorkerKey, worker: &WorkerHandle) {
        {
            let mut inner = lock(&self.inner);
            // The identity check stops a late settle from tearing down the
            // REPLACEMENT that has since taken this key.
            if inner.workers.get(key).map(WorkerHandle::id) == Some(worker.id()) {
                inner.remove(key);
            }
        }
        let worker = worker.clone();
        let drain = self.config().drain_timeout;
        tokio::spawn(async move { worker.terminate(drain).await });
    }

    /// Tear down every warm worker of one extension, after a grant change. The
    /// handles come out UNDER the lock and the terminates are awaited after it,
    /// or every concurrent tool call blocks for a drain timeout per worker.
    pub async fn refresh_permissions(&self, extension_id: &str) {
        let doomed: Vec<WorkerHandle> = {
            let mut inner = lock(&self.inner);
            let keys: Vec<WorkerKey> = inner
                .workers
                .keys()
                .filter(|k| k.extension_id == extension_id)
                .cloned()
                .collect();
            keys.iter().filter_map(|key| inner.remove(key)).collect()
        };
        if doomed.is_empty() {
            return;
        }
        tracing::info!(
            target: "loca::extensions",
            "refreshPermissions: terminating {} warm worker(s) for {extension_id}",
            doomed.len(),
        );
        let drain = self.config().drain_timeout;
        futures_util::future::join_all(doomed.iter().map(|w| w.terminate(drain))).await;
    }

    pub async fn shutdown(&self) {
        let doomed: Vec<WorkerHandle> = {
            let mut inner = lock(&self.inner);
            let keys: Vec<WorkerKey> = inner.workers.keys().cloned().collect();
            keys.iter().filter_map(|key| inner.remove(key)).collect()
        };
        let drain = self.config().drain_timeout;
        futures_util::future::join_all(doomed.iter().map(|w| w.terminate(drain))).await;
    }

    pub fn stats(&self) -> PoolStats {
        let inner = lock(&self.inner);
        PoolStats {
            warm_workers: inner.workers.len(),
            max_warm_workers: self.config().max_warm_workers,
            in_flight_calls: inner
                .workers
                .values()
                .map(WorkerHandle::in_flight_calls)
                .sum(),
        }
    }

    /// Evict the least recently used IDLE worker. In-flight workers are skipped
    /// WITHOUT disturbing LRU order. `false` means every warm worker is busy.
    fn evict_least_recent(&self, inner: &mut Inner) -> bool {
        let victim = inner
            .lru
            .iter()
            .find(|key| {
                inner
                    .workers
                    .get(*key)
                    .is_some_and(|w| w.in_flight_calls() == 0)
            })
            .cloned();
        let Some(key) = victim else {
            return false;
        };
        tracing::info!(target: "loca::extensions", "evicting idle worker for {}", key.extension_id);
        if let Some(worker) = inner.remove(&key) {
            let drain = self.config().drain_timeout;
            tokio::spawn(async move { worker.terminate(drain).await });
        }
        true
    }

    fn bump_idle_timer(&self, key: &WorkerKey) {
        let mut inner = lock(&self.inner);
        inner.clear_idle_timer(key);
        let Some(worker) = inner.workers.get(key) else {
            return;
        };
        if worker.in_flight_calls() > 0 {
            return;
        }
        let pool = self.clone();
        let key_for_task = key.clone();
        let idle = self.config().worker_idle;
        let task = tokio::spawn(async move {
            tokio::time::sleep(idle).await;
            let worker = {
                let mut inner = lock(&pool.inner);
                // A call may have arrived between the timer firing and this
                // task being polled.
                match inner.workers.get(&key_for_task) {
                    Some(w) if w.in_flight_calls() > 0 => None,
                    _ => inner.remove(&key_for_task),
                }
            };
            if let Some(worker) = worker {
                tracing::info!(
                    target: "loca::extensions",
                    "idle eviction for {}", key_for_task.extension_id,
                );
                worker.terminate(pool.config().drain_timeout).await;
            }
        });
        inner.idle_timers.insert(key.clone(), task);
    }

    pub fn clear_idle_timer(&self, key: &WorkerKey) {
        lock(&self.inner).clear_idle_timer(key);
    }

    /// The instant a worker was last active, for tests and for the status view.
    pub fn last_activity(&self, key: &WorkerKey) -> Option<Instant> {
        lock(&self.inner)
            .workers
            .get(key)
            .map(WorkerHandle::idle_since)
    }
}

/// The four lifecycle transitions, as warm-worker bookkeeping. It holds the
/// worker it was built for rather than looking it up by key, so a settle for an
/// already-replaced worker does not decrement the replacement's count and leave
/// the original stuck above zero, permanently exempt from eviction.
struct Accounting {
    pool: WorkerPool,
    key: WorkerKey,
    worker: WorkerHandle,
}

impl PoolCallbacks for Accounting {
    fn on_started(&self) {
        self.worker.call_started();
        // Or a timer armed while it was idle evicts it mid-call.
        self.pool.clear_idle_timer(&self.key);
    }

    fn on_settled(&self) {
        self.worker.call_settled();
        self.pool.retire_or_idle(&self.key, &self.worker);
    }

    fn on_killed(&self) {
        self.worker.call_settled();
        self.pool.kill_worker(&self.key, &self.worker);
    }

    fn on_boot_failed(&self) {
        self.worker.call_settled();
        // Idle rather than killed: the worker is already gone or about to be,
        // and the timer removes it without racing a replacement into its key.
        self.pool.bump_idle_timer(&self.key);
    }
}

impl Inner {
    fn touch_lru(&mut self, key: &WorkerKey) {
        self.lru.retain(|k| k != key);
        self.lru.push(key.clone());
    }

    /// Drop every trace of one worker and hand it back for termination.
    fn remove(&mut self, key: &WorkerKey) -> Option<WorkerHandle> {
        self.lru.retain(|k| k != key);
        self.clear_idle_timer(key);
        self.workers.remove(key)
    }

    fn clear_idle_timer(&mut self, key: &WorkerKey) {
        if let Some(task) = self.idle_timers.remove(key) {
            // Dropping a JoinHandle does not cancel the task, and a surviving
            // timer would evict a worker that has since taken another call.
            task.abort();
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::collections::BTreeMap as Map;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::sync::mpsc;

    use crate::extensions::worker::handle::Incoming;

    use super::*;

    /// A spawner that hands out handles over a channel nobody feeds, and counts
    /// how many it made. The count is what makes every "it reused the warm
    /// worker" assertion mean something.
    #[derive(Default)]
    struct CountingSpawner {
        spawned: AtomicUsize,
        /// Keeps the incoming channels alive, so a driver does not see EOF and
        /// shut down under the test, and lets a test speak AS the worker.
        keepalive: Mutex<Vec<(WorkerKey, mpsc::UnboundedSender<Incoming>)>>,
    }

    impl CountingSpawner {
        fn count(&self) -> usize {
            self.spawned.load(Ordering::SeqCst)
        }

        /// Say something as the worker for `key`: the LAST spawned one, since a
        /// later spawn under the same key is a replacement.
        fn say(&self, key: &WorkerKey, line: &str) {
            let held = lock(&self.keepalive);
            let sender = held
                .iter()
                .rev()
                .find(|(k, _)| k == key)
                .map(|(_, tx)| tx.clone())
                .expect("no worker was spawned for this key");
            drop(held);
            sender
                .send(Incoming::ProtocolLine(line.to_string()))
                .expect("the worker driver is gone");
        }
    }

    #[async_trait]
    impl WorkerSpawner for CountingSpawner {
        async fn spawn(&self, key: &WorkerKey) -> Result<WorkerHandle> {
            self.spawned.fetch_add(1, Ordering::SeqCst);
            let (tx, rx) = mpsc::unbounded_channel();
            lock(&self.keepalive).push((key.clone(), tx));
            Ok(WorkerHandle::start(
                key.extension_id.clone(),
                rx,
                None,
                None,
                Arc::new(Map::<String, String>::new()),
            ))
        }
    }

    fn pool(config: PoolConfig) -> (WorkerPool, Arc<CountingSpawner>) {
        let spawner = Arc::new(CountingSpawner::default());
        (WorkerPool::new(spawner.clone(), config), spawner)
    }

    fn key(extension: &str, tool: &str) -> WorkerKey {
        WorkerKey::new(extension, tool)
    }

    /// Let every spawned task run to a stopping point. A `sleep` rather than a
    /// `yield_now`, which polls once and may not finish the eviction task.
    async fn settle() {
        tokio::time::sleep(Duration::from_millis(1)).await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_worker_is_keyed_per_tool_so_a_sibling_grant_is_never_inherited() {
        // Keying per extension would run a benign tool in a process holding a
        // sibling's `run` or `ffi` flags.
        let (pool, spawner) = pool(DEFAULT_POOL_CONFIG);
        let a = pool.get_or_spawn(&key("demo", "read_file")).await.unwrap();
        let b = pool
            .get_or_spawn(&key("demo", "run_command"))
            .await
            .unwrap();
        assert_ne!(a.id(), b.id(), "two tools shared one worker process");
        assert_eq!(spawner.count(), 2);

        // And the same tool DOES reuse its warm worker, so the assertion above
        // is about the key rather than about caching being off.
        let again = pool.get_or_spawn(&key("demo", "read_file")).await.unwrap();
        assert_eq!(again.id(), a.id());
        assert_eq!(spawner.count(), 2, "a warm worker was not reused");
    }

    #[tokio::test(start_paused = true)]
    async fn the_pool_evicts_the_least_recently_used_idle_worker_at_the_cap() {
        let config = PoolConfig {
            max_warm_workers: 2,
            ..DEFAULT_POOL_CONFIG
        };
        let (pool, spawner) = pool(config);
        let first = pool.get_or_spawn(&key("demo", "a")).await.unwrap();
        pool.get_or_spawn(&key("demo", "b")).await.unwrap();
        // Touch `b` so `a` is the oldest.
        pool.get_or_spawn(&key("demo", "b")).await.unwrap();

        pool.get_or_spawn(&key("demo", "c")).await.unwrap();
        assert_eq!(pool.stats().warm_workers, 2, "the cap was exceeded");
        assert_eq!(spawner.count(), 3);

        // `a` was the victim, so asking for it again spawns a fourth.
        let revived = pool.get_or_spawn(&key("demo", "a")).await.unwrap();
        assert_ne!(revived.id(), first.id());
        assert_eq!(spawner.count(), 4);
    }

    #[tokio::test(start_paused = true)]
    async fn an_in_flight_worker_is_skipped_by_eviction_without_losing_its_place() {
        let config = PoolConfig {
            max_warm_workers: 2,
            ..DEFAULT_POOL_CONFIG
        };
        let (pool, _) = pool(config);
        let busy = pool.get_or_spawn(&key("demo", "busy")).await.unwrap();
        busy.call_started();
        let idle = pool.get_or_spawn(&key("demo", "idle")).await.unwrap();

        pool.get_or_spawn(&key("demo", "new")).await.unwrap();
        // The idle one went, not the busy one, even though the busy one is older.
        let still_busy = pool.get_or_spawn(&key("demo", "busy")).await.unwrap();
        assert_eq!(still_busy.id(), busy.id(), "a busy worker was evicted");
        let revived_idle = pool.get_or_spawn(&key("demo", "idle")).await.unwrap();
        assert_ne!(revived_idle.id(), idle.id());
    }

    #[tokio::test(start_paused = true)]
    async fn the_pool_refuses_a_spawn_rather_than_exceeding_the_cap() {
        let config = PoolConfig {
            max_warm_workers: 2,
            ..DEFAULT_POOL_CONFIG
        };
        let (pool, _) = pool(config);
        for tool in ["a", "b"] {
            pool.get_or_spawn(&key("demo", tool))
                .await
                .unwrap()
                .call_started();
        }
        let err = pool.get_or_spawn(&key("demo", "c")).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::ServerBusy);
        assert_eq!(pool.stats().warm_workers, 2);
        assert_eq!(pool.stats().in_flight_calls, 2);
    }

    #[tokio::test(start_paused = true)]
    async fn a_worker_that_prompted_the_user_is_retired_rather_than_reused() {
        let (pool, spawner) = pool(DEFAULT_POOL_CONFIG);
        let k = key("demo", "a");
        let worker = pool.get_or_spawn(&k).await.unwrap();
        worker.mark_prompt_answered_for_test();
        pool.retire_or_idle(&k, &worker);
        settle().await;

        let replacement = pool.get_or_spawn(&k).await.unwrap();
        assert_ne!(replacement.id(), worker.id(), "the worker was reused");
        assert_eq!(spawner.count(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn a_worker_that_only_auto_denied_is_returned_to_the_warm_set() {
        // Retiring on EVERY prompt would spawn a fresh process per call for any
        // tool whose policy denies something.
        let (pool, spawner) = pool(DEFAULT_POOL_CONFIG);
        let k = key("demo", "a");
        let worker = pool.get_or_spawn(&k).await.unwrap();
        pool.retire_or_idle(&k, &worker);
        let again = pool.get_or_spawn(&k).await.unwrap();
        assert_eq!(again.id(), worker.id());
        assert_eq!(spawner.count(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn an_idle_worker_is_evicted_after_the_idle_budget_and_not_before() {
        let (pool, _) = pool(DEFAULT_POOL_CONFIG);
        let k = key("demo", "a");
        let worker = pool.get_or_spawn(&k).await.unwrap();
        pool.retire_or_idle(&k, &worker);
        // Arm it before moving the clock: the timer is not registered until its
        // spawned task is first polled, and `advance` would pass straight over
        // a deadline that does not exist yet.
        settle().await;

        tokio::time::advance(DEFAULT_POOL_CONFIG.worker_idle - Duration::from_secs(1)).await;
        settle().await;
        assert_eq!(pool.stats().warm_workers, 1, "it was evicted early");

        tokio::time::advance(Duration::from_secs(2)).await;
        settle().await;
        assert_eq!(pool.stats().warm_workers, 0, "the idle timer never fired");
    }

    #[tokio::test(start_paused = true)]
    async fn a_new_call_cancels_the_idle_timer() {
        // The timer has to be ABORTED, not merely replaced: a second call at
        // T+150 arms T+450, but the ORIGINAL T+300 still fires, and by then the
        // worker is idle again so the re-check waves it through. Leaving the
        // call in flight hides that, since the re-check alone saves a BUSY one.
        let (pool, _) = pool(DEFAULT_POOL_CONFIG);
        let k = key("demo", "a");
        let worker = pool.get_or_spawn(&k).await.unwrap();
        pool.retire_or_idle(&k, &worker);
        settle().await;

        tokio::time::advance(DEFAULT_POOL_CONFIG.worker_idle / 2).await;
        let worker = pool.get_or_spawn(&k).await.unwrap();
        worker.call_started();
        pool.clear_idle_timer(&k);
        worker.call_settled();
        pool.retire_or_idle(&k, &worker);
        settle().await;

        // Past the FIRST deadline, still inside the second.
        tokio::time::advance(DEFAULT_POOL_CONFIG.worker_idle * 3 / 4).await;
        settle().await;
        assert_eq!(
            pool.stats().warm_workers,
            1,
            "a stale idle timer evicted a worker whose budget had been reset",
        );
    }

    #[tokio::test(start_paused = true)]
    async fn killing_a_worker_does_not_tear_down_its_replacement() {
        let (pool, _) = pool(DEFAULT_POOL_CONFIG);
        let k = key("demo", "a");
        let first = pool.get_or_spawn(&k).await.unwrap();
        pool.kill_worker(&k, &first);
        let replacement = pool.get_or_spawn(&k).await.unwrap();
        assert_ne!(replacement.id(), first.id());

        // The stale kill arrives now.
        pool.kill_worker(&k, &first);
        assert_eq!(pool.stats().warm_workers, 1, "the replacement was killed");
        let still_there = pool.get_or_spawn(&k).await.unwrap();
        assert_eq!(still_there.id(), replacement.id());
    }

    #[tokio::test(start_paused = true)]
    async fn refresh_permissions_takes_out_every_tool_of_one_extension_only() {
        let (pool, _) = pool(DEFAULT_POOL_CONFIG);
        for tool in ["a", "b"] {
            pool.get_or_spawn(&key("demo", tool)).await.unwrap();
        }
        let other = pool.get_or_spawn(&key("other", "a")).await.unwrap();
        assert_eq!(pool.stats().warm_workers, 3);

        pool.refresh_permissions("demo").await;
        assert_eq!(pool.stats().warm_workers, 1);
        let survivor = pool.get_or_spawn(&key("other", "a")).await.unwrap();
        assert_eq!(
            survivor.id(),
            other.id(),
            "an unrelated extension was torn down"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn refresh_permissions_does_not_hold_the_map_across_its_terminates() {
        // Holding the lock over the awaits blocks every concurrent tool call
        // for up to one drain timeout per worker.
        let (pool, _) = pool(DEFAULT_POOL_CONFIG);
        for tool in ["a", "b", "c"] {
            pool.get_or_spawn(&key("demo", tool)).await.unwrap();
        }
        let refresher = {
            let pool = pool.clone();
            tokio::spawn(async move { pool.refresh_permissions("demo").await })
        };
        let spawned = tokio::time::timeout(
            Duration::from_millis(50),
            pool.get_or_spawn(&key("other", "a")),
        )
        .await;
        assert!(spawned.is_ok(), "the map was held across the terminates");
        refresher.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_empties_the_pool() {
        let (pool, _) = pool(DEFAULT_POOL_CONFIG);
        for tool in ["a", "b"] {
            pool.get_or_spawn(&key("demo", tool)).await.unwrap();
        }
        pool.shutdown().await;
        assert_eq!(pool.stats().warm_workers, 0);
    }

    fn call_spec(extension: &str, tool: &str) -> ToolCallStart {
        ToolCallStart {
            extension_id: extension.to_string(),
            tool_name: tool.to_string(),
            fn_export: "run".to_string(),
            arguments_json: "{}".to_string(),
            chat_context: crate::extensions::worker::protocol::ChatContext {
                user_message: "do the thing".to_string(),
                session_id: None,
                locale: None,
            },
        }
    }

    /// Drive one call from boot to result, speaking as the worker.
    async fn run_one_call(pool: &WorkerPool, spawner: &CountingSpawner, tool: &str) {
        let key = key("demo", tool);
        let (events, _events_rx) = mpsc::unbounded_channel();
        let controller = pool
            .start_call(call_spec("demo", tool), events, None)
            .await
            .unwrap();
        settle().await;
        spawner.say(&key, r#"{"kind":"booted","extensionId":"demo"}"#);
        settle().await;
        assert_eq!(
            pool.stats().in_flight_calls,
            1,
            "the call was never accounted as in flight"
        );
        spawner.say(
            &key,
            &format!(
                r#"{{"kind":"tool_result","callId":"{}","result":{{"ok":true}}}}"#,
                controller.call_id
            ),
        );
        tokio::time::timeout(Duration::from_secs(5), controller.done())
            .await
            .expect("the call never settled")
            .expect("the call failed");
        settle().await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_settled_call_releases_its_worker_back_to_the_warm_set() {
        // The four callbacks are the only place `in_flight_calls` moves, and
        // `get_or_spawn` alone leaves it at zero whatever they do.
        let (pool, spawner) = pool(DEFAULT_POOL_CONFIG);
        run_one_call(&pool, &spawner, "t").await;

        assert_eq!(pool.stats().in_flight_calls, 0, "the settle never released");
        assert_eq!(pool.stats().warm_workers, 1, "the worker was not kept warm");

        // And the SAME process serves the next call, so the count above is
        // about accounting rather than a pool that discards everything.
        run_one_call(&pool, &spawner, "t").await;
        assert_eq!(spawner.count(), 1, "the warm worker was not reused");
    }

    #[tokio::test(start_paused = true)]
    async fn a_worker_that_answered_a_user_prompt_is_retired_when_its_call_settles() {
        let (pool, spawner) = pool(DEFAULT_POOL_CONFIG);
        let worker = pool.get_or_spawn(&key("demo", "t")).await.unwrap();
        worker.mark_prompt_answered_for_test();

        run_one_call(&pool, &spawner, "t").await;
        assert_eq!(
            pool.stats().warm_workers,
            0,
            "a worker holding a cached permission verdict stayed warm"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn two_calls_on_one_worker_settle_independently() {
        // A pool that minted one id for both would settle the wrong caller with
        // somebody else's result.
        let (pool, spawner) = pool(DEFAULT_POOL_CONFIG);
        let key = key("demo", "t");
        let (events_a, _rx_a) = mpsc::unbounded_channel();
        let (events_b, _rx_b) = mpsc::unbounded_channel();
        let first = pool
            .start_call(call_spec("demo", "t"), events_a, None)
            .await
            .unwrap();
        let second = pool
            .start_call(call_spec("demo", "t"), events_b, None)
            .await
            .unwrap();
        assert_ne!(first.call_id, second.call_id, "two calls shared one id");
        settle().await;
        spawner.say(&key, r#"{"kind":"booted","extensionId":"demo"}"#);
        settle().await;
        assert_eq!(pool.stats().in_flight_calls, 2);

        spawner.say(
            &key,
            &format!(
                r#"{{"kind":"tool_result","callId":"{}","result":"for the first"}}"#,
                first.call_id
            ),
        );
        let result = tokio::time::timeout(Duration::from_secs(5), first.done())
            .await
            .expect("the first call never settled")
            .expect("the first call failed");
        assert_eq!(result, serde_json::json!("for the first"));
        settle().await;
        assert_eq!(
            pool.stats().in_flight_calls,
            1,
            "the second call settled on the first call's result"
        );

        spawner.say(
            &key,
            &format!(
                r#"{{"kind":"tool_result","callId":"{}","result":"for the second"}}"#,
                second.call_id
            ),
        );
        let result = tokio::time::timeout(Duration::from_secs(5), second.done())
            .await
            .expect("the second call never settled")
            .expect("the second call failed");
        assert_eq!(result, serde_json::json!("for the second"));
    }

    #[tokio::test(start_paused = true)]
    async fn a_call_runs_on_the_pools_configured_budget_and_not_a_constant() {
        // Every other test uses the default config, so a `start_call` reaching
        // for the shipped constant would pass all of them.
        let config = PoolConfig {
            call_timeout: Duration::from_secs(3),
            ..DEFAULT_POOL_CONFIG
        };
        let (pool, spawner) = pool(config);
        let key = key("demo", "t");
        let (events, _events_rx) = mpsc::unbounded_channel();
        let controller = pool
            .start_call(call_spec("demo", "t"), events, None)
            .await
            .unwrap();
        settle().await;
        spawner.say(&key, r#"{"kind":"booted","extensionId":"demo"}"#);
        settle().await;

        // Just short of the budget, asserted BEFORE the await: an idle paused
        // runtime auto-advances to the next timer.
        tokio::time::advance(Duration::from_millis(2_900)).await;
        settle().await;
        assert_eq!(
            pool.stats().in_flight_calls,
            1,
            "the call ended before its budget"
        );

        tokio::time::advance(Duration::from_millis(200)).await;
        settle().await;
        let err = tokio::time::timeout(Duration::from_secs(5), controller.done())
            .await
            .expect("the timed-out call never settled")
            .expect_err("the call outlived its budget");
        assert!(err.message.contains("timed out"), "{}", err.message);
        assert_eq!(
            pool.stats().warm_workers,
            0,
            "a timed-out worker was kept warm"
        );
    }

    #[test]
    fn the_default_pool_budgets_are_what_the_specification_ships() {
        // Literals rather than derived, so a changed constant is visible here.
        assert_eq!(DEFAULT_POOL_CONFIG.max_warm_workers, 8);
        assert_eq!(DEFAULT_POOL_CONFIG.worker_idle, Duration::from_secs(300));
        assert_eq!(DEFAULT_POOL_CONFIG.call_timeout, Duration::from_secs(60));
        assert_eq!(DEFAULT_POOL_CONFIG.drain_timeout, Duration::from_secs(2));
    }

    fn record(v: serde_json::Value) -> crate::settings::SettingsRecord {
        match v {
            serde_json::Value::Object(map) => map,
            _ => unreachable!("a settings record is an object"),
        }
    }

    /// The settings reach the pool at all. The constants match the advertised
    /// defaults, so an unread key tells the truth until the user changes it.
    #[test]
    fn each_tunable_is_read_from_the_settings_record() {
        let cfg = pool_config_from(&record(serde_json::json!({
            "extensions.maxWarmWorkers": 3,
            "extensions.workerIdleMs": 45_000,
            "extensions.callTimeoutMs": 9_000,
        })));
        assert_eq!(cfg.max_warm_workers, 3);
        assert_eq!(cfg.worker_idle, Duration::from_millis(45_000));
        assert_eq!(cfg.call_timeout, Duration::from_millis(9_000));
        // Not a setting, so it must NOT move with them.
        assert_eq!(cfg.drain_timeout, DEFAULT_POOL_CONFIG.drain_timeout);
    }

    /// An empty record is the defaults, and a hostile one does not disable the
    /// subsystem: zero warm workers means no tool can ever run.
    #[test]
    fn absent_and_out_of_range_values_fall_back_to_the_defaults() {
        assert_eq!(
            pool_config_from(&record(serde_json::json!({}))),
            DEFAULT_POOL_CONFIG
        );
        let hostile = pool_config_from(&record(serde_json::json!({
            "extensions.maxWarmWorkers": 0,
            "extensions.workerIdleMs": 0,
            "extensions.callTimeoutMs": -1,
        })));
        assert_eq!(hostile, DEFAULT_POOL_CONFIG);
    }

    /// A watch set that omits a key its own reader uses means the user changes
    /// a setting and nothing happens.
    #[test]
    fn every_key_the_reader_uses_is_watched() {
        for key in POOL_KEYS {
            let mut one = std::collections::BTreeSet::new();
            one.insert((*key).to_string());
            assert!(WorkerPool::watches(&one), "{key} is watched");
            // And the reader actually reads it.
            let moved = pool_config_from(&record(serde_json::json!({ *key: 7 })));
            assert_ne!(
                moved, DEFAULT_POOL_CONFIG,
                "{key} is read by pool_config_from"
            );
        }
        let mut unrelated = std::collections::BTreeSet::new();
        unrelated.insert("llm.temperature".to_string());
        assert!(!WorkerPool::watches(&unrelated));
    }

    #[tokio::test]
    async fn set_config_replaces_what_the_pool_reads() {
        let pool = WorkerPool::new(Arc::new(CountingSpawner::default()), DEFAULT_POOL_CONFIG);
        assert_eq!(pool.config().max_warm_workers, 8);
        pool.set_config(pool_config_from(&record(
            serde_json::json!({ "extensions.maxWarmWorkers": 2 }),
        )));
        assert_eq!(pool.config().max_warm_workers, 2);
    }
}
