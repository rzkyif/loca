//! A local upstream the transfer tests can point at.
//!
//! Every behaviour this package has to survive is a property of the SERVER:
//! honouring a `Range` or ignoring it, answering 416, declaring one length and
//! sending another, trickling bytes, or falling silent mid-body. None of them is
//! expressible against a stubbed transport, and D-038's closing note is that a
//! test which stubs the transport cannot see a clock bug in the transport's own
//! deadline. So these tests drive a real socket, on a real clock, with the
//! intervals injected and the shipping ratio kept.
//!
//! Compiled only under `cfg(test)`: none of this reaches a production binary.
#![allow(clippy::unwrap_used, clippy::expect_used, dead_code)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::{Request, State};
use axum::http::{HeaderMap, Method, Response, StatusCode};
use sha2::{Digest, Sha256};

/// Bound every awaiting test, so a regression fails rather than hangs (D-036).
/// The message names the package: a bounded failure in a shared working tree has
/// to be attributable without opening the file.
pub async fn bounded<T>(fut: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(10), fut)
        .await
        .expect("d7: test exceeded its bound")
}

/// The sha256 of `bytes`, lowercase hex. The transfer's expected-hash fixtures
/// are computed rather than pasted, so a test cannot pass by asserting a
/// constant that was copied from the implementation it is testing.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

/// What the mock upstream does with a GET.
#[derive(Clone)]
pub enum Serve {
    /// A well-behaved origin: honours `Range` with a 206 plus a `content-range`,
    /// and 416s a range that starts past the end.
    Ranged(Vec<u8>),
    /// An origin that ignores `Range` and answers 200 with the whole body. Real
    /// mirrors and proxies do this, and it is the branch that has to discard the
    /// partial rather than append to it.
    IgnoresRange(Vec<u8>),
    /// 416 for everything, which is what a partial longer than the resource gets.
    Unsatisfiable,
    /// `count` chunks with `gap` between them, then a clean EOF.
    Trickle {
        chunk: Vec<u8>,
        count: usize,
        gap: Duration,
        declare_length: bool,
    },
    /// `count` chunks, then the connection is held open forever with no bytes
    /// and no close: a black-holed transfer, which is the only thing the stall
    /// watchdog exists for.
    Silent {
        chunk: Vec<u8>,
        count: usize,
        declared: u64,
    },
    /// A bare status with no body.
    Status(u16),
    Json(String),
    /// Accepts the connection and never answers, on any method. A black-holed
    /// host, which is the only shape a per-request deadline defends against:
    /// the socket is up, so nothing fails, and the caller waits forever.
    Never,
}

/// What the mock upstream does with a HEAD.
#[derive(Clone, Default)]
struct HeadPlan {
    /// When set, a HEAD anywhere else 302s here, carrying `redirect_headers`.
    /// This is HuggingFace's shape: the resolve endpoint redirects to a CDN and
    /// the redirect itself is what carries `x-linked-etag` / `x-linked-size`.
    redirect_to: Option<String>,
    redirect_headers: Vec<(String, String)>,
    /// Headers on the terminal (non-redirecting) HEAD answer.
    terminal_headers: Vec<(String, String)>,
}

/// One request the upstream received.
#[derive(Clone, Debug)]
pub struct Call {
    pub method: String,
    pub path: String,
    pub range: Option<String>,
}

struct Shared {
    serve: Serve,
    head: Mutex<HeadPlan>,
    calls: Mutex<Vec<Call>>,
    /// Live GET count and the high-water mark. The queue's one-at-a-time
    /// guarantee is a property of the SERVER's view: counting calls cannot see
    /// two transfers overlapping, only a gauge can.
    live: Mutex<(usize, usize)>,
}

