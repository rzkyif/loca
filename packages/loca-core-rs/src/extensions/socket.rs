//! The per-worker loopback control channel.
//!
//! Ports `packages/loca-core/src/extensions/control-socket.ts`.
//!
//! # Why this exists, and why absorbing the helper did not remove it
//!
//! On Windows a tool worker runs under a ConPTY so Deno can show its interactive
//! permission prompt, which needs stdin and stderr to be console handles. A
//! pseudoconsole binds all three std handles and reflows what passes through, so
//! the byte-exact NDJSON protocol cannot ride stdout. The worker connects back
//! here instead and speaks the protocol over loopback, leaving the pseudoconsole
//! carrying only the prompt.
//!
//! That is a property of ConPTY, not of the old helper binary, so absorbing the
//! helper changes nothing about it.
//!
//! # The four security properties
//!
//! 1. **Loopback only, on an ephemeral port.** Never a wildcard bind.
//! 2. **A fresh 256-bit token per worker**, presented as the connection's first
//!    line. It reaches the worker as a spawn argument; on Windows another user
//!    cannot read a process's command line without elevation.
//! 3. **The listener is one-shot but the accept LOOP is not.** A wrong token
//!    drops that connection and keeps listening, so a racing local probe cannot
//!    lock out the real worker by connecting first.
//! 4. **Two independent timeouts.** The per-connection one exists so a
//!    connection that opens and says nothing cannot stall the accept loop for
//!    the whole outer budget. Keeping only the outer bound is a trivial local
//!    denial of service.
//!
//! The token comparison is constant-time here where the TypeScript's was a plain
//! `!==`. That is a strengthening rather than a port, and it is free.

use std::time::Duration;

use subtle::ConstantTimeEq;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

use crate::error::{internal, Result};

/// A worker that never connects (it crashed before boot, or was spawned with
/// flags that deny it the port) must not leave the accept awaiting forever. The
/// pool's boot timeout is 10s, so this is a little less and lets the boot
/// timeout report the real failure.
const ACCEPT_TIMEOUT: Duration = Duration::from_secs(8);

/// Per-connection budget for the token line. A stray probe of the loopback port
/// that opens and sends nothing must not stall the accept loop for the whole
/// [`ACCEPT_TIMEOUT`].
const TOKEN_READ_TIMEOUT: Duration = Duration::from_secs(1);

/// Bound on the token line. A real one is 64 hex characters and a newline;
/// without a bound a connection can stream forever without ever sending one.
const MAX_TOKEN_LINE_BYTES: usize = 256;

/// A one-shot listener with a fresh token.
pub struct ControlListener {
    listener: TcpListener,
    addr: String,
    token: String,
}

/// The authenticated protocol channel for one worker: NDJSON in and out.
pub struct ControlChannel {
    reader: ControlReader,
    writer: ControlWriter,
}

/// The inbound half: the worker's protocol frames.
pub struct ControlReader {
    /// A `BufReader` rather than the raw half, and that is what keeps the
    /// worker's first frame: the token line and the frame after it routinely
    /// arrive in ONE read, so a handshake that consumed the whole buffer and
    /// kept only the token would silently drop that frame. Here the unread
    /// remainder simply stays in the buffer.
    reader: BufReader<tokio::net::tcp::OwnedReadHalf>,
}

/// The outbound half: the frames Core sends the worker.
pub struct ControlWriter {
    writer: tokio::net::tcp::OwnedWriteHalf,
}

