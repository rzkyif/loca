//! loca-core-ptyhost: terminal plumbing between loca-core and a Deno tool
//! worker. Deno only shows its interactive permission prompt when the
//! worker's stdin AND stderr are terminals, so this helper allocates a
//! pseudo-terminal (a unix PTY in `mod unix`, a ConPTY in `mod windows`),
//! attaches the worker's stdin + stderr to it, and bridges the master side to
//! the core over a small NDJSON protocol. ptyhost never writes to its own
//! stdout.
//!
//! The two backends differ only in how the worker protocol reaches core:
//!   - unix: the worker's stdout is inherited, so the core<->worker NDJSON
//!     stream passes through this process untouched (see the ECHO note below).
//!   - windows: a ConPTY merges and reflows the child's stdout, so the protocol
//!     cannot ride it. The worker connects back to core over a per-worker
//!     loopback socket (core's control-socket.ts) and speaks the protocol
//!     there; the ConPTY carries only the prompt. No echo cancellation is
//!     needed since nothing byte-exact flows through the pseudoconsole.
//!
//! Control frames (core -> ptyhost, one JSON object per line on stdin):
//!   {"kind":"spawn","cmd":"/path/to/deno","args":[...],"env":{...},"cwd":null}
//!       Must be the first frame, exactly once. The child's environment is
//!       exactly `env` (nothing inherited).
//!   {"kind":"write","dataB64":"..."}
//!       Worker-protocol frames for the PTY master. These echo back off the
//!       slave (ECHO is on, see below); the reader cancels that echo.
//!   {"kind":"answer","dataB64":"..."}
//!       A permission-prompt answer for the PTY master. Identical to `write`
//!       except it is NOT echo-tracked: Deno reads the answer with echo off,
//!       so it never bounces back and must not seed the echo-cancel queue.
//!   {"kind":"kill"}
//!       SIGKILL the child.
//!
//! Events (ptyhost -> core, one JSON object per line on stderr):
//!   {"kind":"pty","dataB64":"..."}   PTY master output (worker stderr +
//!                                    Deno prompt text; never stdout).
//!   {"kind":"exit","code":N}         Child exited; ptyhost then exits with
//!                                    the same code.
//!   {"kind":"fatal","error":"..."}   Allocation/spawn failure; ptyhost exits.
//!
//! On stdin EOF (core died) the child is SIGKILLed and ptyhost exits. The PTY
//! slave runs with no canonical line limit and no output post-processing so
//! large protocol frames survive the trip, but ECHO stays on: Deno 2.8.3 refuses
//! to prompt when stdin is in raw mode (ICANON and ECHO both off), so disabling
//! echo would silence permission prompts. The master reader cancels the echo of
//! everything written via `write` so the core never sees its own frames.

use std::io::Write;
use std::sync::Mutex;

#[cfg(any(unix, windows, test))]
use base64::engine::general_purpose::STANDARD as B64;
#[cfg(any(unix, windows, test))]
use base64::Engine;
#[cfg(any(unix, windows, test))]
use serde::Deserialize;

#[cfg(any(unix, windows, test))]
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ControlFrame {
    Spawn {
        cmd: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: std::collections::HashMap<String, String>,
        #[serde(default)]
        cwd: Option<String>,
    },
    /// Worker-protocol bytes for the master; echo-tracked so the slave's
    /// bounce-back is cancelled before it reaches the core.
    Write {
        #[serde(rename = "dataB64")]
        data_b64: String,
    },
    /// Permission-prompt answer for the master; NOT echo-tracked (Deno reads it
    /// with echo off, so it never bounces back).
    Answer {
        #[serde(rename = "dataB64")]
        data_b64: String,
    },
    Kill,
}

/// Serializes event lines so concurrent threads (PTY reader, child waiter,
/// control loop) never interleave partial lines on stderr.
static EVENT_LOCK: Mutex<()> = Mutex::new(());

fn emit_event(json: String) {
    let guard = EVENT_LOCK.lock();
    let mut err = std::io::stderr().lock();
    let _ = err.write_all(json.as_bytes());
    let _ = err.write_all(b"\n");
    let _ = err.flush();
    drop(guard);
}

