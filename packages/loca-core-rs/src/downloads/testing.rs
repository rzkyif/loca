//! A local upstream the transfer tests can point at: a real socket on a real
//! clock, since range handling, 416s, trickling, and stalls are server
//! behaviours a stubbed transport cannot express. `cfg(test)` only.
#![allow(clippy::unwrap_used, clippy::expect_used, dead_code)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::{Request, State};
use axum::http::{HeaderMap, Method, Response, StatusCode};

/// Bound every awaiting test, so a regression fails rather than hangs.
pub async fn bounded<T>(fut: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(10), fut)
        .await
        .expect("d7: test exceeded its bound")
}

pub use crate::files::sha256_hex;

/// What the mock upstream does with a GET.
#[derive(Clone)]
pub enum Serve {
    /// A well-behaved origin: honours `Range` with a 206 plus a `content-range`,
    /// and 416s a range that starts past the end.
    Ranged(Vec<u8>),
    /// Ignores `Range` and answers 200 with the whole body, as real mirrors do.
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
    /// `count` chunks, then held open forever with no bytes and no close.
    Silent {
        chunk: Vec<u8>,
        count: usize,
        declared: u64,
    },
    /// A bare status with no body.
    Status(u16),
    Json(String),
    /// Accepts the connection and never answers, on any method.
    Never,
}

/// What the mock upstream does with a HEAD.
#[derive(Clone, Default)]
struct HeadPlan {
    /// When set, a HEAD anywhere else 302s here, carrying `redirect_headers`
    /// (HuggingFace's shape: the redirect carries `x-linked-size`).
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
    pub user_agent: Option<String>,
}

struct Shared {
    serve: Serve,
    head: Mutex<HeadPlan>,
    calls: Mutex<Vec<Call>>,
    /// Live GET count and the high-water mark (overlap is only visible here).
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

    /// The host:port this upstream listens on.
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
            user_agent: header(&headers, "user-agent"),
        });

    if matches!(shared.serve, Serve::Never) {
        // Recorded above so a test can assert the request was made.
        std::future::pending::<()>().await;
    }
    if method == Method::HEAD {
        return head_answer(&shared, &path);
    }
    let guard = LiveGuard::arm(&shared);
    get_answer(&shared, &headers, guard)
}

/// Holds the live-GET gauge up for as long as one response body is being sent:
/// a streamed transfer is still in flight long after the handler returns.
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

/// A HEAD body that declares a length only when the test asked for one:
/// `Body::empty()` makes hyper stamp `content-length: 0`, which a probe would
/// read off the 302 and skip the followed-HEAD fallback.
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
        // `handle` never returns for it; kept for exhaustiveness.
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

/// A pinned `{ url, sha256 }` entry with a sha made of `sha` repeated.
fn pinned(url: &str, sha: char) -> serde_json::Value {
    serde_json::json!({ "url": url, "sha256": sha.to_string().repeat(64) })
}

/// The `binaries.json` a `stable` release publishes: every kind resolved at
/// release time, no `resolver` key anywhere.
pub fn published_stable_manifest() -> serde_json::Value {
    let llama = "https://github.com/ggml-org/llama.cpp/releases/download/b7091";
    let base = "https://github.com/denoland/deno/releases/download/v2.8.2";
    let deno: serde_json::Map<String, serde_json::Value> = deno_assets()
        .into_iter()
        .zip("789abc".chars())
        .map(|((triple, asset), sha)| {
            let url = format!("{base}/{}", asset.as_str().unwrap_or_default());
            (triple, pinned(&url, sha))
        })
        .collect();
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
                    "aarch64-apple-darwin": pinned(&format!("{llama}/llama-b7091-bin-macos-arm64.tar.gz"), '1'),
                    "x86_64-unknown-linux-gnu": {
                        "cpu": pinned(&format!("{llama}/llama-b7091-bin-ubuntu-x64.tar.gz"), '2'),
                        "vulkan": pinned(&format!("{llama}/llama-b7091-bin-ubuntu-vulkan-x64.tar.gz"), '3')
                    },
                    "x86_64-pc-windows-msvc": {
                        "cpu": pinned(&format!("{llama}/llama-b7091-bin-win-cpu-x64.zip"), '4'),
                        "cuda": {
                            "url": format!("{llama}/llama-b7091-bin-win-cuda-13.3-x64.zip"),
                            "sha256": "5".repeat(64),
                            "extras": [pinned(&format!("{llama}/cudart-llama-bin-win-cuda-13.3-x64.zip"), '6')]
                        }
                    }
                }
            },
            // `version` is the upstream release TAG, not a semver.
            "deno": { "version": "v2.8.2", "platforms": deno }
        },
        "signature": ""
    })
}

/// `deno`'s resolver assets: one bare asset name per triple it publishes.
fn deno_assets() -> serde_json::Map<String, serde_json::Value> {
    [
        "aarch64-apple-darwin",
        "x86_64-apple-darwin",
        "aarch64-unknown-linux-gnu",
        "x86_64-unknown-linux-gnu",
        "aarch64-pc-windows-msvc",
        "x86_64-pc-windows-msvc",
    ]
    .into_iter()
    .map(|t| (t.to_string(), format!("deno-{t}.zip").into()))
    .collect()
}

/// The `binaries.json` a `latest` release publishes: the resolver itself, so
/// the core resolves the release at runtime.
pub fn published_latest_manifest() -> serde_json::Value {
    serde_json::json!({
        "schemaVersion": 1,
        "version": "0.4.0",
        "binaries": {
            "llama-server": {
                "resolver": {
                    "repo": "ggml-org/llama.cpp",
                    // A bare string for a single-variant triple, a per-variant
                    // map otherwise, and `{ asset, extra }` for companions.
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
                    "assets": deno_assets(),
                    "pinnedTag": "v2.8.2"
                }
            }
        },
        "signature": ""
    })
}
