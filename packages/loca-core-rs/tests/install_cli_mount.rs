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
