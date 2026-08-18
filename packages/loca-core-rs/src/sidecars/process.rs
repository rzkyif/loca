//! Spawning a child, draining its output, and killing it without leaking it.
//!
//! Every child the core starts goes through here, so the four properties below
//! hold once rather than per caller.

use std::collections::{BTreeMap, VecDeque};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::process::{ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::task::JoinHandle;

use crate::error::{internal, Result};

use super::jobctl::JobObject;

/// SIGTERM to SIGKILL, matching the Deno core's window.
const GRACEFUL_SHUTDOWN: Duration = Duration::from_secs(5);
/// How long SIGKILL gets before we give up and let the drop backstop have it. A
/// process wedged in an uninterruptible syscall can outlast even this.
const KILL_GRACE: Duration = Duration::from_secs(2);
/// How long the output pumps get to finish after the process is gone.
const DRAIN_GRACE: Duration = Duration::from_secs(2);
/// Lines kept for the exit message.
const RECENT_LOG_LINES: usize = 10;
/// A child that emits no newline must not grow the core's buffer without bound.
/// Far above any real log line; a caller parsing a protocol off the pipe takes
/// [`Output::Piped`] and applies its own limit.
pub(super) const MAX_LINE_BYTES: usize = 1_000_000;

/// Environment a child may legitimately need: a search path, a home, a temp
/// dir, a locale.
///
/// It is an allowlist because the core process can hold an operator's
/// `GITHUB_TOKEN` or `OPENAI_API_KEY`, and a downloaded binary or a sandboxed
/// tool has no business seeing either. The keys stay the caller's choice
/// (D12's worker list is deliberately narrower and adds `DENO_DIR`); what is
/// shared is the clear-then-add-exactly-these mechanism in [`Spawn`].
pub const OPERATIONAL_ENV_KEYS: &[&str] = &[
    "PATH",
    "HOME",
    "TMPDIR",
    "TMP",
    "TEMP",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TERM",
    "USER",
    "LOGNAME",
    // Windows essentials.
    "SystemRoot",
    "SYSTEMROOT",
    "windir",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "ProgramData",
    "ProgramFiles",
    "ComSpec",
    "PATHEXT",
    "NUMBER_OF_PROCESSORS",
];

/// The named keys, read from this process, for seeding a [`Spawn`].
pub fn inherited(keys: &[&str]) -> BTreeMap<OsString, OsString> {
    keys.iter()
        .filter_map(|key| std::env::var_os(key).map(|value| (OsString::from(key), value)))
        .collect()
}

/// What the child's stdin is connected to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stdin {
    /// `/dev/null`. A child that reads stdin sees EOF.
    Null,
    /// A pipe the caller takes with [`Child::take_stdin`].
    Piped,
}

/// What happens to one of the child's output streams.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Output {
    /// Drained by this module into the ring buffer that becomes the exit
    /// message, and logged at DEBUG.
    Relay,
    /// Handed to the caller with [`Child::take_stdout`] / [`Child::take_stderr`].
    /// The caller then owns the draining, and owns not deadlocking.
    Piped,
}

/// A child to start. Everything is explicit: nothing is inherited that is not
/// named here.
pub struct Spawn {
    label: String,
    program: PathBuf,
    args: Vec<OsString>,
    env: BTreeMap<OsString, OsString>,
    cwd: Option<PathBuf>,
    stdin: Stdin,
    stdout: Output,
    stderr: Output,
}

impl Spawn {
    /// `label` is what log lines and the exit message are tagged with, so make
    /// it something a reader can act on (`"tool worker weather"`, not `"child"`).
    pub fn new(label: impl Into<String>, program: impl Into<PathBuf>) -> Self {
        Self {
            label: label.into(),
            program: program.into(),
            args: Vec::new(),
            env: BTreeMap::new(),
            cwd: None,
            stdin: Stdin::Null,
            stdout: Output::Relay,
            stderr: Output::Relay,
        }
    }

    pub fn arg(mut self, arg: impl AsRef<OsStr>) -> Self {
        self.args.push(arg.as_ref().to_os_string());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.args
            .extend(args.into_iter().map(|a| a.as_ref().to_os_string()));
        self
    }

