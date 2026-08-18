//! The byte transfer: resolve an integrity hash, resume a partial by `Range`
//! when one is trustworthy, stream to a `.tmp` under a stall watchdog and a
//! runaway-size ceiling, verify the sha256, and rename into place.
//!
//! Progress and size discoveries go out through callbacks, so this module owns
//! the network and the filesystem and nothing else: the queue owns persistence
//! and the broadcast.
//!
//! # The three properties worth reading before changing anything here
//!
//! **Resume is gated on having a hash.** The final sha256 verifies the WHOLE
//! file, so a wrongly resumed file is still caught, deleted and retried clean.
//! Without a hash there is no way to tell a good partial from a bad one, so the
//! transfer always starts over. That single gate is what makes "the file changed
//! under us" cost one wasted download rather than a corrupt install: nothing
//! here validates `Content-Range`, checks an `ETag`, or sends `If-Range`.
//!
//! **A checksum mismatch and a 416 destroy the partial; everything else keeps
//! it.** The queue's `Error` arm keeps the `.tmp` deliberately so a retry
//! resumes, and that arm is reached by the two failures which have already
//! deleted it here. Both halves have to be read together to be correct.
//!
//! **Every deadline is a `tokio::time::Instant`** (D-038), including the
//! progress throttle: one clock rather than two that can disagree, and a
//! watchdog whose deadline is a std instant cannot fire under a paused runtime.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::Instant;

use crate::contract::api::DownloadEntryDestination;
use crate::downloads::cancel::CancellationToken;
use crate::downloads::deps::CatalogHashes;
use crate::downloads::net::{self, Net, Redirect};
use crate::downloads::sources::parse_source;
use crate::downloads::EnqueueSpec;
use crate::error::{validation, AppError, ErrorCode, Result};

/// Abort a transfer that receives no bytes for this long.
///
/// A silently stalled connection (server wedged, network black-holed) otherwise
/// leaves the read blocked forever, with the UI gated on a download that will
/// never finish and never error. Generous on purpose: it fires on silence, never
/// on slowness.
pub const STALL_TIMEOUT: Duration = Duration::from_secs(60);

/// How often a streaming transfer reports progress. Each tick is a row write and
/// a broadcast to every client, so this is a cost rather than a resolution
/// choice.
pub const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);

/// The runaway ceiling: abort once the body has delivered more than
/// `declared * MULTIPLIER + SLACK`. Generous so a transfer-encoding quirk never
/// trips a legitimate download, tight enough that an unbounded stream cannot
/// fill the disk.
pub const CEILING_MULTIPLIER: u64 = 2;
pub const CEILING_SLACK: u64 = 64 * 1024 * 1024;

/// The most a body may deliver before it is cut off.
///
/// **Saturating, and that is the whole point of the function.** `declared` is a
/// `content-length` off the wire, so a hostile or broken origin picks it: at
/// `2^63` the plain arithmetic overflows, which in a debug build panics the
/// transfer worker and in a RELEASE build silently wraps to a 64 MiB ceiling
/// that aborts the next legitimate download. The oracle cannot reach either,
/// because JavaScript numbers are doubles (`transfer.ts:142`), so this is a
/// hazard the port introduced rather than one it inherited.
///
/// Saturating gives the answer the oracle gives: an absurd declared size means
/// an absurd ceiling, so nothing is cut off for exceeding it, and the stall
/// watchdog is what ends a stream that then goes quiet.
fn ceiling(declared: u64, multiplier: u64, slack: u64) -> u64 {
    declared.saturating_mul(multiplier).saturating_add(slack)
}

/// One transfer's inputs. The callbacks are `Fn` rather than `FnMut` because the
/// queue's are shared with a live broadcast.
pub struct TransferOpts<'a> {
    pub spec: &'a EnqueueSpec,
    pub abs_path: &'a Path,
    pub cancel: &'a CancellationToken,
    /// Throttled progress tick: persist and broadcast.
    pub on_progress: &'a (dyn Fn(u64) + Send + Sync),
    /// Fired once the exact total is known and it differs from the hint.
    pub on_size_known: &'a (dyn Fn(u64) + Send + Sync),
}

/// The byte-moving half of the download engine.
pub struct Transfer {
    net: Arc<Net>,
    catalog: Arc<dyn CatalogHashes>,
    hf_base_url: String,
    /// The HuggingFace host, derived from the configured base URL so a mirror
    /// (`LOCA_HF_BASE_URL`) is honoured rather than the literal host being
    /// hard-coded. The etag lookup only runs against this host.
    hf_host: Option<String>,
    stall: Duration,
    progress_interval: Duration,
    ceiling_multiplier: u64,
    ceiling_slack: u64,
}

impl Transfer {
    pub fn new(net: Arc<Net>, catalog: Arc<dyn CatalogHashes>, hf_base_url: &str) -> Self {
        Self {
            net,
            catalog,
            hf_base_url: hf_base_url.to_string(),
            hf_host: net::host_of(hf_base_url),
            stall: STALL_TIMEOUT,
            progress_interval: PROGRESS_INTERVAL,
            ceiling_multiplier: CEILING_MULTIPLIER,
            ceiling_slack: CEILING_SLACK,
        }
    }

    /// Shorten the stall window and the progress throttle.
    ///
    /// Injectable because the alternative is a test that either runs for a
    /// production minute or proves nothing. The shipping RATIO is what a test
    /// has to keep: a stall window well above the inter-chunk interval.
    pub fn with_intervals(mut self, stall: Duration, progress: Duration) -> Self {
        self.stall = stall;
        self.progress_interval = progress;
        self
    }

    /// Move the runaway ceiling, so the boundary case can sit ON the boundary
    /// rather than 64 MiB away from it.
    pub fn with_ceiling(mut self, multiplier: u64, slack: u64) -> Self {
        self.ceiling_multiplier = multiplier;
        self.ceiling_slack = slack;
        self
    }

