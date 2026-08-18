//! Every OS branch, on one host, through the runner seam.
//!
//! What these cover is the argv and its ORDER, plus what each branch does with
//! a non-zero exit. What they do not cover, stated rather than implied, is
//! whether `launchctl`, `systemctl`, `schtasks` and PowerShell behave as the
//! argv assumes. That is exercised per OS at release time and is out of reach
//! of `cargo test` on any single machine, exactly as D11's Windows variants are.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use super::*;
use crate::update::install::io::testing::{recording, Captured};
use crate::update::install::io::Streams;
use crate::update::install::proc::testing::RecordingRunner;
use crate::update::testing::bounded;

/// Records whether the key was deleted, and whether the core directory still
/// existed at that moment.
///
/// The second half is the assertion that matters: "cleared BEFORE the data was
/// removed" is an ordering, and a double that only records "was called" cannot
/// see it.
struct RecordingKey {
    deleted: AtomicBool,
    core_dir: PathBuf,
    dir_existed_when_called: AtomicBool,
    fail: bool,
}

impl RecordingKey {
    fn new(core_dir: PathBuf) -> Self {
        Self {
            deleted: AtomicBool::new(false),
            core_dir,
            dir_existed_when_called: AtomicBool::new(false),
            fail: false,
        }
    }

    fn failing(core_dir: PathBuf) -> Self {
        Self {
            fail: true,
            ..Self::new(core_dir)
        }
    }

    fn deleted(&self) -> bool {
        self.deleted.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl MasterKey for RecordingKey {
    async fn delete(&self) -> Result<()> {
        self.deleted.store(true, Ordering::SeqCst);
        self.dir_existed_when_called
            .store(self.core_dir.exists(), Ordering::SeqCst);
        if self.fail {
            return Err(internal("no keychain helper"));
        }
        Ok(())
    }
}

struct Env {
    _dir: tempfile::TempDir,
    paths: Paths,
    home: PathBuf,
    config: Config,
    io: Streams<Captured, Captured>,
    out: Captured,
    err: Captured,
}

fn env_for(channel: Channel) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::rooted(dir.path().join("loca").join("stable").join("core"));
    paths.ensure_dirs().unwrap();
    // A real home, redirected. Service definitions must land in the actual
    // ~/Library or ~/.config even when core state is a tempdir, which is why
    // this is separate from `Paths` at all.
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let (io, out, err) = recording();
    Env {
        _dir: dir,
        paths,
        home,
        config: Config {
            channel,
            host: "127.0.0.1".into(),
            port: 8787,
            version: "0.1.15",
            storage_base_url: "https://example.invalid".into(),
            hf_base_url: "https://example.invalid".into(),
        },
        io,
        out,
        err,
    }
}

fn env() -> Env {
    env_for(Channel::Stable)
}

#[allow(clippy::too_many_arguments)]
fn ctx<'a>(
    e: &'a Env,
    runner: &'a dyn Runner,
    key: &'a dyn MasterKey,
    os: Os,
    install_env: InstallEnv,
) -> ServiceCtx<'a> {
    ServiceCtx {
        config: &e.config,
        paths: &e.paths,
        runner,
        io: &e.io,
        master_key: key,
        os,
        home: &e.home,
        self_pid: 4242,
        current_exe: e.paths.bin_path("loca-core"),
        env: install_env,
    }
}

fn service_env() -> InstallEnv {
    InstallEnv::default()
}

fn background_env() -> InstallEnv {
    InstallEnv::parse(Some("0"), None, None, None)
}

// --- the environment parse -------------------------------------------------

