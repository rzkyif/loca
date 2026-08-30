//! A local upstream, the published-manifest fixture, and the signing helpers.
//!
//! A real socket on a real clock with bounds injected: a 404, a black-holed
//! host, a body that stops mid-transfer are properties of the SERVER, and a
//! stubbed transport cannot express them (nor can paused time drive real I/O).
//!
//! [`published_manifest`] is transcribed from what the release script emits,
//! not built from [`super::manifest::CoreManifest`]: a fixture derived from the
//! implementation cannot see the implementation being wrong.
#![allow(clippy::unwrap_used, clippy::expect_used, dead_code)]

use std::io::Write as _;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::{Request, State};
use axum::http::{Response, StatusCode};
use serde_json::{json, Value};

/// Bound every awaiting test, so a regression fails rather than hangs. In a
/// shared crate a hang blocks every run silently; a failure is local.
pub async fn bounded<T>(fut: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(10), fut)
        .await
        .expect("d13: test exceeded its bound")
}

// --- the re-entrancy tripwire over every process-global fixture lock -------

/// Which process-global fixture locks THIS thread is currently holding.
/// `#[tokio::test]` is current-thread, so this thread is this test.
#[allow(clippy::missing_const_for_thread_local)]
mod held {
    use std::cell::RefCell;
    thread_local! {
        pub static NAMES: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
    }
}

/// Claim a process-global fixture lock for this thread, refusing re-entry.
///
/// The locks below are non-reentrant and do not fail on re-entry: they park
/// forever, still held, wedging the whole test binary. No timeout is used,
/// since the rollback suite serialises by design; the defect detected is the
/// same thread asking for a lock it already holds, which is load-independent.
pub fn claim_fixture_lock(name: &'static str) -> FixtureClaim {
    held::NAMES.with(|names| {
        let mut names = names.borrow_mut();
        assert!(
            !names.contains(&name),
            "d13: this test already holds the `{name}` fixture lock and is asking for it \
             again, which parks forever and wedges every other test that wants it. Build \
             one fixture at a time: drop the first (scope it in a block, or split the \
             test) before constructing the second."
        );
        names.push(name);
    });
    FixtureClaim(name)
}

