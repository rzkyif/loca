//! The per-worker loopback control channel.
//!
//! # Why this exists
//!
//! On Windows a tool worker runs under a ConPTY so Deno can show its interactive
//! permission prompt, which needs stdin and stderr to be console handles. A
//! pseudoconsole binds all three std handles and reflows what passes through, so
//! the byte-exact NDJSON protocol cannot ride stdout. The worker connects back
//! here instead, leaving the pseudoconsole carrying only the prompt.
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

use std::time::Duration;

use subtle::ConstantTimeEq;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

use crate::error::{internal, Result};

/// A worker that never connects must not leave the accept awaiting forever.
/// Under the pool's 10s boot timeout, so this failure is reported as itself.
const ACCEPT_TIMEOUT: Duration = Duration::from_secs(8);

/// Per-connection budget for the token line, so a probe that opens and sends
/// nothing cannot stall the accept loop for the whole [`ACCEPT_TIMEOUT`].
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
    /// A `BufReader` rather than the raw half, so the frame that arrives in
    /// the same read as the token line stays buffered rather than being lost.
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
        // The platform CSPRNG, called directly because `auth::secretgen` is
        // private; a failure means the OS could not supply entropy.
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

    /// Accept connections until one presents the correct token. Consumes the
    /// listener, so the socket is released either way.
    pub async fn accept(self) -> Result<ControlChannel> {
        self.accept_within(ACCEPT_TIMEOUT).await
    }

    /// [`ControlListener::accept`] with an explicit budget, so the give-up path
    /// can be tested in milliseconds: the bound is real I/O on a real socket,
    /// which a paused clock cannot drive.
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
                // Drop this connection and KEEP LISTENING, or a racing local
                // probe locks out the real worker by connecting first.
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
/// `AsyncBufReadExt::read_until` would buffer without limit.
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
    /// another writes outbound ones. Both need `&mut self`, and serialising
    /// them would make an outbound frame wait for the next inbound line.
    pub fn split(self) -> (ControlReader, ControlWriter) {
        (self.reader, self.writer)
    }
}

impl ControlReader {
    /// The next NDJSON line from the worker, or `None` at end of stream.
    /// Bounded, because an untrusted worker's unterminated line must not grow
    /// Core's heap.
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

    /// Every test here is time-bounded: this module's failures are hangs, so an
    /// unbounded test would reproduce the bug rather than report it.
    const BUDGET: Duration = Duration::from_secs(15);

    async fn connect(addr: &str) -> TcpStream {
        TcpStream::connect(addr).await.unwrap()
    }

    /// Bind a listener and take its address and token.
    async fn listener() -> (ControlListener, String, String) {
        let listener = ControlListener::bind().await.unwrap();
        let addr = listener.addr().to_string();
        let token = listener.token().to_string();
        (listener, addr, token)
    }

    /// A connection that writes `payload` once and then stays open, so the
    /// channel never sees EOF.
    fn speaker(addr: String, payload: String) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut stream = connect(&addr).await;
            stream.write_all(payload.as_bytes()).await.unwrap();
            tokio::time::sleep(Duration::from_secs(30)).await;
        })
    }

    #[tokio::test]
    async fn the_right_token_authenticates_and_the_protocol_flows_both_ways() {
        // The positive control: a listener that rejected everything would pass
        // the whole rest of this file.
        let (listener, addr, token) = listener().await;
        assert_eq!(token.len(), 64, "a 256-bit token in hex");
        assert!(addr.starts_with("127.0.0.1:"), "loopback only: {addr}");

        let worker = speaker(addr, format!("{token}\n{{\"kind\":\"ready\"}}\n"));

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
        // A single eager write puts the token and the first frame in one
        // packet, and dropping the frame hangs one tool call in a thousand.
        let (listener, addr, token) = listener().await;
        let worker = speaker(
            addr,
            format!("{token}\n{{\"kind\":\"booted\",\"extensionId\":\"demo\"}}\n"),
        );
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
        // The listener is one-shot; the accept LOOP is not. Otherwise anything
        // on the machine denies every tool call by racing to connect.
        let (listener, addr, token) = listener().await;
        let probe = speaker(addr.clone(), "not-the-token\n".to_string());
        // Give the probe a head start, so it really is first in the queue.
        tokio::time::sleep(Duration::from_millis(50)).await;
        let worker = speaker(addr, format!("{token}\n{{\"kind\":\"ready\"}}\n"));

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
        // Under a ConPTY this channel IS the worker protocol in both
        // directions, driven by two tasks, and a dropped half shows up as no
        // error anywhere.
        let (listener, addr, token) = listener().await;

        let worker = tokio::spawn(async move {
            let stream = connect(&addr).await;
            let (read_half, mut write_half) = stream.into_split();
            write_half
                .write_all(format!("{token}\n{{\"kind\":\"ready\"}}\n").as_bytes())
                .await
                .unwrap();
            // Blocks until Core's frame arrives, so the flood below is proof
            // the write half delivered rather than a race.
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

        let flooded = tokio::time::timeout(BUDGET, reader.read_line(1_024))
            .await
            .expect("the bounded read hung");
        assert_eq!(flooded, None, "an unbounded line was accepted");
        worker.abort();
    }

    #[tokio::test]
    async fn a_silent_connection_does_not_stall_the_accept_loop() {
        // With only the outer 8s bound, one silent connection delays the real
        // worker past the pool's boot timeout for the cost of one TCP socket.
        let (listener, addr, token) = listener().await;
        let silent = speaker(addr.clone(), String::new());
        tokio::time::sleep(Duration::from_millis(50)).await;
        let worker = speaker(addr, format!("{token}\n"));

        let started = std::time::Instant::now();
        let accepted = tokio::time::timeout(BUDGET, listener.accept()).await;
        assert!(accepted.is_ok(), "the silent connection stalled the accept");
        assert!(accepted.unwrap().is_ok());
        // About one token-read budget, not the whole outer one. Generous
        // because CI is slow, tight enough that removing the timeout fails it.
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
        let (listener, addr, token) = listener().await;
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

        let worker = speaker(addr, format!("{token}\n"));

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
        // Without it the pool's boot timeout is the only thing that reports the
        // failure, later and naming the wrong cause.
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
        // Asserted directly, because from outside the per-connection timeout
        // masks it. A SMALL buffer over a LARGE flood, so the bound rather than
        // EOF is what stops the read.
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

        // The positive control, so the refusal above is about the bound.
        let mut reader = BufReader::new(&b"short line\nrest"[..]);
        let mut out = Vec::new();
        assert!(read_line_bounded(&mut reader, &mut out, 256).await);
        assert_eq!(out, b"short line");
    }

    #[test]
    fn the_accept_budget_stays_under_the_boot_timeout() {
        // Over the boot timeout, a worker that never connects is reported as a
        // boot timeout and the log names the wrong subsystem. A relationship
        // rather than a number, because reading the constant back is not a test.
        assert!(ACCEPT_TIMEOUT < super::super::worker::BOOT_TIMEOUT);
        assert!(TOKEN_READ_TIMEOUT < ACCEPT_TIMEOUT);
    }
}
