//! `GET /update/check` and `POST /update/apply`: the whole self-update flow.
//!
//! Ported from `checkForUpdate` and `applyUpdateInner`
//! (`self-updater.ts:76-310`).
//!
//! # The order, and what each step's position protects
//!
//! | step                                                    | why there                                                          |
//! | ------------------------------------------------------- | ------------------------------------------------------------------ |
//! | fetch and verify the manifest                           | nothing is touched until the document is authentic                 |
//! | refuse a version mismatch, then a downgrade             | see below                                                          |
//! | pick the entry for this triple                          | no entry is a clean refusal, before any disk write                 |
//! | clear and recreate `staging/`                           | staging holds only the set about to be verified; it never accumulates |
//! | download and verify EVERY artifact into staging         | a network drop on artifact N leaves staging dirty and the live dirs consistent with the OLD binary |
//! | resolve the supervisor, check the updater helper        | **before** any rename, so a broken install leaves the live dirs untouched |
//! | commit the worker and helper renames                    | as close to the swap as possible, to minimise skew                 |
//! | **write the rollback marker**                           | see `swap.rs`: BEFORE the swap, unlike the oracle                  |
//! | swap the binary, anchoring `<bin>.old`                  |                                                                     |
//! | restart through the supervisor, return the exit code    |                                                                     |
//!
//! # Downgrades are refused twice, and the two comparisons differ
//!
//! `check` reports `available` only for a STRICTLY newer version
//! (`self-updater.ts:85`), so the UI never offers an update `apply` would then
//! reject. `apply` refuses only an OLDER one (`:116`), so re-applying the
//! running version is a repair rather than a downgrade. Widening either to
//! match the other is a one-character change that a single test cannot see, so
//! both are pinned separately.
//!
//! A validly-signed OLDER manifest is still validly signed. That is the whole
//! reason this is `crate::config::compare_semver` (D-053) and not a string
//! comparison, which gets `1.10.0` versus `1.9.0` backwards.
//!
//! # The exit is the caller's
//!
//! The oracle ends in `Deno.exit` inside the HTTP handler, so the 204 is never
//! delivered. Reproducing that literally would mean a `cargo test` that reaches
//! this path kills the whole test binary and every other package's run with it.
//! So [`apply_update`] RETURNS the exit code and `routes.rs` performs the exit
//! after the same 200ms detach beat. That needs no trait and no injector: the
//! decision is a value, and a value is testable.

use std::path::Path;

use crate::config::{compare_semver, host_triple, Config};
use crate::contract::api::UpdateCheckResponse;
use crate::error::{AppError, ErrorCode, Result};
use crate::paths::Paths;
use crate::update::artifact::{download_and_verify, Decompress};
use crate::update::install::proc::Runner;
use crate::update::install::supervisor::{resolve_supervisor, Os, Supervisor};
use crate::update::manifest::{fetch_core_manifest, CoreManifest};
use crate::update::net::Net;
use crate::update::swap::{commit_rename, swap_binary_in_place, Platform};
use crate::update::{marker, swap};

/// The exit code a restart-requesting run uses.
///
/// Non-zero on purpose: launchd relaunches on any exit, but systemd's
/// `Restart=on-failure` and Task Scheduler's restart-on-failure only relaunch on
/// a failure, so a clean exit there would leave the core down. The same code the
/// rollback path uses, so a supervisor cannot tell them apart and does not need
/// to.
pub const RESTART_EXIT_CODE: i32 = 75;

/// What the caller does once `apply` returns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Applied {
    pub version: String,
    pub exit_code: i32,
}

/// Everything the apply path needs that is not a value.
pub struct UpdateDeps<'a> {
    pub net: &'a Net,
    pub runner: &'a dyn Runner,
    pub os: Os,
    pub home: &'a Path,
    pub platform: Platform,
}