pub struct Upstream {
    base: String,
    shared: Arc<Shared>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Upstream {
    pub async fn start(serve: Serve) -> Self {
        let shared = Arc::new(Shared {
            serve,
            head: Mutex::new(HeadPlan::default()),
            calls: Mutex::new(Vec::new()),
            live: Mutex::new((0, 0)),
        });
        let app = axum::Router::new()
            .fallback(handle)
            .with_state(Arc::clone(&shared));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("d7: the mock upstream could not bind");
        let addr = listener
            .local_addr()
            .expect("d7: the mock upstream has no address");
        let (shutdown, rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = rx.await;
                })
                .await;
        });
        Self {
            base: format!("http://{addr}"),
            shared,
            shutdown: Some(shutdown),
        }
    }

    /// Make HEAD 302 to `location` (a path on this same server) with `headers`
    /// on the redirect itself.
    pub fn with_head_redirect(self, location: &str, headers: &[(&str, &str)]) -> Self {
        {
            let mut plan = self.shared.head.lock().unwrap_or_else(|e| e.into_inner());
            plan.redirect_to = Some(location.to_string());
            plan.redirect_headers = headers
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect();
        }
        self
    }

    /// Headers on the HEAD answer that does not redirect.
    pub fn with_head_headers(self, headers: &[(&str, &str)]) -> Self {
        {
            let mut plan = self.shared.head.lock().unwrap_or_else(|e| e.into_inner());
            plan.terminal_headers = headers
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect();
        }
        self
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    /// The host:port this upstream listens on, for a test that has to make it
    /// look like the HuggingFace host.
    pub fn base(&self) -> &str {
        &self.base
    }

    /// The most GETs that were ever in flight here at once.
    pub fn max_concurrent(&self) -> usize {
        self.shared.live.lock().unwrap_or_else(|e| e.into_inner()).1
    }

    /// Every request received, in order.
    pub fn calls(&self) -> Vec<Call> {
        self.shared
            .calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

impl Drop for Upstream {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }
}

async fn handle(State(shared): State<Arc<Shared>>, req: Request) -> Response<Body> {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let headers = req.headers().clone();
    shared
        .calls
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(Call {
            method: method.to_string(),
            path: path.clone(),
            range: header(&headers, "range"),
        });

    if matches!(shared.serve, Serve::Never) {
        // Recorded above, so a test can still assert the request was MADE, and
        // then never answered.
        std::future::pending::<()>().await;
    }
    if method == Method::HEAD {
        return head_answer(&shared, &path);
    }
    let guard = LiveGuard::arm(&shared);
    get_answer(&shared, &headers, guard)
}

/// Holds the live-GET gauge up for as long as one response body is being sent.
///
/// The gauge is what the one-at-a-time test reads, and it has to span the BODY
/// rather than the handler: a streamed transfer is still in flight long after
/// the handler returns, so releasing on return would report every transfer as
/// instantaneous and no two as overlapping.
struct LiveGuard(Arc<Shared>);

impl LiveGuard {
    fn arm(shared: &Arc<Shared>) -> Self {
        let mut live = shared.live.lock().unwrap_or_else(|e| e.into_inner());
        live.0 += 1;
        live.1 = live.1.max(live.0);
        drop(live);
        Self(Arc::clone(shared))
    }
}

impl Drop for LiveGuard {
    fn drop(&mut self) {
        let mut live = self.0.live.lock().unwrap_or_else(|e| e.into_inner());
        live.0 = live.0.saturating_sub(1);
    }
}

fn head_answer(shared: &Arc<Shared>, path: &str) -> Response<Body> {
    let plan = shared
        .head
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    match plan.redirect_to {
        Some(ref target) if target != path => {
            let mut builder = Response::builder()
                .status(StatusCode::FOUND)
                .header("location", target.as_str());
            for (name, value) in &plan.redirect_headers {
                builder = builder.header(name.as_str(), value.as_str());
            }
            builder
                .body(head_body(&plan.redirect_headers))
                .unwrap_or_default()
        }
        _ => {
            let mut builder = Response::builder().status(StatusCode::OK);
            for (name, value) in &plan.terminal_headers {
                builder = builder.header(name.as_str(), value.as_str());
            }
            builder
                .body(head_body(&plan.terminal_headers))
                .unwrap_or_default()
        }
    }
}

/// A HEAD body that declares a length only when the test asked for one.
///
/// `Body::empty()` has an exact size hint, so hyper stamps `content-length: 0`
/// on an answer the test declared no length for. That is a mock inventing a
/// header, and it hid a real branch: a probe reads `content-length` off the 302
/// as its second choice, so an invented zero satisfied it and the followed-HEAD
/// fallback was never reached in the test that existed to cover it. An
/// unknown-length body makes hyper say nothing, so the answer carries exactly
/// the headers the test wrote and no others.
fn head_body(declared: &[(String, String)]) -> Body {
    if declared
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("content-length"))
    {
        return Body::empty();
    }
    Body::from_stream(futures_util::stream::empty::<
        std::result::Result<Bytes, std::io::Error>,
    >())
}

