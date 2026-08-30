//! A pseudo-terminal for one tool worker: Deno only prompts when the worker's
//! stdin AND stderr are terminals; without one every ask-state permission is a
//! hard `NotCapable`.
//!
//! On unix the worker's stdout is an ordinary pipe carrying the byte-exact
//! NDJSON protocol, and the PTY carries only stderr and the prompt. A ConPTY
//! binds all three std handles and reflows what passes through, so on windows
//! the protocol rides the loopback control socket ([`super::socket`]) instead.
//!
//! Core writes protocol frames to the worker's stdin, which IS the PTY slave,
//! with ECHO on, so they bounce back out of the master and must be cancelled.
//! ECHO cannot be turned off: Deno 2.8.x refuses to prompt when
//! `c_lflag & (ICANON | ECHO) == 0` (denoland/deno#34457).

use std::collections::{BTreeMap, VecDeque};
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Mutex;

#[cfg(unix)]
pub mod unix;
#[cfg(windows)]
pub mod windows;

#[cfg(unix)]
pub use unix::{spawn, PtySession};
#[cfg(windows)]
pub use windows::{spawn, PtySession};

/// What a spawn hands back: the terminal, its event stream, and the protocol
/// pipe when the platform has one.
pub struct Spawned {
    pub session: PtySession,
    pub events: tokio::sync::mpsc::UnboundedReceiver<PtyEvent>,
    /// For the Windows Job Object backstop (`sidecars::Manager::track_external`);
    /// `None` only when the platform would not tell us.
    pub pid: Option<u32>,
    /// The NDJSON protocol pipe: `Some` on unix, `None` under a ConPTY.
    pub stdout: Option<tokio::process::ChildStdout>,
}