/// Releases the claim taken by [`claim_fixture_lock`].
pub struct FixtureClaim(&'static str);

impl Drop for FixtureClaim {
    fn drop(&mut self) {
        held::NAMES.with(|names| names.borrow_mut().retain(|held| held != &self.0));
    }
}

// --- the rollback lock, shared by every test that drives a boot decision ----

/// Serialises every test that drives `rollback::handle_update_marker_on_boot`,
/// because [`install_rename_should_fail`] is process-global: a neighbour
/// driving a rollback concurrently would inherit the injected failure. Every
/// rollback fixture takes it whether or not it knows the injector exists.
///
/// A `tokio::sync::Mutex`: holders await while holding it, and a `std` guard
/// across an `.await` blocks the thread, which on a current-thread runtime
/// is a hang rather than a red.
static ROLLBACK_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
static FAIL_INSTALL_RENAME: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Hold for the life of a test that stages a boot decision. The claim is taken
/// BEFORE the lock, so a test that asks twice panics instead of parking.
pub async fn rollback_guard() -> RollbackGuard {
    let claim = claim_fixture_lock("rollback");
    RollbackGuard {
        _lock: ROLLBACK_LOCK.lock().await,
        _claim: claim,
    }
}

/// What [`rollback_guard`] hands back: the lock plus the re-entry claim.
pub struct RollbackGuard {
    _lock: tokio::sync::MutexGuard<'static, ()>,
    _claim: FixtureClaim,
}

#[cfg(test)]
mod tripwire_tests {
    use super::*;

    #[tokio::test]
    #[should_panic(expected = "already holds the `rollback` fixture lock")]
    async fn asking_twice_for_one_fixture_lock_panics_instead_of_parking() {
        // The second acquisition is bounded so that deleting the tripwire
        // fails on the wrong panic message rather than parking forever.
        let _first = rollback_guard().await;
        let _second = bounded(rollback_guard()).await;
    }

    #[tokio::test]
    async fn a_claim_released_by_the_first_guard_can_be_taken_again() {
        // A tripwire that never lets go would fail everything.
        {
            let _first = rollback_guard().await;
        }
        let _second = rollback_guard().await;
    }
}

/// Read by `rollback::install_rename`, which is the one step of the swap whose
/// failure cannot be staged from outside. See its doc comment.
pub fn install_rename_should_fail() -> bool {
    FAIL_INSTALL_RENAME.load(std::sync::atomic::Ordering::Relaxed)
}

/// Only call while holding [`rollback_guard`]. The fixtures do.
pub fn set_install_rename_failure(fail: bool) {
    FAIL_INSTALL_RENAME.store(fail, std::sync::atomic::Ordering::Relaxed);
}

/// Set while a test wants `swap::create_anchor` to take its COPY fallback. That
/// fallback is for filesystems without hard links, so it is otherwise
/// unreachable from a test.
static NO_HARD_LINKS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn hard_links_disabled() -> bool {
    NO_HARD_LINKS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Only call while holding [`rollback_guard`]. The swap fixtures do.
pub fn set_hard_links_disabled(disabled: bool) {
    NO_HARD_LINKS.store(disabled, std::sync::atomic::Ordering::Relaxed);
}

pub use crate::files::sha256_hex;

/// Gzip `bytes`, the way the release script ships every core artifact. The
/// manifest's sha256 is over the DECOMPRESSED file, so both halves are returned.
pub fn gzipped(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(bytes).expect("d13: gzip encode failed");
    encoder.finish().expect("d13: gzip finish failed")
}

// --- the published manifest ------------------------------------------------

/// A `core.json` in its published shape: `binaries` keyed by triple with a
/// `.gz` URL and the decompressed sha256, `workers` by name, `helpers` by name
/// AND triple.
///
/// `signature` is a valid placeholder rather than absent, because a published
/// document always carries one. Sign with `Trusted::sign`, which strips the
/// field before building the message, so the placeholder is never signed.
pub fn published_manifest() -> Value {
    json!({
        "schemaVersion": 1,
        "version": "0.2.0",
        "binaries": [
            {
                "triple": "aarch64-apple-darwin",
                "url": "https://get.loca.rzkyif.com/core/0.2.0/aarch64-apple-darwin/loca-core.gz",
                "sha256": "1111111111111111111111111111111111111111111111111111111111111111",
            },
            {
                "triple": "x86_64-unknown-linux-gnu",
                "url": "https://get.loca.rzkyif.com/core/0.2.0/x86_64-unknown-linux-gnu/loca-core.gz",
                "sha256": "2222222222222222222222222222222222222222222222222222222222222222",
            },
        ],
        "workers": [
            {
                "name": "tool-worker.ts",
                "url": "https://get.loca.rzkyif.com/core/0.2.0/workers/tool-worker.ts.gz",
                "sha256": "3333333333333333333333333333333333333333333333333333333333333333",
            },
        ],
        "helpers": [
            {
                "name": "loca-core-updater",
                "triple": "aarch64-apple-darwin",
                "url": "https://get.loca.rzkyif.com/core/0.2.0/aarch64-apple-darwin/loca-core-updater.gz",
                "sha256": "4444444444444444444444444444444444444444444444444444444444444444",
            },
            {
                "name": "loca-core-updater",
                "triple": "x86_64-unknown-linux-gnu",
                "url": "https://get.loca.rzkyif.com/core/0.2.0/x86_64-unknown-linux-gnu/loca-core-updater.gz",
                "sha256": "5555555555555555555555555555555555555555555555555555555555555555",
            },
        ],
        "signature": "cGxhY2Vob2xkZXItcmVwbGFjZWQtYnktVHJ1c3RlZDo6c2lnbg==",
    })
}

// --- the mock upstream -----------------------------------------------------

/// What the mock does with a request.
#[derive(Clone)]
pub enum Serve {
    /// A whole body, at once.
    Body(Vec<u8>),
    /// A bare status with an empty body.
    Status(u16),
    /// Accepts the connection and never answers: the shape a deadline defends
    /// against, since the socket is up and nothing ever fails.
    Never,
    /// `count` chunks, then the body is held open with no bytes and no close.
    Silent { chunk: Vec<u8>, count: usize },
    /// `count` chunks with `gap` between them, then a clean end.
    Trickle {
        chunk: Vec<u8>,
        count: usize,
        gap: Duration,
    },
}

struct Shared {
    routes: Vec<(String, Serve)>,
    default: Serve,
    calls: Mutex<Vec<String>>,
}

pub struct Upstream {
    base: String,
    shared: Arc<Shared>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Upstream {
    /// One plan for every path.
    pub async fn start(serve: Serve) -> Self {
        Self::spawn(Vec::new(), serve).await
    }

    /// Per-path plans, with a 404 for anything unlisted: a mock that answers a
    /// path the test did not declare hides the branch depending on its absence.
    pub async fn start_routes(routes: Vec<(&str, Serve)>) -> Self {
        let routes = routes
            .into_iter()
            .map(|(path, serve)| (path.to_string(), serve))
            .collect();
        Self::spawn(routes, Serve::Status(404)).await
    }

    /// Bind the port WITHOUT deciding what to serve yet: a signed manifest
    /// names absolute artifact URLs, so the base must be known before it.
    pub async fn reserve() -> Reserved {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("d13: the mock upstream could not bind");
        let addr = listener
            .local_addr()
            .expect("d13: the mock upstream has no address");
        Reserved {
            listener,
            base: format!("http://{addr}"),
        }
    }

    async fn spawn(routes: Vec<(String, Serve)>, default: Serve) -> Self {
        Reserved::serve_on(Self::reserve().await, routes, default)
    }
}

/// A bound port whose routes are not decided yet.
pub struct Reserved {
    listener: tokio::net::TcpListener,
    base: String,
}

impl Reserved {
    /// The URL the routes will be served from, known before they are built.
    pub fn base(&self) -> &str {
        &self.base
    }

    pub fn serve(self, routes: Vec<(&str, Serve)>) -> Upstream {
        let routes = routes
            .into_iter()
            .map(|(path, serve)| (path.to_string(), serve))
            .collect();
        Reserved::serve_on(self, routes, Serve::Status(404))
    }

    fn serve_on(self, routes: Vec<(String, Serve)>, default: Serve) -> Upstream {
        let shared = Arc::new(Shared {
            routes,
            default,
            calls: Mutex::new(Vec::new()),
        });
        let app = axum::Router::new()
            .fallback(handle)
            .with_state(Arc::clone(&shared));
        let Reserved { listener, base } = self;
        let (shutdown, rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = rx.await;
                })
                .await;
        });
        Upstream {
            base,
            shared,
            shutdown: Some(shutdown),
        }
    }
}

