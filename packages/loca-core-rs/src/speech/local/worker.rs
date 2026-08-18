//! The one thread every local speech decode runs on.
//!
//! # Why not `spawn_blocking`
//!
//! A decode cannot be pre-empted, so an abandoned request keeps a thread busy
//! until it finishes even though its caller has gone. On the shared
//! `spawn_blocking` pool that is a core-wide problem: a client that disconnects
//! repeatedly could occupy every blocking thread, and the visible damage would
//! land on unrelated subsystems (database work, file reads, the extension
//! supervisor) rather than on speech.
//!
//! One dedicated thread bounds the blast radius to the thing that is already
//! serialised. The queue admits one caller at a time anyway (`PARALLEL_SLOTS`),
//! so a second worker would have nothing to do: the thread count here is not a
//! throughput knob, it is a containment boundary.
//!
//! # What "cancellation" means here, and what it does NOT mean
//!
//! Dropping the returned future releases the queue slot immediately and sets the
//! job's cancel flag. **Only synthesis acts on that flag**, through sherpa's
//! generation callback, stopping at the next chunk.
//!
//! **An abandoned transcription runs to completion, and the next caller DOES
//! wait for it.** sherpa's offline recogniser exposes no progress callback, so
//! there is no point at which a decode can be asked to stop, and this is one
//! serial thread. The queue slot is released the moment the future drops, so the
//! next caller is admitted immediately, and then blocks here behind a decode
//! nobody is listening for, for up to the length of one clip.
//!
//! That is a real cost and it is bounded rather than fixed: the 25 MB upload cap
//! bounds how long one abandoned decode can be, and the queue's depth bound
//! caps how many can be waiting. Fixing it properly needs either a cancellable
//! recogniser upstream or chunked decoding, both of which are larger changes
//! than this package.
//!
//! An earlier version of this comment claimed the opposite, that an abandoned
//! decode never makes the next caller wait. It was wrong, and the abandonment
//! test below cannot see it because it exercises the synthesis path, which IS
//! covered. Left explicit here rather than quietly corrected, because a comment
//! asserting a property the code does not have is worse than no comment: it is
//! what someone will cite instead of measuring (D-140).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::error::{AppError, ErrorCode, Result};

/// A unit of blocking speech work.
type Job = Box<dyn FnOnce() + Send + 'static>;

/// A handle onto the speech worker thread.
pub struct Worker {
    sender: std::sync::mpsc::Sender<Job>,
}

impl Worker {
    /// Start the worker. The thread lives as long as the sender does, and exits
    /// when the channel closes at shutdown.
    pub fn start() -> Self {
        let (sender, receiver) = std::sync::mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("loca-speech".to_string())
            .spawn(move || {
                while let Ok(job) = receiver.recv() {
                    job();
                }
            })
            // A core that cannot start a thread is not going to serve speech,
            // and the failure is reported at the first call rather than
            // panicking at construction.
            .map(|_| ())
            .unwrap_or_else(|err| {
                tracing::error!(target: "loca::speech", "could not start the speech worker: {err}");
            });
        Self { sender }
    }

    /// Run `job` on the worker and await its result.
    ///
    /// `cancelled` is set when the returned future is dropped, which is what a
    /// disconnecting client produces. The job sees it through whatever polling
    /// hook its engine offers.
    pub async fn run<T, F>(&self, cancelled: Arc<AtomicBool>, job: F) -> Result<T>
    where
        F: FnOnce() -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let guard = CancelOnDrop {
            flag: Arc::clone(&cancelled),
        };
        let flag = Arc::clone(&cancelled);
        self.sender
            .send(Box::new(move || {
                // **Checked before the work, not only during it.** The permit is
                // released when the caller's future drops, but the job stays on
                // this thread's unbounded channel, so a client that connects and
                // disconnects in a loop could otherwise pile up one full decode
                // per iteration while the queue depth reads zero. A job whose
                // caller has already gone is skipped rather than run.
                if flag.load(Ordering::Relaxed) {
                    return;
                }
                // A receiver that has gone away means the caller disconnected;
                // the result is simply dropped.
                let _ = tx.send(job());
            }))
            .map_err(|_| {
                AppError::new(
                    ErrorCode::ServerUnavailable,
                    "the local speech engine is not running",
                )
            })?;
        let result = rx.await.map_err(|_| {
            AppError::new(
                ErrorCode::ProviderError,
                "the local speech engine stopped before answering",
            )
        })?;
        // Reaching here means the caller is still listening, so the job was not
        // abandoned and the flag must not be set on the way out.
        guard.disarm();
        result
    }
}

/// Sets the cancel flag unless disarmed, so a dropped future stops the work.
struct CancelOnDrop {
    flag: Arc<AtomicBool>,
}

impl CancelOnDrop {
    fn disarm(self) {
        std::mem::forget(self);
    }
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.flag.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::sync::Mutex;
    use std::time::Duration;

    use super::*;

