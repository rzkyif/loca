//! A local upstream, the published-manifest fixture, and the signing helpers.
//!
//! # Why a real socket
//!
//! Half of what this package has to survive is a property of the SERVER rather
//! than of our code: a 404 where a manifest should be, a host that accepts the
//! connection and never answers, a body that stops mid-transfer. None of those
//! is expressible against a stubbed transport, and D-038's closing note is that
//! a test which stubs the transport cannot see a defect in the transport's own
//! deadline. So these tests drive a real socket on a real clock with the bounds
//! injected (D-036: paused time and real I/O do not mix).
//!
//! # Why the manifest fixture is transcribed rather than built
//!
//! [`published_manifest`] is copied field for field from
//! `scripts/release/core.ts:600-644` (`composeCoreManifest`), **not** built from
//! [`super::manifest::CoreManifest`]. D7's worst finding was a manifest parser
//! that modelled neither shipping channel's real shape and survived a
//! 63-mutation pass, because every fixture had been built from the Rust types:
//! a fixture derived from the implementation cannot see the implementation
//! being wrong. See D-060.
//!
//! What I checked while writing it, since the absence is as load-bearing as the
//! content: `core.json` has ONE published shape. There is no resolver variant
//! and no per-triple asset map, unlike `binaries.json`. The channel affects
//! only the URL the document is fetched from and the `storagePrefix` inside the
//! asset URLs. Neither manifest URL serves anything today (no release has
//! shipped), so the release script is the entire specification.
//!
//! Compiled only under `cfg(test)`: none of this reaches a production binary.
#![allow(clippy::unwrap_used, clippy::expect_used, dead_code)]

use std::io::Write as _;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::{Request, State};
use axum::http::{Response, StatusCode};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// Bound every awaiting test, so a regression fails rather than hangs.
///
/// D-070: in a shared crate a hang is not local. A failing test costs its owner
/// a fix; a hanging test costs every agent in the crate every run, silently,
/// because cargo prints nothing while blocked. The message names the package so
/// a bounded failure is attributable without opening the file.
pub async fn bounded<T>(fut: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(10), fut)
        .await
        .expect("d13: test exceeded its bound")
}

// --- the re-entrancy tripwire over every process-global fixture lock -------

/// Which process-global fixture locks THIS thread is currently holding.
///
/// `#[tokio::test]` builds a current-thread runtime, so a test body is one
/// thread and "this thread already holds it" is exactly "this test already
/// holds it".
#[allow(clippy::missing_const_for_thread_local)]
mod held {
    use std::cell::RefCell;
    thread_local! {
        pub static NAMES: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
    }
}

/// Claim a process-global fixture lock for this thread, refusing re-entry.
///
/// Both locks below are non-reentrant, and neither FAILS when a test asks for
/// one it is already holding: it parks forever, still holding it, so every
/// other test that wants that lock parks too and `cargo test` never returns.
/// One such test (an apply fixture built twice in one body) wedged the whole
/// crate's binary and presented as seven unrelated hangs across three modules,
/// including tests that pass alone in 0.01s and never touch a rollback.
///
/// **The wait is deliberately NOT bounded by a timeout.** The rollback suite
/// serialises by design, so a duration long enough to be safe under load is far
/// too long to catch anything, and a duration short enough to be useful turns a
/// loaded machine into a red suite. What is detected instead is the defect
/// itself: the same thread asking for a lock it already has. That has no timing
/// component, so it is exact under any load.
///
/// D-070's rule is that a hang is a fleet outage rather than a slow test. This
/// is that rule applied to the fixtures instead of the operations: `bounded`
/// wraps the call under test and cannot see a fixture that never returns.
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

/// Serialises every test that drives `rollback::handle_update_marker_on_boot`.
///
/// It exists because [`install_rename_should_fail`] is process-global: the
/// injector it drives sits deep inside `perform_rollback` and there is no
/// argument to thread. The first version of that pairing put the lock beside
/// the ARMING test, which made it safe for that test and for nothing else, and
/// a neighbour driving a rollback concurrently inherited the injected failure
/// and went red for a reason unrelated to what it asserted. It passed alone and
/// failed in the suite, which is the signature.
///
/// So the lock lives HERE, one home for two modules, and every rollback fixture
/// takes it whether or not it knows the injector exists. D-049's shape: one
/// control in one place rather than a convention each call site re-implements.
/// A `tokio::sync::Mutex`, not a `std` one, and that is not a style choice.
///
/// Every fixture holding this guard is async and awaits while holding it, and a
/// `std::sync::MutexGuard` held across an `.await` parks the task with the lock
/// still taken: anything else on that runtime wanting it blocks a THREAD rather
/// than yielding, which on a single-threaded test runtime is a hang. This
/// package has already lost runs to a hanging test (D-070), and a hang never
/// produces a red. Tokio's guard is designed to be held across await points.
static ROLLBACK_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
static FAIL_INSTALL_RENAME: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Hold for the life of a test that stages a boot decision.
///
/// The claim is taken BEFORE the lock, so a test that asks twice panics with a
/// name instead of parking. See [`claim_fixture_lock`].
pub async fn rollback_guard() -> RollbackGuard {
    let claim = claim_fixture_lock("rollback");
    RollbackGuard {
        _lock: ROLLBACK_LOCK.lock().await,
        _claim: claim,
    }
}

