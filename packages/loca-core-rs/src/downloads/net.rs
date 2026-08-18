//! The one outbound chokepoint for everything this package fetches.
//!
//! # Why it is a type rather than a convention
//!
//! `AGENTS.md`'s rule is absolute: once installed, Core makes NO outbound
//! request without an explicit user action, and this package is where that rule
//! lives. "We were careful" is not an implementation strategy, so every request
//! the download engine makes goes through one [`Net`], which owns the only HTTP
//! clients in `downloads/` and counts every call it makes.
//!
//! The counter is the structural half. Tests assert it reads **zero** across the
//! paths that must never reach the network (a local-only probe, a file already
//! on disk, boot with an empty queue), so flipping a local branch to a network
//! one turns a test red rather than shipping quietly. A counter can be defeated;
//! that is the point, and it is why the mutation for each of those tests is
//! staged and watched failing rather than assumed.
//!
//! The second half is that no sibling file names `reqwest` at all, which
//! `no_module_outside_net_reaches_the_network` enforces by reading the directory
//! rather than a hand-kept list, so a new file joins the rule by existing.
//!
//! # Two clients, one chokepoint
//!
//! reqwest's redirect policy is per-client, and this package needs both: a
//! following client for transfers, and a non-following one for HuggingFace's
//! resolve redirect, whose 302 is what carries `x-linked-etag` (the content
//! sha256) and `x-linked-size`. Following it loses both to a CDN that publishes
//! neither. Both clients live here and the counter covers both.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures_util::stream::BoxStream;
use futures_util::StreamExt;

use crate::downloads::cancel::CancellationToken;
use crate::error::{AppError, ErrorCode, Result};

/// One streamed piece of a response body. `hyper`'s re-export rather than a
/// `Vec<u8>`, so a chunk crosses this boundary without a copy.
pub type Chunk = hyper::body::Bytes;

/// Whether a request follows redirects. See the module docs: the HuggingFace
/// size and hash both live on the redirect itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Redirect {
    Follow,
    Manual,
}

/// How long a SMALL request may take in total: a HEAD, a manifest, a GitHub
/// release lookup.
///
/// Only the streaming GET is exempt (see [`build_client`]'s note), because a
/// model weight is gigabytes and any deadline that fits one fails another. The
/// small verbs have no such excuse, and without a bound a black-holed host
/// wedges whatever called them: the queue's single permit while a hash HEAD
/// hangs, or an HTTP handler forever, since there is no timeout layer in the
/// router either.
pub const SMALL_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// The outbound side of the download engine.
pub struct Net {
    follow: reqwest::Client,
    manual: reqwest::Client,
    calls: Arc<AtomicU64>,
    small_request_timeout: Duration,
}

impl Net {
    pub fn new() -> Result<Self> {
        Ok(Self {
            follow: build_client(reqwest::redirect::Policy::default())?,
            manual: build_client(reqwest::redirect::Policy::none())?,
            calls: Arc::new(AtomicU64::new(0)),
            small_request_timeout: SMALL_REQUEST_TIMEOUT,
        })
    }

    /// Shorten the small-request deadline, so a test can watch it fire without
    /// waiting the shipping 30 seconds. Injected rather than mocked for the
    /// reason D-038 gives: the deadline is what is under test, so it has to run
    /// on a real clock against a real socket.
    pub fn with_small_request_timeout(mut self, timeout: Duration) -> Self {
        self.small_request_timeout = timeout;
        self
    }