    /// Download `spec` to `abs_path`, returning the final byte count.
    ///
    /// A terminal failure (bad HTTP, checksum mismatch, stall) is an error; a
    /// transient one leaves the `.tmp` partial behind for a later resume.
    pub async fn stream_transfer(&self, opts: TransferOpts<'_>) -> Result<u64> {
        let TransferOpts {
            spec,
            abs_path,
            cancel,
            on_progress,
            on_size_known,
        } = opts;

        let url = match spec.url.as_ref() {
            // Present, not truthy: a caller that sets `url` to the empty string
            // has bypassed the source parser and asked for nothing, and falls
            // through to the refusal below rather than silently downloading the
            // parsed source instead.
            Some(url) => url.clone(),
            None => parse_source(&spec.source, &self.hf_base_url)?.url,
        };
        if url.is_empty() {
            return Err(validation("non-downloadable source"));
        }
        if let Some(parent) = abs_path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|err| {
                AppError::new(
                    ErrorCode::InternalError,
                    format!("could not create {}: {err}", parent.display()),
                )
            })?;
        }
        let tmp_path = tmp_path_for(abs_path);

        // An empty pinned hash is PRESENT for the purpose of the anchor chain
        // (it shadows the catalog and the etag lookup, so neither is consulted)
        // and ABSENT for everything after it: no hasher, no resume, no
        // verification, and the unverified-download warning fires. Both halves
        // are the oracle's, and the second is this one line.
        let expected_sha = self
            .resolve_expected_sha(spec, &url, cancel)
            .await
            .filter(|sha| !sha.is_empty());
        if expected_sha.is_none() {
            // No trustworthy content hash: the bytes are accepted on TLS trust
            // alone. Said out loud rather than silently trusted, because this
            // line is the only record that a given file was never verified.
            tracing::warn!(
                target: "loca::downloads",
                "downloading {url} without integrity verification (no sha256 available)"
            );
        }

        let (resume_from, mut hasher) = open_resume(&tmp_path, expected_sha.is_some()).await;
        let mut downloaded = resume_from;
        let mut total = spec.size_hint;

        // The watchdog covers the wait for response headers too: a host that
        // accepts the connection and then never answers is the same silence as
        // one that stops mid-body.
        let requested = self.net.get_range(&url, Some(resume_from), cancel);
        let mut res = match tokio::time::timeout(self.stall, requested).await {
            Err(_elapsed) => return Err(self.stalled(&url)),
            Ok(res) => res?,
        };

        if res.status() == 416 {
            // The partial is longer than the resource (a corrupt or stale
            // `.tmp`): drop it so the next retry starts clean rather than
            // 416-looping forever.
            let _ = tokio::fs::remove_file(&tmp_path).await;
            return Err(AppError::new(
                ErrorCode::ProviderError,
                format!("partial file for {url} is unusable (HTTP 416); restarting on next retry"),
            ));
        }
        if !(200..300).contains(&res.status()) {
            return Err(AppError::new(
                ErrorCode::ManifestFetchFailed,
                format!("HTTP {} {} for {url}", res.status(), res.status_text()),
            ));
        }

        // 206 means the server honoured the Range and is sending only the tail,
        // so append. Anything else is the whole file from byte zero, so the
        // partial is discarded: hasher reset, file truncated, counters zeroed.
        // Appending to it instead would produce a file that is the partial plus
        // the whole resource, which only the final sha256 would catch.
        let resuming = resume_from > 0 && res.status() == 206;
        if !resuming {
            downloaded = 0;
            if expected_sha.is_some() {
                hasher = Some(Sha256::new());
            }
        }
        let mut file = open_tmp(&tmp_path, resuming).await?;

        // content-length is the BODY length, which is the remaining tail when
        // resuming, so the size of the resource is the offset plus it.
        let mut exact_total: Option<u64> = None;
        if let Some(length) = res.content_length() {
            // Saturating, because both operands come off a response header and
            // the oracle's are JavaScript doubles that cannot overflow. See
            // [`ceiling`].
            exact_total = Some(downloaded.saturating_add(length));
            total = exact_total;
        }
        if let Some(total) = total {
            // A correct hint writes nothing. The extra row write is cheap; the
            // broadcast it triggers is not, and a progress bar that repaints on
            // every resume for no reason is the visible symptom.
            if Some(total) != spec.size_hint {
                on_size_known(total);
            }
        }

        let mut last_emit: Option<Instant> = None;
        let pump = async {
            loop {
                let next = tokio::select! {
                    // Cancel first, so a user's stop is not delayed behind a
                    // chunk that happens to be ready.
                    biased;
                    () = cancel.cancelled() => return Err(net::cancelled()),
                    // A per-chunk deadline IS the re-arm: there is no timer to
                    // reset, because the timeout is rebuilt each iteration.
                    chunk = tokio::time::timeout(self.stall, res.next_chunk()) => chunk,
                };
                let chunk = match next {
                    Err(_elapsed) => return Err(self.stalled(&url)),
                    Ok(None) => break,
                    Ok(Some(chunk)) => chunk?,
                };
                // Checked at the top against the running total, so a stream that
                // far exceeds what it declared is cut off before the write
                // rather than after the disk fills.
                if let Some(declared) = exact_total {
                    if downloaded > ceiling(declared, self.ceiling_multiplier, self.ceiling_slack) {
                        return Err(AppError::new(
                            ErrorCode::ProviderError,
                            format!(
                                "download for {url} far exceeded its declared size ({declared}); aborting"
                            ),
                        ));
                    }
                }
                file.write_all(&chunk).await.map_err(write_failed)?;
                if let Some(hasher) = hasher.as_mut() {
                    hasher.update(&chunk);
                }
                downloaded += chunk.len() as u64;
                let now = Instant::now();
                if last_emit.is_none_or(|last| now.duration_since(last) > self.progress_interval) {
                    last_emit = Some(now);
                    on_progress(downloaded);
                }
            }
            Ok(())
        }
        .await;
        // Flush on every exit, including the failing ones: a buffered tail that
        // never reached the file would make the next resume hash bytes the
        // server never sent.
        //
        // `sync_all` and not just `flush`, and the difference matters more here
        // than anywhere else in the core. `flush` pushes the userspace buffer
        // into the kernel; the rename below then publishes a file whose tail may
        // still be only in page cache. A crash in that window leaves a
        // TRUNCATED model or binary at the final path, which the queue row
        // records as complete and nothing ever re-verifies, so the sha256 this
        // function just checked is checked against bytes that are no longer what
        // is on disk. The next launch loads a corrupt artifact and reports
        // something unrelated.
        let flushed = async {
            file.flush().await.map_err(write_failed)?;
            file.sync_all().await.map_err(write_failed)
        }
        .await;
        drop(file);
        pump?;
        flushed?;

        if let (Some(hasher), Some(expected)) = (hasher, expected_sha.as_ref()) {
            let actual = hex::encode(hasher.finalize());
            // The expected side is lowercased because it is not normalised
            // anywhere upstream: a signed manifest or a GitHub digest can carry
            // uppercase hex, and only two of the three anchors lowercase.
            if actual != expected.to_lowercase() {
                let _ = tokio::fs::remove_file(&tmp_path).await;
                return Err(AppError::new(
                    ErrorCode::ChecksumMismatch,
                    format!("sha256 mismatch: want {expected}, got {actual}"),
                ));
            }
        }

        tokio::fs::rename(&tmp_path, abs_path)
            .await
            .map_err(|err| {
                AppError::new(
                    ErrorCode::InternalError,
                    format!("could not publish {}: {err}", abs_path.display()),
                )
            })?;
        Ok(downloaded)
    }

    /// The most trustworthy sha256 available for `spec`, in order: an explicit
    /// spec hash, then (for a catalog model file) the hash pinned in the SIGNED
    /// catalog, then HuggingFace's published one.
    ///
    /// The order is the point. The signed catalog outranks a header because it
    /// holds even when a proxy strips the etag or the file moves to a mirror,
    /// and an explicit spec hash outranks both because a binary's hash comes
    /// from a manifest this core verified itself.
    async fn resolve_expected_sha(
        &self,
        spec: &EnqueueSpec,
        url: &str,
        cancel: &CancellationToken,
    ) -> Option<String> {
        if let Some(sha) = spec.sha256.clone() {
            return Some(sha);
        }
        if spec.destination == DownloadEntryDestination::Models {
            if let Some(sha) = self.catalog.catalog_sha256(&spec.source).await {
                return Some(sha);
            }
        }
        self.resolve_hf_sha256(url, cancel).await
    }

    /// HuggingFace's published sha256 for a file, or `None`.
    ///
    /// The resolve endpoint 302s to a CDN, and for git-LFS objects (the large
    /// weights) the redirect carries the content sha256 in `x-linked-etag`.
    /// A small non-LFS file's etag is a git blob **sha1**, which is not a
    /// content hash at all, so the shape check is what stops a plausible-looking
    /// header being treated as one.
    async fn resolve_hf_sha256(&self, url: &str, cancel: &CancellationToken) -> Option<String> {
        let host = net::host_of(url)?;
        if Some(&host) != self.hf_host.as_ref() {
            return None;
        }
        let head = self.net.head(url, Redirect::Manual, cancel).await.ok()?;
        let raw = head
            .header("x-linked-etag")
            .or_else(|| head.header("etag"))?;
        let cleaned = raw
            .strip_prefix("W/")
            .unwrap_or(&raw)
            .replace('"', "")
            .trim()
            .to_string();
        if is_sha256_hex(&cleaned) {
            Some(cleaned.to_lowercase())
        } else {
            None
        }
    }

    fn stalled(&self, url: &str) -> AppError {
        AppError::new(
            ErrorCode::ServerUnavailable,
            format!(
                "download for {url} stalled (no data for {}s); aborting",
                self.stall.as_secs_f64()
            ),
        )
    }
}