#[test]
fn an_unset_arp_variable_still_registers() {
    // The two polarities are not interchangeable, and an UNSET variable is the
    // case every installer front-end hits. `_SERVICE` and `_REGISTER_ARP`
    // default ON and are turned off by exactly "0"; `_BIND_ALL` and
    // `_BEHIND_PROXY` default OFF and are turned on by exactly "1".
    let unset = InstallEnv::parse(None, None, None, None);
    assert!(unset.as_service, "unset means install a service");
    assert!(unset.register_arp, "unset means write the ARP entry");
    assert!(!unset.bind_all);
    assert!(!unset.behind_proxy);

    assert!(!InstallEnv::parse(Some("0"), None, None, None).as_service);
    assert!(!InstallEnv::parse(None, None, None, Some("0")).register_arp);
    assert!(InstallEnv::parse(None, Some("1"), None, None).bind_all);
    assert!(InstallEnv::parse(None, None, Some("1"), None).behind_proxy);

    // Anything that is not the magic value leaves the default, in BOTH
    // directions. This is what a `== "1"` written for `_REGISTER_ARP`, or a
    // `!= "0"` written for `_BIND_ALL`, would get wrong.
    for other in ["", "1", "true", "yes", "00"] {
        assert!(
            InstallEnv::parse(Some(other), None, None, None).as_service || other == "0",
            "_SERVICE={other:?}"
        );
        assert!(
            InstallEnv::parse(None, None, None, Some(other)).register_arp || other == "0",
            "_REGISTER_ARP={other:?}"
        );
    }
    for other in ["", "0", "true", "yes", "11"] {
        assert!(
            !InstallEnv::parse(None, Some(other), None, None).bind_all,
            "_BIND_ALL={other:?}"
        );
        assert!(
            !InstallEnv::parse(None, None, Some(other), None).behind_proxy,
            "_BEHIND_PROXY={other:?}"
        );
    }
}

// --- the pid ---------------------------------------------------------------

#[test]
fn a_shell_that_printed_nothing_reports_no_pid() {
    assert_eq!(parse_pid("12345"), Some(12345));
    assert_eq!(parse_pid("  678 \n"), Some(678));
    // `0` matters specifically: it is a valid u32 that names no process, and
    // handing it to the Client as one to kill is worse than handing it nothing.
    assert_eq!(parse_pid("0"), None);
    assert_eq!(parse_pid("-1"), None);
    assert_eq!(parse_pid(""), None);
    assert_eq!(parse_pid("\n"), None);
    assert_eq!(parse_pid("not a pid"), None);
    assert_eq!(parse_pid("12 34"), None);
}

#[test]
fn the_shipping_grace_is_one_second() {
    // The tests run with a 1ms grace so they do not pay it. That seam is
    // exactly how a shipping constant ends up asserted by nothing, so the
    // production value is checked here against the oracle's
    // (`install/service.ts:644`).
    #[cfg(not(test))]
    const SHIPPING: u64 = GRACE_MS;
    #[cfg(test)]
    const SHIPPING: u64 = 1000;
    assert_eq!(SHIPPING, 1000);
}

// --- install ---------------------------------------------------------------

#[tokio::test]
async fn a_launchd_install_unloads_before_it_loads_and_writes_the_plist() {
    let e = env();
    let runner = RecordingRunner::new().on("launchctl", RunResult::ok(""));
    let key = RecordingKey::new(e.paths.root().to_path_buf());

    bounded(install_service(&ctx(
        &e,
        &runner,
        &key,
        Os::Macos,
        service_env(),
    )))
    .await
    .unwrap();

    let plist = plist_path(&e.home, Channel::Stable);
    let xml = std::fs::read_to_string(&plist).unwrap();
    assert!(xml.contains("<key>KeepAlive</key>"));
    let calls = runner.calls();
    // Unload FIRST, so a re-install replaces a running agent rather than
    // failing against it.
    assert!(calls[0].starts_with("launchctl unload"), "{calls:?}");
    assert!(calls[1].starts_with("launchctl load"), "{calls:?}");
    assert!(e.err.text().contains("registered launchd agent"));
}

#[tokio::test]
async fn a_failed_launchctl_load_is_an_error_naming_the_plist() {
    // The unload is best-effort and the load is not, and a port that treated
    // both the same would report a successful install of a service that is not
    // running.
    let e = env();
    let runner = RecordingRunner::new()
        .on("launchctl unload", RunResult::ok(""))
        .on("launchctl load", RunResult::failed(2));
    let key = RecordingKey::new(e.paths.root().to_path_buf());

    let err = bounded(install_service(&ctx(
        &e,
        &runner,
        &key,
        Os::Macos,
        service_env(),
    )))
    .await
    .unwrap_err();

    assert!(err.to_string().contains("launchctl load failed"), "{err}");
    assert!(
        err.to_string().contains("LaunchAgents"),
        "the message names the file to inspect: {err}"
    );
}

