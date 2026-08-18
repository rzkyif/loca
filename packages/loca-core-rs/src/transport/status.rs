//! The aggregate core lifecycle status: starting up, downloading, idle, busy,
//! updating, error.
//!
//! One read-only aggregator with two consumers, both in this package: the
//! health route seeds a connecting client from it, and the WS hub broadcasts it
//! as a `core.status` frame on every edge.
//!
//! Inputs arrive two ways:
//!
//! - **Pulled** on each recompute: sidecar readiness, through
//!   [`SidecarSource`].
//! - **Pushed** by the owning subsystem: the chat turn count, the LLM queue,
//!   the speech queue, boot completion, download activity, and update staging.
//!
//! Push rather than pull for the dynamic signals is what keeps this module
//! dependency-free: it never imports chat or the schedulers, they call a
//! `note_*` setter on it. That is also why it can live in the transport without
//! dragging the transport into domain territory (D-014). The `note_*` setters
//! are a shared surface: a domain package calls them, and adding one is a
//! request rather than an edit.
//!
//! Emission is edge-only. A snapshot equal to the last one is not broadcast, so
//! per-chunk download progress and transient sub-state churn do not spam every
//! connected client with frames.

use std::sync::{Arc, Mutex};

use tokio::sync::broadcast;

use crate::contract::api::{
    CoreQueues, CoreStatus, CoreStatusSnapshot, DownloadEntry, DownloadStatus, SidecarSnapshot,
    SidecarStatus, SubsystemStatus,
};

use super::state::StatusProvider;

/// Where sidecar readiness is read from on each recompute.
///
/// A seam so the derivation is testable without spawning processes, and so the
/// sidecar package can supply the live manager without this module importing
/// it.
pub trait SidecarSource: Send + Sync {
    fn statuses(&self) -> Vec<SidecarSnapshot>;
}

/// A source that reports no sidecars, for a frontend with none wired yet.
pub struct NoSidecars;

impl SidecarSource for NoSidecars {
    fn statuses(&self) -> Vec<SidecarSnapshot> {
        Vec::new()
    }
}

/// How many status edges a slow subscriber may fall behind before it starts
/// missing them. A lagging subscriber resyncs from [`CoreStatusService::snapshot`];
/// it never stalls a recompute.
const CHANGE_CHANNEL_CAPACITY: usize = 16;

#[derive(Debug)]
struct Signals {
    /// True from process start until the frontend reports the listener bound
    /// and sidecar boot kicked. While set, the core cannot serve yet.
    booting: bool,
    /// Set when a self-update is staged and a restart is imminent.
    updating: bool,
    /// In-flight chat turns: generating, or awaiting or executing a tool.
    active_streams: u32,
    llm_active: u32,
    llm_queued: u32,
    speech_active: u32,
    speech_queued: u32,
    /// Pending or downloading transfers. While files are still being fetched
    /// the sidecars that need them sit at Disabled, so without this the bar
    /// would read Idle mid-fetch.
    downloads_active: u32,
    downloads_progress: Option<f64>,
    last: CoreStatusSnapshot,
}

/// The aggregator. Cheap to clone; every clone shares one state and one
/// broadcast.
#[derive(Clone)]
pub struct CoreStatusService {
    inner: Arc<Inner>,
}

struct Inner {
    signals: Mutex<Signals>,
    sidecars: Arc<dyn SidecarSource>,
    changes: broadcast::Sender<CoreStatusSnapshot>,
}

impl std::fmt::Debug for CoreStatusService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CoreStatusService")
    }
}

impl CoreStatusService {
    pub fn new(sidecars: Arc<dyn SidecarSource>) -> Self {
        let (changes, _) = broadcast::channel(CHANGE_CHANNEL_CAPACITY);
        Self {
            inner: Arc::new(Inner {
                signals: Mutex::new(Signals {
                    booting: true,
                    updating: false,
                    active_streams: 0,
                    llm_active: 0,
                    llm_queued: 0,
                    speech_active: 0,
                    speech_queued: 0,
                    downloads_active: 0,
                    downloads_progress: None,
                    last: CoreStatusSnapshot {
                        status: CoreStatus::StartingUp,
                        detail: None,
                        progress: None,
                        queues: None,
                        subsystems: Vec::new(),
                    },
                }),
                sidecars,
                changes,
            }),
        }
    }