/// The partial for `abs_path`. A suffix on the OS string rather than a rebuilt
/// path, so a filename that is not valid UTF-8 keeps its bytes.
pub fn tmp_path_for(abs_path: &Path) -> PathBuf {
    let mut raw = abs_path.as_os_str().to_os_string();
    raw.push(".tmp");
    PathBuf::from(raw)
}

/// Detect a resumable partial and seed the hasher with its bytes, so the
/// streamed tail hashes continuously.
///
/// Returns `(0, None)` when no hash is available: with nothing to verify
/// against, a partial cannot be told apart from garbage, so the transfer starts
/// clean. A read failure falls back to a clean start too.
///
/// The whole partial is re-read on every resume, which for a 20 GB weight is a
/// full pass before the first new byte arrives. That is the price of verifying
/// the whole file rather than the tail, and it is deliberate.
async fn open_resume(tmp_path: &Path, want_hash: bool) -> (u64, Option<Sha256>) {
    if !want_hash {
        return (0, None);
    }
    let mut hasher = Sha256::new();
    let mut resume_from = 0u64;
    if let Ok(meta) = tokio::fs::metadata(tmp_path).await {
        // A zero-length partial is nothing to resume, and a directory named
        // `<file>.tmp` is not a partial at all.
        if meta.is_file() && meta.len() > 0 {
            resume_from = meta.len();
        }
    }
    if resume_from > 0 && seed_hasher(tmp_path, &mut hasher).await.is_err() {
        resume_from = 0;
        hasher = Sha256::new();
    }
    (resume_from, Some(hasher))
}

async fn seed_hasher(tmp_path: &Path, hasher: &mut Sha256) -> std::io::Result<()> {
    let mut file = tokio::fs::File::open(tmp_path).await?;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buf).await?;
        if read == 0 {
            return Ok(());
        }
        hasher.update(&buf[..read]);
    }
}

async fn open_tmp(tmp_path: &Path, resuming: bool) -> Result<tokio::fs::File> {
    let mut options = tokio::fs::OpenOptions::new();
    options.create(true).write(true);
    if resuming {
        options.append(true);
    } else {
        options.truncate(true);
    }
    options.open(tmp_path).await.map_err(|err| {
        AppError::new(
            ErrorCode::InternalError,
            format!("could not open {}: {err}", tmp_path.display()),
        )
    })
}