/// Fetch the signed manifest and compare it against the running version.
pub async fn check_for_update(config: &Config, net: &Net) -> Result<UpdateCheckResponse> {
    let manifest = fetch_core_manifest(net, &config.core_manifest_url()).await?;
    Ok(UpdateCheckResponse {
        current_version: config.version.to_string(),
        latest_version: manifest.version.clone(),
        // STRICTLY newer. An equal or older published version reports
        // unavailable, matching `apply`'s refusal, so the UI never offers an
        // update that the apply step would reject.
        available: compare_semver(&manifest.version, config.version).is_gt(),
        manifest_url: config.core_manifest_url(),
    })
}

/// Stage an update and hand the restart to whoever owns this core's lifetime.
///
/// Returns the exit code the caller must use. See the module docs for why the
/// exit is not performed here.
pub async fn apply_update(
    config: &Config,
    paths: &Paths,
    deps: &UpdateDeps<'_>,
    target_version: Option<&str>,
) -> Result<Applied> {
    let manifest = fetch_core_manifest(deps.net, &config.core_manifest_url()).await?;

    // D-043: an empty string is ABSENT, because the oracle tests
    // `if (targetVersion && ...)`. A form that clears a field to "" rather than
    // omitting the key otherwise pins the update to a version named nowhere.
    if let Some(target) = target_version.filter(|v| !v.is_empty()) {
        if target != manifest.version {
            return Err(AppError::new(
                ErrorCode::UpdateFailed,
                format!(
                    "manifest version {} does not match requested {target}",
                    manifest.version
                ),
            ));
        }
    }
    if compare_semver(&manifest.version, config.version).is_lt() {
        return Err(AppError::new(
            ErrorCode::UpdateFailed,
            format!(
                "refusing downgrade: manifest v{} is older than running v{}",
                manifest.version, config.version
            ),
        ));
    }

    let triple = host_triple();
    let entry = manifest.binary_for(triple).ok_or_else(|| {
        AppError::new(
            ErrorCode::UpdateFailed,
            format!("no binary for triple {triple} in manifest"),
        )
    })?;

    // Cleared wholesale so staging only ever holds the set about to be
    // verified. Safe because an apply runs once and then the process exits.
    let staging = paths.staging_dir();
    let _ = tokio::fs::remove_dir_all(&staging).await;
    tokio::fs::create_dir_all(&staging).await?;

    let exe = crate::config::platform_exe();
    let staged_core = staging.join(format!(
        "{}-{}{exe}",
        config.channel.bin_name("loca-core"),
        manifest.version
    ));
    download_and_verify(
        deps.net,
        &entry.url,
        &staged_core,
        &entry.sha256,
        Decompress::Gzip,
    )
    .await?;

    // Every worker and matching helper is downloaded and verified into staging
    // FIRST, and only renamed into the live directories once the whole set is
    // good. A failure on artifact N then leaves staging dirty and the live
    // workers consistent with the old binary, rather than half-installing a
    // manifest.
    let pending = stage_artifacts(deps.net, paths, &manifest, triple, exe).await?;

    let current_bin = paths.bin_path(&config.channel.core_binary_name("loca-core"));
    let updater_bin = paths.bin_path(&config.channel.core_binary_name("loca-core-updater"));
    let supervisor = resolve_supervisor(deps.runner, deps.os, deps.home, config.channel).await;
    let updater_ready = tokio::fs::metadata(&updater_bin).await.is_ok();

    // Checked BEFORE any rename. In background mode the updater is the only
    // thing that can relaunch core, so a missing one is fatal and bailing here
    // leaves the live directories untouched. On Windows it is the fast path
    // only, because Task Scheduler's restart-on-failure is the backstop, so a
    // missing one costs latency rather than the update.
    if supervisor == Supervisor::Background && !updater_ready {
        return Err(AppError::new(
            ErrorCode::UpdateFailed,
            format!(
                "loca-core-updater is not installed at {}; with no supervisor it is the \
                 only thing that can relaunch core, so this update would leave it down. \
                 Reinstall the Core.",
                updater_bin.display()
            ),
        ));
    }
    if deps.platform == Platform::Windows && !updater_ready {
        tracing::warn!(
            target: "loca::update",
            "loca-core-updater missing at {}; relying on Task Scheduler \
             restart-on-failure to relaunch core",
            updater_bin.display(),
        );
    }

    for (from, to, label) in &pending {
        commit_rename(from, to, deps.platform).await?;
        tracing::info!(target: "loca::update", "updated {label}");
    }

    // BEFORE the swap. See `swap.rs`: the oracle writes it after, which leaves
    // the new binary installed with a good anchor and nothing pointing at
    // either, and a broken release in that window crash-loops with its own
    // recovery unused. Written first, a crash before the swap leaves a marker
    // whose `previousVersion` is the running version, which the boot path
    // already clears as "already rolled back".
    marker::write(paths, &manifest.version, config.version).await?;

    if let Err(err) = swap_binary_in_place(&staged_core, &current_bin, deps.platform).await {
        // Nothing was swapped, so the marker describes an update that did not
        // happen. Removed here, with the boot path's version-mismatch branch as
        // the backstop if this process dies before it runs.
        marker::delete(paths).await;
        return Err(err);
    }

    let exit_code = restart(deps, &supervisor, &updater_bin, &current_bin, updater_ready).await;
    Ok(Applied {
        version: manifest.version,
        exit_code,
    })
}

