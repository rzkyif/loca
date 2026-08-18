//! The one outbound chokepoint for everything this package fetches.
//!
//! # Why it is a type rather than a convention
//!
//! `AGENTS.md`'s rule is absolute: once installed, Core makes NO outbound
//! request without an explicit user action. This package has four outbound
//! paths and exactly one of them is not behind a button, so "we were careful"
//! is not an implementation strategy. Every request under `update/` goes
//! through one [`Net`], which owns the only HTTP client here and counts every
//! call it makes.
//!
//! | path                                     | authorised by                    |
//! | ---------------------------------------- | -------------------------------- |
//! | `core.json` for `GET /update/check`      | the "Check for Updates" button   |
//! | `core.json` + artifacts for `POST /apply` | the "Update" button             |
//! | `core.json` + artifacts for `self-install` | the install phase (see below)  |
//! | the built-in extension plant in `bootstrap` | the install phase             |
//!
//! The last two are the exception `AGENTS.md` names: the install phase itself,
//! the `install-service` subcommand the native installers delegate to, which
//! runs before the app is considered installed. Both are reachable only from a
//! short-lived CLI process someone invoked. **Nothing in this package fetches
//! at boot or on a timer**, and that is the property to keep.
//!
//! # `calls()` counts THIS package, not the core
//!
//! Stated plainly because a counter is exactly the thing a later reader will
//! over-trust. `downloads::net::Net` has an identical counter over the download
//! engine's requests, `models::catalog` fetches through `reqwest` directly and
//! is invisible to both, and this one sees only `update/`. **No single counter
//! here is a core-wide total of outbound requests.** Each is a per-package
//! chokepoint and the consent tests that read one are asserting about that
//! package's paths.
//!
//! Two chokepoints of the same shape is a duplicated control, which D-049 says
//! is the kind most worth hoisting. It is not hoisted yet for three reasons
//! (half this package runs in a CLI with no `AppContext` and would construct
//! its own instance anyway; the gunzip-then-hash verb `artifact.rs` needs does
//! not exist there and adding it would be a write into D7's owned path; and the
//! counter's meaning is already per-package). The hoist is a **tracked Stage 5
//! item**, not a note here, precisely so it does not become permanent by
//! default.
//!
//! # The counter has its own test, and that is not decoration
//!
//! Every consent test asserts `calls() == 0`. Deleting the increment leaves all
//! of them green, because a counter that never counts always reads zero, so the
//! whole enforcement would rest on an assertion that cannot fail. That is
//! D-050 case 3, which cost D7 exactly this. It is paired here with a test that
//! the counter goes UP.

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
///
/// `shared/net.ts:14`'s `FETCH_TIMEOUT_MS`, which is also D7's
/// `SMALL_REQUEST_TIMEOUT`. The two packages already agree on this number; it
/// is not a new constant.
pub const SMALL_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// How long a STREAM may be silent before it is abandoned.
///
/// An idle window rather than a wall-clock cap, because the core binary is
/// ~100 MB and a slow-but-progressing transfer must not be killed while a dead
/// socket fails fast. `shared/net.ts:19`'s `DOWNLOAD_STALL_MS`, and the same
/// number as D7's stall watchdog.
///
/// It is armed BEFORE the connect, not after the first byte, so it also bounds
/// a hung TLS handshake. That is `shared/net.ts:60-63`, where `arm()` runs
/// above the `fetch`.
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

    /// Shrink the two bounds so a test can sit on the deadline instead of
    /// waiting out the shipping value.
    ///
    /// The shipping values are asserted separately, with distinct non-zero
    /// arguments, by `the_shipping_bounds_are_wired_through_and_not_transposed`.
    /// Every timing test injecting its own constants would otherwise leave the
    /// production wiring covered by nothing, which is D7's mutation 27.
    #[cfg(test)]
    pub fn with_bounds(mut self, small_request: Duration, stall: Duration) -> Self {
        self.small_request_timeout = small_request;
        self.stall_timeout = stall;
        self
    }

    /// A GET whose body is a small document: a signed manifest.
    ///
    /// Returns the TEXT rather than a parsed value, because a signature covers
    /// BYTES. See `manifest.rs`: handing back a parsed struct would give the
    /// caller a second chance to differ from what was signed, which is the
    /// defect D-065 records.
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
    /// the built-in extension tarball.
    ///
    /// The stall deadline covers the connect as well as every chunk, so a host
    /// that accepts the TCP connection and then says nothing fails here rather
    /// than hanging an install at "Starting the Core".
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
                // The oracle's wording (`shared/net.ts:66`), kept so a support
                // log reads the same across the cutover.
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
    /// `mint-code` talks to a freshly started core over TLS on loopback, using
    /// the self-signed certificate the core wrote to `tls-cert.pem` at boot.
    /// The point of the whole exercise is that verification stays **on**:
    /// disabling it would make the pairing mint accept any listener that
    /// answered on the port, which is the one request in the package that
    /// carries the admin token.
    ///
    /// Both bounds collapse to `probe_timeout` here, because every request this
    /// client makes is a small loopback exchange that the core answers in
    /// milliseconds. A probe that has not returned in seconds is a stalled
    /// connection rather than a slow one.
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

    /// A bounded GET used as a liveness probe. `true` only on a 2xx.
    ///
    /// Every other outcome, including a TLS failure against an untrusted
    /// certificate, is `false` and the caller retries. That is right for a
    /// probe: a core mid-boot and a core serving the wrong certificate both
    /// mean "not ready yet" to something that is about to retry anyway.
    pub async fn probe_ok(&self, url: &str) -> bool {
        self.calls.fetch_add(1, Ordering::Relaxed);
        let deadline = self.small_request_timeout;
        let probe = async { self.client.get(url).send().await };
        match tokio::time::timeout(deadline, probe).await {
            Ok(Ok(res)) => res.status().is_success(),
            _ => false,
        }
    }

    /// A bounded POST with headers, returning the body only on a 2xx.
    ///
    /// Three outcomes, kept distinct because the caller retries on two of them
    /// and only one is a result: `Err` is a transport failure, `Ok(None)` is a
    /// response the core refused, and `Ok(Some(body))` is an answer.
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
                // The body is dropped rather than read, which is the Rust
                // equivalent of the oracle's explicit `res.body?.cancel()`.
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

    /// How many outbound requests this instance has made.
    ///
    /// Read only by tests. See the module docs: it is a per-package count, not
    /// a core-wide one.
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
    /// The next chunk, `None` at a clean end of body.
    ///
    /// The deadline is per chunk rather than over the whole body, which is what
    /// makes it a stall guard rather than a size limit: an hour-long download
    /// that never pauses for a minute is fine, and a one-byte download that
    /// pauses forever is not.
    pub async fn next_chunk(&mut self) -> Option<Result<Chunk>> {
        match tokio::time::timeout(self.stall, self.stream.next()).await {
            Err(_elapsed) => Some(Err(stalled(&self.url, self.stall))),
            Ok(chunk) => chunk,
        }
    }
}

