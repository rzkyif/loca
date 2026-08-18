//! The unix PTY backend: `openpty`, the termios the prompt depends on, and the
//! echo cancellation that keeps Core's own frames out of the prompt stream.
//!
//! Ports `loca-core-ptyhost/src/main.rs`'s `mod unix` (lines 194-426). The
//! terminal handling is unchanged; what is gone is the process boundary it used
//! to sit behind.

use std::collections::VecDeque;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::os::unix::process::CommandExt as _;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use nix::pty::openpty;
use nix::sys::termios::{cfmakeraw, tcgetattr, tcsetattr, LocalFlags, SetArg};
use tokio::sync::{mpsc, oneshot};

use crate::error::{internal, Result};

use super::{cancel_echo, expect_echo, PtyEvent, PtySpawn, Spawned};

/// How long the terminal reader gets to drain after the child exits, before the
/// exit event goes out regardless.
///
/// The drain is the point: the last thing on the terminal is usually the line
/// that explains the exit, and reporting the exit first would race it away.
const READER_DRAIN_GRACE: Duration = Duration::from_secs(2);

/// A live terminal. Cheap to clone; every clone writes to the same master.
#[derive(Clone)]
pub struct PtySession {
    master: Arc<Mutex<OwnedFd>>,
    pending_echo: Arc<Mutex<VecDeque<u8>>>,
    kill: mpsc::UnboundedSender<()>,
}

impl PtySession {
    /// Write worker-protocol bytes to the terminal.
    ///
    /// Echo-tracked: ECHO is on (Deno will not prompt without it), so every byte
    /// written here bounces back out of the master. The bytes are queued BEFORE
    /// the write, so the reader thread cannot see the echo before we have said
    /// to expect it.
    pub fn write(&self, bytes: &[u8]) {
        expect_echo(&self.pending_echo, bytes);
        self.write_raw(bytes);
    }

    /// Write a permission-prompt answer.
    ///
    /// NOT echo-tracked, and that asymmetry is load-bearing: Deno reads the
    /// answer with echo disabled, so it never bounces back. Tracking it would
    /// leave a stuck prefix at the head of the queue and desync every subsequent
    /// cancellation, which surfaces as Core's own frames appearing in the prompt
    /// parser's input.
    pub fn answer(&self, bytes: &[u8]) {
        self.write_raw(bytes);
    }

    /// Ask for the child to be killed. Returns immediately; the exit arrives as
    /// a [`PtyEvent::Exited`].
    pub fn kill(&self) {
        let _ = self.kill.send(());
    }

    fn write_raw(&self, bytes: &[u8]) {
        let master = lock(&self.master);
        write_all(master.as_fd(), bytes);
    }
}