/// Download and verify every worker and matching helper, returning the renames
/// to commit.
async fn stage_artifacts(
    net: &Net,
    paths: &Paths,
    manifest: &CoreManifest,
    triple: &str,
    exe: &str,
) -> Result<Vec<(std::path::PathBuf, std::path::PathBuf, String)>> {
    let staging = paths.staging_dir();
    let mut pending = Vec::new();

    for worker in manifest.workers() {
        let tmp = staging.join(format!("{}.{}", worker.name, manifest.version));
        download_and_verify(net, &worker.url, &tmp, &worker.sha256, Decompress::Gzip).await?;
        pending.push((
            tmp,
            paths.workers_dir().join(&worker.name),
            format!("worker {}", worker.name),
        ));
    }

    // Only the entries matching this triple apply; the rest are other
    // platforms' and are ignored rather than being an error.
    for helper in manifest.helpers_for(triple) {
        let tmp = staging.join(format!("{}.{}{exe}", helper.name, manifest.version));
        download_and_verify(net, &helper.url, &tmp, &helper.sha256, Decompress::Gzip).await?;
        pending.push((
            tmp,
            paths.bin_path(&format!("{}{exe}", helper.name)),
            format!("helper {}", helper.name),
        ));
    }
    Ok(pending)
}

/// Hand the restart to the single owner, and report the exit code.
///
/// Each branch has a fallback, and the launchd one doing NOTHING is the design
/// rather than an omission: `KeepAlive` relaunches the already-swapped binary
/// the instant this process exits, and a second relauncher is exactly the race
/// this replaced.
async fn restart(
    deps: &UpdateDeps<'_>,
    supervisor: &Supervisor,
    updater_bin: &Path,
    current_bin: &Path,
    updater_ready: bool,
) -> i32 {
    match supervisor {
        Supervisor::Launchd { .. } => 0,
        Supervisor::Systemd { unit } => {
            // Awaited deliberately: `--no-block` returns as soon as the job is
            // queued with the user manager, and awaiting is what guarantees it
            // is queued before this process exits and its cgroup is torn down.
            let rc = deps
                .runner
                .run(&["systemctl", "--user", "restart", "--no-block", unit])
                .await;
            if rc.success() {
                0
            } else {
                tracing::warn!(
                    target: "loca::update",
                    "systemctl restart of {unit} failed; exiting non-zero so \
                     Restart=on-failure relaunches core",
                );
                RESTART_EXIT_CODE
            }
        }
        Supervisor::Schtask { task } => {
            if updater_ready {
                spawn_updater(deps, updater_bin, current_bin, Some(task)).await;
            }
            // Non-zero regardless, so Task Scheduler's own restart-on-failure
            // relaunches the already-swapped binary even if the updater was
            // torn down with core's job. The task's MultipleInstances=IgnoreNew
            // dedupes the two.
            RESTART_EXIT_CODE
        }
        Supervisor::Background => {
            spawn_updater(deps, updater_bin, current_bin, None).await;
            0
        }
    }
}

