//! The unix PTY backend: `openpty`, the termios the prompt depends on, and the
//! echo cancellation that keeps Core's own frames out of the prompt stream.

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

/// How long the terminal reader gets to drain after the child exits, so the
/// line that explains the exit is not raced away by the exit event.
const READER_DRAIN_GRACE: Duration = Duration::from_secs(2);

/// A live terminal. Cheap to clone; every clone writes to the same master.
#[derive(Clone)]
pub struct PtySession {
    master: Arc<Mutex<OwnedFd>>,
    pending_echo: Arc<Mutex<VecDeque<u8>>>,
    kill: mpsc::UnboundedSender<()>,
}

impl PtySession {
    /// Write worker-protocol bytes, echo-tracked. Queued BEFORE the write so the
    /// reader thread cannot see the echo before it expects it.
    pub fn write(&self, bytes: &[u8]) {
        expect_echo(&self.pending_echo, bytes);
        self.write_raw(bytes);
    }

    /// Write a permission-prompt answer. NOT echo-tracked: Deno reads the answer
    /// with echo disabled, so tracking it would leave a stuck prefix in the
    /// queue and desync every later cancellation.
    pub fn answer(&self, bytes: &[u8]) {
        self.write_raw(bytes);
    }

    /// Returns immediately; the exit arrives as a [`PtyEvent::Exited`].
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

    // Raw (canonical mode's ~1 KB line limit would mangle a large frame) but
    // with ECHO on: Deno 2.8.x refuses to prompt when
    // `c_lflag & (ICANON | ECHO) == 0` (denoland/deno#34457).
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
        // Clear first, or every secret the core holds is visible to the worker.
        .env_clear()
        .envs(&spec.env)
        .stdin(Stdio::from(stdin_fd))
        .stdout(Stdio::piped())
        .stderr(Stdio::from(stderr_fd))
        .kill_on_drop(true);
    if let Some(cwd) = &spec.cwd {
        command.current_dir(cwd);
    }
    // SAFETY: setsid and the TIOCSCTTY ioctl are async-signal-safe. Both are
    // required: Deno's prompter reads the answer from its CONTROLLING terminal,
    // not fd 0.
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

    // Release every slave fd we hold, including the clones `command` still owns
    // via `Stdio::from`: on Linux a master read only reports EIO when the LAST
    // slave fd closes, so one lingering clone wedges the reader forever (macOS
    // masks this by delivering EOF on child exit regardless).
    drop(pty.slave);
    drop(command);

    let stdout = child.stdout.take();
    let pid = child.id();
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let (kill_tx, mut kill_rx) = mpsc::unbounded_channel::<()>();
    let pending_echo = Arc::new(Mutex::new(VecDeque::<u8>::new()));

    // A PTY master is not pollable through tokio without an AsyncFd
    // registration, so the read gets a dedicated thread.
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
                        // An all-echo read cancels to nothing.
                        if !out.is_empty() && reader_tx.send(PtyEvent::Output(out)).is_err() {
                            break;
                        }
                    }
                    // EIO: every slave fd is closed, so the child is gone.
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
                // The kill goes out after the select, once the wait future's
                // borrow of `child` is released.
                _ = kill_rx.recv(), if !killing => killing = true,
            }
            let _ = child.start_kill();
        };
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

/// Write all of `bytes`, retrying short writes and `EINTR`; half a JSON line
/// on the terminal corrupts every frame after it.
fn write_all(fd: BorrowedFd<'_>, bytes: &[u8]) {
    let mut off = 0;
    while off < bytes.len() {
        match nix::unistd::write(fd, &bytes[off..]) {
            Ok(0) => break,
            Ok(n) => off += n,
            Err(nix::errno::Errno::EINTR) => continue,
            Err(_) => break,
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Live tests against a real `openpty` and a real child. `/bin/sh` stands in
/// for `deno`, which is a consent-gated download most machines lack.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use tokio::io::AsyncReadExt as _;

    use super::*;

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
        // The negative assertion alone passes with a dead reader, so the same
        // terminal must also show an answer echoing back.
        let mut spawned = spawn(sh("sleep 30")).unwrap();
        let events = &mut spawned.events;

        // Disjoint alphabets, so a coincidental prefix match cannot nibble the other.
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
        // Killed by a signal, so there is no code; the caller settles on the event.
    }

    #[tokio::test]
    async fn a_normal_exit_reports_the_childs_code() {
        let mut spawned = spawn(sh("exit 3")).unwrap();
        assert_eq!(wait_for_exit(&mut spawned.events).await, Some(Some(3)));
    }

    #[tokio::test]
    async fn the_last_terminal_output_is_delivered_before_the_exit_event() {
        // Does NOT cover `READER_DRAIN_GRACE`: on macOS the master EOFs before
        // the exit regardless. The wait matters on Linux, where EIO lags the
        // exit; do not delete it on the evidence that this stays green.
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