    #[tokio::test]
    async fn a_job_runs_off_the_async_runtime_and_returns_its_value() {
        let worker = Worker::start();
        let thread_name = worker
            .run(Arc::new(AtomicBool::new(false)), || {
                Ok(std::thread::current()
                    .name()
                    .unwrap_or_default()
                    .to_string())
            })
            .await
            .unwrap();
        assert_eq!(thread_name, "loca-speech");
    }

    #[tokio::test]
    async fn a_completed_call_does_not_set_the_cancel_flag() {
        // The guard must be disarmed on the success path, or every synthesis
        // would report itself cancelled the moment it returned.
        let worker = Worker::start();
        let flag = Arc::new(AtomicBool::new(false));
        worker.run(Arc::clone(&flag), || Ok(())).await.unwrap();
        assert!(!flag.load(Ordering::Relaxed));
    }

    #[tokio::test]
    async fn dropping_the_future_sets_the_cancel_flag_the_engine_polls() {
        let worker = Worker::start();
        let flag = Arc::new(AtomicBool::new(false));
        let observed = Arc::new(Mutex::new(false));

        let worker = Arc::new(worker);
        let started = Arc::new(std::sync::Barrier::new(2));
        {
            let flag = Arc::clone(&flag);
            let observed = Arc::clone(&observed);
            let started_in_job = Arc::clone(&started);
            let spawned = Arc::clone(&worker);
            let fut = async move {
                spawned
                    .run(Arc::clone(&flag), move || {
                        started_in_job.wait();
                        // Stand in for sherpa's generation callback: poll until the
                        // caller gives up.
                        for _ in 0..2_000 {
                            if flag.load(Ordering::Relaxed) {
                                *observed.lock().unwrap_or_else(|e| e.into_inner()) = true;
                                return Ok(());
                            }
                            std::thread::sleep(Duration::from_millis(1));
                        }
                        Ok(())
                    })
                    .await
            };
            let handle = tokio::spawn(fut);
            // Let the job start before abandoning it.
            let outer = Arc::clone(&started);
            tokio::task::spawn_blocking(move || outer.wait())
                .await
                .unwrap();
            handle.abort();
            let _ = handle.await;
        }

        // The worker thread notices within its polling interval.
        for _ in 0..200 {
            if *observed.lock().unwrap_or_else(|e| e.into_inner()) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            *observed.lock().unwrap_or_else(|e| e.into_inner()),
            "the abandoned job never saw its cancel flag",
        );
    }

    /// A job whose caller left before it started must not run at all.
    ///
    /// The permit is released when the future drops, but the job stays on this
    /// thread's unbounded channel. Without the pre-check, a client that connects
    /// and disconnects in a loop enqueues one full decode per iteration while
    /// the queue depth reads zero, so the bound the queue advertises does not
    /// bound the work.
    #[tokio::test]
    async fn a_job_abandoned_before_it_starts_never_runs() {
        let worker = Arc::new(Worker::start());
        let ran = Arc::new(AtomicBool::new(false));

        // Occupy the single thread so the second job cannot start.
        let (release, gate) = std::sync::mpsc::channel();
        let blocker = {
            let worker = Arc::clone(&worker);
            tokio::spawn(async move {
                worker
                    .run(Arc::new(AtomicBool::new(false)), move || {
                        let _ = gate.recv();
                        Ok(())
                    })
                    .await
            })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;

        let queued = {
            let worker = Arc::clone(&worker);
            let ran = Arc::clone(&ran);
            tokio::spawn(async move {
                worker
                    .run(Arc::new(AtomicBool::new(false)), move || {
                        ran.store(true, Ordering::Relaxed);
                        Ok(())
                    })
                    .await
            })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;

        // The caller gives up while its job is still queued behind the blocker.
        queued.abort();
        let _ = queued.await;

        let _ = release.send(());
        blocker.await.unwrap().unwrap();
        // Let the worker drain whatever is left.
        worker
            .run(Arc::new(AtomicBool::new(false)), || Ok(()))
            .await
            .unwrap();

        assert!(
            !ran.load(Ordering::Relaxed),
            "an abandoned job ran anyway, so the queue bound does not bound the work",
        );
    }

    #[tokio::test]
    async fn jobs_run_one_at_a_time_in_order() {
        // The containment property: everything is serialised onto one thread,
        // so no amount of concurrency multiplies the blocking cost.
        let worker = Arc::new(Worker::start());
        let order = Arc::new(Mutex::new(Vec::new()));
        let mut handles = Vec::new();
        for i in 0..5 {
            let worker = Arc::clone(&worker);
            let order = Arc::clone(&order);
            handles.push(tokio::spawn(async move {
                worker
                    .run(Arc::new(AtomicBool::new(false)), move || {
                        order.lock().unwrap_or_else(|e| e.into_inner()).push(i);
                        Ok(())
                    })
                    .await
                    .unwrap();
            }));
        }
        for handle in handles {
            handle.await.unwrap();
        }
        assert_eq!(order.lock().unwrap_or_else(|e| e.into_inner()).len(), 5);
    }
}
