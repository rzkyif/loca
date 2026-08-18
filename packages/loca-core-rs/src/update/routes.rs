//! `GET /api/v1/update/check` and `POST /api/v1/update/apply`.
//!
//! Ported from `http/routes/update.ts`, mounted behind `bearerMiddleware()`,
//! which is `require_client` here. Both shapes are already generated
//! (`UpdateCheckResponse`, `UpdateApplyRequest`), so nothing on the wire is
//! hand-written.
//!
//! # The 204 is never delivered, and that is the specification
//!
//! A successful apply ends by exiting the process, so the response the handler
//! returns does not reach the client: the connection drops instead. The oracle
//! says exactly this in its own comment and returns the 204 "for the
//! typecheck". This port keeps that, and the client already treats the dropped
//! connection as the success signal.
//!
//! The exit is NOT performed inside [`super::apply::apply_update`], which
//! returns the code as a value. Reproducing `Deno.exit` where the oracle has it
//! would mean any test reaching that path kills the whole crate's test binary
//! and every other package's run with it (D-070 arriving by another route). So
//! the decision is computed in `apply.rs`, where it is asserted as a value, and
//! performed here behind [`Exiter`], whose production implementation is the
//! only `process::exit` in the package.
//!
//! # No graceful shutdown before the exit
//!
//! The oracle drops in-flight requests and severs open WS connections
//! (`Deno.exit` from inside the handler). `Core::shutdown()` exists here and
//! would drain them, and NOT calling it is deliberate: an apply has already
//! swapped the binary on disk, so every extra second spent draining is a second
//! in which the running process and the installed binary disagree, and the
//! supervisor is about to relaunch regardless. Matching the oracle also keeps
//! the conformance comparison honest. Recorded here rather than left to look
//! like an oversight (Q4).
//!
//! # What a failed apply does NOT touch
//!
//! `update.error` broadcasts, and pushes no core status. See `events.rs`: only
//! `staged` sets "updating", because a failed update did not change the binary.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::response::Response;
use axum::routing::{get, post};
use axum::Router;

use crate::config::Config;
use crate::contract::api::{UpdateApplyRequest, UpdateCheckResponse};
use crate::error::Result;
use crate::paths::Paths;
use crate::transport::http::{no_content, Json};
use crate::transport::middleware::{from_fn_with_state, require_client};
use crate::transport::state::AppContext;
use crate::transport::status::CoreStatusService;

use super::apply::{apply_update, check_for_update, UpdateDeps};
use super::events::UpdateEvents;
use super::install::proc::{real_home, Runner, SystemRunner};
use super::install::supervisor::Os;
use super::net::Net;
use super::swap::Platform;

/// The beat between staging an update and exiting.
///
/// `self-updater.ts:305-309` emits `staged`, waits, and then exits. The wait is
/// what lets a spawned relauncher detach from this process before its parent
/// goes away, and it is also the only window the `update.staged` frame has to
/// reach a connected client, since the exit severs every socket.
const DETACH_BEAT: Duration = Duration::from_millis(200);

/// Ending the process, behind a seam.
///
/// One production implementation, in this file so it cannot drift from the
/// caller. The trait returns `()` rather than `!` because a double has to be
/// able to return: `!` is implementable only by something that genuinely never
/// comes back, which would put the four supervisor branches out of reach of any
/// test (Q5).
pub trait Exiter: Send + Sync {
    fn exit(&self, code: i32);
}

/// The real one. Does not return.
pub struct ProcessExiter;

impl Exiter for ProcessExiter {
    fn exit(&self, code: i32) {
        std::process::exit(code)
    }
}

#[derive(Clone)]
struct UpdateState {
    config: Arc<Config>,
    paths: Arc<Paths>,
    /// `None` when the HTTP client could not be built at boot. Absent rather
    /// than fatal: see [`routes`].
    net: Option<Arc<Net>>,
    events: UpdateEvents,
    exiter: Arc<dyn Exiter>,
    runner: Arc<dyn Runner>,
    os: Os,
    home: Arc<std::path::PathBuf>,
    platform: Platform,
}

