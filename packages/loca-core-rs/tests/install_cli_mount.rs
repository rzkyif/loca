//! The install CLI is MOUNTED in the desktop frontend.
//!
//! **Why this is an integration test and not a unit test.** The thing under
//! test is a call in `src/bin/loca-core.rs`, and a binary's `main` is not
//! reachable from the library's own suite. Deleting that call compiled, passed
//! every one of the crate's ~2100 tests, and made the entire `install/` subtree
//! (1,338 lines) unreachable in silence: the NSIS installer, the macOS pkg
//! postinstall, the deb/rpm hooks, every uninstaller and the Client's
//! `start-service` / `stop-service` all invoke it as a subcommand.
//!
//! That is the ported-but-unreachable family (D-134, D-141, D-165, D-166), and
//! it is the one instance the fleet knew about and left standing: `RESUME.md`
//! carried it as "stated rather than left to be found", verified by hand
//! against the real binary and by nothing else.
//!
//! **Why it was left.** `ServiceCtx.home` is deliberately `proc::real_home()`
//! rather than `paths.root()`, because service definitions must land in the
//! real `~/Library` or `~/.config` even when `LOCA_CORE_HOME` redirects core
//! state to a tempdir. So `LOCA_CORE_HOME` does NOT isolate the service verbs,
//! and running one "just to check the wiring" edits the developer's actual
//! machine. Every verb does real work; none is a safe read-only probe.
//!
//! **What makes it testable anyway.** `mint-code` is the one verb whose work is
//! confined to `paths` (which `LOCA_CORE_HOME` does isolate) and to loopback.
//! With no core running it simply waits for a `/health` that never comes. So
//! the observable is not what the verb ACHIEVES, it is what the process
//! BECOMES: a dispatched subcommand waits and never binds a port, while an
//! unmounted binary ignores argv and boots a server. Those two are
//! distinguishable in a couple of seconds and without touching anything
//! outside the tempdir.

// An integration test is its own crate, so the library's `#[cfg(test)]` allows
// do not reach it and the ban has to be lifted here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Plant an empty `loca-core-updater` where the boot-time helper check looks.
///
/// The desktop frontend refuses to start without it (D-025), which is the whole
/// point of that check: a core with no relauncher discovers it mid-update.
/// Nothing here runs the file, so an empty one is enough, and the conformance
/// harness seeds the same stub for the same reason
/// (`tests/conformance/harness/home.ts:80-81`). The name carries the `-dev`
/// channel suffix because these runs set `LOCA_CHANNEL=dev`.
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

/// The distinctive line `mint-code` prints while it waits (`pair.rs:158`).
const MINTING: &str = "waiting for core's TLS cert";
/// The distinctive line a BOOT prints once it is serving (`boot.rs`).
const SERVING: &str = "listening on";

/// Run the built binary with `args` for `budget`, then kill it and return
/// everything it wrote.
///
/// Killed rather than waited on: neither outcome under test exits on its own
/// inside the budget. `mint-code` has a 30-attempt health budget and a booted
/// core runs until signalled, so a test that waited for exit would take 30
/// seconds to learn what two seconds of output already says.
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
    // 7 seconds, and the number is derived rather than picked: `wait_for_health`
    // suppresses its first three attempts (`pair.rs:156`, `attempt > 3`) and
    // beats once a second between them, so the line cannot appear before ~5s. A
    // 3-second budget produced a FALSE RED on the first run of this test, which
    // is a reminder that a timing budget is part of the assertion.
    let output = run_briefly(&["mint-code"], Duration::from_secs(7));

    assert!(
        output.contains(MINTING),
        "`loca-core mint-code` did not reach the install CLI. The subtree that serves every \
         installer hook, every uninstaller and the Client's service controls is unreachable. \
         Output was:\n{output}",
    );
    // The half that catches the specific regression. Without the `maybe_run`
    // call the binary ignores argv entirely and boots normally, which still
    // produces plenty of output; only the ABSENCE of a listener separates that
    // from a dispatch.
    assert!(
        !output.contains(SERVING),
        "a subcommand booted a server instead of dispatching, so argv is being ignored. \
         Output was:\n{output}",
    );
}