#[cfg(any(unix, windows))]
fn emit_pty(data: &[u8]) {
    emit_event(format!(
        "{{\"kind\":\"pty\",\"dataB64\":\"{}\"}}",
        B64.encode(data)
    ));
}

#[cfg(any(unix, windows))]
fn emit_exit(code: i32) {
    emit_event(format!("{{\"kind\":\"exit\",\"code\":{code}}}"));
}

// Cap the outstanding-echo queue. A byte that never echoes back (e.g. the slave
// briefly disabled ECHO) would otherwise wedge the queue and swallow real output
// forever; past this bound we assume desync, drop the backlog, and let any stray
// echo through as harmless stderr noise. Comfortably above one protocol frame.
#[cfg(unix)]
const PENDING_ECHO_CAP: usize = 4 * 1024 * 1024;

/// Record bytes written to the master that the slave's ECHO will bounce back,
/// so the reader can drop them. Clears the backlog on overflow (desync guard).
#[cfg(unix)]
fn expect_echo(pending: &Mutex<std::collections::VecDeque<u8>>, bytes: &[u8]) {
    if let Ok(mut q) = pending.lock() {
        if q.len() + bytes.len() > PENDING_ECHO_CAP {
            q.clear();
            return;
        }
        q.extend(bytes.iter().copied());
    }
}

/// Strip leading echoed bytes from a master read: each byte that matches the
/// head of `pending` is its own echo and is dropped; the rest is genuine child
/// output (prompt text or stderr). Echoed frames bounce back contiguously, so a
/// running prefix match cancels them; a coincidental match only ever drops a
/// byte of a stderr log line, never protocol output (which rides stdout).
#[cfg(unix)]
fn cancel_echo(pending: &Mutex<std::collections::VecDeque<u8>>, chunk: &[u8]) -> Vec<u8> {
    let Ok(mut q) = pending.lock() else {
        return chunk.to_vec();
    };
    let mut out = Vec::with_capacity(chunk.len());
    for &b in chunk {
        if q.front() == Some(&b) {
            q.pop_front();
        } else {
            out.push(b);
        }
    }
    out
}

/// Write all of `bytes` to `fd`, retrying short writes and the spurious `EINTR`
/// (a signal interrupting the syscall), giving up only on genuinely terminal
/// errors (EPIPE, EBADF, ...). Matches std's `Write::write_all` semantics: a
/// bare `Err(_) => break` would abandon the rest of the frame on an EINTR that a
/// simple retry recovers from, corrupting the pty stream.
#[cfg(unix)]
fn write_all(fd: std::os::fd::BorrowedFd<'_>, bytes: &[u8]) {
    let mut off = 0;
    while off < bytes.len() {
        match nix::unistd::write(fd, &bytes[off..]) {
            Ok(0) => break, // no progress and no error: treat the fd as closed
            Ok(n) => off += n,
            Err(nix::errno::Errno::EINTR) => continue, // interrupted; retry
            Err(_) => break,                           // terminal error: give up on the frame
        }
    }
}

fn emit_fatal(error: &str) -> ! {
    // serde_json::to_string on a &str cannot fail; fall back to a fixed
    // message rather than unwrapping.
    let quoted = serde_json::to_string(error).unwrap_or_else(|_| "\"fatal\"".to_string());
    emit_event(format!("{{\"kind\":\"fatal\",\"error\":{quoted}}}"));
    std::process::exit(70);
}

#[cfg(unix)]
fn main() {
    unix::run();
}

#[cfg(windows)]
fn main() {
    windows::run();
}