impl UpdateState {
    /// The client, or the failure the routes report when there is none.
    ///
    /// `manifest_fetch_failed` rather than an internal error because that is
    /// what it is from the caller's side: the trust root cannot be reached.
    /// The distinction the client draws is reachable-versus-forged, and a
    /// missing client is squarely the first.
    fn net(&self) -> Result<&Net> {
        self.net.as_deref().ok_or_else(|| {
            crate::error::AppError::new(
                crate::error::ErrorCode::ManifestFetchFailed,
                "the update client could not be created, so the release manifest \
                 cannot be fetched",
            )
        })
    }
}

/// The slice, wired for production.
///
/// `status` is the concrete `Arc<CoreStatusService>` rather than
/// `ctx.status`, which is an `Arc<dyn StatusProvider>` exposing only
/// `snapshot()`. Threading it is what makes `note_update_staged` reachable at
/// all; without this argument the core bar can never show "updating".
pub fn routes(ctx: &AppContext, status: Arc<CoreStatusService>) -> Router {
    // A failure to build the client or to find HOME must not take the whole
    // router down: the rest of the core is perfectly serviceable without
    // self-update, and refusing to boot over it would turn a degraded feature
    // into an outage. Both surface as a 502 from the route instead.
    let net = match Net::new() {
        Ok(net) => Some(Arc::new(net)),
        Err(err) => {
            tracing::error!(
                target: "loca::update",
                "the update client could not be created, so self-update is \
                 unavailable on this core: {}",
                err.message,
            );
            None
        }
    };
    // An absent HOME degrades supervisor detection to `Background`, which the
    // apply path then refuses unless the updater helper is installed. Logged
    // rather than swallowed, because the symptom (every apply refusing) is a
    // long way from the cause.
    let home = real_home().unwrap_or_else(|err| {
        tracing::error!(
            target: "loca::update",
            "no home directory, so this core cannot tell which supervisor owns \
             it: {}",
            err.message,
        );
        std::path::PathBuf::new()
    });
    build(
        ctx,
        UpdateState {
            config: Arc::clone(&ctx.config),
            paths: Arc::clone(&ctx.paths),
            net,
            events: UpdateEvents::new(Arc::clone(&ctx.frames), Some(status)),
            exiter: Arc::new(ProcessExiter),
            runner: Arc::new(SystemRunner),
            os: Os::host(),
            home: Arc::new(home),
            platform: Platform::host(),
        },
    )
}

/// One assembly for both the production wiring and the tests.
///
/// The handlers AND the auth layer are applied here, so a route test drives the
/// SHIPPING slice with different state rather than a second copy of it. D-084:
/// a test that drives its own copy of production code tests the copy.
///
/// The layer being inside this function rather than beside the caller is not
/// tidiness. It was outside first, and the test applied its own: with the
/// production `.layer(...)` deleted, all ten route tests still passed, because
/// `both_routes_need_a_bearer` was asserting on the layer the TEST had wired.
/// The update routes would have shipped unauthenticated with a green suite
/// naming the exact property that was broken. Staged and watched.
fn build(ctx: &AppContext, state: UpdateState) -> Router {
    Router::new()
        .route("/api/v1/update/check", get(check))
        .route("/api/v1/update/apply", post(apply))
        .layer(from_fn_with_state(ctx.clone(), require_client))
        .with_state(state)
}

async fn check(State(state): State<UpdateState>) -> Result<Json<UpdateCheckResponse>> {
    check_for_update(&state.config, state.net()?)
        .await
        .map(Json)
}