    /// The current aggregate.
    pub fn snapshot(&self) -> CoreStatusSnapshot {
        lock(&self.inner.signals).last.clone()
    }

    /// Every edge, for the hub to broadcast.
    pub fn subscribe(&self) -> broadcast::Receiver<CoreStatusSnapshot> {
        self.inner.changes.subscribe()
    }

    // --- pushed signals ---------------------------------------------------

    /// Boot is past the point where the core can serve.
    pub fn note_boot_done(&self) {
        self.update(|s| {
            if !s.booting {
                return false;
            }
            s.booting = false;
            true
        });
    }

    /// A build is staged and a restart is imminent.
    pub fn note_update_staged(&self) {
        self.update(|s| {
            if s.updating {
                return false;
            }
            s.updating = true;
            true
        });
    }

    /// In-flight chat turns.
    pub fn note_active_streams(&self, n: u32) {
        self.update(|s| {
            if s.active_streams == n {
                return false;
            }
            s.active_streams = n;
            true
        });
    }

    pub fn note_llm_queue(&self, active: u32, queued: u32) {
        self.update(|s| {
            if s.llm_active == active && s.llm_queued == queued {
                return false;
            }
            s.llm_active = active;
            s.llm_queued = queued;
            true
        });
    }

    pub fn note_speech_queue(&self, active: u32, queued: u32) {
        self.update(|s| {
            if s.speech_active == active && s.speech_queued == queued {
                return false;
            }
            s.speech_active = active;
            s.speech_queued = queued;
            true
        });
    }

    /// Fold a download-manager snapshot in.
    ///
    /// Progress is rounded to whole percent so per-chunk byte updates do not
    /// spam status frames, and only size-known entries count toward it: an
    /// unsized entry would contribute downloaded bytes with no total and push
    /// the ratio past 1.
    pub fn note_downloads(&self, entries: &[DownloadEntry]) {
        let active: Vec<&DownloadEntry> = entries
            .iter()
            .filter(|e| {
                matches!(
                    e.status,
                    DownloadStatus::Pending | DownloadStatus::Downloading
                )
            })
            .collect();
        let sized: Vec<&&DownloadEntry> = active
            .iter()
            .filter(|e| e.size_bytes.is_some_and(|b| b > 0))
            .collect();
        let total: i64 = sized.iter().map(|e| e.size_bytes.unwrap_or(0)).sum();
        let done: i64 = sized
            .iter()
            .map(|e| e.downloaded_bytes.min(e.size_bytes.unwrap_or(0)))
            .sum();
        let progress = if total > 0 {
            Some((done as f64 / total as f64 * 100.0).round() / 100.0)
        } else {
            None
        };
        let count = active.len() as u32;
        self.update(|s| {
            if s.downloads_active == count && s.downloads_progress == progress {
                return false;
            }
            s.downloads_active = count;
            s.downloads_progress = progress;
            true
        });
    }

    /// Re-derive from the pulled inputs. Called by the sidecar manager whenever
    /// a sidecar changes state.
    pub fn recompute(&self) {
        self.update(|_| true);
    }

    /// Apply a signal change, then re-derive and broadcast if the aggregate
    /// actually moved.
    fn update(&self, apply: impl FnOnce(&mut Signals) -> bool) {
        let sidecars = self.inner.sidecars.statuses();
        let next = {
            let mut signals = lock(&self.inner.signals);
            if !apply(&mut signals) {
                return;
            }
            let next = derive(&signals, &sidecars);
            if same(&next, &signals.last) {
                return;
            }
            signals.last = next.clone();
            next
        };
        // Sent outside the lock: a subscriber that reacts by reading the
        // snapshot would otherwise deadlock against the lock we still hold.
        let _ = self.inner.changes.send(next);
    }
}