#[cfg(not(any(unix, windows)))]
fn main() {
    // No PTY backend for this platform; exit with a distinctive code so the
    // core's legacy --no-prompt spawn engages.
    emit_fatal("ptyhost is not supported on this platform");
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::io::BufRead;
    use std::os::fd::{AsFd, AsRawFd};
    use std::os::unix::process::CommandExt;
    use std::sync::Arc;

    use nix::pty::openpty;
    use nix::sys::signal::{kill, Signal};
    use nix::sys::termios::{cfmakeraw, tcgetattr, tcsetattr, SetArg};
    use nix::unistd::Pid;

    pub fn run() {
        let stdin = std::io::stdin();
        let mut lines = stdin.lock().lines();

        // First frame must be spawn.
        let first = match lines.next() {
            Some(Ok(line)) => line,
            _ => return, // EOF before spawn: nothing to do.
        };
        let spawn = match serde_json::from_str::<ControlFrame>(&first) {
            Ok(f @ ControlFrame::Spawn { .. }) => f,
            Ok(_) => emit_fatal("first control frame must be spawn"),
            Err(e) => emit_fatal(&format!("bad control frame: {e}")),
        };
        let ControlFrame::Spawn {
            cmd,
            args,
            env,
            cwd,
        } = spawn
        else {
            unreachable!()
        };

        let pty = match openpty(None, None) {
            Ok(p) => p,
            Err(e) => emit_fatal(&format!("openpty failed: {e}")),
        };

        // Slave line discipline: no canonical mode (its ~1 KB line limit would
        // mangle large protocol frames) and no output post-processing, but ECHO
        // stays ON. Deno 2.8.3 refuses to show a permission prompt when stdin is
        // in raw mode, which it defines as `c_lflag & (ICANON | ECHO) == 0` (see
        // denoland/deno#34457); leaving ECHO set keeps prompts working. The cost
        // is that every byte written to the master is echoed back into it; the
        // PTY reader below cancels that echo so it never reaches the core.
        match tcgetattr(pty.slave.as_fd()) {
            Ok(mut t) => {
                use nix::sys::termios::LocalFlags;
                cfmakeraw(&mut t);
                t.local_flags.insert(LocalFlags::ECHO);
                if let Err(e) = tcsetattr(pty.slave.as_fd(), SetArg::TCSANOW, &t) {
                    emit_fatal(&format!("tcsetattr failed: {e}"));
                }
            }
            Err(e) => emit_fatal(&format!("tcgetattr failed: {e}")),
        }

        let (stdin_fd, stderr_fd) = match (pty.slave.try_clone(), pty.slave.try_clone()) {
            (Ok(a), Ok(b)) => (a, b),
            _ => emit_fatal("failed to clone pty slave fd"),
        };

        let slave_raw = pty.slave.as_raw_fd();
        let mut command = std::process::Command::new(&cmd);
        command
            .args(&args)
            .env_clear()
            .envs(&env)
            .stdin(std::process::Stdio::from(stdin_fd))
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::from(stderr_fd));
        if let Some(dir) = &cwd {
            command.current_dir(dir);
        }
        // SAFETY: setsid + TIOCSCTTY are async-signal-safe; required so the
        // PTY becomes the child's controlling terminal (Deno's prompt reads
        // the answer from the terminal it controls).
        unsafe {
            command.pre_exec(move || {
                if nix::libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if nix::libc::ioctl(slave_raw, nix::libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }

        let mut child = match command.spawn() {
            Ok(c) => c,
            Err(e) => emit_fatal(&format!("spawn failed: {e}")),
        };
        let child_pid = Pid::from_raw(child.id() as i32);
        // Guard the child PID so a control-frame kill can't fire AFTER the waiter
        // has reaped the child (whose PID the kernel may have recycled into an
        // unrelated process). The waiter sets it to None on reap; every kill site
        // takes the lock and skips when None.
        let live_pid = Arc::new(Mutex::new(Some(child_pid)));
        let waiter_pid = Arc::clone(&live_pid);
        let kill_live = || {
            if let Ok(guard) = live_pid.lock() {
                if let Some(p) = *guard {
                    let _ = kill(p, Signal::SIGKILL);
                }
            }
        };
        // Release every slave fd we still hold so the master read errors out
        // (EIO) once the child's copies close, ending the reader thread. Both our
        // own `pty.slave` AND the two slave clones that `command` still owns via
        // `Stdio::from` (spawn dup2s them into the child but does not consume the
        // parent handles) must go: on Linux a master read only reports EIO when
        // the LAST slave fd closes, so one lingering clone wedges the reader (and
        // thus ptyhost's exit) forever after the child is gone. macOS masks this
        // by delivering EOF on child exit regardless, so it only bites on Linux.
        drop(pty.slave);
        drop(command);

        let master_writer = match pty.master.try_clone() {
            Ok(fd) => fd,
            Err(_) => emit_fatal("failed to clone pty master fd"),
        };

        // Echo cancellation: the slave echoes every byte written to the master
        // (ECHO is on so Deno still prompts), so each protocol frame the core
        // writes bounces straight back. `pending_echo` holds the bytes still
        // owed an echo; the reader drops them so the core never sees its own
        // frames. Only protocol writes are tracked here: prompt answers go out
        // the `answer` frame, which Deno reads with echo disabled, so tracking
        // them would leave a stuck prefix and desync the queue.
        let pending_echo = Arc::new(Mutex::new(std::collections::VecDeque::<u8>::new()));
        let reader_echo = Arc::clone(&pending_echo);

        // PTY master -> core: chunked, base64-wrapped events, echo stripped.
        let reader = std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match nix::unistd::read(pty.master.as_fd(), &mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let out = cancel_echo(&reader_echo, &buf[..n]);
                        if !out.is_empty() {
                            emit_pty(&out);
                        }
                    }
                    // EIO: every slave fd closed (child exited). Anything
                    // else is equally terminal for the bridge.
                    Err(_) => break,
                }
            }
        });

        // Child exit -> drain the reader, report, and mirror the exit code.
        // Exits the whole process: ptyhost must not outlive its child (the
        // core awaits this process's status as the worker's status).
        let waiter = std::thread::spawn(move || {
            let code = match child.wait() {
                Ok(status) =>
                {
                    #[allow(clippy::unwrap_used, reason = "signal() is Some when code() is None")]
                    status.code().unwrap_or_else(|| {
                        128 + std::os::unix::process::ExitStatusExt::signal(&status).unwrap_or(9)
                    })
                }
                Err(_) => 70,
            };
            // Child reaped: forbid any further signal to its now-recyclable PID.
            if let Ok(mut guard) = waiter_pid.lock() {
                *guard = None;
            }
            let _ = reader.join();
            emit_exit(code);
            std::process::exit(code);
        });

        // Control loop: forward writes to the master, honor kill, kill on EOF.
        for line in lines {
            let Ok(line) = line else { break };
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<ControlFrame>(&line) {
                Ok(ControlFrame::Write { data_b64 }) => {
                    let Ok(bytes) = B64.decode(data_b64) else {
                        continue;
                    };
                    // Expect this frame to echo back; queue it (before the write,
                    // so the reader can't race ahead) so the reader can cancel it.
                    expect_echo(&pending_echo, &bytes);
                    write_all(master_writer.as_fd(), &bytes);
                }
                Ok(ControlFrame::Answer { data_b64 }) => {
                    let Ok(bytes) = B64.decode(data_b64) else {
                        continue;
                    };
                    // Prompt answer: Deno reads it with echo off, so it does not
                    // bounce back. Write straight through without echo tracking.
                    write_all(master_writer.as_fd(), &bytes);
                }
                Ok(ControlFrame::Kill) => {
                    kill_live();
                }
                Ok(ControlFrame::Spawn { .. }) => {
                    // One child per ptyhost; a second spawn is a core bug. SIGKILL
                    // the child (the kernel delivers it even as we exit) and emit
                    // the documented fatal frame. emit_fatal diverges, so the
                    // waiter thread is abandoned as the process exits.
                    kill_live();
                    emit_fatal("duplicate spawn frame");
                }
                Err(_) => continue, // tolerate garbage; the core logs its own errors
            }
        }

        // stdin EOF or read error: the core is gone, take the child with us.
        // The waiter thread exits the process once the child is reaped.
        kill_live();
        let _ = waiter.join();
        std::process::exit(70);
    }
}

