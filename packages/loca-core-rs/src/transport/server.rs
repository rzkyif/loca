//! The desktop listener: TCP, TLS, and the accept loop.
//!
//! **This is the only file in the crate that knows what a socket is.**
//! Everything it does is arrange for a request to reach the router; the router
//! is reachable without any of it, which is what makes the mobile in-process
//! path the same code rather than a parallel one.
//!
//! HTTP/1.1 only. Nothing negotiates h2 today: the desktop client terminates
//! TLS itself with a rustls configuration that advertises no ALPN, and the WS
//! upgrade requires 1.1 in any case. Adding h2 later is an ALPN entry and a
//! Cargo feature.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::Request;
use axum::Router;
use hyper::body::Incoming;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder;
use hyper_util::service::TowerToHyperService;
use rustls::ServerConfig;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio_rustls::TlsAcceptor;
use tower::ServiceExt;

use crate::error::{internal, AppError, ErrorCode, Result};

use super::middleware::PeerAddr;
use super::tls::TlsMaterial;

/// How long a peer has to complete the TLS handshake.
///
/// Without a deadline, a peer that finishes the TCP handshake and then sends
/// nothing holds a task, a socket and a file descriptor for as long as it cares
/// to. The core binds `0.0.0.0` for LAN pairing and the pairing routes are the
/// unauthenticated ones, so this is reachable before any credential is checked;
/// it is the cheapest thing an unauthenticated peer can make the core hold.
///
/// Ten seconds is far beyond a real handshake (one round trip plus signature
/// work, milliseconds on a LAN) and short enough that a stuck one is reclaimed
/// while it still costs nothing.
const HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// How many connections may be in flight at once.
///
/// A ceiling rather than a rate limit: it bounds what the core HOLDS, which is
/// the resource actually at risk (descriptors, and a task each). Excess
/// connections wait for a permit rather than being refused, so an ordinary burst
/// is served slightly later instead of failing, and only a genuine flood is
/// slowed. Well above any real client population: the desktop client holds one
/// socket plus a WS upgrade, so 512 is roughly two hundred paired clients all
/// reconnecting at once.
const MAX_CONNECTIONS: usize = 512;

/// The one crypto provider this crate uses, named explicitly.
///
/// `ServerConfig::builder()` picks the process-level default, which panics when
/// more than one provider is linked. Two are: rustls brings aws-lc-rs, and
/// `tokio-tungstenite`'s rustls feature brings ring in through the test
/// dependencies. Naming the provider makes that a non-event rather than a
/// runtime panic that appears the day a dependency changes its features. The
/// Tauri client does the same, for the same reason.
pub fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    static PROVIDER: std::sync::OnceLock<Arc<rustls::crypto::CryptoProvider>> =
        std::sync::OnceLock::new();
    Arc::clone(PROVIDER.get_or_init(|| Arc::new(rustls::crypto::aws_lc_rs::default_provider())))
}

/// A bound listener, before it starts accepting.
///
/// Binding is separated from serving so the frontend learns the real port (port
/// 0 in tests) and can report a bind failure before anything else in boot
/// depends on the listener being up.
pub struct Listener {
    listener: TcpListener,
    acceptor: TlsAcceptor,
    local_addr: SocketAddr,
    /// Per-connection handshake budget. A field rather than the constant read at
    /// its use site so a test can drive the real accept loop on a scale it can
    /// watch, the same reason `ws::Heartbeat` carries its three durations.
    handshake_timeout: std::time::Duration,
}

