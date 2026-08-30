//! The deferrable-work queue, and the summariser it drives.
//!
//! It lives in `chat` because its idle predicate is chat's: "no session has a
//! turn in flight". Background summaries and embedding refreshes compete with a
//! user-visible stream for the same model, so they run only while nothing is
//! streaming, plus a short quiet period after the last turn ended.
//!
//! Nothing is persisted: every job is re-derivable from source hashes at boot.
//!
//! The behaviour: dedupe by key, FIFO order, one job at a time, the idle gate,
//! the quiet period, the key freeing when a job STARTS rather than when it
//! finishes, and a failing job not stopping the drain.

use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};

use async_trait::async_trait;
use futures_util::future::BoxFuture;
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio::time::{Duration, Instant};

use crate::error::Result;
use crate::llm::{thinking_budget, Llm, LlmRoute, Overrides, SingleShotOptions};
use crate::memories::indexer::{SummaryGenerator, SummaryRequest};
use crate::settings::Settings;

/// How long after the last busy observation the queue waits before running.
const QUIET_PERIOD: Duration = Duration::from_secs(5);

/// How often to re-check while chat is busy.
const BUSY_POLL: Duration = Duration::from_secs(5);

/// Re-enqueue delay for a job that found a prerequisite missing, such as an
/// embedding model that is not downloaded yet. Long enough not to busy-spin
/// while the state persists, short enough to pick the work up once it clears.
const DEFERRED_RETRY: Duration = Duration::from_secs(60);

/// One unit of deferrable work.
pub struct BackgroundJob {
    /// Dedupe identity, such as `memory-index:<id>`. A key already waiting is
    /// not enqueued again.
    pub key: String,
    pub run: Box<dyn FnOnce() -> BoxFuture<'static, Result<()>> + Send>,
}

impl BackgroundJob {
    pub fn new<F>(key: impl Into<String>, run: F) -> Self
    where
        F: FnOnce() -> BoxFuture<'static, Result<()>> + Send + 'static,
    {
        Self {
            key: key.into(),
            run: Box::new(run),
        }
    }
}

/// Whether anything is in flight that background work must not compete with.
pub type BusyFn = Arc<dyn Fn() -> bool + Send + Sync>;

pub struct BackgroundQueue {
    inner: Arc<Inner>,
    drain: Mutex<Option<JoinHandle<()>>>,
}

struct Inner {
    state: Mutex<State>,
    notify: Notify,
    is_busy: BusyFn,
    quiet_period: Duration,
    busy_poll: Duration,
}

struct State {
    queue: VecDeque<BackgroundJob>,
    keys: HashSet<String>,
    disposed: bool,
    /// Seeded to "now" at construction, so boot counts as busy and the first
    /// job waits out one full quiet period rather than firing into a core that
    /// is still starting. `tokio::time::Instant`, not `std::time`: a paused
    /// test runtime advances only tokio's clock.
    last_busy_at: Instant,
    deferred: Vec<JoinHandle<()>>,
}

/// What the drain loop should do next, decided in one critical section.
enum Step {
    Stop,
    /// Nothing queued: sleep until something is.
    Wait,
    Sleep(Duration),
    Run(BackgroundJob),
}

impl BackgroundQueue {
    pub fn new(is_busy: BusyFn) -> Arc<Self> {
        Self::with_timings(is_busy, QUIET_PERIOD, BUSY_POLL)
    }

    /// The same, with the two delays supplied. A test shortens them so the gate
    /// is observable without a paused clock having to carry five real seconds.
    pub fn with_timings(is_busy: BusyFn, quiet_period: Duration, busy_poll: Duration) -> Arc<Self> {
        let inner = Arc::new(Inner {
            state: Mutex::new(State {
                queue: VecDeque::new(),
                keys: HashSet::new(),
                disposed: false,
                last_busy_at: Instant::now(),
                deferred: Vec::new(),
            }),
            notify: Notify::new(),
            is_busy,
            quiet_period,
            busy_poll,
        });
        let drain = tokio::spawn(drain(Arc::clone(&inner)));
        Arc::new(Self {
            inner,
            drain: Mutex::new(Some(drain)),
        })
    }

    /// Queue a job, unless its key is already waiting.
    pub fn enqueue(&self, job: BackgroundJob) {
        {
            let mut state = lock(&self.inner.state);
            if state.disposed || !state.keys.insert(job.key.clone()) {
                return;
            }
            state.queue.push_back(job);
        }
        self.inner.notify.notify_one();
    }