// The DSR cursor-position query (ESC [ 6 n). portable-pty creates the
// pseudoconsole with PSEUDOCONSOLE_INHERIT_CURSOR, which makes conhost emit
// this query at session start and HOLD ALL CLIENT OUTPUT until the hosting
// terminal answers with a cursor position report (ESC [ row ; col R). A real
// terminal answers it automatically; here ptyhost is the terminal, so the
// reader thread answers each query it sees. Without the reply the whole
// session stalls before the child produces a single byte.
#[cfg(any(windows, test))]
const DSR_QUERY: &[u8] = b"\x1b[6n";

/// Count DSR cursor-position queries in `tail ++ chunk` and return the new
/// tail (the last `DSR_QUERY.len() - 1` bytes) to carry to the next chunk, so
/// a query split across reads is still seen. `tail` is shorter than one query,
/// so a counted match always ends in `chunk`: no double counting.
#[cfg(any(windows, test))]
fn scan_dsr_queries(tail: &[u8], chunk: &[u8]) -> (usize, Vec<u8>) {
    let mut window = Vec::with_capacity(tail.len() + chunk.len());
    window.extend_from_slice(tail);
    window.extend_from_slice(chunk);
    let count = window
        .windows(DSR_QUERY.len())
        .filter(|w| *w == DSR_QUERY)
        .count();
    let keep = window.len().min(DSR_QUERY.len() - 1);
    let new_tail = window[window.len() - keep..].to_vec();
    (count, new_tail)
}

