//! `GET /update/check` and `POST /update/apply`: the whole self-update flow.
//!
//! Everything is downloaded and verified into `staging/` before any live file
//! is renamed, the supervisor and updater helper are checked before any rename,
//! and the rollback marker is written BEFORE the swap (see `swap.rs`).
//! `check` reports `available` only for a STRICTLY newer version while `apply`
//! refuses only an OLDER one, so re-applying the running version is a repair.
//! Both use `compare_semver`: a validly-signed older manifest still verifies.
//! [`apply_update`] RETURNS the exit code; `routes.rs` performs the exit, so a
//! test reaching this path does not kill the test binary.

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

/// Exit code of a restart-requesting run. Non-zero because systemd's
/// `Restart=on-failure` and Task Scheduler only relaunch on failure; shared
/// with the rollback path so a supervisor need not tell them apart.
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
        available: compare_semver(&manifest.version, config.version).is_gt(),
        manifest_url: config.core_manifest_url(),
    })
}

/// Stage an update and hand the restart to whoever owns this core's lifetime.
/// Returns the exit code the caller must use.
pub async fn apply_update(
    config: &Config,
    paths: &Paths,
    deps: &UpdateDeps<'_>,
    target_version: Option<&str>,
) -> Result<Applied> {
    let manifest = fetch_core_manifest(deps.net, &config.core_manifest_url()).await?;

    // An empty target is ABSENT: a form clearing a field to "" must not pin
    // the update to a version named nowhere.
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

    // Cleared wholesale so staging only ever holds the set about to be verified.
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
    let pending = stage_artifacts(deps.net, paths, &manifest, triple, exe).await?;

    let current_bin = paths.bin_path(&config.channel.core_binary_name("loca-core"));
    let updater_bin = paths.bin_path(&config.channel.core_binary_name("loca-core-updater"));
    let supervisor = resolve_supervisor(deps.runner, deps.os, deps.home, config.channel).await;
    let updater_ready = tokio::fs::metadata(&updater_bin).await.is_ok();

    // In background mode the updater is the only thing that can relaunch core,
    // so a missing one is fatal, checked before any rename. On Windows Task
    // Scheduler's restart-on-failure is the backstop.
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

    // Written BEFORE the swap so a broken release can never be installed with
    // nothing pointing at its anchor; a crash before the swap leaves a marker
    // the boot path already clears as "already rolled back".
    marker::write(paths, &manifest.version, config.version).await?;

    if let Err(err) = swap_binary_in_place(&staged_core, &current_bin, deps.platform).await {
        // Nothing was swapped, so the marker describes an update that did not happen.
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

    // Other platforms' helpers are ignored rather than being an error.
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

/// Hand the restart to the single owner, and report the exit code. The launchd
/// branch doing NOTHING is the design: `KeepAlive` relaunches the swapped
/// binary on exit, and a second relauncher would race it.
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
            // Awaited so the `--no-block` job is queued before this process
            // exits and its cgroup is torn down.
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
            // Non-zero regardless, so Task Scheduler relaunches even if the
            // updater was torn down with core's job (MultipleInstances=IgnoreNew dedupes).
            RESTART_EXIT_CODE
        }
        Supervisor::Background => {
            spawn_updater(deps, updater_bin, current_bin, None).await;
            0
        }
    }
}