/// Start the out-of-core relauncher.
///
/// The binary is already swapped by the time this runs, so `--staged` is not
/// passed: the updater's swap is a fallback for a Windows in-core swap that
/// failed, and this port treats such a failure as fatal rather than deferring
/// it, because deferring means two processes disagree about which binary is
/// live.
async fn spawn_updater(
    deps: &UpdateDeps<'_>,
    updater_bin: &Path,
    current_bin: &Path,
    start_task: Option<&str>,
) {
    let updater = updater_bin.to_string_lossy().to_string();
    let current = current_bin.to_string_lossy().to_string();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let restart_args = serde_json::to_string(&args).unwrap_or_else(|_| "[]".to_string());
    let mut argv: Vec<&str> = vec![
        &updater,
        "--current",
        &current,
        "--restart-args",
        &restart_args,
    ];
    if let Some(task) = start_task {
        argv.extend_from_slice(&["--supervisor", "schtask", "--service-label", task]);
    }
    deps.runner.run(&argv).await;
}

/// Re-exported so the route slice can reach the anchor helper without importing
/// two modules for one path.
pub use swap::anchor_path;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::config::{Channel, CORE_VERSION};
    use crate::update::install::proc::testing::RecordingRunner;
    use crate::update::install::proc::RunResult;
    use crate::update::manifest::test_trust::Trusted;
    use crate::update::testing::{
        bounded, gzipped, published_manifest, sha256_hex, Serve, Upstream,
    };
    use serde_json::json;

    fn config(base: &str) -> Config {
        Config {
            channel: Channel::Stable,
            host: "127.0.0.1".into(),
            port: 7800,
            version: CORE_VERSION,
            storage_base_url: base.to_string(),
            hf_base_url: base.to_string(),
        }
    }

    /// A manifest whose version is `version` and whose only binary is for this
    /// host, pointing at `/core.gz`.
    fn manifest_for(version: &str) -> serde_json::Value {
        let mut doc = published_manifest();
        let object = doc.as_object_mut().unwrap();
        object.insert("version".into(), json!(version));
        object.insert(
            "binaries".into(),
            json!([{ "triple": host_triple(), "url": "/core.gz", "sha256": "0".repeat(64) }]),
        );
        object.insert("workers".into(), json!([]));
        object.insert("helpers".into(), json!([]));
        doc
    }

    #[tokio::test]
    async fn check_reports_a_newer_version_as_available() {
        let trusted = Trusted::new();
        let raw = trusted.sign_to_string(manifest_for("99.0.0"));
        let up = Upstream::start_routes(vec![(
            "/manifests/core.json",
            Serve::Body(raw.into_bytes()),
        )])
        .await;
        let net = Net::new().unwrap();

        let res = bounded(check_for_update(&config(up.base()), &net))
            .await
            .unwrap();
        assert!(res.available);
        assert_eq!(res.latest_version, "99.0.0");
        assert_eq!(res.current_version, CORE_VERSION);
    }

    #[tokio::test]
    async fn an_equal_version_is_not_reported_as_available() {
        // The UI must never offer an update that apply would refuse. Equal is
        // the boundary, and widening the comparison to `>=` is a
        // one-character change no other test can see.
        let trusted = Trusted::new();
        let raw = trusted.sign_to_string(manifest_for(CORE_VERSION));
        let up = Upstream::start_routes(vec![(
            "/manifests/core.json",
            Serve::Body(raw.into_bytes()),
        )])
        .await;
        let net = Net::new().unwrap();

        let res = bounded(check_for_update(&config(up.base()), &net))
            .await
            .unwrap();
        assert!(!res.available);
    }

    /// A core root with a bin dir and a staged upstream serving a signed
    /// manifest plus the gzipped binary it names.
    struct Env {
        _dir: tempfile::TempDir,
        _guard: crate::update::testing::RollbackGuard,
        paths: Paths,
        home: tempfile::TempDir,
        net: Net,
        up: Upstream,
        trusted: Trusted,
    }

    async fn env_with(version: &str, payload: &[u8]) -> Env {
        let guard = crate::update::testing::rollback_guard().await;
        let trusted = Trusted::new();
        // The port is reserved BEFORE the manifest is built, because a signed
        // manifest names ABSOLUTE artifact URLs and signing a relative one
        // produces a fixture that fails at the HTTP client and reads as a
        // broken apply path.
        let reserved = Upstream::reserve().await;
        let mut doc = manifest_for(version);
        doc.as_object_mut().unwrap()["binaries"] = json!([{
            "triple": host_triple(),
            "url": format!("{}/core.gz", reserved.base()),
            "sha256": sha256_hex(payload),
        }]);
        let raw = trusted.sign_to_string(doc);
        let up = reserved.serve(vec![
            ("/manifests/core.json", Serve::Body(raw.into_bytes())),
            ("/core.gz", Serve::Body(gzipped(payload))),
        ]);
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted(dir.path().join("core"));
        paths.ensure_dirs().unwrap();
        std::fs::create_dir_all(paths.bin_dir()).unwrap();
        Env {
            _dir: dir,
            _guard: guard,
            paths,
            home: tempfile::tempdir().unwrap(),
            net: Net::new().unwrap(),
            up,
            trusted,
        }
    }

    impl Env {
        fn config(&self) -> Config {
            config(self.up.base())
        }

        fn current(&self) -> std::path::PathBuf {
            self.paths.bin_path("loca-core")
        }

        /// Put the updater helper where a real install has it.
        ///
        /// NOT part of `env_with`, because "the updater is missing" is a state
        /// the apply path refuses on and one test asserts. A fixture that
        /// installed it unconditionally would make that refusal unreachable;
        /// one that never installs it makes every supervisor branch except
        /// launchd and systemd unreachable instead. So it is a precondition
        /// each test states.
        fn install_updater(&self) {
            let path = self
                .paths
                .bin_path(&Channel::Stable.core_binary_name("loca-core-updater"));
            self.place(&path, "the updater");
        }

        fn place(&self, path: &std::path::Path, contents: &str) {
            std::fs::write(path, contents).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }

        fn deps<'a>(&'a self, runner: &'a dyn Runner) -> UpdateDeps<'a> {
            UpdateDeps {
                net: &self.net,
                runner,
                os: Os::Macos,
                home: self.home.path(),
                platform: Platform::Unix,
            }
        }
    }

    #[tokio::test]
    async fn an_older_manifest_is_refused_even_though_it_verifies() {
        // A validly-signed OLDER manifest is still validly signed, so this
        // ordering is the only thing between a replayed release and a rollback
        // to a fixed vulnerability (D-053). Driven through the REAL apply
        // against the real trust root, not through a copy that takes a key
        // parameter, which is how D7's equivalent branch came to have no test
        // (D-065).
        let e = env_with("0.0.1", b"older core").await;
        let runner = RecordingRunner::new();

        let err = bounded(apply_update(&e.config(), &e.paths, &e.deps(&runner), None))
            .await
            .unwrap_err();

        assert_eq!(err.code, ErrorCode::UpdateFailed);
        assert!(
            err.message.contains("refusing downgrade"),
            "{}",
            err.message
        );
        assert!(
            !e.paths.update_marker_file().exists(),
            "and nothing was staged"
        );
        let _ = &e.trusted;
    }

    #[tokio::test]
    async fn re_applying_the_running_version_is_a_repair_not_a_downgrade() {
        // The other side of the boundary, and the reason the two comparisons
        // differ: `check` reports equal as unavailable while `apply` accepts it,
        // so a user whose install is damaged can reinstall the version they are
        // on. Widening the refusal from `<` to `<=` is caught only here.
        let e = env_with(CORE_VERSION, b"same version, fresh copy").await;
        e.place(&e.current(), "damaged");
        e.install_updater();
        let runner = RecordingRunner::new();

        let applied = bounded(apply_update(&e.config(), &e.paths, &e.deps(&runner), None))
            .await
            .unwrap();

        assert_eq!(applied.version, CORE_VERSION);
        assert_eq!(
            std::fs::read_to_string(e.current()).unwrap(),
            "same version, fresh copy"
        );
    }

    #[tokio::test]
    async fn the_marker_exists_before_the_binary_is_swapped() {
        // D-081. The ordering is only observable in its end state here, so what
        // is asserted is that a completed apply left BOTH, and the crash-window
        // suite in `swap.rs` covers what each intermediate state means.
        let e = env_with("99.0.0", b"the new core").await;
        e.place(&e.current(), "the old core");
        e.install_updater();
        let runner = RecordingRunner::new();

        let applied = bounded(apply_update(&e.config(), &e.paths, &e.deps(&runner), None))
            .await
            .unwrap();

        assert_eq!(applied.version, "99.0.0");
        assert_eq!(
            std::fs::read_to_string(e.current()).unwrap(),
            "the new core"
        );
        assert_eq!(
            std::fs::read_to_string(anchor_path(&e.current())).unwrap(),
            "the old core",
            "the anchor is the previous binary, which is what makes it undoable"
        );
        let raw = std::fs::read_to_string(e.paths.update_marker_file()).unwrap();
        assert!(raw.contains("99.0.0"), "{raw}");
        assert!(raw.contains(CORE_VERSION), "{raw}");
    }

    #[tokio::test]
    async fn a_requested_version_that_the_manifest_does_not_carry_is_refused() {
        let e = env_with("99.0.0", b"the new core").await;
        e.place(&e.current(), "the old core");
        e.install_updater();
        let runner = RecordingRunner::new();

        let err = bounded(apply_update(
            &e.config(),
            &e.paths,
            &e.deps(&runner),
            Some("98.0.0"),
        ))
        .await
        .unwrap_err();
        assert!(
            err.message.contains("does not match requested"),
            "{}",
            err.message
        );

        // D-043: an EMPTY requested version is absent, not a mismatch. A form
        // that clears its field to "" rather than omitting the key otherwise
        // pins the update to a version named nowhere.
        let applied = bounded(apply_update(
            &e.config(),
            &e.paths,
            &e.deps(&runner),
            Some(""),
        ))
        .await
        .unwrap();
        assert_eq!(applied.version, "99.0.0");
    }

    #[tokio::test]
    async fn background_mode_refuses_without_the_updater_and_touches_nothing() {
        // The updater is the ONLY thing that can relaunch a background-mode
        // core, so a missing one means this update would leave it down. The
        // check sits before any rename, which is what makes the refusal free.
        let e = env_with("99.0.0", b"the new core").await;
        e.place(&e.current(), "the old core");
        let runner = RecordingRunner::new();
        // Os::Other with nothing installed resolves to Background.
        let deps = UpdateDeps {
            net: &e.net,
            runner: &runner,
            os: Os::Other,
            home: e.home.path(),
            platform: Platform::Unix,
        };

        let err = bounded(apply_update(&e.config(), &e.paths, &deps, None))
            .await
            .unwrap_err();

        assert!(err.message.contains("loca-core-updater"), "{}", err.message);
        assert_eq!(
            std::fs::read_to_string(e.current()).unwrap(),
            "the old core",
            "nothing was swapped"
        );
        assert!(
            !e.paths.update_marker_file().exists(),
            "and no marker was written, so the next boot has nothing to undo"
        );
    }

    #[tokio::test]
    async fn a_launchd_install_exits_zero_and_spawns_nothing() {
        // The branch that does NOTHING, which reads like an omission and is the
        // design: KeepAlive relaunches the swapped binary the instant this
        // process exits, and a second relauncher is the race this replaced.
        let e = env_with("99.0.0", b"the new core").await;
        e.place(&e.current(), "the old core");
        let plist = crate::update::install::supervisor::plist_path(e.home.path(), Channel::Stable);
        std::fs::create_dir_all(plist.parent().unwrap()).unwrap();
        std::fs::write(&plist, "<plist/>").unwrap();
        let runner = RecordingRunner::new();

        let applied = bounded(apply_update(&e.config(), &e.paths, &e.deps(&runner), None))
            .await
            .unwrap();

        assert_eq!(applied.exit_code, 0);
        assert!(
            runner.calls().is_empty(),
            "launchd owns the restart; spawning anything here fights it for the port: {:?}",
            runner.calls()
        );
    }

    /// Stage a systemd install and apply through it, with `restart` scripted to
    /// `result`.
    ///
    /// The two halves are a function rather than two `Env`s in one body on
    /// purpose. Every fixture here holds two process-global locks (the trust
    /// root and the rollback lock), neither of which is reentrant, so a second
    /// `env_with` before the first is dropped parks forever holding both, and
    /// every other test that wants either parks behind it. That is what wedged
    /// this crate's test binary. `claim_fixture_lock` now panics instead, and
    /// this shape is what keeps it from being reached.
    async fn systemd_exit_code(result: RunResult) -> (i32, Vec<String>) {
        let e = env_with("99.0.0", b"the new core").await;
        e.place(&e.current(), "the old core");
        e.install_updater();
        let unit =
            crate::update::install::supervisor::systemd_unit_path(e.home.path(), Channel::Stable);
        std::fs::create_dir_all(unit.parent().unwrap()).unwrap();
        std::fs::write(&unit, "[Unit]").unwrap();

        let runner = RecordingRunner::new()
            .on("systemctl --user --version", RunResult::ok(""))
            .on("systemctl --user restart", result);
        let deps = UpdateDeps {
            net: &e.net,
            runner: &runner,
            os: Os::Linux,
            home: e.home.path(),
            platform: Platform::Unix,
        };
        let applied = bounded(apply_update(&e.config(), &e.paths, &deps, None))
            .await
            .unwrap();
        (applied.exit_code, runner.calls())
    }

    #[tokio::test]
    async fn a_systemd_restart_that_is_queued_exits_cleanly() {
        // `--no-block` returns as soon as the job is queued with the user
        // manager, and the call is awaited so it IS queued before this process
        // exits and its cgroup is torn down.
        let (code, calls) = systemd_exit_code(RunResult::ok("")).await;
        assert_eq!(code, 0);
        assert!(calls
            .iter()
            .any(|c| c.contains("--no-block") && c.contains("loca-core.service")));
    }

    #[tokio::test]
    async fn a_systemd_restart_that_fails_falls_back_to_a_failure_exit() {
        // The other outcome, in its own test rather than a second half: a clean
        // exit does not trip `Restart=on-failure`, so if the enqueue fails the
        // only way back up is to exit non-zero. Asserting one outcome alone
        // would pass with the branch deleted, which is why both exist.
        let (code, _) = systemd_exit_code(RunResult::failed(1)).await;
        assert_eq!(code, RESTART_EXIT_CODE);
    }

    #[tokio::test]
    async fn an_older_version_is_not_reported_as_available() {
        let trusted = Trusted::new();
        let raw = trusted.sign_to_string(manifest_for("0.0.1"));
        let up = Upstream::start_routes(vec![(
            "/manifests/core.json",
            Serve::Body(raw.into_bytes()),
        )])
        .await;
        let net = Net::new().unwrap();

        assert!(
            !bounded(check_for_update(&config(up.base()), &net))
                .await
                .unwrap()
                .available
        );
    }
}