#[cfg(windows)]
mod windows {
    //! ConPTY backend. Unlike the unix path there is no termios and no echo
    //! cancellation to do: the worker protocol rides a loopback socket (see
    //! core's control-socket.ts), so nothing byte-exact flows through the
    //! pseudoconsole. The ConPTY exists only so Deno sees stdin + stderr as
    //! console handles and shows its interactive permission prompt; we stream
    //! that prompt text out as `pty` events and write the y/n answer back in.
    //!
    //! portable-pty wraps CreatePseudoConsole + the STARTUPINFOEX spawn. It
    //! attaches all of the child's std handles to the console (the documented,
    //! reliable shape), which is fine here because the protocol is elsewhere.
    //! ptyhost also plays the terminal's side of the ConPTY handshake: conhost's
    //! cursor-position query is answered by the reader thread (see DSR_QUERY).
    use super::*;
    use std::io::{BufRead, Read};
    use std::sync::Arc;

    use portable_pty::{native_pty_system, CommandBuilder, PtySize};

    // A wide console so Deno's permission-prompt lines are not wrapped by the
    // pseudoconsole (ConPTY reflows output at the console width). Prompt lines
    // and the longest resource strings (URLs, paths) stay well under this, so
    // the prompt-parser sees each prompt line intact.
    const CONSOLE_COLS: u16 = 8192;
    const CONSOLE_ROWS: u16 = 50;