fn get_answer(shared: &Arc<Shared>, headers: &HeaderMap, guard: LiveGuard) -> Response<Body> {
    match &shared.serve {
        Serve::Ranged(body) => match range_start(headers) {
            None => whole(body),
            Some(from) if from >= body.len() as u64 => status(416),
            Some(from) => {
                let tail = body[from as usize..].to_vec();
                Response::builder()
                    .status(StatusCode::PARTIAL_CONTENT)
                    .header("content-length", tail.len().to_string())
                    .header(
                        "content-range",
                        format!("bytes {from}-{}/{}", body.len() - 1, body.len()),
                    )
                    .body(Body::from(tail))
                    .unwrap_or_default()
            }
        },
        Serve::IgnoresRange(body) => whole(body),
        Serve::Unsatisfiable => status(416),
        Serve::Trickle {
            chunk,
            count,
            gap,
            declare_length,
        } => {
            let mut builder = Response::builder().status(StatusCode::OK);
            if *declare_length {
                builder = builder.header("content-length", (chunk.len() * count).to_string());
            }
            builder
                .body(trickle_body(chunk.clone(), *count, *gap, guard))
                .unwrap_or_default()
        }
        Serve::Silent {
            chunk,
            count,
            declared,
        } => Response::builder()
            .status(StatusCode::OK)
            .header("content-length", declared.to_string())
            .body(silent_body(chunk.clone(), *count, guard))
            .unwrap_or_default(),
        Serve::Status(code) => status(*code),
        // Answered before it reaches here (`handle` never returns for it), so
        // this arm exists only to keep the match exhaustive.
        Serve::Never => status(504),
        Serve::Json(text) => Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "application/json")
            .body(Body::from(text.clone()))
            .unwrap_or_default(),
    }
}

fn whole(body: &[u8]) -> Response<Body> {
    Response::builder()
        .status(StatusCode::OK)
        .header("content-length", body.len().to_string())
        .body(Body::from(body.to_vec()))
        .unwrap_or_default()
}

fn status(code: u16) -> Response<Body> {
    Response::builder()
        .status(StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR))
        .body(Body::empty())
        .unwrap_or_default()
}

fn trickle_body(chunk: Vec<u8>, count: usize, gap: Duration, guard: LiveGuard) -> Body {
    let stream = futures_util::stream::unfold((0usize, guard), move |(sent, guard)| {
        let chunk = chunk.clone();
        async move {
            if sent >= count {
                return None;
            }
            tokio::time::sleep(gap).await;
            Some((
                Ok::<Bytes, std::io::Error>(Bytes::from(chunk)),
                (sent + 1, guard),
            ))
        }
    });
    Body::from_stream(stream)
}

fn silent_body(chunk: Vec<u8>, count: usize, guard: LiveGuard) -> Body {
    let stream = futures_util::stream::unfold((0usize, guard), move |(sent, guard)| {
        let chunk = chunk.clone();
        async move {
            if sent < count {
                return Some((
                    Ok::<Bytes, std::io::Error>(Bytes::from(chunk)),
                    (sent + 1, guard),
                ));
            }
            // Held open with no bytes and no close: the shape the watchdog is
            // the only defence against.
            std::future::pending::<()>().await;
            None
        }
    });
    Body::from_stream(stream)
}

fn header(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.to_string())
}

fn range_start(headers: &HeaderMap) -> Option<u64> {
    header(headers, "range")?
        .trim()
        .strip_prefix("bytes=")?
        .split('-')
        .next()?
        .parse::<u64>()
        .ok()
}