impl StatusProvider for CoreStatusService {
    fn snapshot(&self) -> CoreStatusSnapshot {
        CoreStatusService::snapshot(self)
    }
}

/// The precedence order is the whole behaviour of this module, so it is one
/// function and reads top to bottom.
fn derive(s: &Signals, sidecars: &[SidecarSnapshot]) -> CoreStatusSnapshot {
    // The per-subsystem breakdown rides every snapshot: it feeds the client's
    // per-sidecar view and the expanded error card. Order follows the source,
    // which is stable, so the list is compared positionally.
    let subsystems: Vec<SubsystemStatus> = sidecars
        .iter()
        .map(|s| SubsystemStatus {
            kind: s.kind,
            status: s.status,
            message: s.message.clone(),
        })
        .collect();

    // 1. Error: a sidecar the manager actually started hit a terminal Error.
    //    A gated-off sidecar sits at Disabled and never reaches here. The
    //    collapsed detail names the first; the card lists them all.
    if let Some(errored) = sidecars.iter().find(|s| s.status == SidecarStatus::Error) {
        return CoreStatusSnapshot {
            status: CoreStatus::Error,
            detail: Some(
                errored
                    .message
                    .clone()
                    .unwrap_or_else(|| format!("{} failed", errored.kind)),
            ),
            progress: None,
            queues: None,
            subsystems,
        };
    }

    // 2. Updating: a build is staged, a restart is imminent.
    if s.updating {
        return CoreStatusSnapshot {
            status: CoreStatus::Updating,
            detail: None,
            progress: None,
            queues: None,
            subsystems,
        };
    }

    // 3. Starting up: still booting, or a sidecar is loading its model.
    let loading = sidecars.iter().find(|s| s.status == SidecarStatus::Loading);
    if s.booting || loading.is_some() {
        return CoreStatusSnapshot {
            status: CoreStatus::StartingUp,
            detail: loading.map(|l| format!("loading {}", l.kind)),
            progress: loading.and_then(|l| l.progress),
            queues: None,
            subsystems,
        };
    }

    // 4. Busy: a turn is generating, or speech / LLM work is in flight or
    //    queued. The counts ride along so the expanded card can show what is
    //    working and what is waiting, per queue.
    let queued = s.llm_queued + s.speech_queued;
    if s.active_streams > 0 || s.llm_active > 0 || s.speech_active > 0 || queued > 0 {
        return CoreStatusSnapshot {
            status: CoreStatus::Busy,
            detail: (queued > 0).then(|| format!("{queued} queued")),
            progress: None,
            queues: Some(CoreQueues {
                llm_active: s.llm_active as i64,
                llm_queued: s.llm_queued as i64,
                speech_active: s.speech_active as i64,
                speech_queued: s.speech_queued as i64,
                active_streams: s.active_streams as i64,
            }),
            subsystems,
        };
    }

    // 5. Downloading. Below busy on purpose, so live turn activity (an external
    //    provider serving chat while a local model downloads) still shows.
    if s.downloads_active > 0 {
        return CoreStatusSnapshot {
            status: CoreStatus::Downloading,
            detail: Some(if s.downloads_active == 1 {
                "1 file".to_string()
            } else {
                format!("{} files", s.downloads_active)
            }),
            progress: s.downloads_progress,
            queues: None,
            subsystems,
        };
    }

    // 6. Idle.
    CoreStatusSnapshot {
        status: CoreStatus::Idle,
        detail: None,
        progress: None,
        queues: None,
        subsystems,
    }
}

/// Field-by-field equality. The generated contract types derive neither
/// `PartialEq` nor `Eq`, and comparing serialized forms would allocate on every
/// signal change.
fn same(a: &CoreStatusSnapshot, b: &CoreStatusSnapshot) -> bool {
    a.status == b.status
        && a.detail == b.detail
        && a.progress == b.progress
        && same_queues(a.queues.as_ref(), b.queues.as_ref())
        && a.subsystems.len() == b.subsystems.len()
        && a.subsystems
            .iter()
            .zip(&b.subsystems)
            .all(|(x, y)| x.kind == y.kind && x.status == y.status && x.message == y.message)
}

