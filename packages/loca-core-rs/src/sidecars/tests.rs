//! Child-process behaviour cannot be asserted without a child, so this suite
//! spawns real ones.
//!
//! Every test below was watched go red against the mutation named in its
//! comment before being trusted (D-015). Several of them look like they assert
//! a language default and do not: `env_clear` and the output pumps are both
//! things whose absence produces a plausible-looking pass elsewhere.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use crate::contract::api::{CoreStatus, SidecarKind, SidecarSnapshot, SidecarStatus};
use crate::transport::status::{CoreStatusService, SidecarSource};

use super::process::{self, Output, Spawn, MAX_LINE_BYTES};
use super::registry::Manager;

// --- helpers -----------------------------------------------------------------

/// A child running a shell script, with a deliberately empty environment: the
/// tests that care pass what they need explicitly.
fn shell(label: &str, script: &str) -> Spawn {
    #[cfg(unix)]
    {
        Spawn::new(label, "/bin/sh").arg("-c").arg(script)
    }
    #[cfg(windows)]
    {
        Spawn::new(label, "cmd.exe").arg("/C").arg(script)
    }
}

fn snapshot(kind: SidecarKind, status: SidecarStatus) -> SidecarSnapshot {
    SidecarSnapshot {
        status,
        ..super::registry::disabled(kind)
    }
}

/// Fails the test rather than hanging the suite: every wait here is supposed to
/// return promptly, and an undrained pipe is exactly the bug that would make
/// one never return.
async fn within<T>(what: &str, secs: u64, f: impl std::future::Future<Output = T>) -> T {
    match tokio::time::timeout(Duration::from_secs(secs), f).await {
        Ok(value) => value,
        Err(_) => panic!("{what} did not finish within {secs}s"),
    }
}

// --- process -----------------------------------------------------------------

/// Mutation: delete `env_clear()` in `process::spawn`. The child then inherits
/// the whole environment and the first assertion fails, naming the leaked key.
///
/// Unix only because it needs a program that prints its environment and adds
/// nothing of its own; `cmd.exe` seeds several variables by itself. The
/// mechanism under test is `env_clear`, which is not platform-specific.
#[cfg(unix)]
#[tokio::test]
async fn the_parent_environment_is_not_inherited() {
    let manager = Manager::new();

    let child = manager
        .spawn(Spawn::new("env-empty", "/usr/bin/env"))
        .unwrap();
    let mut child = child.lock().await;
    within("empty env child", 10, child.wait()).await.unwrap();
    let leaked = child.recent_output();
    assert!(
        leaked.is_empty(),
        "the child saw the core's environment: {leaked:?}",
    );

    // And what IS named arrives, so the first assertion is not passing because
    // the spawn silently produced nothing.
    let child = manager
        .spawn(Spawn::new("env-named", "/usr/bin/env").env("LOCA_TEST_MARKER", "present"))
        .unwrap();
    let mut child = child.lock().await;
    within("named env child", 10, child.wait()).await.unwrap();
    assert_eq!(child.recent_output(), vec!["LOCA_TEST_MARKER=present"]);
}

/// The pipe-deadlock guard, and the reason the output pumps are spawned before
/// anything awaits the child.
///
/// Mutation: drop the `relay` calls in `process::spawn`. A pipe fills at ~64 KB,
/// the child blocks on its next write, `wait` never returns, and this fails on
/// the timeout instead of hanging the suite.
#[tokio::test]
async fn a_chatty_child_does_not_deadlock_the_wait() {
    let manager = Manager::new();
    let child = manager
        .spawn(shell(
            "chatty",
            "yes 0123456789ABCDEF | head -c 2000000; exit 0",
        ))
        .unwrap();
    let mut child = child.lock().await;
    let exit = within("chatty child", 30, child.wait()).await.unwrap();
    assert_eq!(exit.code, Some(0));
    // Two megabytes in, ten lines kept.
    assert_eq!(child.recent_output().len(), 10);
}