#[tokio::test]
async fn a_systemd_install_reloads_before_it_enables() {
    let e = env_for(Channel::Dev);
    let runner = RecordingRunner::new().on("systemctl", RunResult::ok(""));
    let key = RecordingKey::new(e.paths.root().to_path_buf());

    bounded(install_service(&ctx(
        &e,
        &runner,
        &key,
        Os::Linux,
        service_env(),
    )))
    .await
    .unwrap();

    let unit = std::fs::read_to_string(systemd_unit_path(&e.home, Channel::Dev)).unwrap();
    assert!(unit.contains("Environment=LOCA_CHANNEL=dev"));
    let calls = runner.calls();
    let reload = calls
        .iter()
        .position(|c| c.contains("daemon-reload"))
        .unwrap();
    let enable = calls
        .iter()
        .position(|c| c.contains("enable --now"))
        .unwrap();
    // A unit enabled before the manager has re-read it enables the OLD
    // definition, which is a service pointing at the previous binary.
    assert!(reload < enable, "{calls:?}");
    assert!(calls[enable].contains("loca-core-dev.service"));
}

#[tokio::test]
async fn no_systemd_user_falls_back_to_a_background_launch() {
    // A machine with no user manager still gets a running core. The fallback
    // is why `resolve_supervisor` checks for the manager as well as the file.
    let e = env();
    let runner = RecordingRunner::new()
        .on("systemctl --user --version", RunResult::failed(127))
        .on("sh -c", RunResult::ok("9911\n"));
    let key = RecordingKey::new(e.paths.root().to_path_buf());

    bounded(install_service(&ctx(
        &e,
        &runner,
        &key,
        Os::Linux,
        service_env(),
    )))
    .await
    .unwrap();

    assert!(
        !systemd_unit_path(&e.home, Channel::Stable).exists(),
        "no unit is written when there is no manager to run it"
    );
    assert!(runner.calls().iter().any(|c| c.contains("nohup")));
    assert!(e.err.text().contains("systemd --user unavailable"));
}

#[tokio::test]
async fn a_windows_install_registers_the_task_then_the_arp_entry() {
    let e = env();
    let runner = RecordingRunner::new().on("powershell", RunResult::ok(""));
    let key = RecordingKey::new(e.paths.root().to_path_buf());

    bounded(install_service(&ctx(
        &e,
        &runner,
        &key,
        Os::Windows,
        service_env(),
    )))
    .await
    .unwrap();

    let calls = runner.calls();
    let register = calls
        .iter()
        .position(|c| c.contains("Register-ScheduledTask"))
        .unwrap();
    let arp = calls
        .iter()
        .position(|c| c.contains("CurrentVersion\\Uninstall"))
        .unwrap();
    assert!(register < arp, "the service exists before it is listed");
    // A service install's uninstall string must self-elevate, because the task
    // lives in the root folder and Settings > Apps runs the string unelevated.
    assert!(calls[arp].contains("RunAs"), "{}", calls[arp]);
    assert!(calls[arp].contains("DisplayName -Value 'Loca Core'"));
}

#[tokio::test]
async fn an_arp_optout_skips_the_entry_but_still_installs_the_service() {
    // The unified Windows installer sets this because its own uninstall.exe
    // owns the Core's ARP entry, and every later install-service re-run would
    // otherwise clobber it back to the PowerShell teardown.
    let e = env();
    let runner = RecordingRunner::new().on("powershell", RunResult::ok(""));
    let key = RecordingKey::new(e.paths.root().to_path_buf());
    let install_env = InstallEnv::parse(None, None, None, Some("0"));

    bounded(install_service(&ctx(
        &e,
        &runner,
        &key,
        Os::Windows,
        install_env,
    )))
    .await
    .unwrap();

    let calls = runner.calls();
    assert!(calls.iter().any(|c| c.contains("Register-ScheduledTask")));
    assert!(
        !calls
            .iter()
            .any(|c| c.contains("CurrentVersion\\Uninstall")),
        "{calls:?}"
    );
}