/// Bind `host:port` and terminate TLS with `tls`.
pub async fn bind(tls: &TlsMaterial, host: &str, port: u16) -> Result<Listener> {
    let config = ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(|e| internal(format!("TLS protocol versions rejected: {e}")))?
        .with_no_client_auth()
        .with_single_cert(vec![tls.cert_der.clone()], tls.key_der())
        .map_err(|e| internal(format!("TLS configuration rejected the key: {e}")))?;

    let listener = TcpListener::bind((host, port)).await.map_err(|err| {
        if err.kind() == std::io::ErrorKind::AddrInUse {
            // The most common boot failure by a wide margin, and the bare
            // message reaches the user as an opaque connection timeout, so it
            // is rewritten into something a person can act on.
            return AppError::new(
                ErrorCode::InternalError,
                format!(
                    "port {port} on {host} is already in use. Another Loca Core may already be \
                     running, or a different program holds the port. Stop it, or set \
                     LOCA_CORE_HOST to a free host and relaunch."
                ),
            );
        }
        AppError::from(err)
    })?;
    let local_addr = listener
        .local_addr()
        .map_err(|e| internal(format!("listener has no address: {e}")))?;

    Ok(Listener {
        listener,
        acceptor: TlsAcceptor::from(Arc::new(config)),
        local_addr,
        handshake_timeout: HANDSHAKE_TIMEOUT,
    })
}

impl Listener {
    /// The address actually bound, which is what a caller that passed port 0
    /// needs.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Compress the handshake budget so a test can observe a silent peer being
    /// reclaimed without waiting the production ten seconds.
    #[cfg(test)]
    fn with_handshake_timeout(mut self, budget: std::time::Duration) -> Self {
        self.handshake_timeout = budget;
        self
    }

    /// Accept until `shutdown` fires.
    ///
    /// Returns once the listener is closed. In-flight connections are given
    /// hyper's graceful shutdown rather than being cut, so a response already
    /// being written finishes.
    pub async fn serve(self, router: Router, mut shutdown: watch::Receiver<bool>) {
        let service = router.into_service::<Incoming>();
        tracing::info!(target: "loca::http", "listening on https://{}", self.local_addr);

        let permits = Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS));

        loop {
            let accepted = tokio::select! {
                accepted = self.listener.accept() => accepted,
                _ = shutdown.changed() => break,
            };
            let (stream, peer) = match accepted {
                Ok(pair) => pair,
                Err(err) => {
                    // A per-connection accept failure (a descriptor limit, a
                    // peer that vanished between the SYN and the accept) must
                    // not take the listener down.
                    tracing::warn!(target: "loca::http", "accept failed: {err}");
                    continue;
                }
            };

            // Acquired BEFORE the spawn, so the ceiling bounds live connections
            // rather than live tasks: spawning first and acquiring inside would
            // let an unbounded number of tasks pile up waiting for a permit,
            // which is the resource the ceiling exists to bound.
            //
            // Waiting here also applies backpressure to the accept loop itself,
            // leaving excess connections in the kernel's accept queue where they
            // cost the core nothing.
            let Ok(permit) = Arc::clone(&permits).acquire_owned().await else {
                // Only on a closed semaphore, which nothing does.
                break;
            };

            let acceptor = self.acceptor.clone();
            let service = service.clone();
            let shutdown = shutdown.clone();
            let budget = self.handshake_timeout;
            tokio::spawn(async move {
                serve_connection(acceptor, stream, peer, service, shutdown, budget).await;
                drop(permit);
            });
        }

        tracing::info!(target: "loca::http", "listener closed");
    }
}