    /// Queue a job after [`DEFERRED_RETRY`]. For a job that ran, found a
    /// prerequisite missing, and wants another pass without busy-spinning the
    /// drain loop.
    pub fn enqueue_deferred(self: &Arc<Self>, job: BackgroundJob) {
        let queue = Arc::clone(self);
        let handle = tokio::spawn(async move {
            tokio::time::sleep(DEFERRED_RETRY).await;
            queue.enqueue(job);
        });
        let mut state = lock(&self.inner.state);
        if state.disposed {
            handle.abort();
            return;
        }
        // Finished timers accumulate otherwise: one entry per deferral for the
        // life of the core.
        state.deferred.retain(|h| !h.is_finished());
        state.deferred.push(handle);
    }

    /// Jobs waiting, not counting one currently running.
    pub fn len(&self) -> usize {
        lock(&self.inner.state).queue.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Stop the drain loop, drop everything queued, and cancel every deferral.
    pub fn dispose(&self) {
        {
            let mut state = lock(&self.inner.state);
            state.disposed = true;
            state.queue.clear();
            state.keys.clear();
            for handle in state.deferred.drain(..) {
                handle.abort();
            }
        }
        // The loop is parked on `notified()` or a sleep; the wake is what lets
        // it see `disposed` and return.
        self.inner.notify.notify_one();
        if let Some(handle) = lock(&self.drain).take() {
            handle.abort();
        }
    }
}

impl Drop for BackgroundQueue {
    fn drop(&mut self) {
        if let Some(handle) = lock(&self.drain).take() {
            handle.abort();
        }
    }
}

async fn drain(inner: Arc<Inner>) {
    loop {
        match next_step(&inner) {
            Step::Stop => return,
            Step::Wait => inner.notify.notified().await,
            Step::Sleep(delay) => tokio::time::sleep(delay).await,
            Step::Run(job) => {
                let key = job.key.clone();
                // A failing job is logged and the drain continues. One bad
                // summary must not stop every later one.
                if let Err(err) = (job.run)().await {
                    tracing::warn!(
                        target: "loca::bgqueue",
                        "background job {key} failed: {err}",
                    );
                }
            }
        }
    }
}

fn next_step(inner: &Inner) -> Step {
    {
        let state = lock(&inner.state);
        if state.disposed {
            return Step::Stop;
        }
        if state.queue.is_empty() {
            return Step::Wait;
        }
    }
    // Outside the state lock: the predicate reaches into the active-stream
    // registry, which is a different lock, and holding both would order them.
    let busy = (inner.is_busy)();
    let mut state = lock(&inner.state);
    if state.disposed {
        return Step::Stop;
    }
    if busy {
        state.last_busy_at = Instant::now();
        return Step::Sleep(inner.busy_poll);
    }
    let quiet_for = Instant::now().saturating_duration_since(state.last_busy_at);
    if quiet_for < inner.quiet_period {
        return Step::Sleep(inner.quiet_period - quiet_for);
    }
    match state.queue.pop_front() {
        // The key frees when the job STARTS, not when it finishes, so a
        // re-enqueue during the run (the source changed mid-job) queues a fresh
        // pass rather than being swallowed as a duplicate.
        Some(job) => {
            state.keys.remove(&job.key);
            Step::Run(job)
        }
        None => Step::Wait,
    }
}

/// The production [`SummaryGenerator`]: one non-streaming completion, outside
/// the scheduler. Resolves the default endpoint, reads the budget the request
/// names, and adds it to the answer's own allowance so reasoning cannot crowd
/// the summary out.
pub struct LlmSummaries {
    llm: Llm,
    settings: Settings,
}

impl LlmSummaries {
    pub fn new(llm: Llm, settings: Settings) -> Self {
        Self { llm, settings }
    }
}

#[async_trait]
impl SummaryGenerator for LlmSummaries {
    async fn generate(&self, request: SummaryRequest) -> Result<Option<String>> {
        // No client on a background pass, so the core record with the schema
        // defaults under it is the whole of what applies.
        let settings = self.settings.load_resolved().await?;
        let endpoint = self
            .llm
            .resolve_endpoint(&settings, LlmRoute::Default)
            .await?;
        let budget = thinking_budget(&settings, request.thinking_budget_key);
        let text = self
            .llm
            .single_shot(SingleShotOptions {
                system_prompt: request.system_prompt,
                user_message: request.user_message,
                endpoint,
                overrides: Overrides {
                    temperature: Some(request.temperature),
                    max_tokens: Some(request.max_tokens + budget),
                    reasoning_budget: Some(budget),
                    ..Overrides::default()
                },
            })
            .await
            .map_err(|err| crate::llm::classify_provider_error(&err))?;
        let text = text.trim();
        Ok((!text.is_empty()).then(|| text.to_string()))
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}
