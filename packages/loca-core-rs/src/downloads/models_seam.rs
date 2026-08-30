//! This package's implementation of the models [`Downloads`] seam.
//!
//! Four of the six methods are synchronous, so the async work is adapted:
//! `models_snapshot` reads the queue's cached snapshot rather than issuing a
//! blocking database call from inside an async handler, and `cancel`, `retry`
//! and `remove` spawn their work, since all three answer 204 immediately and
//! the outcome reaches the client on the `downloads.snapshot` broadcast.
//!
//! `network` gates the outbound HEAD and never the presence check, which is
//! always a local `stat`, so the always-on requirements snapshot passes `false`
//! and still gets a truthful `already_have`. Converted to [`Probe`] on the first
//! line, because a bare `false` read backwards at a call site is a silent
//! consent violation and `Probe::LocalOnly` is not.

use std::sync::Arc;

use async_trait::async_trait;

use crate::contract::api::{DownloadEntry, DownloadEntryDestination, DownloadPlan};
use crate::downloads::cancel::CancellationToken;
use crate::downloads::net::Net;
use crate::downloads::queue::DownloadQueue;
use crate::downloads::sources::{probe_source, Probe};
use crate::downloads::EnqueueSpec;
use crate::error::Result;
use crate::models::deps::Downloads;
use crate::paths::Paths;

/// The queue, as the model routes use it.
#[derive(Clone)]
pub struct ModelDownloads {
    queue: DownloadQueue,
    net: Arc<Net>,
    paths: Paths,
    hf_base_url: String,
}

impl ModelDownloads {
    pub fn new(queue: DownloadQueue, net: Arc<Net>, paths: Paths, hf_base_url: &str) -> Self {
        Self {
            queue,
            net,
            paths,
            hf_base_url: hf_base_url.to_string(),
        }
    }

    /// The spec a `(source, group)` pair means. Models only: a caller cannot
    /// steer the destination through this seam.
    fn spec(source: &str, group: &str) -> EnqueueSpec {
        EnqueueSpec {
            source: source.to_string(),
            destination: DownloadEntryDestination::Models,
            group_id: group.to_string(),
            size_hint: None,
            // No pinned hash: a model file's comes from the signed catalog or
            // HuggingFace's etag, both resolved inside the transfer.
            sha256: None,
            url: None,
            rel_path: None,
            filename: None,
        }
    }
}

#[async_trait]
impl Downloads for ModelDownloads {
    async fn probe(&self, sources: &[String], network: bool) -> Result<Vec<DownloadPlan>> {
        // The whole of `network`'s meaning, in one conversion.
        let mode = Probe::from_network_flag(network);
        let cancel = CancellationToken::new();
        let root = self.paths.models_dir();
        let mut plans = Vec::with_capacity(sources.len());
        for source in sources {
            // Sequential: `WithSizes` issues a HEAD per missing file, and
            // fanning those out is the burst the single permit exists to avoid.
            let probed =
                probe_source(&self.net, source, root, &self.hf_base_url, mode, &cancel).await?;
            plans.push(probed.to_plan());
        }
        Ok(plans)
    }

    fn enqueue_models(&self, items: &[(String, String)]) -> Result<Vec<String>> {
        // Ids first, and fallibly: `id_for` containment-checks the destination,
        // so a source escaping the models root is refused here rather than
        // spawned and failing invisibly. An empty `jobIds` would instead report
        // that nothing needed downloading.
        let specs: Vec<EnqueueSpec> = items
            .iter()
            .map(|(source, group)| Self::spec(source, group))
            .collect();
        let job_ids = specs
            .iter()
            .map(|spec| self.queue.id_for(spec))
            .collect::<Result<Vec<String>>>()?;

        // Then the transfers, fire-and-forget: the caller is subscribed to
        // `downloads.snapshot`, where progress and failure surface.
        for spec in specs {
            let queue = self.queue.clone();
            tokio::spawn(async move {
                if let Err(err) = queue.enqueue(&spec).await {
                    tracing::warn!(
                        target: "loca::downloads",
                        "enqueue {}: {}", spec.source, err.message
                    );
                }
            });
        }
        Ok(job_ids)
    }

    fn models_snapshot(&self) -> Vec<DownloadEntry> {
        self.queue
            .cached_snapshot()
            .iter()
            .filter(|row| row.destination == DownloadEntryDestination::Models)
            .cloned()
            .collect()
    }

    fn cancel(&self, id: &str) {
        let (queue, id) = (self.queue.clone(), id.to_string());
        tokio::spawn(async move {
            if let Err(err) = queue.cancel(&id).await {
                tracing::warn!(target: "loca::downloads", "cancel {id}: {}", err.message);
            }
        });
    }

    fn retry(&self, id: &str) {
        let (queue, id) = (self.queue.clone(), id.to_string());
        tokio::spawn(async move {
            if let Err(err) = queue.retry(&id).await {
                tracing::warn!(target: "loca::downloads", "retry {id}: {}", err.message);
            }
        });
    }

