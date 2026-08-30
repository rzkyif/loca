//! The subsystem registry: what the aggregate core status pulls on recompute,
//! and the one place a child process is started from.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use crate::contract::api::{SidecarKind, SidecarSnapshot, SidecarStatus};
use crate::error::Result;
use crate::transport::status::{CoreStatusService, SidecarSource};

use super::jobctl::JobObject;
use super::metrics::Metrics;
use super::process::{self, Child, Spawn};

/// A started child, shared so the manager can terminate it at shutdown without
/// taking ownership away from the subsystem that drives it.
pub type ChildHandle = Arc<tokio::sync::Mutex<Child>>;

/// Cheap to clone; every clone shares one registry, one job object and one
/// metrics sampler.
#[derive(Clone)]
pub struct Manager {
    inner: Arc<Inner>,
}

struct Inner {
    /// A `BTreeMap`, and the ordering is load-bearing: `transport::status`
    /// compares the `subsystems` array positionally, so an unordered map would
    /// reorder on rehash and broadcast a `core.status` frame for nothing.
    /// `SidecarKind` derives `Ord` in declaration order, which is the order the
    /// client renders the rows in.
    states: Mutex<BTreeMap<SidecarKind, SidecarSnapshot>>,
    children: Mutex<Vec<Weak<tokio::sync::Mutex<Child>>>>,
    /// Set once by boot, after the aggregator is constructed, because
    /// `CoreStatusService::new` takes the `SidecarSource`. Never attaching is
    /// not fatal: the aggregator re-pulls `statuses()` on every pushed signal,
    /// so only a subsystem change alone stops triggering a recompute.
    status: OnceLock<CoreStatusService>,
    control: OnceLock<Arc<dyn SubsystemControl>>,
    jobs: JobObject,
    metrics: Metrics,
}

impl Default for Manager {
    fn default() -> Self {
        Self::new()
    }
}

