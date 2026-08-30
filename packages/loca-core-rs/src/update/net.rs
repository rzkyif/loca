//! The one outbound chokepoint for everything this package fetches.
//!
//! Every request under `update/` goes through one [`Net`], which owns the only
//! HTTP client here. Nothing in this package fetches at boot or on a timer: the
//! manifest and artifact fetches hang off the check/update buttons, and the
//! `self-install` and built-in-extension plant off the install phase, which
//! `AGENTS.md` names as the one exception.
//!
//! [`Net::calls`] counts THIS package only, not the core: `downloads::net::Net`
//! has its own counter and other modules reach the network directly. It is
//! tested to go UP, since a counter that never counts leaves every
//! `calls() == 0` consent assertion green by construction.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures_util::stream::BoxStream;
use futures_util::StreamExt;

use crate::error::{AppError, ErrorCode, Result};

/// One streamed piece of a response body, uncopied across the boundary.
pub type Chunk = hyper::body::Bytes;

/// How long a SMALL request may take in total: the signed manifest, the
/// built-in extension manifest.
pub const SMALL_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// How long a STREAM may be silent before it is abandoned.
///
/// An idle window rather than a wall-clock cap: the core binary is ~100 MB, so
/// a slow-but-progressing transfer must survive while a dead socket fails fast.
/// Armed BEFORE the connect, so it also bounds a hung TLS handshake.
pub const STALL_TIMEOUT: Duration = Duration::from_secs(60);

/// The outbound side of the update and install paths.
pub struct Net {
    client: reqwest::Client,
    calls: Arc<AtomicU64>,
    small_request_timeout: Duration,
    stall_timeout: Duration,
}

impl Net {
    pub fn new() -> Result<Self> {
        Ok(Self {
            client: build_client()?,
            calls: Arc::new(AtomicU64::new(0)),
            small_request_timeout: SMALL_REQUEST_TIMEOUT,
            stall_timeout: STALL_TIMEOUT,
        })
    }

    /// Shrink the two bounds so a test can sit on the deadline. The shipping
    /// values are asserted separately, by
    /// `the_shipping_bounds_are_wired_through_and_not_transposed`.
    #[cfg(test)]
    pub fn with_bounds(mut self, small_request: Duration, stall: Duration) -> Self {
        self.small_request_timeout = small_request;
        self.stall_timeout = stall;
        self
    }

    /// A GET whose body is a small document: a signed manifest. Returns TEXT
    /// rather than a parsed value, because a signature covers bytes.
    pub async fn text(&self, url: &str) -> Result<String> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        let deadline = self.small_request_timeout;
        let fetch = async {
            let res = self
                .client
                .get(url)
                .send()
                .await
                .map_err(|err| unreachable_host(url, &err))?;
            let status = res.status();
            if !status.is_success() {
                return Err(http_status(url, status.as_u16(), status.canonical_reason()));
            }
            res.text().await.map_err(|err| {
                AppError::new(
                    ErrorCode::ManifestFetchFailed,
                    format!("could not read {url}: {err}"),
                )
            })
        };
        match tokio::time::timeout(deadline, fetch).await {
            Err(_elapsed) => Err(timed_out(url, deadline)),
            Ok(text) => text,
        }
    }

    /// A GET whose body is streamed to disk: a core binary, a worker, a helper,
    /// the built-in extension tarball. The stall deadline covers the connect as
    /// well as every chunk.
    pub async fn stream(&self, url: &str) -> Result<NetStream> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        let stall = self.stall_timeout;
        let connect = async {
            let res = self
                .client
                .get(url)
                .send()
                .await
                .map_err(|err| unreachable_host(url, &err))?;
            let status = res.status();
            if !status.is_success() {
                return Err(AppError::new(
                    ErrorCode::InternalError,
                    format!("download HTTP {} for {url}", status.as_u16()),
                ));
            }
            Ok(res)
        };
        let res = match tokio::time::timeout(stall, connect).await {
            Err(_elapsed) => return Err(stalled(url, stall)),
            Ok(res) => res?,
        };
        let for_errors = url.to_string();
        let stream = res
            .bytes_stream()
            .map(move |chunk| {
                chunk.map_err(|err| {
                    AppError::new(
                        ErrorCode::InternalError,
                        format!("download from {for_errors} failed: {err}"),
                    )
                })
            })
            .boxed();
        Ok(NetStream {
            url: url.to_string(),
            stall,
            stream,
        })
    }

    /// A client that trusts exactly one certificate: a CA of one.
    ///
    /// `mint-code` talks to a freshly started core over loopback TLS using the
    /// self-signed cert it wrote at boot. Verification stays **on**: disabling
    /// it would let the mint, which carries the admin token, accept any
    /// listener on the port. Both bounds collapse to `probe_timeout`, since
    /// every request here is a small loopback exchange.
    pub fn with_root_certificate(pem: &str, probe_timeout: Duration) -> Result<Self> {
        let certificate = reqwest::Certificate::from_pem(pem.as_bytes()).map_err(|err| {
            AppError::new(
                ErrorCode::InternalError,
                format!("the core's TLS certificate could not be read: {err}"),
            )
        })?;
        let client = reqwest::Client::builder()
            .add_root_certificate(certificate)
            .timeout(probe_timeout)
            .build()
            .map_err(|err| {
                AppError::new(
                    ErrorCode::InternalError,
                    format!("could not build the loopback HTTP client: {err}"),
                )
            })?;
        Ok(Self {
            client,
            calls: Arc::new(AtomicU64::new(0)),
            small_request_timeout: probe_timeout,
            stall_timeout: probe_timeout,
        })
    }

    /// A bounded GET used as a liveness probe. `true` only on a 2xx; every
    /// other outcome, TLS failures included, is "not ready yet" and retried.
    pub async fn probe_ok(&self, url: &str) -> bool {
        self.calls.fetch_add(1, Ordering::Relaxed);
        let deadline = self.small_request_timeout;
        let probe = async { self.client.get(url).send().await };
        match tokio::time::timeout(deadline, probe).await {
            Ok(Ok(res)) => res.status().is_success(),
            _ => false,
        }
    }

    /// A bounded POST with headers, returning the body only on a 2xx. `Err` is
    /// a transport failure, `Ok(None)` a refusal; the caller retries on both.
    pub async fn post_json(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        body: &str,
    ) -> Result<Option<String>> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        let deadline = self.small_request_timeout;
        let post = async {
            let mut request = self
                .client
                .post(url)
                .header("Content-Type", "application/json");
            for (name, value) in headers {
                request = request.header(*name, *value);
            }
            let res = request
                .body(body.to_string())
                .send()
                .await
                .map_err(|err| unreachable_host(url, &err))?;
            if !res.status().is_success() {
                return Ok(None);
            }
            res.text().await.map(Some).map_err(|err| {
                AppError::new(
                    ErrorCode::InternalError,
                    format!("could not read the response from {url}: {err}"),
                )
            })
        };
        match tokio::time::timeout(deadline, post).await {
            Err(_elapsed) => Err(timed_out(url, deadline)),
            Ok(result) => result,
        }
    }

    /// How many outbound requests this instance has made. Read only by tests,
    /// and per-package rather than core-wide.
    #[cfg(test)]
    pub fn calls(&self) -> u64 {
        self.calls.load(Ordering::Relaxed)
    }
}