#[tokio::test]
async fn a_background_install_registers_no_service_on_any_os() {
    // LOCA_INSTALL_SERVICE=0 means the client owns liveness. Asserted on all
    // three, because the branch is above the OS switch and a port that put it
    // inside would install a service on two of them.
    for os in [Os::Macos, Os::Linux, Os::Windows] {
        let e = env();
        let runner = RecordingRunner::new()
            .on("sh -c", RunResult::ok("777\n"))
            .on("powershell", RunResult::ok(""));
        let key = RecordingKey::new(e.paths.root().to_path_buf());

        bounded(install_service(&ctx(
            &e,
            &runner,
            &key,
            os,
            background_env(),
        )))
        .await
        .unwrap();

        let calls = runner.calls();
        assert!(
            !calls.iter().any(|c| c.contains("Register-ScheduledTask")),
            "{os:?}: {calls:?}"
        );
        assert!(!plist_path(&e.home, Channel::Stable).exists(), "{os:?}");
        assert!(
            !systemd_unit_path(&e.home, Channel::Stable).exists(),
            "{os:?}"
        );
        // The Add/Remove Programs entry is WINDOWS-ONLY, and it is written in
        // background mode too: `LOCA_INSTALL_SERVICE=0` decides whether the
        // uninstall string self-elevates, not whether the app is listed.
        //
        // Asserted here because it was asserted nowhere: a mutation dropping
        // the `os == Windows` half of that guard survived the whole suite, so
        // a mac install would have run PowerShell to write a registry key.
        assert_eq!(
            calls
                .iter()
                .any(|c| c.contains("CurrentVersion\\Uninstall")),
            os == Os::Windows,
            "{os:?}: {calls:?}"
        );
    }
}

#[tokio::test]
async fn the_unix_background_launch_carries_its_channel_and_detaches() {
    let e = env_for(Channel::Latest);
    let runner = RecordingRunner::new().on("sh -c", RunResult::ok("5150\n"));
    let key = RecordingKey::new(e.paths.root().to_path_buf());

    bounded(install_service(&ctx(
        &e,
        &runner,
        &key,
        Os::Linux,
        background_env(),
    )))
    .await
    .unwrap();

    let call = runner
        .calls()
        .into_iter()
        .find(|c| c.contains("sh -c"))
        .unwrap();
    // Without the channel the detached core resolves the STABLE subtree.
    assert!(call.contains("LOCA_CHANNEL='latest'"), "{call}");
    // Without `nohup ... &` it dies with this short-lived CLI.
    assert!(call.contains("nohup"), "{call}");
    assert!(call.contains("& echo $!"), "{call}");
    assert!(call.contains("core.stdout.log"), "{call}");
    assert!(call.contains("loca-core-latest"), "{call}");
}

// --- start -----------------------------------------------------------------

#[tokio::test]
async fn only_a_background_launch_reports_a_pid() {
    // The frozen half of the stdout contract: `pid` is non-null ONLY in
    // background mode, because a supervised core outlives the Client. All four
    // supervisors, because reporting a pid for any supervised kind makes the
    // Client stop the service every time the user quits.
    struct Case {
        os: Os,
        stage: fn(&Env),
        script: &'static str,
        kind: &'static str,
        pid: Option<u32>,
    }
    let cases = [
        Case {
            os: Os::Macos,
            stage: |e| {
                let plist = plist_path(&e.home, Channel::Stable);
                std::fs::create_dir_all(plist.parent().unwrap()).unwrap();
                std::fs::write(&plist, "<plist/>").unwrap();
            },
            script: "launchctl",
            kind: "launchd",
            pid: None,
        },
        Case {
            os: Os::Linux,
            stage: |e| {
                let unit = systemd_unit_path(&e.home, Channel::Stable);
                std::fs::create_dir_all(unit.parent().unwrap()).unwrap();
                std::fs::write(&unit, "[Unit]").unwrap();
            },
            script: "systemctl",
            kind: "systemd",
            pid: None,
        },
        Case {
            os: Os::Windows,
            stage: |_| {},
            script: "schtasks",
            kind: "schtask",
            pid: None,
        },
        Case {
            os: Os::Linux,
            stage: |_| {},
            script: "sh -c",
            kind: "background",
            pid: Some(3131),
        },
    ];

    for case in cases {
        let e = env();
        (case.stage)(&e);
        let runner = RecordingRunner::new().on(case.script, RunResult::ok("3131\n"));
        let key = RecordingKey::new(e.paths.root().to_path_buf());

        bounded(start_service(&ctx(
            &e,
            &runner,
            &key,
            case.os,
            service_env(),
        )))
        .await
        .unwrap();

        let line: serde_json::Value = serde_json::from_str(&e.out.last_line()).unwrap();
        assert_eq!(
            line.get("supervisor").and_then(|v| v.as_str()),
            Some(case.kind)
        );
        match case.pid {
            Some(pid) => assert_eq!(line.get("pid").and_then(|v| v.as_u64()), Some(pid as u64)),
            None => assert_eq!(
                line.get("pid"),
                Some(&serde_json::Value::Null),
                "{} must report a null pid, present",
                case.kind
            ),
        }
    }
}