    fn remove(&self, id: &str) {
        let (queue, id) = (self.queue.clone(), id.to_string());
        tokio::spawn(async move {
            if let Err(err) = queue.remove(&id).await {
                tracing::warn!(target: "loca::downloads", "remove {id}: {}", err.message);
            }
        });
    }

    async fn reconcile_completed(&self) -> Result<()> {
        self.queue.reconcile_completed().await
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::contract::api::DownloadStatus;
    use crate::db::migrate::migrate;
    use crate::db::Db;
    use crate::downloads::deps::NoCatalog;
    use crate::downloads::store::DownloadStore;
    use crate::downloads::testing::{bounded, Serve, Upstream};
    use crate::downloads::transfer::Transfer;

    const HF: &str = "https://huggingface.co";
    const BODY: &[u8] = b"0123456789abcdef";

    struct Fixture {
        seam: ModelDownloads,
        queue: DownloadQueue,
        net: Arc<Net>,
        dir: tempfile::TempDir,
    }

    fn fixture(hf_base_url: &str) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open_in_memory().unwrap();
        db.call_blocking(|conn| migrate(conn)).unwrap();
        let store = DownloadStore::new(db, hf_base_url);
        let net = Arc::new(Net::new().unwrap());
        let transfer = Arc::new(
            Transfer::new(Arc::clone(&net), Arc::new(NoCatalog), hf_base_url)
                .with_intervals(Duration::from_secs(5), Duration::from_millis(1)),
        );
        let paths = Paths::rooted(dir.path());
        let queue = DownloadQueue::new(store, transfer, paths.clone(), hf_base_url)
            .with_delay(Duration::from_millis(0));
        let seam = ModelDownloads::new(queue.clone(), Arc::clone(&net), paths, hf_base_url);
        Fixture {
            seam,
            queue,
            net,
            dir,
        }
    }