/// A response body being read, with the stall deadline re-armed per chunk.
pub struct NetStream {
    url: String,
    stall: Duration,
    stream: BoxStream<'static, Result<Chunk>>,
}

impl std::fmt::Debug for NetStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NetStream")
            .field("url", &self.url)
            .finish_non_exhaustive()
    }
}

impl NetStream {
    /// The next chunk, `None` at a clean end of body. The deadline is per
    /// chunk, making it a stall guard rather than a size limit.
    pub async fn next_chunk(&mut self) -> Option<Result<Chunk>> {
        match tokio::time::timeout(self.stall, self.stream.next()).await {
            Err(_elapsed) => Some(Err(stalled(&self.url, self.stall))),
            Ok(chunk) => chunk,
        }
    }
}

/// Deliberately no client-level total-request timeout: it would also cap the
/// streaming GET, where no fixed deadline fits every artifact. Silence is
/// bounded by [`STALL_TIMEOUT`], the small verb by [`SMALL_REQUEST_TIMEOUT`].
fn build_client() -> Result<reqwest::Client> {
    reqwest::Client::builder().build().map_err(|err| {
        AppError::new(
            ErrorCode::InternalError,
            format!("could not build the update HTTP client: {err}"),
        )
    })
}

fn unreachable_host(url: &str, err: &reqwest::Error) -> AppError {
    AppError::new(
        ErrorCode::ServerUnavailable,
        format!("could not reach {url}: {err}"),
    )
}

fn http_status(url: &str, status: u16, reason: Option<&str>) -> AppError {
    AppError::new(
        ErrorCode::ManifestFetchFailed,
        format!("HTTP {status} {} for {url}", reason.unwrap_or_default()),
    )
}

fn timed_out(url: &str, after: Duration) -> AppError {
    AppError::new(
        ErrorCode::ServerUnavailable,
        format!("{url} did not answer within {}s; aborting", after.as_secs()),
    )
}