pub fn spawn(spec: PtySpawn) -> Result<Spawned> {
    let pty = openpty(None, None).map_err(|e| internal(format!("openpty failed: {e}")))?;

    // Slave line discipline: no canonical mode, because its ~1 KB line limit
    // would mangle a large protocol frame, and no output post-processing. But
    // ECHO stays ON. Deno 2.8.x refuses to show a permission prompt when stdin
    // is in raw mode, which it defines as `c_lflag & (ICANON | ECHO) == 0`
    // (denoland/deno#34457), so clearing ECHO silences prompts entirely. The
    // cost is the echo, which `PtySession::write` cancels.
    let mut termios =
        tcgetattr(pty.slave.as_fd()).map_err(|e| internal(format!("tcgetattr failed: {e}")))?;
    cfmakeraw(&mut termios);
    termios.local_flags.insert(LocalFlags::ECHO);
    tcsetattr(pty.slave.as_fd(), SetArg::TCSANOW, &termios)
        .map_err(|e| internal(format!("tcsetattr failed: {e}")))?;

    let stdin_fd = pty
        .slave
        .try_clone()
        .map_err(|e| internal(format!("failed to clone pty slave: {e}")))?;
    let stderr_fd = pty
        .slave
        .try_clone()
        .map_err(|e| internal(format!("failed to clone pty slave: {e}")))?;
    let master_writer = pty
        .master
        .try_clone()
        .map_err(|e| internal(format!("failed to clone pty master: {e}")))?;

    let slave_raw = pty.slave.as_raw_fd();
    let mut command = tokio::process::Command::new(&spec.program);
    command
        .args(&spec.args)
        // Clear first: `.envs()` over an inherited environment would leave every
        // secret the core holds visible to the worker, and the allowlist would
        // be decoration rather than a boundary.
        .env_clear()
        .envs(&spec.env)
        .stdin(Stdio::from(stdin_fd))
        // The protocol rides an ordinary pipe. Only stderr and the prompt go
        // through the terminal.
        .stdout(Stdio::piped())
        .stderr(Stdio::from(stderr_fd))
        .kill_on_drop(true);
    if let Some(cwd) = &spec.cwd {
        command.current_dir(cwd);
    }
    // SAFETY: setsid and the TIOCSCTTY ioctl are async-signal-safe, which is the
    // whole constraint on a post-fork hook. Both are required: the PTY has to
    // become the child's CONTROLLING terminal, because Deno's prompter reads the
    // answer from the terminal it controls rather than from fd 0.
    unsafe {
        command.as_std_mut().pre_exec(move || {
            if nix::libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if nix::libc::ioctl(slave_raw, nix::libc::TIOCSCTTY as _, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let mut child = command.spawn().map_err(|e| {
        internal(format!(
            "{}: could not spawn {}: {e}",
            spec.label,
            spec.program.display()
        ))
    })?;

    // Release every slave fd we still hold so the master read errors out (EIO)
    // once the child's copies close, which is what ends the reader thread.
    //
    // BOTH our own `pty.slave` AND the two clones the command still owns via
    // `Stdio::from` have to go: spawn dup2s them into the child but does not
    // consume the parent's handles. On Linux a master read only reports EIO when
    // the LAST slave fd closes, so one lingering clone wedges the reader (and so
    // the exit event) forever after the child is gone. macOS masks this by
    // delivering EOF on child exit regardless, so it only bites on Linux, which
    // is exactly the shape of bug that reaches a user before it reaches CI.
    drop(pty.slave);
    drop(command);

    let stdout = child.stdout.take();
    let pid = child.id();
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let (kill_tx, mut kill_rx) = mpsc::unbounded_channel::<()>();
    let pending_echo = Arc::new(Mutex::new(VecDeque::<u8>::new()));

    // Master to Core: a blocking read on a dedicated thread. A PTY master is not
    // pollable through tokio without an AsyncFd registration, and the read is
    // the only thing this thread does, so a thread is both simpler and exactly
    // what the shipping helper already did.
    let reader_echo = Arc::clone(&pending_echo);
    let reader_tx = event_tx.clone();
    let (drained_tx, drained_rx) = oneshot::channel();
    let label = spec.label.clone();
    std::thread::Builder::new()
        .name(format!("pty-reader {}", spec.label))
        .spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match nix::unistd::read(pty.master.as_fd(), &mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let out = cancel_echo(&reader_echo, &buf[..n]);
                        // An all-echo read cancels to nothing. Sending it anyway
                        // would hand the prompt parser an empty chunk on every
                        // frame Core writes.
                        if !out.is_empty() && reader_tx.send(PtyEvent::Output(out)).is_err() {
                            break;
                        }
                    }
                    // EIO: every slave fd is closed, so the child is gone.
                    // Anything else is equally terminal for the bridge.
                    Err(_) => break,
                }
            }
            let _ = drained_tx.send(());
        })
        .map_err(|e| internal(format!("{label}: could not start the pty reader: {e}")))?;

    tokio::spawn(async move {
        let mut killing = false;
        let status = loop {
            tokio::select! {
                status = child.wait() => break status.ok(),
                // The handler is empty on purpose: it must not borrow `child`,
                // which the wait future above still holds. The kill goes out
                // after the select, where the borrow has been released.
                _ = kill_rx.recv(), if !killing => killing = true,
            }
            let _ = child.start_kill();
        };
        // Let the terminal drain before reporting the exit, so the last prompt
        // or stderr line is not raced away by the exit event.
        let _ = tokio::time::timeout(READER_DRAIN_GRACE, drained_rx).await;
        let code = status.and_then(|s| s.code());
        let _ = event_tx.send(PtyEvent::Exited { code });
    });

    Ok(Spawned {
        session: PtySession {
            master: Arc::new(Mutex::new(master_writer)),
            pending_echo,
            kill: kill_tx,
        },
        events: event_rx,
        stdout,
        pid,
    })
}