/// What [`rollback_guard`] hands back: the lock, plus the claim that makes a
/// second request from the same test loud rather than fatal.
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
        // The hazard is not hypothetical: one apply test that built two
        // fixtures in one body parked forever holding both global locks and
        // took the whole crate's test binary with it.
        //
        // The second acquisition is BOUNDED rather than bare, and that is the
        // load-bearing part. With the tripwire deleted this test would park,
        // and a parked test is not a red: it is the exact outage being fixed,
        // reintroduced by the test that claims to cover it. Bounded, the same
        // deletion fails on the WRONG panic message, so the mutation is red in
        // seconds instead of silent forever.
        let _first = rollback_guard().await;
        let _second = bounded(rollback_guard()).await;
    }

    #[tokio::test]
    async fn a_claim_released_by_the_first_guard_can_be_taken_again() {
        // The other half. A tripwire that never lets go would serialise nothing
        // and fail everything, and a test of the panic alone cannot see that.
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

/// Set while a test wants `swap::create_anchor` to take its COPY fallback.
///
/// The fallback exists for filesystems without hard links, and every filesystem
/// a test can create here has them, so the copy path is unreachable from
/// outside and the branch would be covered only by its failure case. That is
/// the shape this package keeps meeting: a branch that does nothing on the host
/// the fleet runs on, under a test whose NAME claims to cover it.
static NO_HARD_LINKS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn hard_links_disabled() -> bool {
    NO_HARD_LINKS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Only call while holding [`rollback_guard`]. The swap fixtures do.
pub fn set_hard_links_disabled(disabled: bool) {
    NO_HARD_LINKS.store(disabled, std::sync::atomic::Ordering::Relaxed);
}

/// Lowercase-hex sha256. Fixture digests are COMPUTED rather than pasted, so a
/// test cannot pass by asserting a constant copied out of the code it tests.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

/// Gzip `bytes`, the way the release script ships every core artifact.
///
/// The manifest's sha256 is over the DECOMPRESSED file
/// (`scripts/release/core.ts:608`), so a fixture has to carry both halves: the
/// compressed bytes the server sends and the digest of what comes out.
pub fn gzipped(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(bytes).expect("d13: gzip encode failed");
    encoder.finish().expect("d13: gzip finish failed")
}

// --- the published manifest ------------------------------------------------

/// A `core.json` in the shape `composeCoreManifest` emits.
///
/// Transcribed from `scripts/release/core.ts:610-644`: `binaries` keyed by
/// triple with a `.gz` URL and the decompressed sha256; `workers` by name;
/// `helpers` by name AND triple, because only the entries matching the running
/// triple apply.
///
/// The `signature` is a syntactically valid placeholder rather than absent,
/// because `composeCoreManifest` returns `{ ...manifest, signature }` and a
/// published document therefore ALWAYS carries one. Sign with
/// [`test_trust::Trusted::sign`] to get a document the production verifier
/// accepts; it strips this field before building the message, so the
/// placeholder never reaches the signed bytes.
///
/// [`test_trust::Trusted::sign`]: super::manifest::test_trust::Trusted::sign
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
    /// Accepts the connection and never answers. A black-holed host, which is
    /// the only shape a deadline defends against: the socket is up, so nothing
    /// fails and the caller waits forever.
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

    /// Per-path plans, with a 404 for anything unlisted.
    ///
    /// The 404 default is deliberate rather than convenient: a mock that
    /// helpfully answers a path the test did not declare hides the branch that
    /// depends on it being absent, which is D-050 case 1.
    pub async fn start_routes(routes: Vec<(&str, Serve)>) -> Self {
        let routes = routes
            .into_iter()
            .map(|(path, serve)| (path.to_string(), serve))
            .collect();
        Self::spawn(routes, Serve::Status(404)).await
    }

    /// Bind the port WITHOUT deciding what to serve yet.
    ///
    /// A signed manifest names absolute artifact URLs, so a fixture has to know
    /// the base before it can build the document it is going to serve. Without
    /// this the manifest carries a relative URL, the download fails at the HTTP
    /// client, and the test reports a broken apply path rather than a broken
    /// fixture.
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

    /// Every path the upstream was asked for, in order. Lets a test assert that
    /// a path was reached, or was not.
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
            // Then nothing, forever, with the connection still open: the shape
            // the stall watchdog is the only defence against.
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