    /// Wait for the spawned work behind a sync seam method to settle.
    async fn settle<F>(mut done: F)
    where
        F: FnMut() -> bool,
    {
        for _ in 0..200 {
            if done() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("d7: the spawned seam work never settled");
    }

    #[tokio::test]
    async fn a_local_only_probe_reports_presence_and_makes_no_outbound_call() {
        // The mode the always-on requirements snapshot passes, which must
        // still answer truthfully.
        let fx = fixture(HF);
        let present = fx.dir.path().join("models/u/r/have.gguf");
        std::fs::create_dir_all(present.parent().unwrap()).unwrap();
        std::fs::write(&present, b"here").unwrap();

        let plans = bounded(fx.seam.probe(
            &[
                "@u/r/main/have.gguf".to_string(),
                "@u/r/main/missing.gguf".to_string(),
            ],
            false,
        ))
        .await
        .unwrap();

        assert_eq!(fx.net.calls(), 0, "a local-only probe reached the network");
        assert!(plans[0].already_have, "presence is always a local check");
        assert!(!plans[1].already_have);
        assert_eq!(plans[1].size_hint, None, "no HEAD means no size");
    }

    #[tokio::test]
    async fn a_networked_probe_adds_a_size_hint_for_what_is_missing() {
        let server = Upstream::start(Serve::Status(200))
            .await
            .with_head_redirect("/cdn", &[("x-linked-size", "4096")]);
        let fx = fixture(server.base());

        let plans = bounded(fx.seam.probe(&["@u/r/main/missing.gguf".to_string()], true))
            .await
            .unwrap();

        assert_eq!(plans[0].size_hint, Some(4096));
        assert!(fx.net.calls() > 0);
    }

    #[tokio::test]
    async fn the_network_flag_gates_only_the_head_so_presence_agrees_either_way() {
        // The two modes must never disagree about `already_have`.
        let server = Upstream::start(Serve::Status(200))
            .await
            .with_head_redirect("/cdn", &[("x-linked-size", "4096")]);
        let fx = fixture(server.base());
        let present = fx.dir.path().join("models/u/r/have.gguf");
        std::fs::create_dir_all(present.parent().unwrap()).unwrap();
        std::fs::write(&present, b"here").unwrap();
        let sources = vec![
            "@u/r/main/have.gguf".to_string(),
            "@u/r/main/missing.gguf".to_string(),
        ];

        let offline = bounded(fx.seam.probe(&sources, false)).await.unwrap();
        let online = bounded(fx.seam.probe(&sources, true)).await.unwrap();

        assert_eq!(
            offline.iter().map(|p| p.already_have).collect::<Vec<_>>(),
            online.iter().map(|p| p.already_have).collect::<Vec<_>>(),
        );
    }

    #[tokio::test]
    async fn enqueue_returns_ids_that_cancel_retry_and_remove_accept() {
        // These ids are what the client sends to the other three routes, so a
        // mismatch makes all three no-op.
        let server = Upstream::start(Serve::Ranged(BODY.to_vec())).await;
        let fx = fixture(server.base());

        let ids = fx
            .seam
            .enqueue_models(&[("@u/r/main/model.gguf".to_string(), "llm".to_string())])
            .unwrap();

        assert_eq!(ids.len(), 1);
        settle(|| !fx.seam.models_snapshot().is_empty()).await;
        let rows = fx.seam.models_snapshot();
        assert_eq!(rows[0].id, ids[0]);
    }

    #[tokio::test]
    async fn an_enqueue_that_would_escape_the_models_root_is_refused_rather_than_spawned() {
        // Fallible on purpose: an empty `jobIds` would report that nothing
        // needed downloading.
        let fx = fixture(HF);

        let err = fx
            .seam
            .enqueue_models(&[("@u/../../etc/passwd".to_string(), "llm".to_string())])
            .unwrap_err();

        assert_eq!(err.code, crate::error::ErrorCode::ValidationError);
        assert!(fx.seam.models_snapshot().is_empty());
    }

    #[tokio::test]
    async fn the_snapshot_carries_models_rows_and_hides_the_binaries_ones() {
        // All THREE destinations: with only two staged, a filter written as
        // "not binaries" is indistinguishable from one written as "is models".
        let fx = fixture(HF);
        let mut binary = ModelDownloads::spec("@u/r/main/deno.zip", "bin");
        binary.destination = DownloadEntryDestination::Binaries;
        binary.rel_path = Some("staging/deno.zip".into());
        let mut extension = ModelDownloads::spec("@u/r/main/ext.tgz", "ext");
        extension.destination = DownloadEntryDestination::Extensions;
        let model = ModelDownloads::spec("@u/r/main/model.gguf", "llm");

        for spec in [&model, &binary, &extension] {
            let id = fx.queue.id_for(spec).unwrap();
            let abs = fx.dir.path().join("anywhere").join(id.replace(':', "_"));
            fx.queue
                .store_for_test()
                .upsert_pending(&id, spec, &abs)
                .await
                .unwrap();
        }
        fx.queue.reconcile_completed().await.unwrap();

        let rows = fx.seam.models_snapshot();

        assert_eq!(
            rows.len(),
            1,
            "a non-models row leaked into the models list"
        );
        assert_eq!(rows[0].destination, DownloadEntryDestination::Models);
    }

    #[tokio::test]
    async fn the_snapshot_is_readable_without_blocking_and_survives_a_restart() {
        // The boot pass primes the cache, so the first read after a restart
        // reports the persisted queue rather than an empty one.
        let fx = fixture(HF);
        let spec = ModelDownloads::spec("@u/r/main/model.gguf", "llm");
        let id = fx.queue.id_for(&spec).unwrap();
        fx.queue
            .store_for_test()
            .upsert_pending(&id, &spec, &fx.dir.path().join("models/u/r/model.gguf"))
            .await
            .unwrap();

        assert!(
            fx.seam.models_snapshot().is_empty(),
            "nothing has refreshed the cache yet"
        );
        fx.queue.normalize().await.unwrap();

        assert_eq!(fx.seam.models_snapshot().len(), 1);
    }

    #[tokio::test]
    async fn cancel_retry_and_remove_reach_the_queue_despite_being_sync() {
        let fx = fixture("http://127.0.0.1:1");
        let spec = ModelDownloads::spec("@u/r/main/model.gguf", "llm");
        let id = fx.queue.id_for(&spec).unwrap();
        let dest = fx.dir.path().join("models/u/r/model.gguf");
        fx.queue
            .store_for_test()
            .upsert_pending(&id, &spec, &dest)
            .await
            .unwrap();

        fx.seam.cancel(&id);
        settle(|| {
            fx.queue
                .cached_snapshot()
                .iter()
                .any(|r| r.id == id && r.status == DownloadStatus::Cancelled)
        })
        .await;

        // The retried worker cannot reach 127.0.0.1:1, so the row settles on
        // Error. A TERMINAL status matters for the step after it.
        fx.seam.retry(&id);
        settle(|| {
            fx.queue
                .cached_snapshot()
                .iter()
                .any(|r| r.id == id && r.status == DownloadStatus::Error)
        })
        .await;

        // `remove` refuses while an in-flight entry is live, and the worker
        // writes the terminal status BEFORE dropping that entry, so a single
        // call can land in the window and no-op. This retries as a client would.
        settle(|| {
            fx.seam.remove(&id);
            fx.queue.cached_snapshot().iter().all(|r| r.id != id)
        })
        .await;
    }

    #[tokio::test]
    async fn reconcile_drops_a_completed_row_whose_file_vanished() {
        let fx = fixture(HF);
        let spec = ModelDownloads::spec("@u/r/main/model.gguf", "llm");
        let id = fx.queue.id_for(&spec).unwrap();
        let dest = fx.dir.path().join("models/u/r/model.gguf");
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::write(&dest, b"here").unwrap();
        fx.queue
            .store_for_test()
            .upsert_completed(&id, &spec, &dest)
            .await
            .unwrap();
        fx.queue.reconcile_completed().await.unwrap();
        assert_eq!(fx.seam.models_snapshot().len(), 1);

        std::fs::remove_file(&dest).unwrap();
        fx.seam.reconcile_completed().await.unwrap();

        assert!(
            fx.seam.models_snapshot().is_empty(),
            "a stale done entry did not self-clear"
        );
    }
}