/// Stage an update, tell everyone, and end the process.
///
/// The body is read with [`axum::extract::Request`] rather than a typed
/// extractor because the oracle SWALLOWS a malformed body and leaves `version`
/// undefined (`routes/update.ts:16-22`). A client sending no body at all is the
/// normal case: `update.apply()` with no argument serialises to `{}`, and some
/// send nothing. Extracting `UpdateApplyRequest` strictly would 400 exactly the
/// requests the oracle accepts.
async fn apply(State(state): State<UpdateState>, body: axum::body::Bytes) -> Result<Response> {
    let version = requested_version(&body);
    let deps = UpdateDeps {
        net: state.net()?,
        runner: state.runner.as_ref(),
        os: state.os,
        home: state.home.as_path(),
        platform: state.platform,
    };
    match apply_update(&state.config, &state.paths, &deps, version.as_deref()).await {
        Ok(applied) => {
            state.events.staged(&applied.version);
            tokio::time::sleep(DETACH_BEAT).await;
            state.exiter.exit(applied.exit_code);
            // Unreachable in production: `ProcessExiter` does not return. Kept
            // for the type, exactly as the oracle keeps its 204.
            Ok(no_content())
        }
        Err(err) => {
            state.events.failed(&err);
            Err(err)
        }
    }
}