    /// A HEAD, for a size hint or a content hash.
    ///
    /// Cancellable because its caller is a transfer that a user can stop: the
    /// hash lookup runs before the first byte is requested, so a HEAD to a
    /// black-holed host would otherwise outlive the cancel that was meant to
    /// stop the whole transfer. Bounded as well as cancellable, because the
    /// commonest caller (the etag lookup) runs inside the queue's single permit
    /// with no user watching, so silence there stalls every queued download.
    pub async fn head(
        &self,
        url: &str,
        follow: Redirect,
        cancel: &CancellationToken,
    ) -> Result<Head> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        let client = match follow {
            Redirect::Follow => &self.follow,
            Redirect::Manual => &self.manual,
        };
        let deadline = self.small_request_timeout;
        let sent = tokio::time::timeout(deadline, client.head(url).send());
        let res = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(cancelled()),
            res = sent => match res {
                Err(_elapsed) => return Err(timed_out(url, deadline)),
                Ok(res) => res.map_err(|err| unreachable_host(url, &err))?,
            },
        };
        Ok(Head {
            status: res.status().as_u16(),
            headers: res.headers().clone(),
        })
    }

    /// A GET, optionally resuming at `from`, streamed rather than buffered.
    ///
    /// `from` of `Some(0)` sends no `Range`: a range starting at zero is the
    /// whole resource, and asking for it explicitly makes a server answer 206
    /// where a plain GET would answer 200, which is a difference the transfer
    /// then has to unpick for no gain.
    pub async fn get_range(
        &self,
        url: &str,
        from: Option<u64>,
        cancel: &CancellationToken,
    ) -> Result<NetBody> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        let mut req = self.follow.get(url);
        if let Some(offset) = from.filter(|n| *n > 0) {
            req = req.header(reqwest::header::RANGE, format!("bytes={offset}-"));
        }
        let sent = req.send();
        let res = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(cancelled()),
            res = sent => res.map_err(|err| unreachable_host(url, &err))?,
        };
        let status = res.status();
        let content_length = header_u64(res.headers(), "content-length");
        let url = url.to_string();
        let stream = res
            .bytes_stream()
            .map(move |chunk| {
                chunk.map_err(|err| {
                    AppError::new(
                        ErrorCode::ProviderError,
                        format!("download for {url} failed mid-stream: {err}"),
                    )
                })
            })
            .boxed();
        Ok(NetBody {
            status: status.as_u16(),
            status_text: status.canonical_reason().unwrap_or_default().to_string(),
            content_length,
            stream,
        })
    }

    /// A GET whose body is a small document: the signed manifests and the
    /// GitHub release lookup. Buffered, because both are small and both are
    /// consumed as a whole document rather than streamed.
    ///
    /// Returns the TEXT rather than a parsed value because a signature covers
    /// the bytes; see the manifest module's docs on why re-encoding a parsed
    /// struct is a second chance to differ.
    pub async fn text(&self, url: &str, cancel: &CancellationToken) -> Result<String> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        let deadline = self.small_request_timeout;
        let fetch = async {
            let res = self
                .follow
                .get(url)
                .send()
                .await
                .map_err(|err| unreachable_host(url, &err))?;
            let status = res.status();
            if !status.is_success() {
                return Err(AppError::new(
                    ErrorCode::ManifestFetchFailed,
                    format!(
                        "HTTP {} {} for {url}",
                        status.as_u16(),
                        status.canonical_reason().unwrap_or_default()
                    ),
                ));
            }
            res.text().await.map_err(|err| {
                AppError::new(
                    ErrorCode::ManifestFetchFailed,
                    format!("could not read {url}: {err}"),
                )
            })
        };
        tokio::select! {
            biased;
            () = cancel.cancelled() => Err(cancelled()),
            answered = tokio::time::timeout(deadline, fetch) => match answered {
                Err(_elapsed) => Err(timed_out(url, deadline)),
                Ok(text) => text,
            },
        }
    }

    /// [`Self::text`], parsed.
    pub async fn json(&self, url: &str, cancel: &CancellationToken) -> Result<serde_json::Value> {
        let text = self.text(url, cancel).await?;
        serde_json::from_str(&text).map_err(|err| {
            AppError::new(
                ErrorCode::ManifestFetchFailed,
                format!("malformed JSON from {url}: {err}"),
            )
        })
    }

    /// How many outbound requests this instance has made. The consent tests
    /// assert it is zero on every path that must not reach the network.
    #[cfg(test)]
    pub fn calls(&self) -> u64 {
        self.calls.load(Ordering::Relaxed)
    }
}

/// The host of `url`, or `None` when it does not parse as one.
///
/// Here rather than at the call site so URL parsing stays inside the one module
/// that is allowed to name the HTTP crate.
pub fn host_of(url: &str) -> Option<String> {
    reqwest::Url::parse(url)
        .ok()?
        .host_str()
        .map(|host| host.to_string())
}

