//! The install CLI is MOUNTED in the desktop frontend.
//!
//! An integration test because the thing under test is a call in
//! `src/bin/loca-core.rs`, and a binary's `main` is not reachable from the
//! library's suite. Deleting that call compiles, passes every unit test, and
//! silently makes the whole `install/` subtree unreachable: every installer
//! hook, every uninstaller, and the Client's service controls invoke it.
//!
//! **Why it is hard.** `ServiceCtx.home` is deliberately `proc::real_home()`
//! rather than `paths.root()`, because service definitions must land in the
//! real `~/Library` or `~/.config` even when `LOCA_CORE_HOME` redirects core
//! state. So `LOCA_CORE_HOME` does NOT isolate the service verbs, and every one
//! of them does real work on the developer's machine.
//!
//! `mint-code` is the exception: its work is confined to `paths` and to
//! loopback, and with no core running it just waits. So the observable is what
//! the process BECOMES, not what the verb achieves: a dispatched subcommand
//! waits and never binds a port, while an unmounted binary boots a server.

// An integration test is its own crate, so the library's `#[cfg(test)]` allows
// do not reach it and the ban has to be lifted here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Plant an empty `loca-core-updater` where the boot-time helper check looks:
/// the desktop frontend refuses to start without one. Nothing runs the file, so
/// empty is enough; the `-dev` suffix matches `LOCA_CHANNEL=dev` below.
fn seed_updater_stub(home: &std::path::Path) {
    let bin = home.join("bin");
    std::fs::create_dir_all(&bin).expect("bin dir");
    let name = if cfg!(target_os = "windows") {
        "loca-core-updater-dev.exe"
    } else {
        "loca-core-updater-dev"
    };
    std::fs::write(bin.join(name), b"").expect("updater stub");
}

/// The distinctive line `mint-code` prints while it waits.
const MINTING: &str = "waiting for core's TLS cert";
/// The distinctive line a BOOT prints once it is serving (`boot.rs`).
const SERVING: &str = "listening on";