/// The exit message is the child's own last words, not a bare status line.
///
/// Mutation: compose the message from an empty ring in `Child::settle`. The
/// caller then reports "exited with code 3" for a child that said exactly why.
///
/// **What this test does NOT guard**, stated because the comment that claimed
/// it did would have been worse than none: `settle` joins the output pumps
/// BEFORE reading the ring, because on a multi-threaded runtime `wait` can
/// return while another worker still holds the child's final write. Moving the
/// read back before the join leaves this test green, with a 2 MB backlog ahead
/// of the marker and across repeated runs: the pump always wins the race on a
/// current-thread test runtime. Forcing it would mean a `cfg(test)` delay
/// inside the read loop, which is production code shaped by a test. The
/// ordering stays because it is correct and free, not because this proves it.
#[tokio::test]
async fn the_exit_message_carries_the_childs_last_line() {
    let manager = Manager::new();
    let child = manager
        .spawn(shell("dying", "echo final-line-marker; exit 3"))
        .unwrap();
    let mut child = child.lock().await;
    let exit = within("dying child", 10, child.wait()).await.unwrap();
    assert_eq!(exit.code, Some(3));
    assert!(
        exit.message.contains("final-line-marker"),
        "exit message lost the child's last line: {:?}",
        exit.message,
    );
}

/// Mutation: raise `RECENT_LOG_LINES`, or drop the `pop_front`. The ring grows
/// without bound, which on a chatty child is an unbounded allocation and here
/// is a wrong length.
#[tokio::test]
async fn the_ring_keeps_the_last_ten_lines_in_order() {
    let manager = Manager::new();
    let child = manager
        .spawn(shell(
            "counter",
            "i=1; while [ $i -le 20 ]; do echo line-$i; i=$((i+1)); done",
        ))
        .unwrap();
    let mut child = child.lock().await;
    within("counter child", 10, child.wait()).await.unwrap();
    let expected: Vec<String> = (11..=20).map(|i| format!("line-{i}")).collect();
    assert_eq!(child.recent_output(), expected);
}

/// A child that never emits a newline must cost one capped line, not the heap.
///
/// Mutation: remove the `MAX_LINE_BYTES` branch in `relay`. The whole 1.5 MB is
/// buffered and the assertion on the capped length fails.
#[tokio::test]
async fn a_line_with_no_newline_is_capped_rather_than_buffered() {
    let manager = Manager::new();
    let child = manager
        .spawn(shell(
            "flood",
            r"head -c 1500000 /dev/zero | tr '\000' 'a'; echo",
        ))
        .unwrap();
    let mut child = child.lock().await;
    within("flood child", 30, child.wait()).await.unwrap();
    let lines = child.recent_output();
    assert_eq!(lines.len(), 1);
    assert!(
        lines[0].ends_with("[truncated]"),
        "the capped line is not marked",
    );
    assert_eq!(lines[0].len(), MAX_LINE_BYTES + " [truncated]".len());
}

/// Mutation: drop the SIGKILL escalation in `Child::terminate` and only send
/// SIGTERM. The child ignores it and `terminate` waits forever, so this fails on
/// the harness timeout.
///
/// Takes the full five-second grace window by construction: shortening it would
/// mean making the constant injectable, which is API surface added for a test.
#[cfg(unix)]
#[tokio::test]
async fn a_child_that_ignores_sigterm_is_killed() {
    let manager = Manager::new();
    let child = manager
        .spawn(shell("stubborn", "trap '' TERM; sleep 60"))
        .unwrap();
    // Let the shell install its trap before signalling it.
    tokio::time::sleep(Duration::from_millis(300)).await;

    let started = Instant::now();
    let exit = within("stubborn child", 30, async {
        child.lock().await.terminate().await
    })
    .await;
    let elapsed = started.elapsed();

    assert_eq!(
        exit.signal,
        Some(nix::libc::SIGKILL),
        "not killed: {exit:?}"
    );
    assert!(
        elapsed >= Duration::from_secs(5),
        "escalated before the grace window elapsed: {elapsed:?}",
    );
    // Grace, then the kill, then one shared drain budget. `sleep` inherited the
    // pipes and outlives the shell, so the pumps never see EOF and always spend
    // that budget in full: the ceiling here is what proves the budget is shared
    // across pipes rather than spent per pipe.
    assert!(
        elapsed < Duration::from_secs(8),
        "escalation took too long: {elapsed:?}",
    );
}