/// A HEAD response, reduced to what this package reads off one.
#[derive(Debug)]
pub struct Head {
    status: u16,
    headers: reqwest::header::HeaderMap,
}

impl Head {
    pub fn status(&self) -> u16 {
        self.status
    }

    pub fn header(&self, name: &str) -> Option<String> {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(|value| value.to_string())
    }

    /// A header parsed as a byte count, ignoring anything that is not one.
    pub fn header_u64(&self, name: &str) -> Option<u64> {
        header_u64(&self.headers, name)
    }
}

/// A GET response whose body has not been read yet.
pub struct NetBody {
    status: u16,
    status_text: String,
    content_length: Option<u64>,
    stream: BoxStream<'static, Result<Chunk>>,
}

impl std::fmt::Debug for NetBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NetBody")
            .field("status", &self.status)
            .field("content_length", &self.content_length)
            .finish_non_exhaustive()
    }
}

impl NetBody {
    pub fn status(&self) -> u16 {
        self.status
    }

    pub fn status_text(&self) -> &str {
        &self.status_text
    }

    /// The **body** length, which is the remaining tail on a resumed 206 rather
    /// than the size of the resource. The transfer adds the offset back.
    pub fn content_length(&self) -> Option<u64> {
        self.content_length
    }

    pub async fn next_chunk(&mut self) -> Option<Result<Chunk>> {
        self.stream.next().await
    }
}

/// Deliberately no client-level total-request timeout: it would apply to the
/// streaming GET too, and a model weight is gigabytes, so any deadline that fits
/// one download fails another. Silence during a transfer is bounded instead by
/// the transfer's per-chunk stall watchdog, which also covers the wait for its
/// response headers. The small verbs get [`SMALL_REQUEST_TIMEOUT`] per call.
fn build_client(policy: reqwest::redirect::Policy) -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .redirect(policy)
        .build()
        .map_err(|err| {
            AppError::new(
                ErrorCode::InternalError,
                format!("could not build the download HTTP client: {err}"),
            )
        })
}

fn timed_out(url: &str, after: Duration) -> AppError {
    AppError::new(
        ErrorCode::ServerUnavailable,
        format!("{url} did not answer within {}s; aborting", after.as_secs()),
    )
}

fn header_u64(headers: &reqwest::header::HeaderMap, name: &str) -> Option<u64> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
}

fn unreachable_host(url: &str, err: &reqwest::Error) -> AppError {
    AppError::new(
        ErrorCode::ServerUnavailable,
        format!("could not reach {url}: {err}"),
    )
}

/// The message a stopped transfer carries into the queue row, matching the
/// TypeScript's `throw new Error("cancelled")`.
pub const CANCELLED_MESSAGE: &str = "cancelled";

/// The error a stopped transfer returns.
///
/// The code is nearly inert: this never reaches a client, because the queue
/// catches it and writes the row. What the queue decides on is the **token**,
/// not this error (`manager.ts:332` reads the abort signal rather than the
/// message), so a cancel that races a genuine failure is still classified as a
/// cancel. The message exists so the row text matches the oracle's.
pub fn cancelled() -> AppError {
    AppError::new(ErrorCode::Conflict, CANCELLED_MESSAGE)
}