/// Deliberately no client-level total-request timeout.
///
/// It would apply to the streaming GET too, and the core binary is ~100 MB on a
/// connection this code cannot predict, so any deadline that fits one artifact
/// fails another. Silence during a transfer is bounded by [`STALL_TIMEOUT`]
/// instead; the small verb gets [`SMALL_REQUEST_TIMEOUT`] per call.
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
        // The test that makes every `calls() == 0` assertion mean something.
        // Without it, deleting the increment leaves the whole consent suite
        // green: zero is what they expect, and a counter that never counts
        // always reads zero. D-050 case 3.
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
        // A host that accepts the connection and never answers is the only
        // shape a per-request deadline defends against: nothing fails, so the
        // caller waits forever. `/api/v1/update/check` runs inside an HTTP
        // handler with no timeout layer in the router, so unbounded here means
        // a request that never returns.
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
        // Arming before the connect rather than after the first byte is what
        // covers this case at all: with the deadline armed on first chunk, a
        // handshake that never completes is not a stall and never fires.
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
        // The converse, and the reason the bound is an idle window rather than
        // a wall clock: this transfer takes 5x the stall window in total and
        // must survive. A test for the stall alone passes just as happily
        // against a total-duration cap, which is the wrong bound.
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
        // Every timing test above injects its own constants, which is right (it
        // lets the assertion sit on the boundary), and it leaves the SHIPPING
        // wiring covered by nothing: `Net::new` could swap the two fields and
        // the suite would stay green. D7's mutation 27.
        //
        // The magnitudes are bounded rather than pinned, so a deliberate retune
        // stays legal while a transposition or a unit slip does not. The two
        // values are distinct, because equal ones cannot see a swap.
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
        // Read from the directory rather than a hand-kept list, so a new file
        // joins the rule by existing. The chokepoint is only a chokepoint while
        // it is the only thing naming the HTTP crate.
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/update");
        let mut offenders = Vec::new();
        walk(&dir, &mut |path: &std::path::Path| {
            if path.file_name().and_then(|n| n.to_str()) == Some("net.rs") {
                return;
            }
            let body = std::fs::read_to_string(path).unwrap_or_default();
            // `reqwest::` rather than the bare word, so the module docs and
            // this comment do not trip it.
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