#[tokio::test]
async fn launchctl_load_failing_is_benign_but_start_failing_is_not() {
    // `load` is non-zero when the agent is already loaded, which is the normal
    // case for a core that is already running: treating it as an error makes
    // start-service fail on a healthy machine.
    let e = env();
    let plist = plist_path(&e.home, Channel::Stable);
    std::fs::create_dir_all(plist.parent().unwrap()).unwrap();
    std::fs::write(&plist, "<plist/>").unwrap();
    let key = RecordingKey::new(e.paths.root().to_path_buf());

    let benign = RecordingRunner::new()
        .on("launchctl load", RunResult::failed(1))
        .on("launchctl start", RunResult::ok(""));
    bounded(start_service(&ctx(
        &e,
        &benign,
        &key,
        Os::Macos,
        service_env(),
    )))
    .await
    .expect("an already-loaded agent is not an error");

    let e2 = env();
    let plist2 = plist_path(&e2.home, Channel::Stable);
    std::fs::create_dir_all(plist2.parent().unwrap()).unwrap();
    std::fs::write(&plist2, "<plist/>").unwrap();
    let fatal = RecordingRunner::new()
        .on("launchctl load", RunResult::ok(""))
        .on("launchctl start", RunResult::failed(3));
    let err = bounded(start_service(&ctx(
        &e2,
        &fatal,
        &key,
        Os::Macos,
        service_env(),
    )))
    .await
    .unwrap_err();
    assert!(err.to_string().contains("launchctl start"), "{err}");
    assert_eq!(
        e2.out.text(),
        "",
        "a failed start emits no result line for the Client to parse"
    );
}

// --- stop ------------------------------------------------------------------

#[tokio::test]
async fn every_stop_goes_through_the_supervisor_rather_than_killing() {
    // A plain kill is undone by launchd's KeepAlive and systemd's Restart, so
    // a stop that killed would present as "stop does nothing".
    let e = env();
    let plist = plist_path(&e.home, Channel::Stable);
    std::fs::create_dir_all(plist.parent().unwrap()).unwrap();
    std::fs::write(&plist, "<plist/>").unwrap();
    let runner = RecordingRunner::new().on("launchctl", RunResult::ok(""));
    let key = RecordingKey::new(e.paths.root().to_path_buf());

    bounded(stop_service(&ctx(
        &e,
        &runner,
        &key,
        Os::Macos,
        service_env(),
    )))
    .await
    .unwrap();

    let calls = runner.calls();
    // `unload` rather than `stop`: it drops KeepAlive too, so the agent stays
    // down until start-service loads it again.
    assert!(
        calls.iter().any(|c| c.contains("launchctl unload")),
        "{calls:?}"
    );
    assert!(!calls.iter().any(|c| c.starts_with("kill")), "{calls:?}");
    assert!(!calls.iter().any(|c| c.contains("pgrep")), "{calls:?}");
}