/// What to run under the terminal. The environment is exact: nothing not named
/// here is inherited.
#[derive(Debug, Clone)]
pub struct PtySpawn {
    pub label: String,
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: BTreeMap<OsString, OsString>,
    pub cwd: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PtyEvent {
    /// Stderr and prompt text with Core's echoed writes removed; never protocol.
    Output(Vec<u8>),
    /// The child is gone. `code` is `None` when it died of a signal.
    Exited { code: Option<i32> },
}

/// How long the ConPTY reader gets to drain after the child exits.
#[cfg(windows)]
pub(crate) const WINDOWS_READER_DRAIN_GRACE: std::time::Duration =
    std::time::Duration::from_secs(2);

/// Bound on the outstanding-echo queue. A byte that never echoes back would
/// wedge it and swallow real output forever; past this we assume desync, drop
/// the backlog, and let stray echo through as stderr noise.
#[cfg(any(unix, test))]
pub(crate) const PENDING_ECHO_CAP: usize = 4 * 1024 * 1024;

/// Record bytes written to the master that the slave's ECHO will bounce back, so
/// the reader can drop them. Clears the backlog on overflow.
#[cfg(any(unix, test))]
pub(crate) fn expect_echo(pending: &Mutex<VecDeque<u8>>, bytes: &[u8]) {
    let mut queue = lock(pending);
    if queue.len() + bytes.len() > PENDING_ECHO_CAP {
        queue.clear();
        return;
    }
    queue.extend(bytes.iter().copied());
}

/// Strip echoed bytes from a master read by running prefix match against
/// `pending`. A coincidental match only costs a byte of stderr, never protocol.
#[cfg(any(unix, test))]
pub(crate) fn cancel_echo(pending: &Mutex<VecDeque<u8>>, chunk: &[u8]) -> Vec<u8> {
    let mut queue = lock(pending);
    let mut out = Vec::with_capacity(chunk.len());
    for &byte in chunk {
        if queue.front() == Some(&byte) {
            queue.pop_front();
        } else {
            out.push(byte);
        }
    }
    out
}

/// The DSR cursor-position query. portable-pty opens the pseudoconsole with
/// `PSEUDOCONSOLE_INHERIT_CURSOR`, so conhost emits this at session start and
/// holds ALL client output until Core (the terminal here) answers it.
#[cfg(any(windows, test))]
pub(crate) const DSR_QUERY: &[u8] = b"\x1b[6n";

/// Cursor position report: row 1, column 1.
#[cfg(any(windows, test))]
pub(crate) const DSR_REPLY: &[u8] = b"\x1b[1;1R";

/// Count DSR queries in `tail ++ chunk`, returning the new tail to carry so a
/// query split across reads is still seen exactly once.
#[cfg(any(windows, test))]
pub(crate) fn scan_dsr_queries(tail: &[u8], chunk: &[u8]) -> (usize, Vec<u8>) {
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

/// Translate `\n` to `\r`: a console line read completes only on Enter, so
/// `"y\n"` is never read. Only prompt answers flow this way, so a blanket
/// swap is safe.
#[cfg(any(windows, test))]
pub(crate) fn console_newlines(bytes: &[u8]) -> Vec<u8> {
    bytes
        .iter()
        .map(|&b| if b == b'\n' { b'\r' } else { b })
        .collect()
}

#[cfg(any(unix, test))]
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn queue() -> Mutex<VecDeque<u8>> {
        Mutex::new(VecDeque::new())
    }

    #[test]
    fn echo_cancellation_strips_our_own_writes_and_keeps_real_output() {
        let q = queue();
        expect_echo(&q, b"{\"kind\":\"call\"}\n");
        assert_eq!(cancel_echo(&q, b"{\"kind\":\"call\"}\n"), b"");
        assert!(lock(&q).is_empty());
        assert_eq!(cancel_echo(&q, b"hello\n"), b"hello\n");
    }

    #[test]
    fn echo_cancellation_passes_real_output_that_follows_an_echo_in_one_read() {
        let q = queue();
        expect_echo(&q, b"AB");
        assert_eq!(cancel_echo(&q, b"ABxy"), b"xy");
        assert!(lock(&q).is_empty());
    }

    #[test]
    fn echo_cancellation_spans_a_read_boundary() {
        let q = queue();
        expect_echo(&q, b"{\"kind\":\"cancel\"}\n");
        assert_eq!(cancel_echo(&q, b"{\"kind\":\"can"), b"");
        assert_eq!(cancel_echo(&q, b"cel\"}\n"), b"");
        assert!(lock(&q).is_empty());
    }

    #[test]
    fn echo_cancellation_recovers_from_an_overflow_instead_of_wedging() {
        let q = queue();
        expect_echo(&q, &vec![b'z'; PENDING_ECHO_CAP + 1]);
        assert!(lock(&q).is_empty(), "the backlog should have been dropped");
        assert_eq!(cancel_echo(&q, b"ok"), b"ok");
    }

    #[test]
    fn the_echo_cap_admits_a_frame_just_under_it() {
        let q = queue();
        let frame = vec![b'x'; PENDING_ECHO_CAP - 1];
        expect_echo(&q, &frame);
        assert_eq!(lock(&q).len(), PENDING_ECHO_CAP - 1);
        assert_eq!(cancel_echo(&q, &frame), b"");
    }

    #[test]
    fn the_dsr_reply_is_a_cursor_position_report() {
        assert_eq!(DSR_QUERY, b"\x1b[6n");
        assert_eq!(DSR_REPLY, b"\x1b[1;1R");
        assert!(DSR_REPLY.ends_with(b"R"), "a CPR ends in R");
    }

    #[test]
    fn dsr_queries_are_counted_and_other_output_is_not() {
        let (n, tail) = scan_dsr_queries(b"", b"hello \x1b[6n world \x1b[6n");
        assert_eq!(n, 2);
        assert_eq!(tail, b"[6n");
        let (n, _) = scan_dsr_queries(b"", b"plain output, no query");
        assert_eq!(n, 0);
    }

    #[test]
    fn a_dsr_query_split_across_chunks_is_counted_once() {
        for cut in 1..DSR_QUERY.len() {
            let (head, tail_bytes) = DSR_QUERY.split_at(cut);
            let (n1, carry) = scan_dsr_queries(b"", head);
            assert_eq!(n1, 0, "cut at {cut}");
            let (n2, carry2) = scan_dsr_queries(&carry, tail_bytes);
            assert_eq!(n2, 1, "cut at {cut}");
            let (n3, _) = scan_dsr_queries(&carry2, b"more output");
            assert_eq!(n3, 0, "cut at {cut}");
        }
    }

    #[test]
    fn a_console_answer_ends_in_carriage_return_not_newline() {
        assert_eq!(console_newlines(b"y\n"), b"y\r");
        assert_eq!(console_newlines(b"n\n"), b"n\r");
        assert_eq!(console_newlines(b"abc"), b"abc");
    }
}