    pub fn env(mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> Self {
        self.env
            .insert(key.as_ref().to_os_string(), value.as_ref().to_os_string());
        self
    }

    pub fn envs(mut self, env: BTreeMap<OsString, OsString>) -> Self {
        self.env.extend(env);
        self
    }

    pub fn cwd(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cwd = Some(dir.into());
        self
    }

    pub fn stdin(mut self, mode: Stdin) -> Self {
        self.stdin = mode;
        self
    }

    pub fn stdout(mut self, mode: Output) -> Self {
        self.stdout = mode;
        self
    }

    pub fn stderr(mut self, mode: Output) -> Self {
        self.stderr = mode;
        self
    }

    pub fn program(&self) -> &Path {
        &self.program
    }
}

/// How a child ended.
#[derive(Clone, Debug)]
pub struct Exit {
    pub code: Option<i32>,
    /// Unix only. `None` on Windows and when the child exited normally.
    pub signal: Option<i32>,
    /// What to show a user: the child's last output if it produced any, and the
    /// exit status if it went quietly.
    pub message: String,
}

/// A running child.
///
/// Dropping it kills the process (`kill_on_drop`), which is the backstop for a
/// task that panics between spawn and terminate. The graceful path is
/// [`Child::terminate`].
pub struct Child {
    label: String,
    pid: u32,
    /// `Some` until the child is reaped. Every signal is sent while this is
    /// held, which is what makes the pid safe to use: an un-awaited child is a
    /// zombie at worst, and its pid cannot be reused until it is reaped.
    child: Option<tokio::process::Child>,
    stdin: Option<ChildStdin>,
    stdout: Option<ChildStdout>,
    stderr: Option<ChildStderr>,
    drains: Vec<JoinHandle<()>>,
    recent: Arc<Mutex<VecDeque<String>>>,
    exit: Option<Exit>,
}

impl Child {
    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    /// `Some` once, and only when the spec asked for [`Stdin::Piped`].
    pub fn take_stdin(&mut self) -> Option<ChildStdin> {
        self.stdin.take()
    }

    /// `Some` once, and only when the spec asked for [`Output::Piped`].
    pub fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.stdout.take()
    }

    /// `Some` once, and only when the spec asked for [`Output::Piped`].
    pub fn take_stderr(&mut self) -> Option<ChildStderr> {
        self.stderr.take()
    }

    /// The last few relayed lines, oldest first.
    pub fn recent_output(&self) -> Vec<String> {
        lock(&self.recent).iter().cloned().collect()
    }

    /// Wait for the child to exit on its own. Idempotent: the exit is cached,
    /// so a second call returns the same answer rather than failing.
    pub async fn wait(&mut self) -> Result<Exit> {
        if let Some(exit) = &self.exit {
            return Ok(exit.clone());
        }
        let Some(mut child) = self.child.take() else {
            return Err(internal(format!("{}: child handle is gone", self.label)));
        };
        let status = child.wait().await?;
        Ok(self.settle(status).await)
    }

    /// Stop the child: SIGTERM, a grace window, then SIGKILL. Idempotent.
    ///
    /// On Windows there is no SIGTERM, so this is a TerminateProcess straight
    /// away, exactly as the Deno core did.
    pub async fn terminate(&mut self) -> Exit {
        if let Some(exit) = &self.exit {
            return exit.clone();
        }
        let Some(mut child) = self.child.take() else {
            return self.settle_missing().await;
        };

        // The observed exit, if we manage to reap it. Threaded through both
        // branches rather than returned from one: dropping it on the graceful
        // path would report a clean shutdown as an exit of unknown cause.
        let mut reaped = None;

        let escalated = match soft_signal(&mut child, self.pid) {
            // Windows, or a signal that could not be sent because the process
            // is already gone. Either way there is nothing to escalate to.
            SoftSignal::Terminal => true,
            SoftSignal::Sent => match tokio::time::timeout(GRACEFUL_SHUTDOWN, child.wait()).await {
                Ok(status) => {
                    reaped = status.ok();
                    false
                }
                Err(_) => {
                    tracing::warn!(
                        target: "loca::sidecars",
                        "{}: ignored SIGTERM after {}s, killing (pid {})",
                        self.label,
                        GRACEFUL_SHUTDOWN.as_secs(),
                        self.pid,
                    );
                    let _ = child.start_kill();
                    true
                }
            },
        };

        if escalated {
            match tokio::time::timeout(KILL_GRACE, child.wait()).await {
                Ok(status) => reaped = status.ok(),
                Err(_) => {
                    // Uninterruptible sleep, or a Windows handle that will not
                    // close. The drop backstop owns it from here; blocking
                    // shutdown on it would be worse than leaking it.
                    tracing::warn!(
                        target: "loca::sidecars",
                        "{}: still alive after SIGKILL (pid {}); abandoning it",
                        self.label,
                        self.pid,
                    );
                }
            }
        }

        match reaped {
            Some(status) => self.settle(status).await,
            None => self.settle_missing().await,
        }
    }