#[test]
fn empty_argv_still_boots_rather_than_being_treated_as_nothing_to_do() {
    // The `None` arm, and it is as load-bearing as the `Some`. launchd starts
    // this binary with EMPTY argv and the updater relaunches it with
    // `--restart-args`, so a frontend that treated "not a subcommand" as
    // "nothing to do" would give a service install that never starts a core and
    // a self-update that never comes back.
    //
    // Cheap to get wrong in the obvious direction: an over-eager guard that
    // dispatched on any argv, or a `maybe_run` that returned `Some(0)` for an
    // unknown verb, would exit silently here and this is what notices.
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
/// own: one refuses to start and one rolls back, and the exit CODE is half of
/// what each asserts.
fn run_to_exit(home: &std::path::Path, port: &str, budget: Duration) -> (Option<i32>, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_loca-core"))
        .env("LOCA_CHANNEL", "dev")
        .env("LOCA_CORE_HOME", home)
        // Its own port. Neither outcome here should bind anything, but a
        // REGRESSION in either boots a full core, and on the shared dev port
        // that would fail the neighbouring test instead of this one.
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
    // Reaped on both paths, not just the timeout one: a process that exited on
    // its own was already collected by `try_wait`, and calling `wait` again on
    // it is a no-op that keeps the "always waited" rule readable.
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
    // `main`'s `ensure_helper_binaries` call (D-025).
    //
    // Without it a core with no `loca-core-updater` starts happily and finds
    // out mid-update that nothing can relaunch it, which turns a broken install
    // into a core that is DOWN rather than one that refused to come up. The
    // check is in the frontend rather than in `boot()` on purpose: `boot()` is
    // shared with the mobile embed, which has no updater and no self-update.
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
    // `main`'s `handle_update_marker_on_boot` call.
    //
    // The write half of this was already wired (`commit_update` in `boot`), so
    // markers were being CLEARED on healthy boots and never CONSULTED on
    // unhealthy ones: a core that self-updated into a binary that cannot start
    // would crash-loop forever with its own recovery sitting unused on disk.
    //
    // **Exit 75, not 0.** launchd relaunches on any exit, but systemd's
    // `Restart=on-failure` and Task Scheduler's only relaunch on a failure
    // exit, so a clean exit would leave the rolled-back binary stopped on two
    // of three platforms.
    let home = tempfile::tempdir().expect("tempdir");
    seed_updater_stub(home.path());
    let paths = loca_core::paths::Paths::rooted(home.path());

    // The rollback moves the LIVE binary aside and renames the anchor into its
    // place, so both files have to exist where an install would have put them.
    // The process under test runs from cargo's target dir, which is why this is
    // planted rather than assumed.
    let live = home
        .path()
        .join("bin")
        .join(if cfg!(target_os = "windows") {
            "loca-core-dev.exe"
        } else {
            "loca-core-dev"
        });
    std::fs::write(&live, b"the core that cannot start").expect("live binary");
    // The anchor the rollback restores. Non-empty, because an empty one is what
    // an interrupted copy leaves and the rollback refuses it by design.
    let anchor = home
        .path()
        .join("bin")
        .join(if cfg!(target_os = "windows") {
            "loca-core-dev.exe.old"
        } else {
            "loca-core-dev.old"
        });
    std::fs::write(&anchor, b"the previous core").expect("anchor");
    // Executable, because the rollback inspects the anchor rather than trusting
    // its existence and refuses to install one it can see is unrunnable.
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
        // Re-read between bumps: `bump` writes `marker.attempts + 1` from the
        // snapshot it is given, so bumping the same one twice still lands on 1.
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