/// Mutation: in `Manager::shutdown`, skip the `terminate` call. The child
/// survives and its own wait never returns, so this fails on the timeout.
#[cfg(unix)]
#[tokio::test]
async fn shutdown_stops_a_child_that_is_still_running() {
    let manager = Manager::new();
    let child = manager.spawn(shell("long-lived", "sleep 60")).unwrap();
    within("shutdown", 30, manager.shutdown()).await;
    // Idempotent: the exit is cached, so the owner still gets an answer.
    let exit = within("post-shutdown wait", 5, async {
        child.lock().await.wait().await
    })
    .await
    .unwrap();
    assert!(exit.code.is_some() || exit.signal.is_some());
}

/// The caller can take a stream instead, which is what D12 needs for its
/// NDJSON protocol. Mutation: return `None` from `take_stdout`, or pipe the
/// stream into `relay` regardless of the mode.
#[tokio::test]
async fn a_piped_stream_is_handed_over_instead_of_relayed() {
    use tokio::io::AsyncReadExt as _;

    let manager = Manager::new();
    let child = manager
        .spawn(shell("piped", "echo protocol-frame").stdout(Output::Piped))
        .unwrap();
    let mut child = child.lock().await;
    let mut stdout = child.take_stdout().expect("stdout was not handed over");
    let mut read = String::new();
    within("piped read", 10, stdout.read_to_string(&mut read))
        .await
        .unwrap();
    assert_eq!(read.trim(), "protocol-frame");
    // It went to the caller, so it is not in the ring the exit message uses.
    within("piped child", 10, child.wait()).await.unwrap();
    assert!(child.recent_output().is_empty());
}

/// Mutation: drop a key from `OPERATIONAL_ENV_KEYS`, or make `inherited` read
/// something other than this process's environment.
#[test]
fn inherited_reads_only_the_named_keys() {
    let env = process::inherited(&["PATH", "LOCA_DEFINITELY_NOT_SET_ANYWHERE"]);
    assert!(env.contains_key(std::ffi::OsStr::new("PATH")));
    assert_eq!(env.len(), 1, "an unset key must not appear at all");
}

// --- registry ----------------------------------------------------------------

/// Mutation: swap the `BTreeMap` in `Inner::states` for a `HashMap`. The order
/// then depends on the hash seed, `transport::status` compares the subsystem
/// list positionally, and every recompute looks like a change.
///
/// Repeated over fresh managers, and not because ordering is flaky. A single
/// three-entry `HashMap` came out in ascending order by coincidence and passed
/// this test with the guarantee removed, which is precisely the shape D-015
/// warns about. `RandomState` reseeds per instance, so each iteration is a new
/// draw: all four kinds inserted in descending order have a 1-in-24 chance of
/// coming back ascending by luck, and thirty-two independent draws take that to
/// nothing.
#[test]
fn published_snapshots_come_back_in_kind_order() {
    let ascending = vec![
        SidecarKind::Llama,
        SidecarKind::LlamaEmbed,
        SidecarKind::Speech,
        SidecarKind::Tool,
    ];
    for attempt in 0..32 {
        let manager = Manager::new();
        for kind in ascending.iter().rev() {
            manager.publish(snapshot(*kind, SidecarStatus::Running));
        }
        let kinds: Vec<SidecarKind> = manager.statuses().into_iter().map(|s| s.kind).collect();
        assert_eq!(kinds, ascending, "out of order on attempt {attempt}");
    }
}

/// Mutation: seed every kind at construction. A subsystem nobody owns then gets
/// a row in the client's status card that never changes.
#[test]
fn an_unpublished_kind_reads_disabled_and_occupies_no_row() {
    let manager = Manager::new();
    assert_eq!(
        manager.status(SidecarKind::Speech).status,
        SidecarStatus::Disabled,
    );
    assert!(manager.statuses().is_empty());

    manager.publish(snapshot(SidecarKind::Speech, SidecarStatus::Running));
    assert_eq!(manager.statuses().len(), 1);
    manager.clear(SidecarKind::Speech);
    assert!(manager.statuses().is_empty(), "clear must drop the row");
}