    /// Compose the exit, after the output pumps have finished.
    ///
    /// The join is the point. The pipes can still hold the last thing the child
    /// wrote when `wait` returns, and that last line is usually the panic that
    /// explains the exit. Reading the ring first would drop it.
    async fn settle(&mut self, status: std::process::ExitStatus) -> Exit {
        self.drain_out().await;
        let recent = self.recent_output();
        let code = status.code();
        let signal = exit_signal(&status);
        let message = if recent.is_empty() {
            match signal {
                Some(signal) => format!("exited with signal {signal}"),
                None => format!("exited with code {}", code.unwrap_or(-1)),
            }
        } else {
            recent.join("\n")
        };
        let exit = Exit {
            code,
            signal,
            message,
        };
        self.exit = Some(exit.clone());
        exit
    }

    /// The exit we could not observe: the handle was already consumed, or the
    /// process outlived SIGKILL.
    async fn settle_missing(&mut self) -> Exit {
        self.drain_out().await;
        let recent = self.recent_output();
        let exit = Exit {
            code: None,
            signal: None,
            message: if recent.is_empty() {
                "exited".to_string()
            } else {
                recent.join("\n")
            },
        };
        self.exit = Some(exit.clone());
        exit
    }

    /// Let the output pumps finish, then stop waiting on them.
    ///
    /// One budget for all of them, not one each: a child's stdout and stderr
    /// end at the same moment, so serialising the waits would multiply the
    /// worst case by the number of pipes and then by the number of children at
    /// shutdown.
    ///
    /// The abort is not decoration. A grandchild that inherited the pipes keeps
    /// them open after the child is gone, so EOF never arrives and the pump
    /// would otherwise sit in a live task for the rest of the process's life.
    /// Dropping a `JoinHandle` does not cancel the task; only `abort` does.
    async fn drain_out(&mut self) {
        let handles = std::mem::take(&mut self.drains);
        if handles.is_empty() {
            return;
        }
        let aborts: Vec<_> = handles.iter().map(|h| h.abort_handle()).collect();
        let all = futures_util::future::join_all(handles);
        if tokio::time::timeout(DRAIN_GRACE, all).await.is_err() {
            tracing::warn!(
                target: "loca::sidecars",
                "{}: output pipes still open {}s after exit (a surviving grandchild holds them); dropping the pumps",
                self.label,
                DRAIN_GRACE.as_secs(),
            );
            for abort in aborts {
                abort.abort();
            }
        }
    }
}

enum SoftSignal {
    /// A graceful signal is on its way; escalate if it is ignored.
    Sent,
    /// Nothing gentler than a kill was available or possible.
    Terminal,
}

#[cfg(unix)]
fn soft_signal(child: &mut tokio::process::Child, pid: u32) -> SoftSignal {
    use nix::sys::signal::{kill, Signal};
    use nix::unistd::Pid;

    // Signalling the pid rather than the process group is deliberate, and the
    // reverse of what the textbook says. Orphan cleanup on unix is delegated to
    // the service supervisor: systemd kills the unit's cgroup and launchd kills
    // THE JOB'S PROCESS GROUP. Putting a child in its own group with
    // `Command::process_group(0)` would let us kill its whole tree, and would
    // defeat launchd's cleanup exactly, on the platform most users run. So
    // children stay in the core's group, and a grandchild (a `run`-granted
    // tool's own subprocess) is not covered by this signal. That gap is
    // inherited from the Deno core rather than introduced here; Windows covers
    // it with the Job Object.
    //
    // The pid is safe to use because `child` is un-awaited: an exited child is
    // a zombie until reaped, and a zombie's pid cannot be reused. Holding the
    // borrow is what makes that a compiler-checked claim rather than a comment.
    let _held = child;
    match kill(Pid::from_raw(pid as i32), Signal::SIGTERM) {
        Ok(()) => SoftSignal::Sent,
        // ESRCH: already gone. Anything else is not going to improve with a
        // second attempt either.
        Err(_) => SoftSignal::Terminal,
    }
}

#[cfg(not(unix))]
fn soft_signal(child: &mut tokio::process::Child, _pid: u32) -> SoftSignal {
    // Windows has no SIGTERM. `start_kill` is TerminateProcess, which is what
    // the Deno core sent there too.
    let _ = child.start_kill();
    SoftSignal::Terminal
}

#[cfg(unix)]
fn exit_signal(status: &std::process::ExitStatus) -> Option<i32> {
    std::os::unix::process::ExitStatusExt::signal(status)
}

#[cfg(not(unix))]
fn exit_signal(_status: &std::process::ExitStatus) -> Option<i32> {
    None
}