impl Upstream {
    pub fn base(&self) -> &str {
        &self.base
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    /// Every path the upstream was asked for, in order.
    pub fn calls(&self) -> Vec<String> {
        self.shared
            .calls
            .lock()
            .unwrap_or_else(|err| err.into_inner())
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
    let path = req.uri().path().to_string();
    shared
        .calls
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .push(path.clone());
    let serve = shared
        .routes
        .iter()
        .find(|(route, _)| *route == path)
        .map(|(_, serve)| serve.clone())
        .unwrap_or_else(|| shared.default.clone());
    answer(serve).await
}

async fn answer(serve: Serve) -> Response<Body> {
    match serve {
        Serve::Body(bytes) => Response::builder()
            .status(StatusCode::OK)
            .body(Body::from(bytes))
            .expect("d13: could not build the mock body"),
        Serve::Status(code) => Response::builder()
            .status(StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR))
            .body(Body::empty())
            .expect("d13: could not build the mock status"),
        Serve::Never => std::future::pending().await,
        Serve::Silent { chunk, count } => {
            let head = futures_util::stream::iter(
                (0..count).map(move |_| Ok::<Bytes, std::io::Error>(Bytes::from(chunk.clone()))),
            );
            // Then nothing, forever, connection still open: the stall shape.
            let tail = futures_util::stream::pending::<Result<Bytes, std::io::Error>>();
            Response::builder()
                .status(StatusCode::OK)
                .body(Body::from_stream(futures_util::StreamExt::chain(
                    head, tail,
                )))
                .expect("d13: could not build the silent body")
        }
        Serve::Trickle { chunk, count, gap } => {
            let stream = futures_util::stream::unfold(0usize, move |sent| {
                let chunk = chunk.clone();
                async move {
                    if sent >= count {
                        return None;
                    }
                    if sent > 0 {
                        tokio::time::sleep(gap).await;
                    }
                    Some((Ok::<Bytes, std::io::Error>(Bytes::from(chunk)), sent + 1))
                }
            });
            Response::builder()
                .status(StatusCode::OK)
                .body(Body::from_stream(stream))
                .expect("d13: could not build the trickled body")
        }
    }
}