impl ControlListener {
    /// Bind a loopback listener on an ephemeral port with a fresh token.
    pub async fn bind() -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .map_err(|e| internal(format!("could not bind the worker control socket: {e}")))?;
        let addr = listener
            .local_addr()
            .map_err(|e| internal(format!("control socket has no address: {e}")))?;
        // The platform CSPRNG, which is the same source `auth::secretgen` draws
        // every other secret from. Called directly because that module is
        // private to `auth`; a failure means the OS could not supply entropy,
        // which is not something to paper over with a weaker source.
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes)
            .map_err(|e| internal(format!("failed to read from the CSPRNG: {e}")))?;
        Ok(Self {
            listener,
            addr: format!("{}:{}", addr.ip(), addr.port()),
            token: hex::encode(bytes),
        })
    }

    pub fn addr(&self) -> &str {
        &self.addr
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    pub fn port(&self) -> u16 {
        self.listener.local_addr().map(|a| a.port()).unwrap_or(0)
    }

    /// Accept connections until one presents the correct token.
    ///
    /// Consumes the listener: it is one-shot by type, so the socket is released
    /// whether the handshake succeeded or timed out.
    pub async fn accept(self) -> Result<ControlChannel> {
        self.accept_within(ACCEPT_TIMEOUT).await
    }

    /// [`ControlListener::accept`] with an explicit budget.
    ///
    /// Exists so the give-up path can be tested in milliseconds. The bound is
    /// real I/O on a real socket, so a paused clock cannot drive it (D-036), and
    /// an eight-second test to prove a timeout fires is eight seconds every
    /// contributor pays on every run.
    pub async fn accept_within(self, budget: Duration) -> Result<ControlChannel> {
        tokio::time::timeout(budget, self.accept_loop())
            .await
            .unwrap_or_else(|_| {
                Err(internal(
                    "the worker did not connect to the control socket in time",
                ))
            })
    }

    async fn accept_loop(self) -> Result<ControlChannel> {
        loop {
            let Ok((stream, _)) = self.listener.accept().await else {
                continue;
            };
            match authenticate(stream, &self.token).await {
                Some(channel) => return Ok(channel),
                // Wrong or missing token: drop this connection and KEEP
                // LISTENING. Returning here would let a racing local probe lock
                // out the real worker just by connecting first.
                None => {
                    tracing::warn!(
                        target: "loca::extensions",
                        "control socket connection presented an invalid token",
                    );
                }
            }
        }
    }
}

/// Read the first line and compare it to the token.
async fn authenticate(stream: TcpStream, token: &str) -> Option<ControlChannel> {
    let (read_half, writer) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let mut line = Vec::new();
    let read = tokio::time::timeout(
        TOKEN_READ_TIMEOUT,
        read_line_bounded(&mut reader, &mut line, MAX_TOKEN_LINE_BYTES),
    )
    .await;
    if !matches!(read, Ok(true)) {
        return None;
    }
    let presented = String::from_utf8_lossy(&line);
    let presented = presented.trim_end_matches(['\r', '\n']);
    // Constant time, and length-checked first because `ct_eq` needs equal
    // lengths. The length of a token is not a secret.
    if presented.len() != token.len()
        || presented.as_bytes().ct_eq(token.as_bytes()).unwrap_u8() != 1
    {
        return None;
    }
    Some(ControlChannel {
        reader: ControlReader { reader },
        writer: ControlWriter { writer },
    })
}

/// Read up to and including one `\n`, refusing at `max` bytes.
///
/// `AsyncBufReadExt::read_until` would buffer without limit, which is the whole
/// thing the bound exists to prevent.
async fn read_line_bounded<R>(reader: &mut R, out: &mut Vec<u8>, max: usize) -> bool
where
    R: AsyncBufReadExt + Unpin,
{
    loop {
        let available = match reader.fill_buf().await {
            Ok([]) | Err(_) => return false,
            Ok(buf) => buf,
        };
        match available.iter().position(|&b| b == b'\n') {
            Some(i) => {
                out.extend_from_slice(&available[..i]);
                reader.consume(i + 1);
                return true;
            }
            None => {
                let taken = available.len();
                out.extend_from_slice(available);
                reader.consume(taken);
                if out.len() > max {
                    return false;
                }
            }
        }
    }
}

impl ControlChannel {
    /// Write one already-newline-terminated line.
    pub async fn write_line(&mut self, line: &str) -> Result<()> {
        self.writer.write_line(line).await
    }

    /// The next NDJSON line from the worker, or `None` at end of stream.
    pub async fn read_line(&mut self, max_bytes: usize) -> Option<String> {
        self.reader.read_line(max_bytes).await
    }

    pub async fn shutdown(&mut self) {
        self.writer.shutdown().await;
    }

    /// Take the two halves apart, so one task can pump inbound frames while
    /// another writes outbound ones.
    ///
    /// Both sides need `&mut self`, so a single task cannot hold the channel
    /// and `select!` over a read and a write: the borrow checker refuses, and
    /// serialising them behind one lock would mean an outbound frame waits for
    /// the next inbound line, which for a worker that is thinking is forever.
    pub fn split(self) -> (ControlReader, ControlWriter) {
        (self.reader, self.writer)
    }
}