fn write_failed(err: std::io::Error) -> AppError {
    AppError::new(
        ErrorCode::InternalError,
        format!("could not write the download: {err}"),
    )
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::sync::Mutex;

    use async_trait::async_trait;

    use super::*;
    use crate::downloads::testing::{bounded, sha256_hex, Serve, Upstream};

    const BODY: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";

    struct Catalog(Option<String>);

    #[async_trait]
    impl CatalogHashes for Catalog {
        async fn catalog_sha256(&self, _spec: &str) -> Option<String> {
            self.0.clone()
        }
    }

    /// A recording pair of callbacks, so the tests can assert what the queue
    /// would have persisted.
    #[derive(Default)]
    struct Reports {
        progress: Mutex<Vec<u64>>,
        sizes: Mutex<Vec<u64>>,
    }

    impl Reports {
        fn sizes(&self) -> Vec<u64> {
            self.sizes.lock().unwrap_or_else(|e| e.into_inner()).clone()
        }

        fn progress(&self) -> Vec<u64> {
            self.progress
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
        }
    }

    struct Fixture {
        net: Arc<Net>,
        transfer: Transfer,
        dir: tempfile::TempDir,
        reports: Arc<Reports>,
        cancel: CancellationToken,
    }

    impl Fixture {
        fn new(hf_base_url: &str, catalog: Option<String>) -> Self {
            let net = Arc::new(Net::new().unwrap());
            let transfer = Transfer::new(Arc::clone(&net), Arc::new(Catalog(catalog)), hf_base_url)
                // 250 ms against a stream that emits far more often keeps the
                // shipping ratio (60 s against a stream that emits per chunk).
                .with_intervals(Duration::from_millis(250), Duration::from_millis(10));
            Self {
                net,
                transfer,
                dir: tempfile::tempdir().unwrap(),
                reports: Arc::new(Reports::default()),
                cancel: CancellationToken::new(),
            }
        }

        /// A `Transfer` with nothing tuned, for the one test that is about the
        /// shipping constants rather than about moving bytes. `Fixture::new`
        /// deliberately shortens the intervals, so it cannot answer that.
        fn bare_transfer() -> Transfer {
            let net = Arc::new(Net::new().unwrap());
            Transfer::new(net, Arc::new(Catalog(None)), "https://huggingface.co")
        }

        fn dest(&self) -> PathBuf {
            self.dir.path().join("sub/model.gguf")
        }

        fn tmp(&self) -> PathBuf {
            tmp_path_for(&self.dest())
        }

        /// Pre-stage a partial download, the way a killed process leaves one.
        fn stage_partial(&self, bytes: &[u8]) {
            std::fs::create_dir_all(self.dest().parent().unwrap()).unwrap();
            std::fs::write(self.tmp(), bytes).unwrap();
        }

        async fn run(&self, spec: &EnqueueSpec) -> Result<u64> {
            let reports = Arc::clone(&self.reports);
            let progress_reports = Arc::clone(&reports);
            let on_progress = move |downloaded: u64| {
                progress_reports
                    .progress
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(downloaded);
            };
            let on_size_known = move |total: u64| {
                reports
                    .sizes
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(total);
            };
            let dest = self.dest();
            bounded(self.transfer.stream_transfer(TransferOpts {
                spec,
                abs_path: &dest,
                cancel: &self.cancel,
                on_progress: &on_progress,
                on_size_known: &on_size_known,
            }))
            .await
        }

        /// What actually landed on disk. The assertion that means something:
        /// a reported byte count can be right while the file is wrong (D-048).
        fn landed(&self) -> Option<Vec<u8>> {
            std::fs::read(self.dest()).ok()
        }
    }

    fn spec(url: &str, sha256: Option<&str>) -> EnqueueSpec {
        EnqueueSpec {
            source: "@u/r/main/model.gguf".into(),
            destination: DownloadEntryDestination::Models,
            group_id: "g".into(),
            size_hint: None,
            sha256: sha256.map(|s| s.to_string()),
            url: Some(url.to_string()),
            rel_path: None,
            filename: None,
        }
    }

    // -- the hash anchors ---------------------------------------------------

    #[tokio::test]
    async fn a_verified_download_lands_its_bytes_and_removes_the_partial() {
        let server = Upstream::start(Serve::Ranged(BODY.to_vec())).await;
        let fx = Fixture::new(server.base(), None);

        let bytes = fx
            .run(&spec(&server.url("/f"), Some(&sha256_hex(BODY))))
            .await
            .unwrap();

        assert_eq!(bytes, BODY.len() as u64);
        assert_eq!(fx.landed().as_deref(), Some(BODY));
        assert!(
            !fx.tmp().exists(),
            "the partial is renamed, not left behind"
        );
    }

    #[tokio::test]
    async fn a_checksum_mismatch_destroys_the_partial_and_names_both_hashes() {
        let server = Upstream::start(Serve::Ranged(BODY.to_vec())).await;
        let fx = Fixture::new(server.base(), None);
        let wrong = "f".repeat(64);

        let err = fx
            .run(&spec(&server.url("/f"), Some(&wrong)))
            .await
            .unwrap_err();

        assert_eq!(err.code, ErrorCode::ChecksumMismatch);
        assert!(err.message.contains(&wrong), "{}", err.message);
        assert!(err.message.contains(&sha256_hex(BODY)), "{}", err.message);
        assert!(
            !fx.tmp().exists(),
            "a mismatched partial is destroyed, or the retry resumes it forever"
        );
        assert!(fx.landed().is_none(), "nothing is published on a mismatch");
    }

    #[tokio::test]
    async fn an_uppercase_pinned_sha256_still_verifies() {
        // Neither a signed manifest nor a GitHub digest is normalised upstream.
        let server = Upstream::start(Serve::Ranged(BODY.to_vec())).await;
        let fx = Fixture::new(server.base(), None);

        let bytes = fx
            .run(&spec(
                &server.url("/f"),
                Some(&sha256_hex(BODY).to_uppercase()),
            ))
            .await
            .unwrap();

        assert_eq!(bytes, BODY.len() as u64);
        assert_eq!(fx.landed().as_deref(), Some(BODY));
    }

    #[tokio::test]
    async fn an_explicit_spec_hash_outranks_the_catalog() {
        let server = Upstream::start(Serve::Ranged(BODY.to_vec())).await;
        // The catalog would reject these bytes; the spec's hash accepts them.
        let fx = Fixture::new(server.base(), Some("a".repeat(64)));

        let bytes = fx
            .run(&spec(&server.url("/f"), Some(&sha256_hex(BODY))))
            .await
            .unwrap();

        assert_eq!(bytes, BODY.len() as u64);
    }

    #[tokio::test]
    async fn the_catalog_hash_is_used_when_the_spec_carries_none() {
        let server = Upstream::start(Serve::Ranged(BODY.to_vec())).await;
        let fx = Fixture::new(server.base(), Some("a".repeat(64)));

        let err = fx.run(&spec(&server.url("/f"), None)).await.unwrap_err();

        assert_eq!(
            err.code,
            ErrorCode::ChecksumMismatch,
            "the signed catalog's hash has to be consulted, or nothing verifies a model"
        );
    }

    #[tokio::test]
    async fn a_binaries_download_never_consults_the_model_catalog() {
        let server = Upstream::start(Serve::Ranged(BODY.to_vec())).await;
        let fx = Fixture::new(server.base(), Some("a".repeat(64)));
        let mut spec = spec(&server.url("/f"), None);
        spec.destination = DownloadEntryDestination::Binaries;

        // Unverified rather than rejected: the catalog answers for model files.
        let bytes = fx.run(&spec).await.unwrap();
        assert_eq!(bytes, BODY.len() as u64);
    }

    #[tokio::test]
    async fn an_empty_sha256_disables_verification_and_makes_no_hash_lookup() {
        // `""` is present for the anchor chain (it shadows the catalog AND the
        // etag HEAD) and absent for everything after it.
        let server = Upstream::start(Serve::Ranged(BODY.to_vec())).await;
        let fx = Fixture::new(server.base(), Some("a".repeat(64)));
        fx.stage_partial(b"stale");

        let bytes = fx.run(&spec(&server.url("/f"), Some(""))).await.unwrap();

        assert_eq!(bytes, BODY.len() as u64);
        assert_eq!(fx.landed().as_deref(), Some(BODY));
        assert_eq!(fx.net.calls(), 1, "the GET, and no etag HEAD");
        assert_eq!(
            server.calls()[0].range,
            None,
            "an empty hash disables resume as well as verification"
        );
    }

    #[tokio::test]
    async fn the_huggingface_etag_verifies_a_file_with_no_pinned_hash() {
        let server = Upstream::start(Serve::Ranged(BODY.to_vec()))
            .await
            .with_head_redirect(
                "/cdn",
                &[("x-linked-etag", &format!("\"{}\"", sha256_hex(BODY)))],
            );
        let fx = Fixture::new(server.base(), None);

        let bytes = fx.run(&spec(&server.url("/f"), None)).await.unwrap();

        assert_eq!(bytes, BODY.len() as u64);
        assert_eq!(fx.net.calls(), 2, "the etag HEAD, then the GET");
    }

    #[tokio::test]
    async fn a_git_blob_sha1_etag_is_not_a_content_hash() {
        // A small non-LFS file's etag is a 40-hex git blob sha1. Accepting it
        // would fail every such download with a checksum mismatch.
        let server = Upstream::start(Serve::Ranged(BODY.to_vec()))
            .await
            .with_head_redirect(
                "/cdn",
                &[("etag", "\"5c1b0e2f9a3d4e6c8b7a9f0d1e2c3b4a5d6e7f80\"")],
            );
        let fx = Fixture::new(server.base(), None);

        let bytes = fx.run(&spec(&server.url("/f"), None)).await.unwrap();

        assert_eq!(bytes, BODY.len() as u64, "the download proceeds unverified");
        assert_eq!(fx.landed().as_deref(), Some(BODY));
    }

    #[tokio::test]
    async fn a_weak_quoted_etag_is_unwrapped_before_it_is_checked() {
        let server = Upstream::start(Serve::Ranged(BODY.to_vec()))
            .await
            .with_head_redirect("/cdn", &[("etag", &format!("W/\"{}\"", sha256_hex(BODY)))]);
        let fx = Fixture::new(server.base(), None);

        assert_eq!(
            fx.run(&spec(&server.url("/f"), None)).await.unwrap(),
            BODY.len() as u64
        );
    }

    #[tokio::test]
    async fn a_non_huggingface_host_is_never_asked_for_an_etag() {
        let server = Upstream::start(Serve::Ranged(BODY.to_vec())).await;
        // The configured HF host is somewhere else entirely.
        let fx = Fixture::new("https://huggingface.co", None);

        let bytes = fx.run(&spec(&server.url("/f"), None)).await.unwrap();

        assert_eq!(bytes, BODY.len() as u64);
        assert_eq!(fx.net.calls(), 1, "one GET, and no HEAD to a foreign host");
    }

    // -- resume by Range ----------------------------------------------------

    #[tokio::test]
    async fn a_partial_is_resumed_and_the_bytes_on_disk_are_the_whole_file() {
        let server = Upstream::start(Serve::Ranged(BODY.to_vec())).await;
        let fx = Fixture::new(server.base(), None);
        fx.stage_partial(&BODY[..10]);

        let bytes = fx
            .run(&spec(&server.url("/f"), Some(&sha256_hex(BODY))))
            .await
            .unwrap();

        assert_eq!(server.calls()[0].range.as_deref(), Some("bytes=10-"));
        assert_eq!(bytes, BODY.len() as u64, "the count is the whole file");
        assert_eq!(
            fx.landed().as_deref(),
            Some(BODY),
            "the bytes on disk are the whole file, in order"
        );
    }

    #[tokio::test]
    async fn a_partial_is_not_resumed_without_a_content_hash() {
        // Without a hash a good partial cannot be told from a bad one, so the
        // transfer starts clean rather than trusting it.
        let server = Upstream::start(Serve::Ranged(BODY.to_vec())).await;
        let fx = Fixture::new(server.base(), None);
        fx.stage_partial(b"garbage that is not this file at all");

        let bytes = fx.run(&spec(&server.url("/f"), None)).await.unwrap();

        assert_eq!(server.calls()[0].range, None, "no Range without a hash");
        assert_eq!(bytes, BODY.len() as u64);
        assert_eq!(fx.landed().as_deref(), Some(BODY));
    }

    #[tokio::test]
    async fn a_server_that_ignores_the_range_header_restarts_clean() {
        // The branch that has to discard the partial: hasher reset, file
        // truncated, counters zeroed. Appending instead yields partial + whole.
        let server = Upstream::start(Serve::IgnoresRange(BODY.to_vec())).await;
        let fx = Fixture::new(server.base(), None);
        fx.stage_partial(&BODY[..10]);

        let bytes = fx
            .run(&spec(&server.url("/f"), Some(&sha256_hex(BODY))))
            .await
            .unwrap();

        assert_eq!(server.calls()[0].range.as_deref(), Some("bytes=10-"));
        assert_eq!(bytes, BODY.len() as u64, "the count is the body, not 46");
        assert_eq!(
            fx.landed().as_deref(),
            Some(BODY),
            "the partial was truncated rather than appended to"
        );
    }

    #[tokio::test]
    async fn a_416_drops_the_partial_so_the_next_retry_starts_clean() {
        let server = Upstream::start(Serve::Unsatisfiable).await;
        let fx = Fixture::new(server.base(), None);
        fx.stage_partial(b"longer than the resource itself");

        let err = fx
            .run(&spec(&server.url("/f"), Some(&sha256_hex(BODY))))
            .await
            .unwrap_err();

        assert_eq!(err.code, ErrorCode::ProviderError);
        assert!(err.message.contains("416"), "{}", err.message);
        assert!(!fx.tmp().exists(), "a 416 that keeps the partial 416-loops");
    }

    #[tokio::test]
    async fn a_non_2xx_keeps_the_partial_for_a_later_resume() {
        let server = Upstream::start(Serve::Status(503)).await;
        let fx = Fixture::new(server.base(), None);
        fx.stage_partial(&BODY[..10]);

        let err = fx
            .run(&spec(&server.url("/f"), Some(&sha256_hex(BODY))))
            .await
            .unwrap_err();

        assert_eq!(err.code, ErrorCode::ManifestFetchFailed);
        assert!(err.message.contains("503"), "{}", err.message);
        assert!(
            fx.tmp().exists(),
            "a transient failure keeps the partial; only 416 and a mismatch destroy it"
        );
    }

    #[tokio::test]
    async fn a_zero_length_partial_is_not_a_resume() {
        let server = Upstream::start(Serve::Ranged(BODY.to_vec())).await;
        let fx = Fixture::new(server.base(), None);
        fx.stage_partial(b"");

        let bytes = fx
            .run(&spec(&server.url("/f"), Some(&sha256_hex(BODY))))
            .await
            .unwrap();

        assert_eq!(server.calls()[0].range, None);
        assert_eq!(bytes, BODY.len() as u64);
    }

    // -- sizes --------------------------------------------------------------

    #[tokio::test]
    async fn resuming_reports_the_full_size_not_the_tail() {
        let server = Upstream::start(Serve::Ranged(BODY.to_vec())).await;
        let fx = Fixture::new(server.base(), None);
        fx.stage_partial(&BODY[..10]);

        fx.run(&spec(&server.url("/f"), Some(&sha256_hex(BODY))))
            .await
            .unwrap();

        assert_eq!(
            fx.reports.sizes(),
            vec![BODY.len() as u64],
            "content-length is the tail; the size is the offset plus it"
        );
    }

    #[tokio::test]
    async fn a_correct_size_hint_writes_no_size_update() {
        let server = Upstream::start(Serve::Ranged(BODY.to_vec())).await;
        let fx = Fixture::new(server.base(), None);
        let mut spec = spec(&server.url("/f"), Some(&sha256_hex(BODY)));
        spec.size_hint = Some(BODY.len() as u64);

        fx.run(&spec).await.unwrap();

        assert!(
            fx.reports.sizes().is_empty(),
            "a correct hint must not repaint every client's progress bar"
        );
    }

    #[tokio::test]
    async fn a_wrong_size_hint_is_corrected_once() {
        let server = Upstream::start(Serve::Ranged(BODY.to_vec())).await;
        let fx = Fixture::new(server.base(), None);
        let mut spec = spec(&server.url("/f"), Some(&sha256_hex(BODY)));
        spec.size_hint = Some(99);

        fx.run(&spec).await.unwrap();

        assert_eq!(fx.reports.sizes(), vec![BODY.len() as u64]);
    }

    #[tokio::test]
    async fn progress_is_reported_and_ends_at_the_full_count() {
        let server = Upstream::start(Serve::Trickle {
            chunk: b"abcd".to_vec(),
            count: 4,
            gap: Duration::from_millis(20),
            declare_length: true,
        })
        .await;
        let fx = Fixture::new(server.base(), None);

        let bytes = fx.run(&spec(&server.url("/f"), None)).await.unwrap();

        assert_eq!(bytes, 16);
        let progress = fx.reports.progress();
        assert_eq!(progress.first(), Some(&4), "the first chunk always reports");
        assert_eq!(progress.last(), Some(&16));
    }

    // -- the stall watchdog -------------------------------------------------

    #[tokio::test]
    async fn a_slow_but_progressing_stream_is_not_killed() {
        // 8 chunks 60 ms apart runs 480 ms against a 250 ms stall window, so a
        // watchdog that bounds TOTAL duration kills it and a per-chunk one does
        // not. Three chunks would finish inside the window and prove nothing.
        let body: Vec<u8> = (0..8).flat_map(|_| b"abcdefgh".to_vec()).collect();
        let server = Upstream::start(Serve::Trickle {
            chunk: b"abcdefgh".to_vec(),
            count: 8,
            gap: Duration::from_millis(60),
            declare_length: true,
        })
        .await;
        let fx = Fixture::new(server.base(), None);

        let bytes = fx
            .run(&spec(&server.url("/f"), Some(&sha256_hex(&body))))
            .await
            .unwrap();

        assert_eq!(bytes, 64);
        assert_eq!(fx.landed(), Some(body));
    }

    #[tokio::test]
    async fn a_silent_stream_is_aborted_and_says_so() {
        let server = Upstream::start(Serve::Silent {
            chunk: b"abcdefgh".to_vec(),
            count: 2,
            declared: 4096,
        })
        .await;
        let fx = Fixture::new(server.base(), None);

        let err = fx.run(&spec(&server.url("/f"), None)).await.unwrap_err();

        assert_eq!(err.code, ErrorCode::ServerUnavailable);
        assert!(err.message.contains("stalled"), "{}", err.message);
        assert!(
            fx.tmp().exists(),
            "a stall is transient: the partial survives for the retry"
        );
    }

    #[tokio::test]
    async fn a_host_that_never_answers_stalls_rather_than_hanging_forever() {
        // The watchdog covers the wait for response headers, not just the body.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let accepted = tokio::spawn(async move {
            let held = listener.accept().await;
            std::future::pending::<()>().await;
            drop(held);
        });
        let fx = Fixture::new("https://huggingface.co", None);

        let err = fx
            .run(&spec(&format!("http://{addr}/f"), None))
            .await
            .unwrap_err();

        assert_eq!(err.code, ErrorCode::ServerUnavailable);
        assert!(err.message.contains("stalled"), "{}", err.message);
        accepted.abort();
    }

    #[tokio::test]
    async fn a_cancel_beats_a_stall_to_the_error() {
        let server = Upstream::start(Serve::Silent {
            chunk: b"abcdefgh".to_vec(),
            count: 1,
            declared: 4096,
        })
        .await;
        let mut fx = Fixture::new(server.base(), None);
        // A stall window far longer than the cancel, so whichever error comes
        // back names which mechanism fired.
        //
        // What this does NOT cover, stated because the name suggests otherwise:
        // the pump's `biased`. Only one branch is ever ready here (the stream is
        // silent and the stall is five seconds away), so the cancel wins by
        // being the only thing that can, with or without the bias. Removing
        // `biased` survives this test, and pinning it would need both branches
        // ready at once and deterministically, which no test at this level can
        // arrange. Recorded as a survivor in `status.md` rather than papered
        // over with a flaky round-robin.
        fx.transfer = fx
            .transfer
            .with_intervals(Duration::from_secs(5), Duration::from_millis(10));
        let cancel = fx.cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            cancel.cancel();
        });

        let err = fx.run(&spec(&server.url("/f"), None)).await.unwrap_err();

        assert!(net::is_cancelled(&err), "{err:?}");
    }

    // -- the runaway ceiling ------------------------------------------------

    /// Every ceiling and watchdog test below injects its constants, which is
    /// what lets the boundary case sit ON the boundary. The cost is that those
    /// tests cannot see the SHIPPING values at all, so this covers the two
    /// things that go wrong on the other side of the injection seam.
    ///
    /// First, wiring: `new` must reach for the production constants and the two
    /// setters must assign to the fields they name. The arguments here are
    /// distinct and non-zero on purpose, because a transposed pair is the defect
    /// this is really watching for and equal values cannot see one.
    ///
    /// Second, magnitude, asserted as a range rather than an equality. Pinning
    /// `STALL_TIMEOUT == 60s` would be a change-detector: it fails on a
    /// deliberate retune to 45s, which is a decision rather than a defect. The
    /// property that actually matters is the one the doc comments claim, that
    /// the window is generous enough to fire on silence and never on slowness,
    /// and a bound catches the catastrophic edit (60s to 60ms) while leaving the
    /// tuning free.
    // Asserting on constants is the point here rather than an oversight: these
    // are the shipping values, and the bounds are what stops a catastrophic
    // retune landing unnoticed behind tests that all inject their own.
    #[allow(clippy::assertions_on_constants)]
    #[tokio::test]
    async fn the_shipping_constants_are_wired_through_and_the_injectors_do_not_transpose() {
        let transfer = Fixture::bare_transfer();
        assert_eq!(transfer.stall, STALL_TIMEOUT);
        assert_eq!(transfer.progress_interval, PROGRESS_INTERVAL);
        assert_eq!(transfer.ceiling_multiplier, CEILING_MULTIPLIER);
        assert_eq!(transfer.ceiling_slack, CEILING_SLACK);

        let tuned = Fixture::bare_transfer()
            .with_intervals(Duration::from_secs(7), Duration::from_secs(3))
            .with_ceiling(5, 11);
        assert_eq!(tuned.stall, Duration::from_secs(7));
        assert_eq!(tuned.progress_interval, Duration::from_secs(3));
        assert_eq!(tuned.ceiling_multiplier, 5);
        assert_eq!(tuned.ceiling_slack, 11);

        assert!(
            STALL_TIMEOUT >= Duration::from_secs(10),
            "the stall window has to fire on silence, never on slowness"
        );
        assert!(
            CEILING_MULTIPLIER >= 2 && CEILING_SLACK >= 1024 * 1024,
            "the ceiling has to be too generous to trip a legitimate download"
        );
        assert!(
            PROGRESS_INTERVAL >= Duration::from_millis(50),
            "each tick is a row write plus a broadcast to every client"
        );
    }

    #[tokio::test]
    async fn a_stream_that_exceeds_its_declared_size_is_cut_off_at_the_ceiling() {
        // The constants are injected so the boundary case sits ON the boundary:
        // ceiling = declared * 0 + 8, and 4-byte chunks cross it at 12 bytes.
        let server = Upstream::start(Serve::Trickle {
            chunk: b"abcd".to_vec(),
            count: 8,
            gap: Duration::from_millis(1),
            declare_length: true,
        })
        .await;
        let mut fx = Fixture::new(server.base(), None);
        fx.transfer = fx.transfer.with_ceiling(0, 8);

        let err = fx.run(&spec(&server.url("/f"), None)).await.unwrap_err();

        assert_eq!(err.code, ErrorCode::ProviderError);
        assert!(err.message.contains("far exceeded"), "{}", err.message);
    }

    #[tokio::test]
    async fn a_stream_that_stays_under_the_ceiling_completes() {
        // The other side of the same boundary: 32 bytes against a ceiling of 32.
        let server = Upstream::start(Serve::Trickle {
            chunk: b"abcd".to_vec(),
            count: 8,
            gap: Duration::from_millis(1),
            declare_length: true,
        })
        .await;
        let mut fx = Fixture::new(server.base(), None);
        fx.transfer = fx.transfer.with_ceiling(0, 32);

        assert_eq!(fx.run(&spec(&server.url("/f"), None)).await.unwrap(), 32);
    }

    #[test]
    fn an_absurd_declared_size_saturates_rather_than_overflowing() {
        // The operands come off a response header, so the origin picks them.
        // Plain `declared * multiplier + slack` at 2^63 panics the transfer
        // worker in debug and WRAPS in release, where the ceiling collapses to
        // 64 MiB and the next legitimate multi-gigabyte download is aborted for
        // "far exceeding" a size it is nowhere near.
        assert_eq!(
            ceiling(u64::MAX, CEILING_MULTIPLIER, CEILING_SLACK),
            u64::MAX
        );
        assert_eq!(ceiling(1 << 63, 2, CEILING_SLACK), u64::MAX);
        assert_eq!(ceiling(u64::MAX, 1, 1), u64::MAX);
        // And the ordinary case is unchanged, so the fix is not a widening.
        assert_eq!(ceiling(100, 2, 64), 264);
        assert_eq!(ceiling(0, 2, 8), 8);
    }

    #[tokio::test]
    async fn a_host_that_declares_an_absurd_length_does_not_kill_its_worker() {
        // End to end over a real socket, because the panic is what wedged the
        // queue: the worker unwound before its `in_flight.remove`, so the
        // waiter's sender leaked inside the map entry, `enqueue` never
        // resolved, and cancel, retry and remove all no-opped on that id
        // forever. Here the transfer must simply come back with an error.
        // 2^63, which is what makes `declared * 2` overflow exactly. `u64::MAX`
        // is not usable here: hyper refuses to send a `content-length` it
        // cannot represent as a body length, so the request never leaves.
        let server = Upstream::start(Serve::Silent {
            chunk: b"abcd".to_vec(),
            count: 1,
            declared: 1 << 63,
        })
        .await;
        let mut fx = Fixture::new(server.base(), None);
        fx.transfer = fx
            .transfer
            .with_intervals(Duration::from_millis(150), Duration::from_millis(1));

        let err = fx.run(&spec(&server.url("/f"), None)).await.unwrap_err();

        assert_eq!(
            err.code,
            ErrorCode::ServerUnavailable,
            "an absurd size is not itself a runaway stream; the silence is what ends it: {err:?}"
        );
        assert!(err.message.contains("stalled"), "{}", err.message);
    }

    #[tokio::test]
    async fn a_body_with_no_declared_length_has_no_ceiling() {
        // Stated rather than fixed: the ceiling arms only when content-length
        // was present and parsable, so a chunked response has none at all.
        let server = Upstream::start(Serve::Trickle {
            chunk: b"abcd".to_vec(),
            count: 8,
            gap: Duration::from_millis(1),
            declare_length: false,
        })
        .await;
        let mut fx = Fixture::new(server.base(), None);
        fx.transfer = fx.transfer.with_ceiling(0, 0);

        assert_eq!(fx.run(&spec(&server.url("/f"), None)).await.unwrap(), 32);
    }

    // -- refusals -----------------------------------------------------------

    #[tokio::test]
    async fn a_source_with_no_url_is_refused_before_anything_opens_a_socket() {
        let fx = Fixture::new("https://huggingface.co", None);
        let mut spec = spec("", None);
        spec.source = "/an/absolute/path.gguf".into();
        spec.url = None;

        let err = fx.run(&spec).await.unwrap_err();

        assert_eq!(err.code, ErrorCode::ValidationError);
        assert_eq!(err.message, "non-downloadable source");
        assert_eq!(fx.net.calls(), 0);
    }

    #[tokio::test]
    async fn an_empty_url_is_present_and_refused_rather_than_reparsed() {
        // `url: ""` is PRESENT, so the parsed source is never substituted for it.
        let fx = Fixture::new("https://huggingface.co", None);

        let err = fx.run(&spec("", None)).await.unwrap_err();

        assert_eq!(err.message, "non-downloadable source");
        assert_eq!(fx.net.calls(), 0);
    }

    #[tokio::test]
    async fn an_unsafe_source_is_refused_by_the_parser() {
        let fx = Fixture::new("https://huggingface.co", None);
        let mut spec = spec("", None);
        spec.source = "@u/r/main/../../etc/passwd".into();
        spec.url = None;

        let err = fx.run(&spec).await.unwrap_err();

        assert_eq!(err.code, ErrorCode::ValidationError);
        assert!(err.message.contains("unsafe source component"));
        assert_eq!(fx.net.calls(), 0);
    }

    #[test]
    fn a_partial_is_named_by_suffixing_the_destination() {
        assert_eq!(
            tmp_path_for(Path::new("/models/u/r/model.gguf")),
            PathBuf::from("/models/u/r/model.gguf.tmp")
        );
    }

    #[test]
    fn only_a_64_character_hex_string_is_a_content_hash() {
        assert!(is_sha256_hex(&"a".repeat(64)));
        assert!(is_sha256_hex(&"A".repeat(64)));
        assert!(!is_sha256_hex(&"a".repeat(63)));
        assert!(!is_sha256_hex(&"a".repeat(65)));
        assert!(!is_sha256_hex(&"g".repeat(64)));
        assert!(!is_sha256_hex(""));
    }
}