impl Manager {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                states: Mutex::new(BTreeMap::new()),
                children: Mutex::new(Vec::new()),
                status: OnceLock::new(),
                control: OnceLock::new(),
                jobs: JobObject::new(),
                metrics: Metrics::new(),
            }),
        }
    }

    /// Wire the aggregator so a subsystem change recomputes the core status.
    /// Called once by boot; a second call is ignored.
    pub fn attach_status(&self, status: CoreStatusService) {
        let _ = self.inner.status.set(status);
    }

    /// Apply the current settings to a subsystem again: the `start` / `restart`
    /// route, and the client's Retry on a row stuck in `Error`.
    ///
    /// Set-once, and absent until boot wires it. An unwired or uncontrollable
    /// kind answers a validation error rather than a 204 that did nothing,
    /// because the caller is a human pressing Retry.
    pub fn attach_control(&self, control: Arc<dyn SubsystemControl>) -> bool {
        self.inner.control.set(control).is_ok()
    }

    /// Whether a control seam was wired. A read port for the boot-seam suite.
    pub fn has_control(&self) -> bool {
        self.inner.control.get().is_some()
    }

    pub async fn restart(&self, kind: SidecarKind) -> crate::error::Result<()> {
        match self.inner.control.get() {
            Some(control) => control.restart(kind).await,
            None => Err(crate::error::validation(
                "this core has no controllable subsystems",
            )),
        }
    }

    /// Report a subsystem's state. Called by whoever owns the subsystem: N2 for
    /// the language model, N3 for speech, D12 for tools.
    ///
    /// A snapshot equal to the stored one is dropped rather than recomputed, so
    /// a subsystem re-reporting on a timer costs nothing. The aggregator dedupes
    /// too, so the returned bool is what makes this guard assertable.
    pub fn publish(&self, snapshot: SidecarSnapshot) -> bool {
        {
            let mut states = lock(&self.inner.states);
            if states
                .get(&snapshot.kind)
                .is_some_and(|s| same(s, &snapshot))
            {
                return false;
            }
            states.insert(snapshot.kind, snapshot);
        }
        self.recompute();
        true
    }

    /// Drop a subsystem from the list entirely, which is different from
    /// publishing `Disabled`: a cleared kind stops appearing in `subsystems`,
    /// so the client stops rendering a row for it.
    pub fn clear(&self, kind: SidecarKind) {
        if lock(&self.inner.states).remove(&kind).is_none() {
            return;
        }
        self.recompute();
    }

    /// The stored snapshot, or `Disabled` for a kind nobody has published.
    pub fn status(&self, kind: SidecarKind) -> SidecarSnapshot {
        lock(&self.inner.states)
            .get(&kind)
            .cloned()
            .unwrap_or_else(|| disabled(kind))
    }

    /// Start a child, tracked for orphan cleanup and for shutdown.
    ///
    /// The only spawn entry point, so Windows Job Object membership cannot be
    /// forgotten at a call site and a killed core leaves no tool workers.
    pub fn spawn(&self, spec: Spawn) -> Result<ChildHandle> {
        let child = process::spawn(spec, &self.inner.jobs)?;
        let handle = Arc::new(tokio::sync::Mutex::new(child));
        let mut children = lock(&self.inner.children);
        children.retain(|c| c.strong_count() > 0);
        children.push(Arc::downgrade(&handle));
        Ok(handle)
    }

    /// Enrol a child this manager did NOT spawn, so a hard core kill still
    /// takes it down.
    ///
    /// One path cannot go through [`Self::spawn`]: a PTY-mode extension worker.
    /// On unix its stdin and stderr must BE the slave fd and it needs a
    /// `pre_exec` for `setsid` plus `TIOCSCTTY`; on Windows `portable-pty`
    /// performs the spawn itself. So it is enrolled after the fact through
    /// `AssignProcessToJobObject` instead.
    ///
    /// **This enrols the Job Object backstop only.** The child is not added to
    /// `children`, so [`Self::shutdown`] will not stop it gracefully: the
    /// caller owns that, holding the PTY handle. Use this in addition to your
    /// own shutdown path, never instead of it.
    ///
    /// A no-op off Windows: on unix the child has left this process group via
    /// `setsid`, so the backstop is the supervisor's own scope.
    pub fn track_external(&self, pid: u32, label: &str) {
        self.inner.jobs.track(pid, label);
    }

    /// Terminate every child still alive, before the listener closes. A child
    /// whose owner already dropped it is skipped: the drop backstop killed it,
    /// and its pid may have been reused.
    pub async fn shutdown(&self) {
        let live: Vec<ChildHandle> = {
            let mut children = lock(&self.inner.children);
            let live = children.iter().filter_map(Weak::upgrade).collect();
            children.clear();
            live
        };
        if live.is_empty() {
            return;
        }
        tracing::info!(target: "loca::sidecars", "stopping {} child process(es)", live.len());
        for handle in live {
            handle.lock().await.terminate().await;
        }
    }

    pub(super) fn metrics(&self) -> &Metrics {
        &self.inner.metrics
    }

    fn recompute(&self) {
        if let Some(status) = self.inner.status.get() {
            status.recompute();
        }
    }
}

impl std::fmt::Debug for Manager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Manager")
    }
}

impl SidecarSource for Manager {
    /// Published kinds only, in kind order: a kind nobody has published
    /// occupies no subsystem row.
    fn statuses(&self) -> Vec<SidecarSnapshot> {
        lock(&self.inner.states).values().cloned().collect()
    }
}

pub(super) fn disabled(kind: SidecarKind) -> SidecarSnapshot {
    SidecarSnapshot {
        kind,
        status: SidecarStatus::Disabled,
        message: None,
        progress: None,
        pid: None,
        cpu_pct: None,
        rss_mb: None,
    }
}

/// Field by field: the contract types derive no `PartialEq`.
fn same(a: &SidecarSnapshot, b: &SidecarSnapshot) -> bool {
    a.kind == b.kind
        && a.status == b.status
        && a.message == b.message
        && a.progress == b.progress
        && a.pid == b.pid
        && a.cpu_pct == b.cpu_pct
        && a.rss_mb == b.rss_mb
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Re-apply the current settings to an in-process subsystem. Lives here because
/// the routes do and neither `inference` nor `speech` may name the other; boot
/// supplies the one implementation.
#[async_trait::async_trait]
pub trait SubsystemControl: Send + Sync {
    async fn restart(&self, kind: SidecarKind) -> crate::error::Result<()>;
}