/// The requested version, or `None` for every shape the oracle tolerates.
///
/// An empty body, a body that is not JSON, a JSON value that is not an object,
/// and an object with no `version` all mean "whatever the manifest carries".
fn requested_version(body: &[u8]) -> Option<String> {
    serde_json::from_slice::<UpdateApplyRequest>(body)
        .ok()
        .and_then(|parsed| parsed.version)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::sync::Mutex;

    use axum::body::{to_bytes, Body};
    use axum::http::{Request, StatusCode};
    use serde_json::{json, Value};
    use tower::ServiceExt as _;

    use super::*;
    use crate::config::{Channel, CORE_VERSION};
    use crate::error::ErrorCode;
    use crate::transport::testing::{context_with, StubAuth};
    use crate::update::install::proc::testing::RecordingRunner;
    use crate::update::manifest::test_trust::Trusted;
    use crate::update::testing::{published_manifest, Serve, Upstream};

    const TOKEN: &str = "token-abc";

    /// Records the code instead of ending the process.
    #[derive(Default)]
    struct RecordingExiter(Mutex<Vec<i32>>);

    impl RecordingExiter {
        fn codes(&self) -> Vec<i32> {
            self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
        }
    }

    impl Exiter for RecordingExiter {
        fn exit(&self, code: i32) {
            self.0.lock().unwrap_or_else(|e| e.into_inner()).push(code);
        }
    }

    struct Env {
        router: Router,
        exiter: Arc<RecordingExiter>,
        frames: Arc<crate::transport::FrameBus>,
        paths: Arc<crate::paths::Paths>,
        _up: Upstream,
        _dir: tempfile::TempDir,
        home: tempfile::TempDir,
    }

    impl Env {
        fn home(&self) -> &std::path::Path {
            self.home.path()
        }

        fn current_binary(&self) -> std::path::PathBuf {
            self.paths
                .bin_path(&Channel::Stable.core_binary_name("loca-core"))
        }

        /// The binary an update replaces. It has to exist and be executable, or
        /// the anchor cannot be created and the swap refuses before it starts.
        fn stage_current_binary(&self, contents: &str) {
            let path = self.current_binary();
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, contents).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
    }

    /// A slice whose upstream serves `routes`, behind the real auth layer.
    fn env(up: Upstream) -> Env {
        let ctx = context_with(Arc::new(StubAuth::new().with_client(TOKEN, "client-1")));
        ctx.paths.ensure_dirs().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let exiter = Arc::new(RecordingExiter::default());
        let config = Arc::new(Config {
            channel: Channel::Stable,
            host: "127.0.0.1".into(),
            port: 0,
            version: CORE_VERSION,
            storage_base_url: up.base().to_string(),
            hf_base_url: up.base().to_string(),
        });
        let paths = Arc::new(crate::paths::Paths::rooted(dir.path().join("core")));
        paths.ensure_dirs().unwrap();
        let state = UpdateState {
            config,
            paths: Arc::clone(&paths),
            net: Some(Arc::new(Net::new().unwrap())),
            events: UpdateEvents::new(Arc::clone(&ctx.frames), None),
            exiter: Arc::clone(&exiter) as Arc<dyn Exiter>,
            runner: Arc::new(RecordingRunner::new()),
            os: Os::Macos,
            home: Arc::new(home.path().to_path_buf()),
            platform: Platform::Unix,
        };
        let router = build(&ctx, state);
        Env {
            router,
            exiter,
            frames: Arc::clone(&ctx.frames),
            paths,
            _up: up,
            _dir: dir,
            home,
        }
    }

    async fn call(env: &Env, method: &str, path: &str, body: Option<Body>) -> (u16, Value) {
        let request = Request::builder()
            .method(method)
            .uri(path)
            .header("authorization", format!("Bearer {TOKEN}"))
            .header("content-type", "application/json")
            .body(body.unwrap_or_else(Body::empty))
            .unwrap();
        let response = env.router.clone().oneshot(request).await.unwrap();
        let status = response.status().as_u16();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        };
        (status, value)
    }

    /// An upstream serving a signed 99.0.0 manifest and the gzipped binary it
    /// names, with the port reserved first because a signed manifest carries
    /// ABSOLUTE artifact URLs.
    ///
    /// **The caller must BIND the `Trusted` for the life of the test.** It
    /// swaps the trust root only while it is alive, so passing `&Trusted::new()`
    /// hands over a temporary that is dropped at the end of the call
    /// expression, the real production key comes back, and every request then
    /// fails with `signature_invalid`. Both tests below were written that way
    /// first and said so.
    async fn staged_upstream(trusted: &Trusted, payload: &[u8]) -> Upstream {
        let reserved = Upstream::reserve().await;
        let mut doc = published_manifest();
        {
            let object = doc.as_object_mut().unwrap();
            object.insert("version".into(), json!("99.0.0"));
            object.insert(
                "binaries".into(),
                json!([{
                    "triple": crate::config::host_triple(),
                    "url": format!("{}/core.gz", reserved.base()),
                    "sha256": crate::update::testing::sha256_hex(payload),
                }]),
            );
            object.insert("workers".into(), json!([]));
            object.insert("helpers".into(), json!([]));
        }
        let raw = trusted.sign_to_string(doc);
        reserved.serve(vec![
            ("/manifests/core.json", Serve::Body(raw.into_bytes())),
            (
                "/core.gz",
                Serve::Body(crate::update::testing::gzipped(payload)),
            ),
        ])
    }

    fn next_frame(conn: &mut crate::transport::Connection) -> Option<Value> {
        use futures_util::FutureExt as _;
        match conn.next().now_or_never().flatten() {
            Some(crate::transport::Outbound::Frame(json)) => {
                Some(serde_json::from_str(&json).unwrap())
            }
            Some(other) => panic!("expected a frame, got {other:?}"),
            None => None,
        }
    }

    async fn unreachable_upstream() -> Upstream {
        // 404 for every path, which is what both cores see against the
        // conformance harness's mock: it serves no manifest path at all.
        Upstream::start_routes(vec![]).await
    }

    #[tokio::test]
    async fn check_reports_the_versions_and_the_manifest_url() {
        let trusted = Trusted::new();
        let mut doc = published_manifest();
        doc.as_object_mut().unwrap()["version"] = json!("99.0.0");
        let raw = trusted.sign_to_string(doc);
        let up = Upstream::start_routes(vec![(
            "/manifests/core.json",
            Serve::Body(raw.into_bytes()),
        )])
        .await;
        let env = env(up);

        let (status, body) = call(&env, "GET", "/api/v1/update/check", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["latestVersion"], "99.0.0");
        assert_eq!(body["currentVersion"], CORE_VERSION);
        assert_eq!(body["available"], true);
        assert!(
            body["manifestUrl"].as_str().unwrap().ends_with("core.json"),
            "{body}"
        );
    }

    #[tokio::test]
    async fn an_unreachable_manifest_is_a_bad_gateway_rather_than_a_tamper_report() {
        // The two failures are not interchangeable. `manifest_fetch_failed`
        // means the trust root could not be REACHED; `signature_invalid` means
        // it answered with something forged. Reporting the first as the second
        // would tell a user with a captive portal that their release is
        // compromised.
        let env = env(unreachable_upstream().await);

        let (status, body) = call(&env, "GET", "/api/v1/update/check", None).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(
            body["error"]["code"],
            ErrorCode::ManifestFetchFailed.as_str()
        );
    }

    #[tokio::test]
    async fn apply_refuses_before_touching_anything_and_exits_nothing() {
        // The property the conformance case of the same name asserts on the
        // wire: a failed apply must not have staged, swapped or exited. The
        // exit assertion is the one that matters, because a request that failed
        // and a core that swapped its binary and died look identical from
        // outside.
        let env = env(unreachable_upstream().await);

        let (status, body) = call(&env, "POST", "/api/v1/update/apply", None).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(
            body["error"]["code"],
            ErrorCode::ManifestFetchFailed.as_str()
        );
        assert!(
            env.exiter.codes().is_empty(),
            "a refused apply must not end the process"
        );
    }

    #[tokio::test]
    async fn a_staged_update_exits_with_the_code_apply_chose() {
        // Without this the exit assertions everywhere else are vacuous: an
        // `exit()` that is never called for ANY outcome makes every
        // `codes().is_empty()` pass for a reason unrelated to what it names.
        // That is D-050 case 3, the same shape as a call counter that never
        // counts, and it cost D7 the enforcement of an absolute rule.
        //
        // This is also the only test in the package that reaches the end of a
        // successful apply, which is unreachable from conformance entirely: on
        // the wire it is indistinguishable from the core crashing.
        let _guard = crate::update::testing::rollback_guard().await;
        let trusted = Trusted::new();
        let up = staged_upstream(&trusted, b"the new core").await;
        let env = env(up);

        // launchd, so the restart is a no-op and the chosen code is 0. The
        // binary being replaced has to exist for the anchor to be creatable.
        env.stage_current_binary("the old core");
        let plist = crate::update::install::supervisor::plist_path(env.home(), Channel::Stable);
        std::fs::create_dir_all(plist.parent().unwrap()).unwrap();
        std::fs::write(&plist, "<plist/>").unwrap();

        let (status, _) = call(&env, "POST", "/api/v1/update/apply", None).await;

        assert_eq!(
            status,
            StatusCode::NO_CONTENT,
            "the 204 the oracle returns for the typecheck"
        );
        assert_eq!(
            env.exiter.codes(),
            vec![0],
            "launchd relaunches on any exit, so a staged update exits cleanly"
        );
        assert_eq!(
            std::fs::read_to_string(env.current_binary()).unwrap(),
            "the new core",
            "and the swap really happened, so the exit is not the only evidence"
        );
    }

    #[tokio::test]
    async fn a_staged_update_notifies_before_it_exits() {
        // The frame is the ONLY warning a connected client gets: the exit
        // severs every socket a fraction of a second later, and the 204 never
        // arrives. Emitting it after the exit, or not at all, leaves the client
        // showing a live core that has already gone.
        let _guard = crate::update::testing::rollback_guard().await;
        let trusted = Trusted::new();
        let up = staged_upstream(&trusted, b"the new core").await;
        let env = env(up);
        env.stage_current_binary("the old core");
        let plist = crate::update::install::supervisor::plist_path(env.home(), Channel::Stable);
        std::fs::create_dir_all(plist.parent().unwrap()).unwrap();
        std::fs::write(&plist, "<plist/>").unwrap();
        let mut conn = env.frames.register("c1");

        call(&env, "POST", "/api/v1/update/apply", None).await;

        let frame = next_frame(&mut conn).expect("a staged update broadcasts");
        assert_eq!(frame["kind"], "update.staged");
        assert_eq!(frame["version"], "99.0.0");
    }

    #[tokio::test]
    async fn a_failed_update_broadcasts_the_error_and_pushes_no_status() {
        // The asymmetry `events.rs` documents, asserted at the route so it is
        // not just a claim in a doc comment: a failure tells the client, and
        // deliberately does NOT set the core bar to "updating", because the
        // binary did not change.
        let env = env(unreachable_upstream().await);
        let mut conn = env.frames.register("c1");

        call(&env, "POST", "/api/v1/update/apply", None).await;

        let frame = next_frame(&mut conn).expect("a failed update broadcasts too");
        assert_eq!(frame["kind"], "update.error");
        assert_eq!(frame["code"], ErrorCode::ManifestFetchFailed.as_str());
        assert!(
            next_frame(&mut conn).is_none(),
            "and nothing else: no staged frame for an update that did not happen"
        );
    }

    #[tokio::test]
    async fn an_empty_body_is_the_normal_case_rather_than_a_bad_request() {
        // `update.apply()` with no argument serialises to `{}`, and some
        // clients send nothing at all. A typed extractor would 400 both, which
        // is the shape the oracle deliberately avoids by swallowing the parse.
        // Asserted through the FAILURE the upstream produces, because reaching
        // 502 proves the body was accepted and the operation was attempted.
        //
        // **What this cannot see, stated because the name overclaims.** It
        // covers "the body was accepted", not "the version was read as
        // absent". Every request here dies at the unreachable manifest, which
        // is BEFORE the version is compared, so a reader that invented a
        // version instead of returning `None` produces exactly this 502 too.
        // That mutation was staged and survived here; it is caught by
        // `the_body_reader_treats_every_tolerated_shape_as_absent` and by the
        // two tests that drive a reachable manifest, where an invented version
        // mismatches the manifest and is refused. Same shape as the vacuous
        // conformance case in plan section 15: an assertion can hold for a
        // reason unrelated to the property it names.
        let env = env(unreachable_upstream().await);

        for body in [None, Some(Body::empty()), Some(Body::from("{}"))] {
            let (status, value) = call(&env, "POST", "/api/v1/update/apply", body).await;
            assert_eq!(status, StatusCode::BAD_GATEWAY, "{value}");
        }
    }

    #[tokio::test]
    async fn a_malformed_body_is_swallowed_exactly_as_the_oracle_swallows_it() {
        // `routes/update.ts:16-22` catches the parse and leaves `version`
        // undefined. Rejecting here instead would be a behaviour change that no
        // conformance case could see, because both cores fail this request for
        // the unreachable manifest either way.
        let env = env(unreachable_upstream().await);

        let (status, _) = call(
            &env,
            "POST",
            "/api/v1/update/apply",
            Some(Body::from("not json at all")),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn both_routes_need_a_bearer() {
        let env = env(unreachable_upstream().await);
        for (method, path) in [
            ("GET", "/api/v1/update/check"),
            ("POST", "/api/v1/update/apply"),
        ] {
            let response = env
                .router
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(path)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{path}");
        }
    }

    #[test]
    fn the_body_reader_treats_every_tolerated_shape_as_absent() {
        // The four shapes the oracle's try/catch admits, plus the one it reads.
        // Pinned directly as well as through the route, because the route-level
        // assertions above all land on the same 502 and could not distinguish
        // "version was absent" from "version was ignored".
        assert_eq!(requested_version(b""), None);
        assert_eq!(requested_version(b"not json"), None);
        assert_eq!(requested_version(b"[]"), None);
        assert_eq!(requested_version(b"{}"), None);
        assert_eq!(
            requested_version(br#"{"version":"1.2.3"}"#),
            Some("1.2.3".to_string())
        );
    }
}
