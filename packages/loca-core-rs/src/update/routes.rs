//! `GET /api/v1/update/check` and `POST /api/v1/update/apply`, behind
//! `require_client`.
//!
//! A successful apply ends by exiting the process, so its 204 never reaches
//! the client: the dropped connection is the success signal. The exit is
//! performed here behind [`Exiter`] rather than inside `apply_update`, so the
//! exit code is testable as a value and no test can kill the test binary. No
//! graceful shutdown first: the binary on disk has already changed, and every
//! second spent draining is a second the process and the install disagree.
//!
//! A failed apply broadcasts `update.error` and pushes no core status, since
//! the binary did not change (see `events.rs`).

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

/// The beat between staging and exiting: lets a spawned relauncher detach and
/// gives the `update.staged` frame time to reach connected clients.
const DETACH_BEAT: Duration = Duration::from_millis(200);

/// Ending the process, behind a seam. Returns `()` rather than `!` so a test
/// double can come back.
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
    /// `None` when the HTTP client could not be built at boot. See [`routes`].
    net: Option<Arc<Net>>,
    events: UpdateEvents,
    exiter: Arc<dyn Exiter>,
    runner: Arc<dyn Runner>,
    os: Os,
    home: Arc<std::path::PathBuf>,
    platform: Platform,
}

impl UpdateState {
    /// `manifest_fetch_failed` rather than internal: from the caller's side,
    /// the trust root cannot be reached, which is not the same as forged.
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

/// The slice, wired for production. `status` is the concrete service (not
/// `ctx.status`) so `note_update_staged` is reachable.
pub fn routes(ctx: &AppContext, status: Arc<CoreStatusService>) -> Router {
    // Neither failure takes the router down: the rest of the core is
    // serviceable without self-update, and both surface as a 502 instead.
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
    // No HOME degrades supervisor detection to `Background`, which then refuses
    // every apply; logged because the symptom is far from the cause.
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

/// One assembly for production and tests. The auth layer is applied HERE so a
/// route test drives the shipping slice, not a copy that wires its own layer.
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
/// Raw bytes rather than a typed extractor: a malformed or absent body means
/// "whatever the manifest carries", and a strict extractor would 400 it.
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
            // Unreachable in production: `ProcessExiter` does not return.
            Ok(no_content())
        }
        Err(err) => {
            state.events.failed(&err);
            Err(err)
        }
    }
}

/// The requested version; `None` for an empty, non-JSON, non-object, or
/// `version`-less body.
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

        /// The binary an update replaces; must exist and be executable or the
        /// anchor cannot be created.
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

        /// A launchd plist, so the restart is a no-op and the exit code is 0.
        fn under_launchd(&self) {
            let plist =
                crate::update::install::supervisor::plist_path(self.home(), Channel::Stable);
            std::fs::create_dir_all(plist.parent().unwrap()).unwrap();
            std::fs::write(&plist, "<plist/>").unwrap();
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
    /// names. The port is reserved first because the manifest carries ABSOLUTE
    /// artifact URLs. The caller must keep `trusted` alive for the whole test.
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

    /// An env whose apply will succeed: staged upstream, old binary, launchd.
    async fn staged_env(trusted: &Trusted) -> Env {
        let env = env(staged_upstream(trusted, b"the new core").await);
        env.stage_current_binary("the old core");
        env.under_launchd();
        env
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

    /// 404 for every path.
    async fn unreachable_upstream() -> Upstream {
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
        // The only test reaching the end of a successful apply; without it every
        // `codes().is_empty()` elsewhere would be vacuous.
        let _guard = crate::update::testing::rollback_guard().await;
        let trusted = Trusted::new();
        let env = staged_env(&trusted).await;

        let (status, _) = call(&env, "POST", "/api/v1/update/apply", None).await;

        assert_eq!(
            status,
            StatusCode::NO_CONTENT,
            "the 204 returned for the type"
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
        let _guard = crate::update::testing::rollback_guard().await;
        let trusted = Trusted::new();
        let env = staged_env(&trusted).await;
        let mut conn = env.frames.register("c1");

        call(&env, "POST", "/api/v1/update/apply", None).await;

        let frame = next_frame(&mut conn).expect("a staged update broadcasts");
        assert_eq!(frame["kind"], "update.staged");
        assert_eq!(frame["version"], "99.0.0");
    }

    #[tokio::test]
    async fn a_failed_update_broadcasts_the_error_and_pushes_no_status() {
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
        // Reaching 502 proves the body was accepted; that the version was read
        // as absent is pinned by `the_body_reader_treats_every_tolerated_shape_as_absent`.
        let env = env(unreachable_upstream().await);

        for body in [None, Some(Body::empty()), Some(Body::from("{}"))] {
            let (status, value) = call(&env, "POST", "/api/v1/update/apply", body).await;
            assert_eq!(status, StatusCode::BAD_GATEWAY, "{value}");
        }
    }

    #[tokio::test]
    async fn a_malformed_body_is_swallowed_rather_than_rejected() {
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