/// True for the error [`cancelled`] produces. A convenience for tests and for a
/// caller with no token to hand; the token is the authority.
pub fn is_cancelled(err: &AppError) -> bool {
    err.message == CANCELLED_MESSAGE
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::path::Path;
    use std::time::Duration;

    use super::*;
    use crate::downloads::testing::{bounded, Serve, Upstream};

    #[tokio::test]
    async fn a_head_and_a_get_and_a_json_each_count_once() {
        let net = Net::new().unwrap();
        assert_eq!(net.calls(), 0, "a constructed Net has made no request");
        let cancel = CancellationToken::new();

        let server = Upstream::start(Serve::Json("{\"ok\":true}".into())).await;
        let head = bounded(net.head(&server.url("/probe"), Redirect::Follow, &cancel))
            .await
            .unwrap();
        assert_eq!(head.status(), 200);
        assert_eq!(net.calls(), 1);

        let mut body = bounded(net.get_range(&server.url("/probe"), None, &cancel))
            .await
            .unwrap();
        assert_eq!(body.status(), 200);
        while bounded(body.next_chunk()).await.is_some() {}
        assert_eq!(net.calls(), 2);

        let json = bounded(net.json(&server.url("/probe"), &cancel))
            .await
            .unwrap();
        assert_eq!(json["ok"], serde_json::Value::Bool(true));
        assert_eq!(net.calls(), 3);
    }

    #[tokio::test]
    async fn a_resume_offset_of_zero_sends_no_range_header() {
        let net = Net::new().unwrap();
        let cancel = CancellationToken::new();
        let server = Upstream::start(Serve::Ranged(b"0123456789".to_vec())).await;

        let mut body = bounded(net.get_range(&server.url("/f"), Some(0), &cancel))
            .await
            .unwrap();
        while bounded(body.next_chunk()).await.is_some() {}

        assert_eq!(body.status(), 200, "no Range means a plain 200");
        assert_eq!(server.calls()[0].range, None);
    }

    #[tokio::test]
    async fn a_range_offset_is_sent_verbatim() {
        let net = Net::new().unwrap();
        let cancel = CancellationToken::new();
        let server = Upstream::start(Serve::Ranged(b"0123456789".to_vec())).await;

        let mut body = bounded(net.get_range(&server.url("/f"), Some(4), &cancel))
            .await
            .unwrap();
        let mut seen = Vec::new();
        while let Some(chunk) = bounded(body.next_chunk()).await {
            seen.extend_from_slice(&chunk.unwrap());
        }

        assert_eq!(server.calls()[0].range.as_deref(), Some("bytes=4-"));
        assert_eq!(body.status(), 206);
        assert_eq!(
            body.content_length(),
            Some(6),
            "the tail, not the whole file"
        );
        assert_eq!(seen, b"456789");
    }

    #[tokio::test]
    async fn a_manual_redirect_head_stops_at_the_302() {
        // The whole reason two clients exist: the 302 carries x-linked-etag and
        // x-linked-size, and the CDN it points at publishes neither.
        let net = Net::new().unwrap();
        let cancel = CancellationToken::new();
        let server = Upstream::start(Serve::Ranged(b"0123456789".to_vec()))
            .await
            .with_head_redirect("/cdn", &[("x-linked-size", "4096")]);

        let manual = bounded(net.head(&server.url("/f"), Redirect::Manual, &cancel))
            .await
            .unwrap();
        assert_eq!(manual.status(), 302);
        assert_eq!(manual.header_u64("x-linked-size"), Some(4096));

        let followed = bounded(net.head(&server.url("/f"), Redirect::Follow, &cancel))
            .await
            .unwrap();
        assert_eq!(followed.status(), 200);
        assert_eq!(followed.header("x-linked-size"), None);
    }

    #[tokio::test]
    async fn a_cancelled_request_never_reaches_the_host() {
        let net = Net::new().unwrap();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let server = Upstream::start(Serve::Ranged(b"0123456789".to_vec())).await;

        let err = bounded(net.get_range(&server.url("/f"), None, &cancel))
            .await
            .unwrap_err();
        assert!(is_cancelled(&err), "{err:?}");
        assert!(server.calls().is_empty());
    }

    #[tokio::test]
    async fn a_non_2xx_json_fetch_names_the_status_and_the_url() {
        let net = Net::new().unwrap();
        let cancel = CancellationToken::new();
        let server = Upstream::start(Serve::Status(503)).await;

        let err = bounded(net.json(&server.url("/manifest.json"), &cancel))
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::ManifestFetchFailed);
        assert!(err.message.contains("503"), "{}", err.message);
        assert!(err.message.contains("manifest.json"), "{}", err.message);
    }

    #[tokio::test]
    async fn a_black_holed_host_bounds_every_small_verb() {
        // The module's stated bound used to cover ONE of the three verbs. A
        // host that accepts the connection and never answers is not a failure
        // anywhere: `head` ran inside the queue's single permit and stalled
        // every queued download behind it, and `json`/`text` ran inside an HTTP
        // handler with no timeout layer above it, so the request hung forever.
        let server = Upstream::start(Serve::Never).await;
        let net = Net::new()
            .unwrap()
            .with_small_request_timeout(Duration::from_millis(150));
        let cancel = CancellationToken::new();

        for err in [
            bounded(net.head(&server.url("/f"), Redirect::Follow, &cancel))
                .await
                .unwrap_err(),
            bounded(net.head(&server.url("/f"), Redirect::Manual, &cancel))
                .await
                .unwrap_err(),
            bounded(net.json(&server.url("/manifest.json"), &cancel))
                .await
                .unwrap_err(),
            bounded(net.text(&server.url("/manifest.json"), &cancel))
                .await
                .unwrap_err(),
        ] {
            assert_eq!(err.code, ErrorCode::ServerUnavailable, "{err:?}");
            assert!(err.message.contains("did not answer"), "{}", err.message);
        }
        assert_eq!(
            server.calls().len(),
            4,
            "each verb reached the host and then gave up on it"
        );
    }

    #[tokio::test]
    async fn a_host_that_answers_inside_the_deadline_is_not_cut_off() {
        // The other half: a bound that fires on a healthy host is worse than no
        // bound, so the deadline has to be visible from both sides.
        let server = Upstream::start(Serve::Json("{\"ok\":true}".into())).await;
        let net = Net::new()
            .unwrap()
            .with_small_request_timeout(Duration::from_secs(5));
        let cancel = CancellationToken::new();

        let json = bounded(net.json(&server.url("/manifest.json"), &cancel))
            .await
            .unwrap();

        assert_eq!(json["ok"], serde_json::Value::Bool(true));
    }

    #[test]
    fn the_shipping_small_request_timeout_is_generous_but_finite() {
        // Bounded rather than pinned, so a deliberate retune stays legal while
        // a deletion or a millisecond typo does not. Every test above injects
        // its own value, which is what leaves the SHIPPING one unwitnessed.
        assert!(SMALL_REQUEST_TIMEOUT >= Duration::from_secs(5));
        assert!(SMALL_REQUEST_TIMEOUT <= Duration::from_secs(120));
        assert_eq!(
            Net::new().unwrap().small_request_timeout,
            SMALL_REQUEST_TIMEOUT,
            "the constructor wired something other than the shipping constant"
        );
    }

    #[tokio::test]
    async fn an_unreachable_host_is_server_unavailable_rather_than_a_panic() {
        let net = Net::new().unwrap();
        let cancel = CancellationToken::new();
        // Port 0 never listens, so this fails at connect rather than at HTTP.
        let err = bounded(net.json("http://127.0.0.1:1/nothing", &cancel))
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::ServerUnavailable);
    }

    #[test]
    fn a_content_length_that_is_not_a_byte_count_is_ignored() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("content-length", "12".parse().unwrap());
        assert_eq!(header_u64(&headers, "content-length"), Some(12));
        headers.insert("content-length", "12.5".parse().unwrap());
        assert_eq!(header_u64(&headers, "content-length"), None);
        headers.insert("content-length", "-4".parse().unwrap());
        assert_eq!(header_u64(&headers, "content-length"), None);
    }

    #[test]
    fn a_host_is_extracted_and_a_non_url_is_not() {
        assert_eq!(
            host_of("https://huggingface.co/a/b"),
            Some("huggingface.co".into())
        );
        assert_eq!(host_of("not a url"), None);
        assert_eq!(host_of(""), None);
    }

    /// The structural half of the consent rule: `net.rs` is the only file in
    /// this package that may name the HTTP crate. Read from the directory rather
    /// than a hand-kept list, so a file added tomorrow is covered by existing.
    #[test]
    fn no_module_outside_net_reaches_the_network() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/downloads");
        let mut offenders = Vec::new();
        let mut stack = vec![root.clone()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("downloads/ is readable") {
                let path = entry.expect("readable entry").path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                    continue;
                }
                let name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_default();
                if name == "net.rs" || name == "testing.rs" {
                    // testing.rs is the mock upstream: it SERVES HTTP, it never
                    // originates a request, and it is compiled out of production.
                    continue;
                }
                let body = std::fs::read_to_string(&path).expect("readable file");
                if body.contains("reqwest") {
                    offenders.push(
                        path.strip_prefix(&root)
                            .unwrap_or(&path)
                            .display()
                            .to_string(),
                    );
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "every outbound request in downloads/ goes through net.rs; these name reqwest directly: {offenders:?}"
        );
    }
}