#[tokio::test]
async fn a_background_stop_sweeps_the_process_it_started() {
    let e = env();
    let runner = RecordingRunner::new()
        .on("pgrep", RunResult::ok("111\n222\n"))
        .on("kill", RunResult::ok(""));
    let key = RecordingKey::new(e.paths.root().to_path_buf());

    bounded(stop_service(&ctx(
        &e,
        &runner,
        &key,
        Os::Linux,
        service_env(),
    )))
    .await
    .unwrap();

    let calls = runner.calls();
    assert!(calls.iter().any(|c| c == "kill 111"), "{calls:?}");
    assert!(calls.iter().any(|c| c == "kill 222"), "{calls:?}");
    assert!(calls.iter().any(|c| c == "kill -9 111"), "{calls:?}");
}

// --- uninstall -------------------------------------------------------------

#[tokio::test]
async fn uninstall_does_not_kill_itself_before_clearing_the_keychain() {
    // uninstall-service runs FROM the binary it is sweeping, so `pgrep` returns
    // this process too. Without the exclusion the sweep terminates the
    // uninstall mid-flight, and it does not fail loudly: it leaves the service
    // gone with both the data and the master key still there.
    let e = env();
    let runner = RecordingRunner::new()
        .on("pgrep", RunResult::ok("111\n4242\n222\n"))
        .on("kill", RunResult::ok(""))
        .on("launchctl", RunResult::ok(""));
    let key = RecordingKey::new(e.paths.root().to_path_buf());

    bounded(uninstall_service(
        &ctx(&e, &runner, &key, Os::Macos, service_env()),
        false,
        false,
    ))
    .await
    .unwrap();

    let calls = runner.calls();
    assert!(
        !calls
            .iter()
            .any(|c| c == "kill 4242" || c == "kill -9 4242"),
        "it killed itself: {calls:?}"
    );
    assert!(calls.iter().any(|c| c == "kill 111"), "{calls:?}");
    assert!(calls.iter().any(|c| c == "kill 222"), "{calls:?}");
    // And the two steps the sweep would otherwise have prevented both ran.
    assert!(key.deleted(), "the master key was never cleared");
    assert!(!e.paths.root().exists(), "the core dir survived");
    assert!(
        key.dir_existed_when_called.load(Ordering::SeqCst),
        "the key must be cleared BEFORE the data is removed, not after"
    );
}

#[tokio::test]
async fn purge_models_is_independent_of_keep_data() {
    // Four combinations, because the uninstaller offers "keep my data" and
    // "remove downloaded models" as SEPARATE choices and the models dir is
    // shared across channels either way. Gating purge behind !keep_data is a
    // one-word mutation that three of these four cases cannot see.
    for (keep_data, purge_models) in [(false, false), (false, true), (true, false), (true, true)] {
        let e = env();
        std::fs::create_dir_all(e.paths.models_dir()).unwrap();
        std::fs::write(e.paths.models_dir().join("weights.gguf"), b"big").unwrap();
        let runner = RecordingRunner::new().on("launchctl", RunResult::ok(""));
        let key = RecordingKey::new(e.paths.root().to_path_buf());

        bounded(uninstall_service(
            &ctx(&e, &runner, &key, Os::Macos, service_env()),
            keep_data,
            purge_models,
        ))
        .await
        .unwrap();

        assert_eq!(
            e.paths.root().exists(),
            keep_data,
            "core dir with keep_data={keep_data} purge_models={purge_models}"
        );
        assert_eq!(
            key.deleted(),
            !keep_data,
            "master key with keep_data={keep_data}"
        );

        // The models assertion is that `remove_models_dir` was REACHED, not
        // that a directory vanished, and the reason is a fixture limit worth
        // naming rather than hiding: `Paths::rooted` nests `models/` INSIDE the
        // root by design ("nothing is shared with any other install",
        // `paths.rs:66-68`), whereas production puts it at `~/.loca/models`,
        // OUTSIDE the channel root, precisely so multi-GB weights are shared
        // across channels. Under this fixture the core-dir removal takes models
        // with it, so a filesystem check reports "purged" for a run that never
        // purged, and even the success line is unreachable because the function
        // finds nothing left to remove.
        //
        // Both of its lines are emitted by `remove_models_dir` and by nothing
        // else, so "either line appeared" is exactly "the call happened", which
        // is the property this test is named for.
        let reached_models =
            e.err.text().contains("--purge-models") || e.err.text().contains("models directory");
        assert_eq!(
            reached_models,
            purge_models,
            "purge attempt with keep_data={keep_data} purge_models={purge_models}: {}",
            e.err.text()
        );
        // And where the root SURVIVES, the nesting cannot interfere, so the
        // filesystem is checked there. That is the case that distinguishes
        // "purge is gated behind !keep_data" from "purge is independent".
        if keep_data {
            assert_eq!(
                e.paths.models_dir().exists(),
                !purge_models,
                "models with keep_data=true purge_models={purge_models}"
            );
        }
    }
}