/// Run the built binary with `args` for `budget`, then kill it and return
/// everything it wrote.
///
/// Killed rather than waited on: neither outcome exits on its own inside the
/// budget, and two seconds of output already says which one happened.
fn run_briefly(args: &[&str], budget: Duration) -> String {
    let home = tempfile::tempdir().expect("tempdir");
    seed_updater_stub(home.path());
    let mut child = Command::new(env!("CARGO_BIN_EXE_loca-core"))
        .args(args)
        // `dev`, so `secrets::store::select` keeps the vault master key in a
        // file and nothing reaches the OS keychain. On any other channel this
        // would prompt a human on macOS and hang the suite.
        .env("LOCA_CHANNEL", "dev")
        .env("LOCA_CORE_HOME", home.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the core binary");

    let started = Instant::now();
    while started.elapsed() < budget {
        if child.try_wait().ok().flatten().is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();

    let mut out = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        let _ = stdout.read_to_string(&mut out);
    }
    let mut err = String::new();
    if let Some(mut stderr) = child.stderr.take() {
        let _ = stderr.read_to_string(&mut err);
    }
    format!("{out}{err}")
}

#[test]
fn a_subcommand_reaches_the_install_cli_instead_of_booting_a_server() {
    // 7 seconds, derived rather than picked: `wait_for_health` suppresses its
    // first three attempts and beats once a second, so the line cannot appear
    // before ~5s and a shorter budget is a false red.
    let output = run_briefly(&["mint-code"], Duration::from_secs(7));

    assert!(
        output.contains(MINTING),
        "`loca-core mint-code` did not reach the install CLI. The subtree that serves every \
         installer hook, every uninstaller and the Client's service controls is unreachable. \
         Output was:\n{output}",
    );
    // Without the `maybe_run` call the binary ignores argv and boots normally,
    // which still produces output: only the absence of a listener separates
    // that from a dispatch.
    assert!(
        !output.contains(SERVING),
        "a subcommand booted a server instead of dispatching, so argv is being ignored. \
         Output was:\n{output}",
    );
}

#[test]
fn empty_argv_still_boots_rather_than_being_treated_as_nothing_to_do() {
    // The `None` arm, as load-bearing as the `Some`: launchd starts this
    // binary with EMPTY argv and the updater relaunches it with
    // `--restart-args`, so treating "not a subcommand" as "nothing to do"
    // gives a service install that never starts a core.
    let output = run_briefly(&[], Duration::from_secs(6));

    assert!(
        output.contains(SERVING),
        "a bare launch did not reach `boot`, so launchd would start a core that never \
         serves. Output was:\n{output}",
    );
}

/// Run the built binary to COMPLETION and return `(exit code, output)`.
///
/// Separate from [`run_briefly`] because the two outcomes below exit on their
/// own, and the exit code is half of what each asserts.
fn run_to_exit(home: &std::path::Path, port: &str, budget: Duration) -> (Option<i32>, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_loca-core"))
        .env("LOCA_CHANNEL", "dev")
        .env("LOCA_CORE_HOME", home)
        // Its own port: a regression in either boots a full core, and on the
        // shared dev port that would fail the neighbouring test.
        .env("LOCA_CORE_PORT", port)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the core binary");

    let started = Instant::now();
    let mut status = None;
    while started.elapsed() < budget {
        if let Some(exited) = child.try_wait().ok().flatten() {
            status = Some(exited);
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    // Reaped on both paths: a second `wait` on an already-collected process
    // is a no-op.
    let _ = child.kill();
    let _ = child.wait();

    let mut out = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        let _ = stdout.read_to_string(&mut out);
    }
    let mut err = String::new();
    if let Some(mut stderr) = child.stderr.take() {
        let _ = stderr.read_to_string(&mut err);
    }
    (status.and_then(|s| s.code()), format!("{out}{err}"))
}

#[test]
fn a_missing_updater_stops_the_core_starting_at_all() {
    // `main`'s `ensure_helper_binaries` call. Without it a core with no
    // `loca-core-updater` starts happily and finds out mid-update that nothing
    // can relaunch it. In the frontend rather than `boot()`, which is shared
    // with the mobile embed, where there is no updater at all.
    let home = tempfile::tempdir().expect("tempdir");
    // Deliberately NOT seeded: this is the one test that wants it missing.
    let (code, output) = run_to_exit(home.path(), "7741", Duration::from_secs(15));

    assert!(
        output.contains("missing helper binaries"),
        "a core with no updater started without complaining, so the missing \
         relauncher will surface mid-update instead. Output was:\n{output}",
    );
    assert!(
        !output.contains(SERVING),
        "a core with no updater bound a listener instead of refusing. Output was:\n{output}",
    );
    assert_eq!(code, Some(1), "the refusal must be a failure exit");
}

#[test]
fn a_spent_update_marker_rolls_back_and_exits_75() {
    // `main`'s `handle_update_marker_on_boot` call. `commit_update` in `boot`
    // clears markers on healthy boots; without this half nothing consults one
    // on an unhealthy boot, so a core that updated into a binary that cannot
    // start crash-loops with its recovery unused.
    //
    // **Exit 75, not 0.** systemd's `Restart=on-failure` and Task Scheduler's
    // only relaunch on a failure exit.
    let home = tempfile::tempdir().expect("tempdir");
    seed_updater_stub(home.path());
    let paths = loca_core::paths::Paths::rooted(home.path());

    // The rollback moves the LIVE binary aside and renames the anchor into its
    // place, so both must exist where an install would have put them; the
    // process under test runs from cargo's target dir.
    let live = home
        .path()
        .join("bin")
        .join(if cfg!(target_os = "windows") {
            "loca-core-dev.exe"
        } else {
            "loca-core-dev"
        });
    std::fs::write(&live, b"the core that cannot start").expect("live binary");
    // The anchor. Non-empty, because an empty one is an interrupted copy and
    // the rollback refuses it.
    let anchor = home
        .path()
        .join("bin")
        .join(if cfg!(target_os = "windows") {
            "loca-core-dev.exe.old"
        } else {
            "loca-core-dev.old"
        });
    std::fs::write(&anchor, b"the previous core").expect("anchor");
    // Executable: the rollback refuses an anchor it can see is unrunnable.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        for path in [&live, &anchor] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("mode");
        }
    }

    // A marker for THIS version whose retry budget is already spent, which is
    // the state two failed boots leave behind.
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    runtime.block_on(async {
        loca_core::update::marker::write(&paths, loca_core::config::CORE_VERSION, "0.0.1")
            .await
            .expect("marker");
        let loca_core::update::marker::MarkerState::Present(marker) =
            loca_core::update::marker::read(&paths).await
        else {
            panic!("the marker was not written");
        };
        // Re-read between bumps: `bump` writes `attempts + 1` from the
        // snapshot it is given.
        let mut marker = marker;
        for _ in 0..loca_core::update::rollback::MAX_BOOT_ATTEMPTS {
            loca_core::update::marker::bump(&paths, &marker)
                .await
                .expect("bump");
            let loca_core::update::marker::MarkerState::Present(next) =
                loca_core::update::marker::read(&paths).await
            else {
                panic!("the marker vanished mid-bump");
            };
            marker = next;
        }
    });

    let (code, output) = run_to_exit(home.path(), "7742", Duration::from_secs(20));

    assert_eq!(
        code,
        Some(75),
        "a spent update marker did not roll back and exit 75, so a core that \
         updated into a broken binary crash-loops with its recovery unused. \
         Output was:\n{output}",
    );
    assert!(
        !output.contains(SERVING),
        "the rollback path bound a listener instead of exiting. Output was:\n{output}",
    );
}