/// Start the child described by `spec`, tracked by `jobs`.
///
/// Not public: every spawn goes through [`super::Manager::spawn`], so the
/// Windows Job Object membership and the shutdown registration cannot be
/// forgotten at a call site.
pub(super) fn spawn(spec: Spawn, jobs: &JobObject) -> Result<Child> {
    let mut cmd = Command::new(&spec.program);
    cmd.args(&spec.args);
    // Clear first. `.envs()` on an inherited environment would leave every
    // secret the core holds visible to the child, and the allowlist would be
    // decoration rather than a boundary.
    cmd.env_clear();
    cmd.envs(&spec.env);
    if let Some(cwd) = &spec.cwd {
        cmd.current_dir(cwd);
    }
    cmd.stdin(match spec.stdin {
        Stdin::Null => Stdio::null(),
        Stdin::Piped => Stdio::piped(),
    });
    // Both output modes need a pipe; they differ only in who reads it.
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    // The backstop for a task that panics between spawn and terminate. The
    // graceful path always reaps before dropping, so this only ever fires on
    // the abnormal one.
    cmd.kill_on_drop(true);
    #[cfg(windows)]
    {
        // A console-subsystem child with no console to inherit ALLOCATES ONE,
        // and its window is visible. The Deno core could not set this flag
        // (`Deno.Command` has no such option) and worked around it by
        // allocating a console for the core itself and hiding the window with
        // user32 FFI. Setting the flag per child is the fix that hack stood in
        // for, so the hack is not ported.
        use std::os::windows::process::CommandExt as _;
        cmd.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
    }

    let mut child = cmd.spawn().map_err(|e| {
        internal(format!(
            "{}: could not spawn {}: {e}",
            spec.label,
            spec.program.display()
        ))
    })?;
    let pid = child.id().ok_or_else(|| {
        internal(format!(
            "{}: child exited before it was tracked",
            spec.label
        ))
    })?;

    // Windows only: assign to the kill-on-close job so a hard core kill cannot
    // leave the child running.
    jobs.track(pid, &spec.label);

    tracing::info!(
        target: "loca::sidecars",
        "{}: started (pid {}): {}",
        spec.label,
        pid,
        spec.program.display(),
    );

    let recent = Arc::new(Mutex::new(VecDeque::with_capacity(RECENT_LOG_LINES)));
    let mut drains = Vec::new();
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    if spec.stdout == Output::Relay {
        if let Some(pipe) = stdout.take() {
            drains.push(relay(pipe, spec.label.clone(), "stdout", recent.clone()));
        }
    }
    if spec.stderr == Output::Relay {
        if let Some(pipe) = stderr.take() {
            drains.push(relay(pipe, spec.label.clone(), "stderr", recent.clone()));
        }
    }

    Ok(Child {
        label: spec.label,
        pid,
        stdin: child.stdin.take(),
        child: Some(child),
        stdout,
        stderr,
        drains,
        recent,
        exit: None,
    })
}

/// Pump one output stream into the ring buffer and the log.
///
/// Spawned immediately, before anything awaits the child, because an undrained
/// pipe fills at ~64 KB and then blocks the child's next write forever. The
/// child never exits, so `wait` never returns, and the core looks hung with
/// nothing in the log to say why.
fn relay<R>(
    mut reader: R,
    label: String,
    tag: &'static str,
    recent: Arc<Mutex<VecDeque<String>>>,
) -> JoinHandle<()>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut chunk = [0u8; 8192];
        let mut pending: Vec<u8> = Vec::new();
        // Set when `pending` hit the cap: the rest of that line is discarded
        // rather than buffered, so a child emitting megabytes without a newline
        // costs us one capped line and not the heap.
        let mut overflowed = false;
        loop {
            let n = match reader.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            for &byte in &chunk[..n] {
                if byte == b'\n' {
                    emit(&label, tag, &pending, overflowed, &recent);
                    pending.clear();
                    overflowed = false;
                } else if pending.len() < MAX_LINE_BYTES {
                    pending.push(byte);
                } else {
                    overflowed = true;
                }
            }
        }
        // A final line with no trailing newline is still a line, and on a crash
        // it is often the only one that matters.
        if !pending.is_empty() {
            emit(&label, tag, &pending, overflowed, &recent);
        }
    })
}

fn emit(
    label: &str,
    tag: &'static str,
    line: &[u8],
    overflowed: bool,
    recent: &Arc<Mutex<VecDeque<String>>>,
) {
    let text = String::from_utf8_lossy(line);
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    let text = if overflowed {
        format!("{text} [truncated]")
    } else {
        text.to_string()
    };
    // DEBUG, which `log.rs` drops for this scope: a child's chatter would
    // drown the console and fill core.log. Crash context is not lost, because
    // the ring below becomes the exit message, which is logged at WARN by
    // whoever owns the child.
    tracing::debug!(target: "loca::sidecars", "[{label}/{tag}] {text}");
    let mut ring = lock(recent);
    if ring.len() == RECENT_LOG_LINES {
        ring.pop_front();
    }
    ring.push_back(text);
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}