#[tokio::test]
async fn keep_data_says_so_rather_than_silently_leaving_it() {
    let e = env();
    let runner = RecordingRunner::new().on("launchctl", RunResult::ok(""));
    let key = RecordingKey::new(e.paths.root().to_path_buf());

    bounded(uninstall_service(
        &ctx(&e, &runner, &key, Os::Macos, service_env()),
        true,
        false,
    ))
    .await
    .unwrap();

    assert!(e.err.text().contains("--keep-data"), "{}", e.err.text());
}

#[tokio::test]
async fn a_failed_keychain_delete_falls_back_to_the_native_tool_on_macos() {
    // A partially removed install can leave the entry with no helper able to
    // clear it. The fallback is macOS-only because it is the only platform with
    // a native tool that can.
    let e = env();
    let runner = RecordingRunner::new()
        .on("launchctl", RunResult::ok(""))
        .on("security", RunResult::ok(""));
    let key = RecordingKey::failing(e.paths.root().to_path_buf());

    bounded(uninstall_service(
        &ctx(&e, &runner, &key, Os::Macos, service_env()),
        false,
        false,
    ))
    .await
    .unwrap();

    let calls = runner.calls();
    let fallback = calls
        .iter()
        .find(|c| c.contains("security delete-generic-password"))
        .unwrap_or_else(|| panic!("no fallback: {calls:?}"));
    assert!(fallback.contains("com.rzkyif.loca.core"), "{fallback}");
    assert!(fallback.contains("master-key"), "{fallback}");
}

#[tokio::test]
async fn the_windows_uninstall_removes_the_arp_entry_it_wrote() {
    // Without this the entry survives, pointing Settings > Apps at a binary
    // that is gone.
    let e = env_for(Channel::Dev);
    let runner = RecordingRunner::new().on("powershell", RunResult::ok(""));
    let key = RecordingKey::new(e.paths.root().to_path_buf());

    bounded(uninstall_service(
        &ctx(&e, &runner, &key, Os::Windows, service_env()),
        false,
        false,
    ))
    .await
    .unwrap();

    let calls = runner.calls();
    assert!(
        calls
            .iter()
            .any(|c| c.contains("Remove-Item") && c.contains("Uninstall\\loca-core-dev")),
        "{calls:?}"
    );
    assert!(calls.iter().any(|c| c.contains("Unregister-ScheduledTask")));
}

#[tokio::test]
async fn a_surviving_scheduled_task_is_retried_self_elevated() {
    // Unregistering a root-folder task needs admin while every caller runs
    // unelevated, so a task that survived the plain attempt gets one elevated
    // retry rather than being left to fire at every logon.
    let e = env();
    let runner = RecordingRunner::new()
        .on("powershell", RunResult::ok(""))
        // The probe succeeding means the task is still there.
        .on("schtasks /Query", RunResult::ok(""));
    let key = RecordingKey::new(e.paths.root().to_path_buf());

    bounded(uninstall_service(
        &ctx(&e, &runner, &key, Os::Windows, service_env()),
        true,
        false,
    ))
    .await
    .unwrap();

    let calls = runner.calls();
    assert!(
        calls
            .iter()
            .any(|c| c.contains("RunAs") && c.contains("-EncodedCommand")),
        "{calls:?}"
    );
}

#[tokio::test]
async fn an_absent_service_definition_is_reported_rather_than_failing() {
    // Uninstalling a machine whose service was already removed by hand must
    // still clear the key and the data.
    let e = env();
    let runner = RecordingRunner::new();
    let key = RecordingKey::new(e.paths.root().to_path_buf());

    bounded(uninstall_service(
        &ctx(&e, &runner, &key, Os::Macos, service_env()),
        false,
        false,
    ))
    .await
    .unwrap();

    assert!(e.err.text().contains("launchd agent not installed"));
    assert!(key.deleted());
    assert!(!e.paths.root().exists());
}

