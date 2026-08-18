//! A pseudo-terminal for one tool worker, so Deno shows its interactive
//! permission prompt.
//!
//! Absorbs `packages/loca-core-ptyhost` (773 lines of Rust that already ships).
//! What disappears is the helper BINARY and the NDJSON control protocol used to
//! talk to it, not the terminal handling: `spawn`/`write`/`answer`/`kill` become
//! methods, events become a channel, and the base64 round trip on every terminal
//! chunk goes away because bytes can stay bytes inside one process.
//!
//! # Why a terminal at all
//!
//! Deno only prompts when the worker's stdin AND stderr are terminals. Without
//! one, every ask-state permission is a hard `NotCapable` instead of a question
//! the user can answer. A Deno service could not open a PTY, which is the only
//! reason the helper existed; a Rust core can.
//!
//! # The two platforms differ in where the protocol rides
//!
//! - **unix**: the worker's stdout is an ordinary pipe, so the byte-exact NDJSON
//!   protocol rides it untouched and the PTY carries only stderr and the prompt.
//! - **windows**: a ConPTY binds all three std handles and reflows what passes
//!   through, so the protocol cannot ride stdout. The worker connects back over
//!   the loopback control socket ([`super::socket`]) and the pseudoconsole
//!   carries only the prompt.
//!
//! # Echo cancellation is not removable by absorbing
//!
//! Core writes protocol frames to the worker's stdin, which IS the PTY slave,
//! with ECHO on. Those bytes come straight back out of the master mixed into the
//! prompt stream. Cancelling them is what keeps the parser from reading Core's
//! own frames as terminal output. ECHO cannot simply be turned off: Deno 2.8.x
//! refuses to prompt when stdin is in raw mode, which it defines as
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
    /// The child's process id, for the Windows Job Object backstop
    /// (`sidecars::Manager::track_external`). A PTY spawn cannot go through
    /// `sidecars::Spawn`, so a worker started here joins the job after the fact
    /// or not at all, and "not at all" is a worker that survives a hard core
    /// kill. `None` only when the platform would not tell us.
    pub pid: Option<u32>,
    /// The worker's stdout, carrying the NDJSON protocol. `Some` on unix;
    /// `None` under a ConPTY, where the protocol rides the control socket
    /// instead because the pseudoconsole reflows what passes through it.
    pub stdout: Option<tokio::process::ChildStdout>,
}

/// What to run under the terminal. The environment is exact: nothing is
/// inherited that is not named here, matching `Spawn`'s contract in D14.
#[derive(Debug, Clone)]
pub struct PtySpawn {
    /// What log lines are tagged with, so a reader can act on them.
    pub label: String,
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: BTreeMap<OsString, OsString>,
    pub cwd: Option<PathBuf>,
}

/// What the terminal produces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PtyEvent {
    /// Terminal output: the worker's stderr and Deno's prompt text, with Core's
    /// own echoed writes already removed. Never the protocol stream.
    Output(Vec<u8>),
    /// The child is gone. `code` is `None` when it died of a signal.
    Exited { code: Option<i32> },
}

/// How long the terminal reader gets to drain after the child exits, before the
/// exit event goes out regardless. The unix backend has its own copy of this
/// constant next to the code that uses it; this one is the ConPTY's.
#[cfg(windows)]
pub(crate) const WINDOWS_READER_DRAIN_GRACE: std::time::Duration =
    std::time::Duration::from_secs(2);

/// Bound on the outstanding-echo queue.
///
/// A byte that never echoes back (the slave briefly disabled ECHO, say) would
/// otherwise wedge the queue and swallow real output forever. Past this bound we
/// assume desync, drop the backlog, and let any stray echo through as harmless
/// stderr noise. Comfortably above one protocol frame.
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

/// Strip leading echoed bytes from a master read.
///
/// Each byte matching the head of `pending` is its own echo and is dropped; the
/// rest is genuine child output. Echoed frames bounce back contiguously, so a
/// running prefix match cancels them, and a coincidental match only ever costs a
/// byte of a stderr log line, never protocol output (which rides stdout).
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

/// The DSR cursor-position query, `ESC [ 6 n`.
///
/// portable-pty creates the pseudoconsole with `PSEUDOCONSOLE_INHERIT_CURSOR`,
/// which makes conhost emit this query at session start and **hold all client
/// output** until the hosting terminal answers with a cursor position report. A
/// real terminal answers automatically; here Core is the terminal, so the reader
/// answers each query it sees. Without the reply the whole session stalls before
/// the child produces a single byte.
#[cfg(any(windows, test))]
pub(crate) const DSR_QUERY: &[u8] = b"\x1b[6n";