fn same_queues(a: Option<&CoreQueues>, b: Option<&CoreQueues>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => {
            a.llm_active == b.llm_active
                && a.llm_queued == b.llm_queued
                && a.speech_active == b.speech_active
                && a.speech_queued == b.speech_queued
                && a.active_streams == b.active_streams
        }
        _ => false,
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::contract::api::{DownloadEntryDestination, SidecarKind};

    #[derive(Default)]
    struct Sidecars(Mutex<Vec<SidecarSnapshot>>);

    impl SidecarSource for Sidecars {
        fn statuses(&self) -> Vec<SidecarSnapshot> {
            lock(&self.0).clone()
        }
    }

    fn sidecar(kind: SidecarKind, status: SidecarStatus) -> SidecarSnapshot {
        SidecarSnapshot {
            kind,
            status,
            message: None,
            progress: None,
            pid: None,
            cpu_pct: None,
            rss_mb: None,
        }
    }

    fn service() -> (CoreStatusService, Arc<Sidecars>) {
        let sidecars = Arc::new(Sidecars::default());
        (
            CoreStatusService::new(Arc::clone(&sidecars) as Arc<dyn SidecarSource>),
            sidecars,
        )
    }

    fn download(status: DownloadStatus, size: Option<i64>, done: i64) -> DownloadEntry {
        DownloadEntry {
            id: "d".into(),
            abs_path: "/tmp/model.gguf".into(),
            added_at_ms: 0,
            destination: DownloadEntryDestination::Models,
            downloaded_bytes: done,
            error: None,
            filename: "model.gguf".into(),
            group_id: "g".into(),
            rel_path: "model.gguf".into(),
            size_bytes: size,
            source: "https://example.test/model.gguf".into(),
            status,
        }
    }

    #[test]
    fn a_fresh_core_is_starting_up_until_boot_completes() {
        let (svc, _) = service();
        assert_eq!(svc.snapshot().status, CoreStatus::StartingUp);
        svc.note_boot_done();
        assert_eq!(svc.snapshot().status, CoreStatus::Idle);
    }

    #[test]
    fn precedence_runs_error_updating_starting_busy_downloading_idle() {
        let (svc, sidecars) = service();
        svc.note_boot_done();
        svc.note_downloads(&[download(DownloadStatus::Downloading, Some(100), 50)]);
        assert_eq!(svc.snapshot().status, CoreStatus::Downloading);

        // Busy outranks downloading, so a turn served by an external provider
        // during a model download still reads as busy.
        svc.note_active_streams(1);
        assert_eq!(svc.snapshot().status, CoreStatus::Busy);

        // A loading sidecar outranks busy.
        *lock(&sidecars.0) = vec![sidecar(SidecarKind::Llama, SidecarStatus::Loading)];
        svc.recompute();
        let snap = svc.snapshot();
        assert_eq!(snap.status, CoreStatus::StartingUp);
        assert_eq!(snap.detail.as_deref(), Some("loading llama"));

        // Updating outranks starting up.
        svc.note_update_staged();
        assert_eq!(svc.snapshot().status, CoreStatus::Updating);

        // An errored sidecar outranks everything.
        *lock(&sidecars.0) = vec![SidecarSnapshot {
            message: Some("port already bound".into()),
            ..sidecar(SidecarKind::Llama, SidecarStatus::Error)
        }];
        svc.recompute();
        let snap = svc.snapshot();
        assert_eq!(snap.status, CoreStatus::Error);
        assert_eq!(snap.detail.as_deref(), Some("port already bound"));
    }

    #[test]
    fn an_errored_sidecar_with_no_message_still_names_itself() {
        let (svc, sidecars) = service();
        svc.note_boot_done();
        *lock(&sidecars.0) = vec![sidecar(SidecarKind::Speech, SidecarStatus::Error)];
        svc.recompute();
        assert_eq!(svc.snapshot().detail.as_deref(), Some("speech failed"));
    }

    #[test]
    fn busy_carries_the_queue_counts_and_the_queued_detail() {
        let (svc, _) = service();
        svc.note_boot_done();
        svc.note_llm_queue(1, 2);
        svc.note_speech_queue(0, 1);
        let snap = svc.snapshot();
        assert_eq!(snap.status, CoreStatus::Busy);
        assert_eq!(snap.detail.as_deref(), Some("3 queued"));
        let q = snap.queues.unwrap();
        assert_eq!((q.llm_active, q.llm_queued), (1, 2));
        assert_eq!((q.speech_active, q.speech_queued), (0, 1));

        // Work in flight with nothing waiting is busy with no detail.
        svc.note_llm_queue(1, 0);
        svc.note_speech_queue(0, 0);
        assert_eq!(svc.snapshot().detail, None);
    }

    #[test]
    fn download_progress_is_rounded_and_ignores_unsized_entries() {
        let (svc, _) = service();
        svc.note_boot_done();
        svc.note_downloads(&[
            download(DownloadStatus::Downloading, Some(3), 1),
            // No size: it would otherwise add bytes with no total and push the
            // ratio past 1.
            download(DownloadStatus::Downloading, None, 900),
            // Not active: excluded from both the count and the ratio.
            download(DownloadStatus::Completed, Some(10), 10),
        ]);
        let snap = svc.snapshot();
        assert_eq!(snap.status, CoreStatus::Downloading);
        assert_eq!(snap.detail.as_deref(), Some("2 files"));
        assert_eq!(snap.progress, Some(0.33));
    }

    #[test]
    fn a_single_active_download_reads_as_one_file() {
        let (svc, _) = service();
        svc.note_boot_done();
        svc.note_downloads(&[download(DownloadStatus::Pending, None, 0)]);
        let snap = svc.snapshot();
        assert_eq!(snap.detail.as_deref(), Some("1 file"));
        assert_eq!(snap.progress, None, "no sized entry means no ratio");
    }

    #[tokio::test]
    async fn only_edges_are_broadcast() {
        let (svc, _) = service();
        let mut rx = svc.subscribe();
        svc.note_boot_done();
        assert_eq!(rx.recv().await.unwrap().status, CoreStatus::Idle);

        // Same value again: no signal change, so nothing is sent.
        svc.note_active_streams(0);
        // A real change is.
        svc.note_active_streams(1);
        assert_eq!(rx.recv().await.unwrap().status, CoreStatus::Busy);

        // A signal that moves without changing the aggregate is not an edge:
        // per-chunk download bytes while busy would otherwise spam every
        // connected client.
        svc.note_downloads(&[download(DownloadStatus::Downloading, Some(100), 1)]);
        svc.note_downloads(&[download(DownloadStatus::Downloading, Some(100), 2)]);
        svc.note_active_streams(0);
        let next = rx.recv().await.unwrap();
        assert_eq!(next.status, CoreStatus::Downloading);
        assert!(rx.try_recv().is_err(), "extra frames were broadcast");
    }

    #[test]
    fn the_subsystem_breakdown_rides_every_snapshot() {
        let (svc, sidecars) = service();
        svc.note_boot_done();
        *lock(&sidecars.0) = vec![
            sidecar(SidecarKind::Llama, SidecarStatus::Running),
            sidecar(SidecarKind::Speech, SidecarStatus::Disabled),
        ];
        svc.recompute();
        let snap = svc.snapshot();
        assert_eq!(snap.status, CoreStatus::Idle);
        assert_eq!(snap.subsystems.len(), 2);
        assert_eq!(snap.subsystems[0].kind, SidecarKind::Llama);
        assert_eq!(snap.subsystems[1].status, SidecarStatus::Disabled);
    }
}