fn stalled(url: &str, after: Duration) -> AppError {
    AppError::new(
        ErrorCode::ServerUnavailable,
        format!(
            "download from {url} stalled (no data for {}ms)",
            after.as_millis()
        ),
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::update::testing::{bounded, Serve, Upstream};

    #[tokio::test]
    async fn a_text_and_a_stream_each_count_once() {
        // Makes every `calls() == 0` assertion mean something: a counter that
        // never counts always reads zero.
        let up = Upstream::start(Serve::Body(b"hello".to_vec())).await;
        let net = Net::new().unwrap();
        assert_eq!(net.calls(), 0);

        net.text(&up.url("/doc")).await.unwrap();
        assert_eq!(net.calls(), 1);

        let mut stream = net.stream(&up.url("/blob")).await.unwrap();
        while let Some(chunk) = stream.next_chunk().await {
            chunk.unwrap();
        }
        assert_eq!(net.calls(), 2);
    }

    #[tokio::test]
    async fn a_non_success_status_is_a_manifest_fetch_failure() {
        let up = Upstream::start(Serve::Status(404)).await;
        let net = Net::new().unwrap();
        let err = net.text(&up.url("/manifests/core.json")).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::ManifestFetchFailed);
        assert!(err.message.contains("404"), "{}", err.message);
    }

    #[tokio::test]
    async fn a_black_holed_host_bounds_the_small_verb() {
        // `/api/v1/update/check` runs in a handler with no timeout layer, so
        // unbounded here means a request that never returns.
        let up = Upstream::start(Serve::Never).await;
        let net = Net::new()
            .unwrap()
            .with_bounds(Duration::from_millis(150), Duration::from_millis(150));
        let err = bounded(net.text(&up.url("/hangs"))).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::ServerUnavailable);
        assert!(err.message.contains("did not answer"), "{}", err.message);
    }

    #[tokio::test]
    async fn a_stream_that_never_connects_is_bounded_by_the_stall_window() {
        // With the deadline armed on first chunk instead, a handshake that
        // never completes is not a stall and never fires.
        let up = Upstream::start(Serve::Never).await;
        let net = Net::new()
            .unwrap()
            .with_bounds(Duration::from_secs(30), Duration::from_millis(150));
        let err = bounded(net.stream(&up.url("/hangs"))).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::ServerUnavailable);
        assert!(err.message.contains("stalled"), "{}", err.message);
    }

    #[tokio::test]
    async fn a_stream_that_falls_silent_mid_body_is_abandoned() {
        let up = Upstream::start(Serve::Silent {
            chunk: vec![7u8; 8],
            count: 2,
        })
        .await;
        let net = Net::new()
            .unwrap()
            .with_bounds(Duration::from_secs(30), Duration::from_millis(200));
        let mut stream = net.stream(&up.url("/blob")).await.unwrap();
        let mut seen = 0usize;
        let err = bounded(async {
            loop {
                match stream.next_chunk().await {
                    Some(Ok(chunk)) => seen += chunk.len(),
                    Some(Err(err)) => return err,
                    None => panic!("d13: the silent upstream closed the body"),
                }
            }
        })
        .await;
        assert_eq!(seen, 16, "the chunks that did arrive were delivered");
        assert!(err.message.contains("stalled"), "{}", err.message);
    }

    #[tokio::test]
    async fn a_slow_but_progressing_stream_is_not_killed() {
        // The converse: this transfer takes 5x the stall window in total and
        // must survive, which a total-duration cap would not allow.
        let up = Upstream::start(Serve::Trickle {
            chunk: vec![3u8; 4],
            count: 5,
            gap: Duration::from_millis(40),
        })
        .await;
        let net = Net::new()
            .unwrap()
            .with_bounds(Duration::from_secs(30), Duration::from_millis(100));
        let mut stream = net.stream(&up.url("/blob")).await.unwrap();
        let mut seen = 0usize;
        bounded(async {
            while let Some(chunk) = stream.next_chunk().await {
                seen += chunk.unwrap().len();
            }
        })
        .await;
        assert_eq!(seen, 20);
    }

    #[test]
    fn the_shipping_bounds_are_wired_through_and_not_transposed() {
        // The timing tests above inject their own constants, leaving the
        // shipping wiring uncovered: `Net::new` could swap the two fields. The
        // magnitudes are bounded rather than pinned, so a retune stays legal
        // while a transposition or unit slip does not.
        let net = Net::new().unwrap();
        assert_eq!(net.small_request_timeout, SMALL_REQUEST_TIMEOUT);
        assert_eq!(net.stall_timeout, STALL_TIMEOUT);
        assert!(
            net.stall_timeout > net.small_request_timeout,
            "a stream may be silent for longer than a whole small request may take"
        );
        assert!(net.small_request_timeout >= Duration::from_secs(5));
        assert!(net.stall_timeout <= Duration::from_secs(300));
    }

    #[test]
    fn no_module_outside_net_reaches_the_network() {
        // Read from the directory, so a new file joins the rule by existing.
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/update");
        let mut offenders = Vec::new();
        walk(&dir, &mut |path: &std::path::Path| {
            if path.file_name().and_then(|n| n.to_str()) == Some("net.rs") {
                return;
            }
            let body = std::fs::read_to_string(path).unwrap_or_default();
            // `reqwest::` rather than the bare word, so this comment passes.
            if body.contains("reqwest::") {
                offenders.push(path.display().to_string());
            }
        });
        assert!(
            offenders.is_empty(),
            "these files reach the network directly instead of through net.rs: {offenders:?}"
        );
    }

    fn walk(dir: &std::path::Path, visit: &mut impl FnMut(&std::path::Path)) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, visit);
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                visit(&path);
            }
        }
    }
}