impl ControlReader {
    /// The next NDJSON line from the worker, or `None` at end of stream.
    ///
    /// Bounded by `max_bytes` for the same reason the token line is: the worker
    /// is untrusted and a line with no newline must not grow Core's heap.
    pub async fn read_line(&mut self, max_bytes: usize) -> Option<String> {
        let mut line = Vec::new();
        read_line_bounded(&mut self.reader, &mut line, max_bytes)
            .await
            .then(|| String::from_utf8_lossy(&line).into_owned())
    }
}

impl ControlWriter {
    /// Write one already-newline-terminated line.
    pub async fn write_line(&mut self, line: &str) -> Result<()> {
        self.writer.write_all(line.as_bytes()).await?;
        self.writer.flush().await?;
        Ok(())
    }

    pub async fn shutdown(&mut self) {
        let _ = self.writer.shutdown().await;
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use tokio::io::AsyncWriteExt as _;

    use super::*;

    /// Every test here is time-bounded. The failures this module can have are
    /// hangs, so an unbounded test would reproduce the bug rather than report
    /// it.
    const BUDGET: Duration = Duration::from_secs(15);

    async fn connect(addr: &str) -> TcpStream {
        TcpStream::connect(addr).await.unwrap()
    }

    #[tokio::test]
    async fn the_right_token_authenticates_and_the_protocol_flows_both_ways() {
        // The positive control for every refusal below. Without it, a listener
        // that rejected everything would pass the whole rest of this file.
        let listener = ControlListener::bind().await.unwrap();
        let addr = listener.addr().to_string();
        let token = listener.token().to_string();
        assert_eq!(token.len(), 64, "a 256-bit token in hex");
        assert!(addr.starts_with("127.0.0.1:"), "loopback only: {addr}");

        let worker = tokio::spawn(async move {
            let mut stream = connect(&addr).await;
            stream
                .write_all(format!("{token}\n{{\"kind\":\"ready\"}}\n").as_bytes())
                .await
                .unwrap();
            // Hold the connection open so the channel does not see EOF.
            tokio::time::sleep(Duration::from_secs(30)).await;
        });

        let mut channel = tokio::time::timeout(BUDGET, listener.accept())
            .await
            .expect("accept timed out")
            .expect("the correct token was refused");
        let line = tokio::time::timeout(BUDGET, channel.read_line(4096))
            .await
            .expect("read timed out");
        assert_eq!(line.as_deref(), Some(r#"{"kind":"ready"}"#));
        channel
            .write_line("{\"kind\":\"shutdown\"}\n")
            .await
            .unwrap();
        worker.abort();
    }

    #[tokio::test]
    async fn a_frame_sharing_the_handshake_packet_is_not_lost() {
        // The worker writes its token and its first frame together, which is
        // what a single eager write produces. A handshake that read a whole
        // buffer and kept only the token line would silently drop that frame,
        // and the symptom is one tool call in a thousand hanging at boot.
        let listener = ControlListener::bind().await.unwrap();
        let addr = listener.addr().to_string();
        let token = listener.token().to_string();
        let worker = tokio::spawn(async move {
            let mut stream = connect(&addr).await;
            let payload = format!("{token}\n{{\"kind\":\"booted\",\"extensionId\":\"demo\"}}\n");
            stream.write_all(payload.as_bytes()).await.unwrap();
            tokio::time::sleep(Duration::from_secs(30)).await;
        });
        let mut channel = tokio::time::timeout(BUDGET, listener.accept())
            .await
            .expect("accept timed out")
            .unwrap();
        let line = tokio::time::timeout(BUDGET, channel.read_line(4096))
            .await
            .expect("read timed out");
        assert_eq!(
            line.as_deref(),
            Some(r#"{"kind":"booted","extensionId":"demo"}"#)
        );
        worker.abort();
    }

    #[tokio::test]
    async fn a_wrong_token_does_not_lock_out_the_real_worker() {
        // The listener is one-shot; the accept LOOP is not. A local probe that
        // connects first and guesses wrong must be dropped while the listener
        // keeps accepting, or anything on the machine can deny every tool call
        // by racing to connect.
        let listener = ControlListener::bind().await.unwrap();
        let addr = listener.addr().to_string();
        let token = listener.token().to_string();

        let probe_addr = addr.clone();
        let probe = tokio::spawn(async move {
            let mut stream = connect(&probe_addr).await;
            stream.write_all(b"not-the-token\n").await.unwrap();
            tokio::time::sleep(Duration::from_secs(30)).await;
        });
        // Give the probe a head start, so it really is first in the queue.
        tokio::time::sleep(Duration::from_millis(50)).await;

        let worker = tokio::spawn(async move {
            let mut stream = connect(&addr).await;
            stream
                .write_all(format!("{token}\n{{\"kind\":\"ready\"}}\n").as_bytes())
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_secs(30)).await;
        });

        let mut channel = tokio::time::timeout(BUDGET, listener.accept())
            .await
            .expect("the probe locked out the real worker")
            .expect("the real worker was refused");
        let line = tokio::time::timeout(BUDGET, channel.read_line(4096))
            .await
            .unwrap();
        assert_eq!(line.as_deref(), Some(r#"{"kind":"ready"}"#));
        probe.abort();
        worker.abort();
    }

    #[tokio::test]
    async fn the_split_halves_carry_the_protocol_in_both_directions_and_the_reader_stays_bounded() {
        // Under a ConPTY this channel IS the worker protocol, in both
        // directions, and the two halves are driven by two tasks. A split that
        // dropped either one leaves a worker that can be spoken to and never
        // answers, or answers and is never spoken to, and neither shows up as
        // an error anywhere.
        let listener = ControlListener::bind().await.unwrap();
        let addr = listener.addr().to_string();
        let token = listener.token().to_string();

        let worker = tokio::spawn(async move {
            let stream = connect(&addr).await;
            let (read_half, mut write_half) = stream.into_split();
            write_half
                .write_all(format!("{token}\n{{\"kind\":\"ready\"}}\n").as_bytes())
                .await
                .unwrap();
            // Blocks until Core's frame actually arrives, so the flood below is
            // proof that the write half delivered rather than a race.
            let mut reader = BufReader::new(read_half);
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            assert_eq!(line, "{\"kind\":\"shutdown\"}\n");
            // A frame with no newline, past the bound.
            write_half.write_all(&vec![b'x'; 5_000]).await.unwrap();
            tokio::time::sleep(Duration::from_secs(30)).await;
        });

        let channel = tokio::time::timeout(BUDGET, listener.accept())
            .await
            .expect("the accept hung")
            .expect("the worker was refused");
        let (mut reader, mut writer) = channel.split();

        let first = tokio::time::timeout(BUDGET, reader.read_line(4096))
            .await
            .unwrap();
        assert_eq!(first.as_deref(), Some(r#"{"kind":"ready"}"#));

        tokio::time::timeout(BUDGET, writer.write_line("{\"kind\":\"shutdown\"}\n"))
            .await
            .expect("the write hung")
            .expect("the write failed");

        // The bound is what stops an untrusted worker growing Core's heap with
        // a frame it never terminates.
        let flooded = tokio::time::timeout(BUDGET, reader.read_line(1_024))
            .await
            .expect("the bounded read hung");
        assert_eq!(flooded, None, "an unbounded line was accepted");
        worker.abort();
    }

    #[tokio::test]
    async fn a_silent_connection_does_not_stall_the_accept_loop() {
        // The per-connection budget. With only the outer 8s bound, one
        // connection that opens and says nothing delays the real worker past
        // the pool's boot timeout, which is a local denial of service costing
        // one TCP connection.
        let listener = ControlListener::bind().await.unwrap();
        let addr = listener.addr().to_string();
        let token = listener.token().to_string();

        let silent_addr = addr.clone();
        let silent = tokio::spawn(async move {
            let _stream = connect(&silent_addr).await;
            tokio::time::sleep(Duration::from_secs(30)).await;
        });
        tokio::time::sleep(Duration::from_millis(50)).await;

        let worker = tokio::spawn(async move {
            let mut stream = connect(&addr).await;
            stream
                .write_all(format!("{token}\n").as_bytes())
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_secs(30)).await;
        });

        let started = std::time::Instant::now();
        let accepted = tokio::time::timeout(BUDGET, listener.accept()).await;
        assert!(accepted.is_ok(), "the silent connection stalled the accept");
        assert!(accepted.unwrap().is_ok());
        // The real assertion: it took about one token-read budget, not the
        // whole outer one. Generous, because CI machines are slow; tight enough
        // that removing the per-connection timeout fails it.
        let elapsed = started.elapsed();
        assert!(
            elapsed < ACCEPT_TIMEOUT - Duration::from_secs(2),
            "waited {elapsed:?}, which is the outer budget rather than the per-connection one",
        );
        silent.abort();
        worker.abort();
    }

    #[tokio::test]
    async fn an_unbounded_token_line_is_refused_rather_than_buffered() {
        // Without the bound, a connection can stream forever with no newline and
        // Core buffers all of it.
        let listener = ControlListener::bind().await.unwrap();
        let addr = listener.addr().to_string();
        let token = listener.token().to_string();

        let flood_addr = addr.clone();
        let flood = tokio::spawn(async move {
            let mut stream = connect(&flood_addr).await;
            // No newline, ever.
            let chunk = vec![b'x'; 8192];
            for _ in 0..64 {
                if stream.write_all(&chunk).await.is_err() {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_secs(30)).await;
        });
        tokio::time::sleep(Duration::from_millis(50)).await;

        let worker = tokio::spawn(async move {
            let mut stream = connect(&addr).await;
            stream
                .write_all(format!("{token}\n").as_bytes())
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_secs(30)).await;
        });

        let accepted = tokio::time::timeout(BUDGET, listener.accept()).await;
        assert!(
            accepted.is_ok() && accepted.unwrap().is_ok(),
            "a flooding connection kept the real worker out",
        );
        flood.abort();
        worker.abort();
    }

    #[tokio::test]
    async fn a_worker_that_never_connects_gives_up_rather_than_hanging() {
        // The mechanism, at a budget the suite can afford. Without it the accept
        // waits forever and the pool's boot timeout is the only thing that ever
        // reports the failure, ten seconds later and naming the wrong cause.
        let listener = ControlListener::bind().await.unwrap();
        let budget = Duration::from_millis(300);
        let started = std::time::Instant::now();
        let result = tokio::time::timeout(BUDGET, listener.accept_within(budget)).await;
        assert!(result.is_ok(), "accept never gave up");
        assert!(result.unwrap().is_err(), "accept invented a channel");
        assert!(started.elapsed() >= budget, "it gave up before its budget");
    }

    #[tokio::test]
    async fn the_line_bound_stops_reading_rather_than_buffering() {
        // Asserted directly, because from the outside the per-connection
        // timeout masks it: a flood is dropped after a second either way, and
        // the difference is only how many megabytes were buffered first. That
        // makes the accept-loop test unable to see this bound, so it gets one.
        // A SMALL buffer over a LARGE flood, so the bound is what stops the
        // read rather than the end of the input. Over a finite slice the loop
        // stops at EOF whatever the bound does, which is why the obvious
        // version of this test cannot fail.
        let flood = vec![b'x'; 1_000_000];
        let mut reader = BufReader::with_capacity(64, &flood[..]);
        let mut out = Vec::new();
        let found = read_line_bounded(&mut reader, &mut out, 256).await;
        assert!(!found, "an unterminated line was accepted as a line");
        assert!(
            out.len() < 4096,
            "it buffered {} bytes of a line bounded at 256",
            out.len(),
        );

        // The positive control: a line inside the bound is read whole, so the
        // refusal above is about the bound and not about reading being broken.
        let mut reader = BufReader::new(&b"short line\nrest"[..]);
        let mut out = Vec::new();
        assert!(read_line_bounded(&mut reader, &mut out, 256).await);
        assert_eq!(out, b"short line");
    }

    #[test]
    fn the_accept_budget_stays_under_the_boot_timeout() {
        // The only thing the VALUE of ACCEPT_TIMEOUT has to satisfy, and the one
        // way it can be wrong: if it exceeded the pool's boot timeout, a worker
        // that never connects would be reported as a boot timeout rather than as
        // the control-socket failure it is, and the log would name the wrong
        // subsystem. Asserted as a relationship rather than as a number, because
        // a test that reads the constant back is not a test.
        assert!(ACCEPT_TIMEOUT < super::super::worker::BOOT_TIMEOUT);
        assert!(TOKEN_READ_TIMEOUT < ACCEPT_TIMEOUT);
    }
}