/// Mutation: remove the `same()` guard in `publish`. The second call then
/// reports a change and recomputes the aggregate for nothing.
///
/// The aggregator dedupes too, so this guard is an optimisation rather than a
/// correctness property; returning the bool is what makes it assertable instead
/// of invisible.
#[test]
fn republishing_an_identical_snapshot_is_not_a_change() {
    let manager = Manager::new();
    let snap = snapshot(SidecarKind::Llama, SidecarStatus::Running);
    assert!(manager.publish(snap.clone()));
    assert!(!manager.publish(snap));
    assert!(manager.publish(snapshot(SidecarKind::Llama, SidecarStatus::Error)));
}

/// The wiring test: a subsystem change has to reach the aggregate status, or
/// the core bar never moves off whatever it last showed.
///
/// Mutation: drop the `recompute()` call in `publish`. No frame arrives and
/// this fails on the timeout.
#[tokio::test]
async fn a_published_change_recomputes_the_core_status() {
    let manager = Arc::new(Manager::new());
    let status = CoreStatusService::new(Arc::clone(&manager) as Arc<dyn SidecarSource>);
    manager.attach_status(status.clone());
    status.note_boot_done();

    let mut frames = status.subscribe();
    manager.publish(SidecarSnapshot {
        message: Some("model file is missing".to_string()),
        ..snapshot(SidecarKind::Llama, SidecarStatus::Error)
    });

    let frame = within("status frame", 5, frames.recv()).await.unwrap();
    assert_eq!(frame.status, CoreStatus::Error);
    assert_eq!(frame.detail.as_deref(), Some("model file is missing"));
    assert_eq!(frame.subsystems.len(), 1);
}

// --- metrics -----------------------------------------------------------------

/// Mutation: report a zeroed sample for a pid that is gone instead of omitting
/// it. One dead child then shows a row claiming it uses no memory, which reads
/// as "running and idle" rather than "not running".
#[test]
fn metrics_sample_this_process_and_skip_a_dead_pid() {
    let manager = Manager::new();
    let me = std::process::id();
    // Far above any platform's pid ceiling, so it cannot collide with a real
    // process on the machine running the suite.
    let gone = u32::MAX - 1;
    let samples = manager.metrics().sample(&[me, gone]);

    let mine = samples.get(&me).expect("this process was not sampled");
    assert!(mine.rss_mb > 0.0, "a live process with no memory: {mine:?}");
    assert!(!samples.contains_key(&gone));
}

// --- routes ------------------------------------------------------------------

/// Mutation: drop the `require_client` layer. The route then serves process
/// pids and memory figures to an unauthenticated caller.
#[tokio::test]
async fn the_status_route_needs_a_bearer() {
    let ctx = crate::transport::testing::context();
    let router = super::routes::routes(&ctx, Arc::new(Manager::new()));
    let response = router
        .oneshot(
            Request::builder()
                .uri("/api/v1/sidecars/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

/// Mutation: take the pid set from the request, or drop the metrics merge. The
/// core row is what the Services field's "Core Service" line reads.
#[tokio::test]
async fn the_status_route_reports_the_core_process_and_the_published_rows() {
    let ctx = crate::transport::testing::context_with(Arc::new(
        crate::transport::testing::StubAuth::new().with_client("t0ken", "client-a"),
    ));
    let manager = Arc::new(Manager::new());
    manager.publish(snapshot(SidecarKind::Tool, SidecarStatus::Running));

    let router = super::routes::routes(&ctx, Arc::clone(&manager));
    let response = router
        .oneshot(
            Request::builder()
                .uri("/api/v1/sidecars/status")
                .header("authorization", "Bearer t0ken")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        body["core"]["pid"].as_f64(),
        Some(f64::from(std::process::id())),
    );
    assert!(body["core"]["rssMb"].as_f64().unwrap() > 0.0);
    assert_eq!(body["sidecars"].as_array().unwrap().len(), 1);
    assert_eq!(body["sidecars"][0]["kind"], "tool");
}