/// Start the out-of-core relauncher. The binary is already swapped, so
/// `--staged` is not passed: a failed in-core swap is fatal, never deferred.
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

    /// A manifest at `version` whose only binary is for this host at `/core.gz`.
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

    /// Serve a signed manifest at `version` and run `check_for_update` against it.
    async fn check(version: &str) -> UpdateCheckResponse {
        let trusted = Trusted::new();
        let raw = trusted.sign_to_string(manifest_for(version));
        let up = Upstream::start_routes(vec![(
            "/manifests/core.json",
            Serve::Body(raw.into_bytes()),
        )])
        .await;
        let net = Net::new().unwrap();
        bounded(check_for_update(&config(up.base()), &net))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn check_reports_a_newer_version_as_available() {
        let res = check("99.0.0").await;
        assert!(res.available);
        assert_eq!(res.latest_version, "99.0.0");
        assert_eq!(res.current_version, CORE_VERSION);
    }

    #[tokio::test]
    async fn an_equal_version_is_not_reported_as_available() {
        assert!(!check(CORE_VERSION).await.available);
    }

    #[tokio::test]
    async fn an_older_version_is_not_reported_as_available() {
        assert!(!check("0.0.1").await.available);
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
        // Reserved BEFORE the manifest is built: a signed manifest names
        // ABSOLUTE artifact URLs.
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

        /// Not part of `env_with`: "the updater is missing" is a state one
        /// test asserts on, so each test states it as a precondition.
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
            self.deps_on(runner, Os::Macos)
        }

        fn deps_on<'a>(&'a self, runner: &'a dyn Runner, os: Os) -> UpdateDeps<'a> {
            UpdateDeps {
                net: &self.net,
                runner,
                os,
                home: self.home.path(),
                platform: Platform::Unix,
            }
        }

        async fn apply(&self, runner: &dyn Runner, target: Option<&str>) -> Result<Applied> {
            bounded(apply_update(
                &self.config(),
                &self.paths,
                &self.deps(runner),
                target,
            ))
            .await
        }
    }

    #[tokio::test]
    async fn an_older_manifest_is_refused_even_though_it_verifies() {
        let e = env_with("0.0.1", b"older core").await;
        let runner = RecordingRunner::new();

        let err = e.apply(&runner, None).await.unwrap_err();

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
        let e = env_with(CORE_VERSION, b"same version, fresh copy").await;
        e.place(&e.current(), "damaged");
        e.install_updater();
        let runner = RecordingRunner::new();

        let applied = e.apply(&runner, None).await.unwrap();

        assert_eq!(applied.version, CORE_VERSION);
        assert_eq!(
            std::fs::read_to_string(e.current()).unwrap(),
            "same version, fresh copy"
        );
    }

    #[tokio::test]
    async fn the_marker_exists_before_the_binary_is_swapped() {
        let e = env_with("99.0.0", b"the new core").await;
        e.place(&e.current(), "the old core");
        e.install_updater();
        let runner = RecordingRunner::new();

        let applied = e.apply(&runner, None).await.unwrap();

        assert_eq!(applied.version, "99.0.0");
        assert_eq!(
            std::fs::read_to_string(e.current()).unwrap(),
            "the new core"
        );
        assert_eq!(
            std::fs::read_to_string(anchor_path(&e.current())).unwrap(),
            "the old core",
            "the anchor is the previous binary"
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

        let err = e.apply(&runner, Some("98.0.0")).await.unwrap_err();
        assert!(
            err.message.contains("does not match requested"),
            "{}",
            err.message
        );

        // An EMPTY requested version is absent, not a mismatch.
        let applied = e.apply(&runner, Some("")).await.unwrap();
        assert_eq!(applied.version, "99.0.0");
    }

    #[tokio::test]
    async fn background_mode_refuses_without_the_updater_and_touches_nothing() {
        let e = env_with("99.0.0", b"the new core").await;
        e.place(&e.current(), "the old core");
        let runner = RecordingRunner::new();
        // Os::Other with nothing installed resolves to Background.
        let deps = e.deps_on(&runner, Os::Other);

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
            "no marker was written"
        );
    }

    #[tokio::test]
    async fn a_launchd_install_exits_zero_and_spawns_nothing() {
        let e = env_with("99.0.0", b"the new core").await;
        e.place(&e.current(), "the old core");
        let plist = crate::update::install::supervisor::plist_path(e.home.path(), Channel::Stable);
        std::fs::create_dir_all(plist.parent().unwrap()).unwrap();
        std::fs::write(&plist, "<plist/>").unwrap();
        let runner = RecordingRunner::new();

        let applied = e.apply(&runner, None).await.unwrap();

        assert_eq!(applied.exit_code, 0);
        assert!(
            runner.calls().is_empty(),
            "launchd owns the restart: {:?}",
            runner.calls()
        );
    }

    /// One `Env` per call: each holds two non-reentrant process-global locks,
    /// so two live at once would wedge the test binary.
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
        let deps = e.deps_on(&runner, Os::Linux);
        let applied = bounded(apply_update(&e.config(), &e.paths, &deps, None))
            .await
            .unwrap();
        (applied.exit_code, runner.calls())
    }

    #[tokio::test]
    async fn a_systemd_restart_that_is_queued_exits_cleanly() {
        let (code, calls) = systemd_exit_code(RunResult::ok("")).await;
        assert_eq!(code, 0);
        assert!(calls
            .iter()
            .any(|c| c.contains("--no-block") && c.contains("loca-core.service")));
    }

    #[tokio::test]
    async fn a_systemd_restart_that_fails_falls_back_to_a_failure_exit() {
        let (code, _) = systemd_exit_code(RunResult::failed(1)).await;
        assert_eq!(code, RESTART_EXIT_CODE);
    }
}