// --- what the release actually publishes -----------------------------------
//
// The two documents below are what `scripts/release/core.ts`
// `composeBinaryManifest` emits, transcribed field for field:
//
//  - **stable** (`:801-874`) resolves every kind at RELEASE time and publishes
//    `{ version, platforms: { <triple>: <target-or-variant-map> } }` with no
//    `resolver` key anywhere;
//  - **latest** (`:788-798`) publishes the resolver itself,
//    `{ resolver: { repo, assets: { <triple>: <pattern-or-variant-map> },
//    pinnedTag? } }`, so the core resolves the release at runtime;
//  - both channels additionally carry the self-hosted `loca-core-speech` entry
//    in the PINNED shape (`:875-877`, `composeSpeechEntry` at `:746-772`),
//    which is why "latest means resolver" is not true of the whole document.
//
// They are here rather than inside one test's module because the whole reason
// the review found the parser unable to read either of them is that every test
// hand-built the shape the Rust already expected. A fixture built from the
// Rust types cannot see the Rust types being wrong.

/// The `binaries.json` a `stable` release publishes.
pub fn published_stable_manifest() -> serde_json::Value {
    serde_json::json!({
        "schemaVersion": 1,
        "version": "0.4.0",
        "binaries": {
            // A cpu-only triple collapses to the bare pinned shape; a triple
            // with GPU builds emits a per-variant map, and a cuda variant
            // carries its companion `extras`.
            "llama-server": {
                "version": "b7091",
                "platforms": {
                    "aarch64-apple-darwin": {
                        "url": "https://github.com/ggml-org/llama.cpp/releases/download/b7091/llama-b7091-bin-macos-arm64.tar.gz",
                        "sha256": "1111111111111111111111111111111111111111111111111111111111111111"
                    },
                    "x86_64-unknown-linux-gnu": {
                        "cpu": {
                            "url": "https://github.com/ggml-org/llama.cpp/releases/download/b7091/llama-b7091-bin-ubuntu-x64.tar.gz",
                            "sha256": "2222222222222222222222222222222222222222222222222222222222222222"
                        },
                        "vulkan": {
                            "url": "https://github.com/ggml-org/llama.cpp/releases/download/b7091/llama-b7091-bin-ubuntu-vulkan-x64.tar.gz",
                            "sha256": "3333333333333333333333333333333333333333333333333333333333333333"
                        }
                    },
                    "x86_64-pc-windows-msvc": {
                        "cpu": {
                            "url": "https://github.com/ggml-org/llama.cpp/releases/download/b7091/llama-b7091-bin-win-cpu-x64.zip",
                            "sha256": "4444444444444444444444444444444444444444444444444444444444444444"
                        },
                        "cuda": {
                            "url": "https://github.com/ggml-org/llama.cpp/releases/download/b7091/llama-b7091-bin-win-cuda-13.3-x64.zip",
                            "sha256": "5555555555555555555555555555555555555555555555555555555555555555",
                            "extras": [{
                                "url": "https://github.com/ggml-org/llama.cpp/releases/download/b7091/cudart-llama-bin-win-cuda-13.3-x64.zip",
                                "sha256": "6666666666666666666666666666666666666666666666666666666666666666"
                            }]
                        }
                    }
                }
            },
            // The kind this core actually installs. `version` is the upstream
            // release TAG, not a semver: `binaries[kind] = { version: tag, ... }`.
            "deno": {
                "version": "v2.8.2",
                "platforms": {
                    "aarch64-apple-darwin": {
                        "url": "https://github.com/denoland/deno/releases/download/v2.8.2/deno-aarch64-apple-darwin.zip",
                        "sha256": "7777777777777777777777777777777777777777777777777777777777777777"
                    },
                    "x86_64-apple-darwin": {
                        "url": "https://github.com/denoland/deno/releases/download/v2.8.2/deno-x86_64-apple-darwin.zip",
                        "sha256": "8888888888888888888888888888888888888888888888888888888888888888"
                    },
                    "aarch64-unknown-linux-gnu": {
                        "url": "https://github.com/denoland/deno/releases/download/v2.8.2/deno-aarch64-unknown-linux-gnu.zip",
                        "sha256": "9999999999999999999999999999999999999999999999999999999999999999"
                    },
                    "x86_64-unknown-linux-gnu": {
                        "url": "https://github.com/denoland/deno/releases/download/v2.8.2/deno-x86_64-unknown-linux-gnu.zip",
                        "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    },
                    "aarch64-pc-windows-msvc": {
                        "url": "https://github.com/denoland/deno/releases/download/v2.8.2/deno-aarch64-pc-windows-msvc.zip",
                        "sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                    },
                    "x86_64-pc-windows-msvc": {
                        "url": "https://github.com/denoland/deno/releases/download/v2.8.2/deno-x86_64-pc-windows-msvc.zip",
                        "sha256": "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"
                    }
                }
            },
            "loca-core-speech": {
                "version": "0.4.0",
                "platforms": {
                    "aarch64-apple-darwin": {
                        "url": "https://get.loca.rzkyif.com/speech/0.4.0/aarch64-apple-darwin/loca-core-speech.tar.gz",
                        "sha256": "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd"
                    }
                }
            }
        },
        "signature": ""
    })
}

/// The `binaries.json` a `latest` release publishes.
pub fn published_latest_manifest() -> serde_json::Value {
    serde_json::json!({
        "schemaVersion": 1,
        "version": "0.4.0",
        "binaries": {
            "llama-server": {
                "resolver": {
                    "repo": "ggml-org/llama.cpp",
                    // Verbatim from `UPSTREAM_BINARIES` in
                    // `packages/loca-shared/src/domain/model.ts:176-206`: a bare
                    // string for a single-variant triple, a per-variant map
                    // otherwise, and `{ asset, extra }` where a variant needs
                    // companion archives.
                    "assets": {
                        "aarch64-apple-darwin": "llama-{tag}-bin-macos-arm64.tar.gz",
                        "x86_64-apple-darwin": "llama-{tag}-bin-macos-x64.tar.gz",
                        "x86_64-unknown-linux-gnu": {
                            "cpu": "llama-{tag}-bin-ubuntu-x64.tar.gz",
                            "vulkan": "llama-{tag}-bin-ubuntu-vulkan-x64.tar.gz",
                            "rocm": "llama-{tag}-bin-ubuntu-rocm-7.2-x64.tar.gz"
                        },
                        "aarch64-unknown-linux-gnu": {
                            "cpu": "llama-{tag}-bin-ubuntu-arm64.tar.gz",
                            "vulkan": "llama-{tag}-bin-ubuntu-vulkan-arm64.tar.gz"
                        },
                        "x86_64-pc-windows-msvc": {
                            "cpu": "llama-{tag}-bin-win-cpu-x64.zip",
                            "vulkan": "llama-{tag}-bin-win-vulkan-x64.zip",
                            "cuda": {
                                "asset": "llama-{tag}-bin-win-cuda-13.3-x64.zip",
                                "extra": ["cudart-llama-bin-win-cuda-13.3-x64.zip"]
                            },
                            "hip": "llama-{tag}-bin-win-hip-radeon-x64.zip"
                        },
                        "aarch64-pc-windows-msvc": "llama-{tag}-bin-win-cpu-arm64.zip"
                    }
                }
            },
            "deno": {
                "resolver": {
                    "repo": "denoland/deno",
                    "assets": {
                        "aarch64-apple-darwin": "deno-aarch64-apple-darwin.zip",
                        "x86_64-apple-darwin": "deno-x86_64-apple-darwin.zip",
                        "aarch64-unknown-linux-gnu": "deno-aarch64-unknown-linux-gnu.zip",
                        "x86_64-unknown-linux-gnu": "deno-x86_64-unknown-linux-gnu.zip",
                        "x86_64-pc-windows-msvc": "deno-x86_64-pc-windows-msvc.zip",
                        "aarch64-pc-windows-msvc": "deno-aarch64-pc-windows-msvc.zip"
                    },
                    "pinnedTag": "v2.8.2"
                }
            },
            "loca-core-speech": {
                "version": "0.4.0",
                "platforms": {
                    "aarch64-apple-darwin": {
                        "url": "https://get.loca.rzkyif.com/speech/0.4.0/aarch64-apple-darwin/loca-core-speech.tar.gz",
                        "sha256": "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd"
                    }
                }
            }
        },
        "signature": ""
    })
}