/// Write all of `bytes`, retrying short writes and `EINTR`.
///
/// Matches `Write::write_all`'s semantics deliberately: a bare `Err(_) => break`
/// abandons the rest of the frame on an `EINTR` that a retry recovers from, and
/// half a JSON line on the terminal corrupts the stream for every frame after
/// it.
fn write_all(fd: BorrowedFd<'_>, bytes: &[u8]) {
    let mut off = 0;
    while off < bytes.len() {
        match nix::unistd::write(fd, &bytes[off..]) {
            // No progress and no error: treat the fd as closed.
            Ok(0) => break,
            Ok(n) => off += n,
            Err(nix::errno::Errno::EINTR) => continue,
            // Terminal (EPIPE, EBADF): the worker is gone.
            Err(_) => break,
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Live tests against a real pseudo-terminal and a real child.
///
/// The pure halves of this module (the echo queue, the DSR scan) are tested in
/// [`super`], and those tests would pass with the terminal wired up wrongly in
/// every way that matters. What is asserted here can only be asserted against an
/// actual `openpty`: that the child's stdin and stderr really are terminals,
/// that Core's own writes really do bounce back, and that the cancellation
/// really removes them from what the prompt parser will see.
///
/// `/bin/sh` stands in for `deno`. The pinned `deno` lives behind the
/// consent-gated download flow, so a test that needed it would skip on most
/// machines, and a skipping test is one that cannot fail (D-015).
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use tokio::io::AsyncReadExt as _;

    use super::*;

    /// Every test is time-bounded: a tripwire that hangs is not a tripwire, and
    /// a wedged PTY is exactly the failure these tests exist to catch.
    const BUDGET: Duration = Duration::from_secs(10);

    fn sh(script: &str) -> PtySpawn {
        PtySpawn {
            label: "pty test".to_string(),
            program: PathBuf::from("/bin/sh"),
            args: vec!["-c".to_string(), script.to_string()],
            env: BTreeMap::new(),
            cwd: None,
        }
    }

    /// Collect terminal output until `wanted` shows up or the child exits.
    async fn read_until(
        events: &mut mpsc::UnboundedReceiver<PtyEvent>,
        wanted: &str,
    ) -> (String, bool) {
        let mut seen = String::new();
        let found = tokio::time::timeout(BUDGET, async {
            while let Some(event) = events.recv().await {
                match event {
                    PtyEvent::Output(bytes) => {
                        seen.push_str(&String::from_utf8_lossy(&bytes));
                        if seen.contains(wanted) {
                            return true;
                        }
                    }
                    PtyEvent::Exited { .. } => return false,
                }
            }
            false
        })
        .await
        .unwrap_or(false);
        (seen, found)
    }

    async fn wait_for_exit(events: &mut mpsc::UnboundedReceiver<PtyEvent>) -> Option<Option<i32>> {
        tokio::time::timeout(BUDGET, async {
            while let Some(event) = events.recv().await {
                if let PtyEvent::Exited { code } = event {
                    return Some(code);
                }
            }
            None
        })
        .await
        .unwrap_or(None)
    }

    #[tokio::test]
    async fn the_child_gets_a_terminal_on_stdin_and_stderr_and_a_pipe_on_stdout() {
        // This is the entire reason the module exists. Deno shows its
        // interactive permission prompt only when stdin AND stderr are
        // terminals, and the byte-exact protocol needs stdout NOT to be one. Get
        // any of the three wrong and the failure is silent: prompts simply never
        // appear, and every ask-state permission becomes a NotCapable.
        let mut spawned = spawn(sh(concat!(
            "[ -t 0 ] && echo IN_TTY || echo IN_NOT_TTY;",
            "[ -t 1 ] && echo OUT_TTY || echo OUT_PIPE;",
            "[ -t 2 ] && echo ERR_TTY || echo ERR_NOT_TTY"
        )))
        .unwrap();

        let mut stdout = spawned.stdout.take().expect("unix spawns a stdout pipe");
        let mut text = String::new();
        tokio::time::timeout(BUDGET, stdout.read_to_string(&mut text))
            .await
            .expect("reading the protocol pipe timed out")
            .unwrap();

        assert!(text.contains("IN_TTY"), "stdin is not a terminal: {text:?}");
        assert!(
            text.contains("ERR_TTY"),
            "stderr is not a terminal: {text:?}"
        );
        assert!(
            text.contains("OUT_PIPE"),
            "stdout is a terminal, so the protocol would be reflowed: {text:?}"
        );
    }

    #[tokio::test]
    async fn a_core_write_is_cancelled_while_an_answer_and_real_output_are_not() {
        // Three assertions that only mean something together.
        //
        // The negative one ("Core's frames never reach the parser") is the one
        // that matters, and on its own it is exactly the assertion D-050 warns
        // about: it passes just as well if the reader is dead, if the terminal
        // echoes nothing, or if the events never arrive. So the same terminal
        // also has to show an ANSWER coming back (proving the echo is real and
        // the reader sees it) and the child's own output coming back (proving
        // the canceller is not simply eating everything).
        let mut spawned = spawn(sh("sleep 30")).unwrap();
        let events = &mut spawned.events;

        // Deliberately disjoint alphabets. A shared leading byte would let a
        // coincidental prefix match nibble the other string, which is a real
        // (documented, accepted) property of the canceller and would make this
        // test flaky rather than wrong.
        spawned.session.answer(b"AAAA\n");
        let (seen, found) = read_until(events, "AAAA").await;
        assert!(
            found,
            "an untracked answer did not echo back, so this terminal proves nothing: {seen:?}"
        );

        spawned.session.write(b"BBBB\n");
        spawned.session.answer(b"CCCC\n");
        let (seen, found) = read_until(events, "CCCC").await;
        assert!(found, "the terminal stopped producing output: {seen:?}");
        assert!(
            !seen.contains("BBBB"),
            "a protocol frame Core wrote came back as terminal output: {seen:?}"
        );

        spawned.session.kill();
        assert!(wait_for_exit(events).await.is_some());
    }

    #[tokio::test]
    async fn the_protocol_pipe_carries_bytes_the_terminal_never_touches() {
        // The unix arrangement's other half: what the worker writes to stdout is
        // the NDJSON protocol and must arrive byte-exact, not reflowed by a line
        // discipline and not mixed with the prompt.
        let line = r#"{"kind":"booted","extensionId":"demo"}"#;
        let mut spawned = spawn(sh(&format!("printf '%s\\n' '{line}'"))).unwrap();
        let mut stdout = spawned.stdout.take().unwrap();
        let mut text = String::new();
        tokio::time::timeout(BUDGET, stdout.read_to_string(&mut text))
            .await
            .expect("reading the protocol pipe timed out")
            .unwrap();
        assert_eq!(text, format!("{line}\n"));
    }

    #[tokio::test]
    async fn kill_stops_a_child_that_would_otherwise_outlive_the_test() {
        let mut spawned = spawn(sh("sleep 300")).unwrap();
        spawned.session.kill();
        let code = wait_for_exit(&mut spawned.events).await;
        assert!(code.is_some(), "no exit event arrived after kill");
        // Killed by a signal, so there is no exit code. Asserting the event
        // rather than the code is the point: the caller settles the call on the
        // event, and a kill that never produced one would hang every in-flight
        // call until the pool's timeout.
    }

    #[tokio::test]
    async fn a_normal_exit_reports_the_childs_code() {
        let mut spawned = spawn(sh("exit 3")).unwrap();
        assert_eq!(wait_for_exit(&mut spawned.events).await, Some(Some(3)));
    }

    #[tokio::test]
    async fn the_last_terminal_output_is_delivered_before_the_exit_event() {
        // Ordering, which holds here for a reason this test cannot see.
        //
        // **This does not cover the drain wait, and its name used to say it
        // did.** Removing `READER_DRAIN_GRACE` leaves this green: the events
        // share one channel, so once the reader thread has SENT the tail the
        // order is fixed whatever the waiter does, and on macOS the master EOFs
        // promptly enough that it always has. The wait matters where EOF lags
        // the exit (Linux reports EIO only when the last slave fd closes), and
        // no test at this layer can make that deterministic without a fake fd.
        //
        // Kept because the ordering itself is worth pinning, and recorded so a
        // later reader does not delete the wait on the evidence that nothing
        // fails.
        let mut spawned = spawn(sh("echo LAST_WORDS >&2; exit 0")).unwrap();
        let mut saw_output = false;
        let ordered = tokio::time::timeout(BUDGET, async {
            while let Some(event) = spawned.events.recv().await {
                match event {
                    PtyEvent::Output(bytes) => {
                        if String::from_utf8_lossy(&bytes).contains("LAST_WORDS") {
                            saw_output = true;
                        }
                    }
                    PtyEvent::Exited { .. } => return saw_output,
                }
            }
            false
        })
        .await
        .unwrap_or(false);
        assert!(ordered, "the exit event overtook the child's last output");
    }
}