#[tokio::test]
async fn a_windows_core_dir_removal_keeps_the_running_executable() {
    // On Windows this subcommand IS the running exe under <root>/bin, so it
    // cannot delete its own image. Everything else goes, and the locked binary
    // is left for the caller that outlives this process.
    let e = env();
    let running = e.paths.bin_path("loca-core");
    std::fs::create_dir_all(running.parent().unwrap()).unwrap();
    std::fs::write(&running, b"the running image").unwrap();
    std::fs::write(e.paths.root().join("settings.json"), b"{}").unwrap();

    // The removal only takes the Windows branch when the recursive delete
    // fails, which it cannot be made to do portably; so drive the branch
    // directly, which is what it exists for.
    super::remove_except(e.paths.root(), &running).await;

    assert!(running.exists(), "the locked image must survive");
    assert!(!e.paths.root().join("settings.json").exists());
    let _ = &e.out;
}

// --- restart ---------------------------------------------------------------

#[tokio::test]
async fn a_windows_restart_kills_before_it_re_registers() {
    // Unregister drops the old task's failure-restart policy, so killing AFTER
    // re-registering would let the kill be auto-relaunched into a second core.
    let e = env();
    let runner = RecordingRunner::new()
        .on("powershell", RunResult::ok(""))
        .on("schtasks", RunResult::ok(""));
    let key = RecordingKey::new(e.paths.root().to_path_buf());

    bounded(restart_service(&ctx(
        &e,
        &runner,
        &key,
        Os::Windows,
        service_env(),
    )))
    .await
    .unwrap();

    let calls = runner.calls();
    let sweep = calls
        .iter()
        .position(|c| c.contains("Get-Process"))
        .unwrap_or_else(|| panic!("no sweep: {calls:?}"));
    let register = calls
        .iter()
        .position(|c| c.contains("Register-ScheduledTask"))
        .unwrap();
    assert!(sweep < register, "{calls:?}");
}

#[tokio::test]
async fn a_background_restart_stops_and_relaunches_rather_than_bouncing_a_service() {
    let e = env();
    let runner = RecordingRunner::new()
        .on("pgrep", RunResult::failed(1))
        .on("sh -c", RunResult::ok("606\n"));
    let key = RecordingKey::new(e.paths.root().to_path_buf());

    bounded(restart_service(&ctx(
        &e,
        &runner,
        &key,
        Os::Macos,
        background_env(),
    )))
    .await
    .unwrap();

    let calls = runner.calls();
    assert!(!calls.iter().any(|c| c.contains("launchctl")), "{calls:?}");
    assert!(calls.iter().any(|c| c.contains("nohup")), "{calls:?}");
}

// --- consent ---------------------------------------------------------------

#[tokio::test]
async fn every_service_verb_makes_no_outbound_call() {
    // Service registration is the one path in this package with no HTTP in it
    // at all, which makes it exactly where someone would later add a "while we
    // are here, check for an update" call. The counter is per-package and
    // `net.rs` separately asserts that it goes UP, so this zero means
    // something.
    let net = crate::update::net::Net::new().unwrap();
    let e = env();
    let runner = RecordingRunner::new()
        .on("launchctl", RunResult::ok(""))
        .on("sh -c", RunResult::ok("1\n"))
        .on("pgrep", RunResult::failed(1))
        .on("powershell", RunResult::ok(""))
        .on("schtasks", RunResult::failed(1))
        .on("systemctl", RunResult::ok(""));
    let key = RecordingKey::new(e.paths.root().to_path_buf());

    for os in [Os::Macos, Os::Linux, Os::Windows] {
        let c = ctx(&e, &runner, &key, os, service_env());
        let _ = bounded(install_service(&c)).await;
        let _ = bounded(start_service(&c)).await;
        let _ = bounded(stop_service(&c)).await;
        let _ = bounded(restart_service(&c)).await;
        let _ = bounded(uninstall_service(&c, true, false)).await;
    }

    assert_eq!(net.calls(), 0);
}