/// The answer to a [`DSR_QUERY`]: row 1, column 1.
#[cfg(any(windows, test))]
pub(crate) const DSR_REPLY: &[u8] = b"\x1b[1;1R";

/// Count DSR queries in `tail ++ chunk`, returning the new tail to carry into
/// the next chunk so a query split across reads is still seen.
///
/// `tail` is shorter than one query, so a counted match always ends inside
/// `chunk`: no double counting.
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

/// Translate `\n` to `\r` for a console write.
///
/// A Windows console line read completes on Enter (`\r`) and never on `\n`, so
/// an answer sent as `"y\n"` is read as nothing at all and the prompt hangs
/// until the give-up kill. Core stays platform-agnostic and keeps sending
/// `"y\n"`; the translation lives here. Only prompt answers flow this way on
/// Windows (the protocol rides the control socket), so a blanket swap is safe.
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
        // A protocol frame is written and bounces straight back: fully removed.
        expect_echo(&q, b"{\"kind\":\"call\"}\n");
        assert_eq!(cancel_echo(&q, b"{\"kind\":\"call\"}\n"), b"");
        assert!(lock(&q).is_empty());
        // With nothing pending, genuine child output passes through untouched.
        // Without this half, a canceller that ate everything would pass.
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
        // The echo of one frame routinely arrives split across master reads.
        let q = queue();
        expect_echo(&q, b"{\"kind\":\"cancel\"}\n");
        assert_eq!(cancel_echo(&q, b"{\"kind\":\"can"), b"");
        assert_eq!(cancel_echo(&q, b"cel\"}\n"), b"");
        assert!(lock(&q).is_empty());
    }

    #[test]
    fn echo_cancellation_recovers_from_an_overflow_instead_of_wedging() {
        // A byte that never echoes back would otherwise leave a stuck prefix and
        // swallow every subsequent byte of real output forever.
        let q = queue();
        expect_echo(&q, &vec![b'z'; PENDING_ECHO_CAP + 1]);
        assert!(lock(&q).is_empty(), "the backlog should have been dropped");
        assert_eq!(cancel_echo(&q, b"ok"), b"ok");
    }

    #[test]
    fn the_echo_cap_admits_a_frame_just_under_it() {
        // The other direction of the constant: a large but legitimate protocol
        // frame must still be tracked, or its echo reaches the prompt parser.
        let q = queue();
        let frame = vec![b'x'; PENDING_ECHO_CAP - 1];
        expect_echo(&q, &frame);
        assert_eq!(lock(&q).len(), PENDING_ECHO_CAP - 1);
        assert_eq!(cancel_echo(&q, &frame), b"");
    }

    #[test]
    fn the_dsr_reply_is_a_cursor_position_report() {
        // conhost holds ALL client output until its cursor-position query is
        // answered with a CPR. A reply that is not one leaves the session
        // stalled before the child writes a byte, and the symptom is a worker
        // that never boots on Windows only.
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
        // conhost holds ALL client output until the first query is answered, so
        // a query missed at a read boundary stalls the session before the child
        // writes a byte.
        for cut in 1..DSR_QUERY.len() {
            let (head, tail_bytes) = DSR_QUERY.split_at(cut);
            let (n1, carry) = scan_dsr_queries(b"", head);
            assert_eq!(n1, 0, "cut at {cut}");
            let (n2, carry2) = scan_dsr_queries(&carry, tail_bytes);
            assert_eq!(n2, 1, "cut at {cut}");
            // And a following chunk must not re-count the same query.
            let (n3, _) = scan_dsr_queries(&carry2, b"more output");
            assert_eq!(n3, 0, "cut at {cut}");
        }
    }

    #[test]
    fn a_console_answer_ends_in_carriage_return_not_newline() {
        // A console line read completes on Enter. An answer written as "y\n" is
        // never read at all, and the prompt hangs until the give-up kill.
        assert_eq!(console_newlines(b"y\n"), b"y\r");
        assert_eq!(console_newlines(b"n\n"), b"n\r");
        // Other bytes are untouched.
        assert_eq!(console_newlines(b"abc"), b"abc");
    }
}