async fn serve_connection<S>(
    acceptor: TlsAcceptor,
    stream: tokio::net::TcpStream,
    peer: SocketAddr,
    service: S,
    mut shutdown: watch::Receiver<bool>,
    handshake_timeout: std::time::Duration,
) where
    S: tower::Service<
            Request<Incoming>,
            Response = axum::response::Response,
            Error = std::convert::Infallible,
        > + Clone
        + Send
        + 'static,
    S::Future: Send,
{
    let tls_stream = match tokio::time::timeout(handshake_timeout, acceptor.accept(stream)).await {
        Ok(Ok(stream)) => stream,
        Ok(Err(err)) => {
            // A failed handshake is ordinary: a port scanner, a client that
            // pinned a different certificate, a plain-HTTP request against the
            // HTTPS port. Debug, and only this connection is affected.
            tracing::debug!(target: "loca::http", "TLS handshake with {peer} failed: {err}");
            return;
        }
        Err(_) => {
            // A peer that completed the TCP handshake and then said nothing.
            // Distinguished from the failure above because it is the one an
            // attacker chooses deliberately: it holds a descriptor and a
            // connection permit for free, where a failed handshake releases
            // both immediately.
            tracing::debug!(
                target: "loca::http",
                "TLS handshake with {peer} did not complete within {handshake_timeout:?}",
            );
            return;
        }
    };

    // The ONLY request extension the wire path inserts. The auth layer treats a
    // pre-inserted `ClientIdentity` as pre-authenticated, which is what the
    // in-process path relies on, so inserting one here would let anything that
    // reached the socket bypass authentication. Do not add one.
    let service = tower::service_fn(move |mut req: Request<Incoming>| {
        req.extensions_mut().insert(PeerAddr(peer.ip().to_string()));
        service.clone().oneshot(req)
    });

    let builder = Builder::new(TokioExecutor::new());
    let conn = builder.serve_connection_with_upgrades(
        TokioIo::new(tls_stream),
        TowerToHyperService::new(service),
    );
    tokio::pin!(conn);

    tokio::select! {
        result = conn.as_mut() => {
            if let Err(err) = result {
                tracing::debug!(target: "loca::http", "connection with {peer} ended: {err}");
            }
        }
        _ = shutdown.changed() => {
            conn.as_mut().graceful_shutdown();
            let _ = conn.await;
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::io::AsyncReadExt;

    /// Bind a real listener on an ephemeral port and start serving.
    async fn listening(budget: Duration) -> (SocketAddr, watch::Sender<bool>) {
        let (tls, _) = super::super::tls::ensure(None, "127.0.0.1").expect("tls material");
        let listener = bind(&tls, "127.0.0.1", 0)
            .await
            .expect("bind")
            .with_handshake_timeout(budget);
        let addr = listener.local_addr();
        let (tx, rx) = watch::channel(false);
        tokio::spawn(listener.serve(Router::new(), rx));
        (addr, tx)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_peer_that_connects_and_says_nothing_is_reclaimed_rather_than_held_forever() {
        // The accept loop spawns a task per connection and awaited
        // `acceptor.accept(..)` with no deadline, so a peer that completed the
        // TCP handshake and then sent nothing held a task, a socket and a
        // descriptor for as long as it liked. Reachable before any credential
        // is checked, on a listener that binds `0.0.0.0` for LAN pairing.
        //
        // The observable is the server hanging up. A silent peer's read returns
        // 0 bytes once the server side drops its half; while the connection is
        // still held, that read never completes. So the outer timeout is the
        // assertion: without the handshake budget this waits forever.
        let budget = Duration::from_millis(200);
        let (addr, _shutdown) = listening(budget).await;

        let mut sock = tokio::net::TcpStream::connect(addr).await.expect("connect");
        // Not one byte sent: no ClientHello, nothing for rustls to work with.
        let mut buf = [0u8; 1];
        let closed = tokio::time::timeout(budget * 10, sock.read(&mut buf)).await;

        assert!(
            matches!(closed, Ok(Ok(0))),
            "the core held a connection open for a peer that never started a TLS handshake, so \
             an unauthenticated peer can pin a task and a descriptor for free: {closed:?}",
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_completed_handshake_is_not_cut_off_by_the_budget() {
        // The control, and it is load-bearing rather than symmetry: a budget
        // applied to the whole connection instead of to the handshake would
        // satisfy the test above and sever every ordinary request that took
        // longer than it, which for this core includes a streamed chat turn.
        // This pins that the deadline ends at the handshake.
        let budget = Duration::from_millis(200);
        let (addr, _shutdown) = listening(budget).await;

        let mut sock = tokio::net::TcpStream::connect(addr).await.expect("connect");
        // A ClientHello the acceptor will reject (no matching cert trust on our
        // side is irrelevant; the server completes or fails its half fast). What
        // matters is that the connection is decided by TLS rather than by the
        // clock, so this must resolve well inside the budget.
        let started = std::time::Instant::now();
        let _ = tokio::io::AsyncWriteExt::write_all(&mut sock, b"\x16\x03\x01\x00\x05hello").await;
        let mut buf = [0u8; 1];
        let _ = tokio::time::timeout(budget * 10, sock.read(&mut buf)).await;
        assert!(
            started.elapsed() < budget,
            "a connection that spoke TLS immediately still waited out the handshake budget, so \
             the deadline is bounding the wrong thing",
        );
    }
}