    pub fn run() {
        let stdin = std::io::stdin();
        let mut lines = stdin.lock().lines();

        // First frame must be spawn.
        let first = match lines.next() {
            Some(Ok(line)) => line,
            _ => return, // EOF before spawn: nothing to do.
        };
        let spawn = match serde_json::from_str::<ControlFrame>(&first) {
            Ok(f @ ControlFrame::Spawn { .. }) => f,
            Ok(_) => emit_fatal("first control frame must be spawn"),
            Err(e) => emit_fatal(&format!("bad control frame: {e}")),
        };
        let ControlFrame::Spawn {
            cmd,
            args,
            env,
            cwd,
        } = spawn
        else {
            unreachable!()
        };

        let pty_system = native_pty_system();
        let pair = match pty_system.openpty(PtySize {
            rows: CONSOLE_ROWS,
            cols: CONSOLE_COLS,
            pixel_width: 0,
            pixel_height: 0,
        }) {
            Ok(p) => p,
            Err(e) => emit_fatal(&format!("openpty (conpty) failed: {e}")),
        };

        // The child's environment is exactly `env` (env_clear, then apply), the
        // same contract as the unix path.
        let mut builder = CommandBuilder::new(&cmd);
        builder.args(&args);
        builder.env_clear();
        for (k, v) in &env {
            builder.env(k, v);
        }
        if let Some(dir) = &cwd {
            builder.cwd(dir);
        }

        let mut child = match pair.slave.spawn_command(builder) {
            Ok(c) => c,
            Err(e) => emit_fatal(&format!("spawn failed: {e}")),
        };
        // Drop the slave so the master read ends when the child's handles close.
        drop(pair.slave);

        let mut reader = match pair.master.try_clone_reader() {
            Ok(r) => r,
            Err(e) => emit_fatal(&format!("clone reader failed: {e}")),
        };
        // Shared between the control loop (prompt answers) and the reader
        // thread (DSR replies, see below).
        let writer = match pair.master.take_writer() {
            Ok(w) => Arc::new(Mutex::new(w)),
            Err(e) => emit_fatal(&format!("take writer failed: {e}")),
        };
        // The master must stay alive while the child runs (dropping it tears
        // down the pseudoconsole), and must be dropped as soon as the child
        // exits: ConPTY never EOFs its output pipe on child exit by itself;
        // the host has to close the pseudoconsole (ClosePseudoConsole, via
        // Drop), which ends conhost and lets the reader thread drain the tail
        // and hit EOF. The waiter owns it for exactly that lifecycle.
        let master = pair.master;

        // A killer handle usable from the control loop while the waiter blocks
        // in child.wait().
        let killer = Arc::new(Mutex::new(child.clone_killer()));
        let kill_child = {
            let killer = Arc::clone(&killer);
            move || {
                if let Ok(mut k) = killer.lock() {
                    let _ = k.kill();
                }
            }
        };

        // ConPTY output -> core: chunked, base64-wrapped `pty` events. This is
        // Deno's prompt text plus the worker's stderr; no protocol rides it.
        // The thread also answers conhost's cursor-position queries (DSR_QUERY):
        // conhost blocks the whole session on the first one until it is
        // answered, so the reply must happen here, not in core.
        let dsr_writer = Arc::clone(&writer);
        let reader_thread = std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            let mut tail: Vec<u8> = Vec::new();
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        emit_pty(&buf[..n]);
                        let (queries, new_tail) = scan_dsr_queries(&tail, &buf[..n]);
                        tail = new_tail;
                        for _ in 0..queries {
                            if let Ok(mut w) = dsr_writer.lock() {
                                let _ = w.write_all(b"\x1b[1;1R");
                                let _ = w.flush();
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        // Child exit -> drain the reader, report, and mirror the exit code.
        // ptyhost must not outlive its child (core awaits this process's status
        // as the worker's status).
        let waiter = std::thread::spawn(move || {
            let code = match child.wait() {
                Ok(status) => status.exit_code() as i32,
                Err(_) => 70,
            };
            drop(master);
            let _ = reader_thread.join();
            emit_exit(code);
            std::process::exit(code);
        });

        // Control loop: forward writes/answers to the console input, honor kill,
        // kill on EOF. `write` and `answer` are identical on Windows (no echo
        // tracking): the console input carries only the prompt answer.
        for line in lines {
            let Ok(line) = line else { break };
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<ControlFrame>(&line) {
                Ok(ControlFrame::Write { data_b64 }) | Ok(ControlFrame::Answer { data_b64 }) => {
                    if let Ok(bytes) = B64.decode(data_b64) {
                        // A console line read completes on Enter (\r), never on
                        // \n, so translate: core stays platform-agnostic and
                        // keeps sending "y\n". Only prompt answers flow this
                        // way on Windows (the protocol rides the control
                        // socket), so a blanket swap is safe.
                        let bytes: Vec<u8> = bytes
                            .iter()
                            .map(|&b| if b == b'\n' { b'\r' } else { b })
                            .collect();
                        if let Ok(mut w) = writer.lock() {
                            let _ = w.write_all(&bytes);
                            let _ = w.flush();
                        }
                    }
                }
                Ok(ControlFrame::Kill) => kill_child(),
                Ok(ControlFrame::Spawn { .. }) => {
                    // One child per ptyhost; a second spawn is a core bug.
                    kill_child();
                    emit_fatal("duplicate spawn frame");
                }
                Err(_) => continue, // tolerate garbage; core logs its own errors
            }
        }

        // stdin EOF or read error: core is gone, take the child with us. The
        // waiter thread exits the process once the child is reaped.
        kill_child();
        let _ = waiter.join();
        std::process::exit(70);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::unwrap_used)]
    fn parses_spawn_frame() {
        let f: ControlFrame = serde_json::from_str(
            r#"{"kind":"spawn","cmd":"/bin/deno","args":["run"],"env":{"A":"1"},"cwd":null}"#,
        )
        .unwrap();
        match f {
            ControlFrame::Spawn {
                cmd,
                args,
                env,
                cwd,
            } => {
                assert_eq!(cmd, "/bin/deno");
                assert_eq!(args, vec!["run"]);
                assert_eq!(env.get("A").map(String::as_str), Some("1"));
                assert_eq!(cwd, None);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn parses_write_and_kill_frames() {
        let w: ControlFrame = serde_json::from_str(r#"{"kind":"write","dataB64":"aGk="}"#).unwrap();
        match w {
            ControlFrame::Write { data_b64 } => {
                assert_eq!(B64.decode(data_b64).unwrap(), b"hi");
            }
            _ => panic!("wrong variant"),
        }
        let k: ControlFrame = serde_json::from_str(r#"{"kind":"kill"}"#).unwrap();
        assert!(matches!(k, ControlFrame::Kill));
    }

    #[test]
    fn dsr_scan_counts_queries_and_ignores_other_output() {
        let (n, tail) = scan_dsr_queries(b"", b"hello \x1b[6n world \x1b[6n");
        assert_eq!(n, 2);
        assert_eq!(tail, b"[6n");
        let (n, _) = scan_dsr_queries(b"", b"plain output, no query");
        assert_eq!(n, 0);
    }

    #[test]
    fn dsr_scan_matches_query_split_across_chunks_without_double_count() {
        // Query split at every possible boundary: exactly one match total.
        for cut in 1..DSR_QUERY.len() {
            let (a, b) = DSR_QUERY.split_at(cut);
            let (n1, tail) = scan_dsr_queries(b"", a);
            assert_eq!(n1, 0);
            let (n2, tail2) = scan_dsr_queries(&tail, b);
            assert_eq!(n2, 1, "cut at {cut}");
            // A third empty-ish chunk must not re-count the same query.
            let (n3, _) = scan_dsr_queries(&tail2, b"more");
            assert_eq!(n3, 0);
        }
    }

    #[test]
    fn parses_answer_frame() {
        let a: ControlFrame =
            serde_json::from_str(r#"{"kind":"answer","dataB64":"eQo="}"#).unwrap();
        match a {
            ControlFrame::Answer { data_b64 } => {
                assert_eq!(B64.decode(data_b64).unwrap(), b"y\n");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn echo_cancel_strips_own_writes_and_keeps_real_output() {
        use std::collections::VecDeque;
        let q = Mutex::new(VecDeque::<u8>::new());
        // A protocol frame is written, then bounces straight back: fully cancelled.
        expect_echo(&q, b"{\"k\":1}\n");
        assert_eq!(cancel_echo(&q, b"{\"k\":1}\n"), b"");
        assert!(q.lock().unwrap().is_empty());
        // With nothing pending, genuine child output passes through untouched.
        assert_eq!(cancel_echo(&q, b"hello\n"), b"hello\n");
    }

    #[cfg(unix)]
    #[test]
    fn echo_cancel_passes_real_output_interleaved_after_echo() {
        use std::collections::VecDeque;
        let q = Mutex::new(VecDeque::<u8>::new());
        expect_echo(&q, b"AB");
        // Echo arrives first (cancelled), then real output in the same read.
        assert_eq!(cancel_echo(&q, b"ABxy"), b"xy");
        assert!(q.lock().unwrap().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn echo_cancel_recovers_from_overflow() {
        use std::collections::VecDeque;
        let q = Mutex::new(VecDeque::<u8>::new());
        let huge = vec![b'z'; PENDING_ECHO_CAP + 1];
        expect_echo(&q, &huge); // overflow: backlog dropped rather than wedged
        assert!(q.lock().unwrap().is_empty());
        // Queue is clear, so output flows instead of being swallowed forever.
        assert_eq!(cancel_echo(&q, b"ok"), b"ok");
    }

    #[test]
    fn pty_event_encoding_is_one_json_line() {
        let line = format!(
            "{{\"kind\":\"pty\",\"dataB64\":\"{}\"}}",
            B64.encode(b"a\nb")
        );
        let parsed: serde_json::Value = serde_json::from_str(&line).expect("valid json");
        assert_eq!(parsed["kind"], "pty");
        assert_eq!(
            B64.decode(parsed["dataB64"].as_str().expect("string"))
                .expect("b64"),
            b"a\nb"
        );
    }
}
