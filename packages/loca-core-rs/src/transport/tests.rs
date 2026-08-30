//! Cross-cutting transport tests.
//!
//! The centrepiece is [`the_same_assertions_hold_over_tls_and_in_process`]: one
//! table of request and expectation, run twice, once through a real TLS socket
//! and once through [`Core::call`] with no listener. It is the only automated
//! guard on the premise that mobile embeds the real core, so routing, auth or
//! error handling drifting into `server.rs` fails the in-process run.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use tokio::sync::watch;
use tokio_tungstenite::tungstenite::Message;

use crate::transport::testing::{context_with, StubAuth};
use crate::transport::{inproc::InProcRequest, tls, ClientIdentity, Core, InProcResponse};

const TOKEN: &str = "token-abc";
const CLIENT: &str = "client-1";

fn core() -> Arc<Core> {
    let auth = Arc::new(
        StubAuth::new()
            .with_client(TOKEN, CLIENT)
            .with_admin_token("admin-secret")
            .with_admin_password(),
    );
    // The assertions run against the StubAuth in the context; the real `Auth`
    // is here only because the pairing slice needs the concrete handle.
    let ctx = context_with(auth);
    let real = Arc::new(crate::auth::Auth::new(
        ctx.db.clone(),
        Arc::clone(&ctx.paths),
    ));
    let services = super::router::Services::unwired(&ctx, real)
        .unwrap_or_else(|e| panic!("unwired services: {e}"));
    Arc::new(Core::new(ctx, services))
}

fn local_client() -> ClientIdentity {
    ClientIdentity {
        id: CLIENT.to_string(),
        name: "local".to_string(),
        created_at_ms: 0,
        last_seen_ms: 0,
    }
}

// --- the shared assertion table -------------------------------------------

struct Case {
    name: &'static str,
    method: &'static str,
    path: &'static str,
    /// Sent as a bearer over the wire, and as a pre-authenticated identity
    /// in-process. Both mean "this request is client-1".
    authed: bool,
    want_status: u16,
    /// The `error.code` expected in the envelope, when the case is a failure.
    want_code: Option<&'static str>,
}

const CASES: &[Case] = &[
    Case {
        name: "health is public",
        method: "GET",
        path: "/api/v1/health",
        authed: false,
        want_status: 200,
        want_code: None,
    },
    Case {
        name: "an unknown path is a not_found envelope",
        method: "GET",
        path: "/api/v1/nope",
        authed: false,
        want_status: 404,
        want_code: Some("not_found"),
    },
    Case {
        name: "a known path with the wrong method is 405",
        method: "DELETE",
        path: "/api/v1/health",
        authed: false,
        want_status: 405,
        want_code: Some("not_found"),
    },
    Case {
        name: "an authed route without a bearer is missing_token",
        method: "POST",
        path: "/api/v1/ws/ticket",
        authed: false,
        want_status: 401,
        want_code: Some("missing_token"),
    },
    Case {
        name: "an authed route with one succeeds",
        method: "POST",
        path: "/api/v1/ws/ticket",
        authed: true,
        want_status: 200,
        want_code: None,
    },
];

fn check(case: &Case, transport: &str, status: u16, body: &[u8]) {
    assert_eq!(
        status,
        case.want_status,
        "[{transport}] {}: status; body was {}",
        case.name,
        String::from_utf8_lossy(body)
    );
    if let Some(code) = case.want_code {
        let value: serde_json::Value = serde_json::from_slice(body)
            .unwrap_or_else(|e| panic!("[{transport}] {}: body is not JSON: {e}", case.name));
        assert_eq!(
            value["error"]["code"], code,
            "[{transport}] {}: error code",
            case.name
        );
    }
}

#[tokio::test]
async fn the_same_assertions_hold_over_tls_and_in_process() {
    let core = core();

    // In-process: no socket, no TLS, no port.
    for case in CASES {
        let mut req = InProcRequest::get(case.path);
        req.method = case.method.to_string();
        if case.authed {
            req = req.as_client(local_client());
        }
        let res: InProcResponse = core.call(req).await.unwrap();
        check(case, "in-process", res.status, &res.body);
    }

    // Over the wire: the same table through TLS, hyper and the accept loop.
    let server = Server::start(Arc::clone(&core)).await;
    for case in CASES {
        let mut headers = HashMap::new();
        if case.authed {
            headers.insert("authorization".to_string(), format!("Bearer {TOKEN}"));
        }
        let (status, _, body) = server
            .request(case.method, case.path, headers, Vec::new())
            .await;
        check(case, "tls", status, &body);
    }
    server.stop().await;
}

/// A bearer is not a substitute for the in-process identity and vice versa, so
/// neither path can be used to smuggle authentication into the other.
#[tokio::test]
async fn a_wire_request_cannot_forge_the_in_process_identity() {
    let core = core();
    let server = Server::start(Arc::clone(&core)).await;

    // Extensions are not reachable from the wire at all.
    let mut headers = HashMap::new();
    headers.insert("x-client-identity".to_string(), CLIENT.to_string());
    let (status, _, body) = server
        .request("POST", "/api/v1/ws/ticket", headers, Vec::new())
        .await;
    assert_eq!(status, 401);
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["error"]["code"], "missing_token");

    // A bad bearer is invalid_token, distinct from having sent none.
    let mut headers = HashMap::new();
    headers.insert("authorization".to_string(), "Bearer nope".to_string());
    let (status, _, body) = server
        .request("POST", "/api/v1/ws/ticket", headers, Vec::new())
        .await;
    assert_eq!(status, 401);
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["error"]["code"], "invalid_token");

    server.stop().await;
}

// --- the WS path, end to end ----------------------------------------------

#[tokio::test]
async fn a_client_mints_a_ticket_upgrades_and_talks() {
    let core = core();
    let server = Server::start(Arc::clone(&core)).await;

    let ticket = server.mint_ticket().await;
    let (mut ws, _) = server.connect_ws(&ticket).await;

    // A client ping is answered.
    ws.send(Message::Text(r#"{"kind":"ping"}"#.into()))
        .await
        .unwrap();
    assert_eq!(next_text(&mut ws).await, r#"{"kind":"pong"}"#);

    // A broadcast from a service reaches it.
    core.frames()
        .broadcast_all(&crate::contract::api::ServerToClientFrame::UpdateStaged {
            version: "9.9.9".to_string(),
        });
    let frame: serde_json::Value = serde_json::from_str(&next_text(&mut ws).await).unwrap();
    assert_eq!(frame["kind"], "update.staged");
    assert_eq!(frame["version"], "9.9.9");

    // Malformed frames are dropped and the socket keeps working.
    for bad in ["not-json{", r#"{"no":"kind"}"#, r#"{"kind":""}"#] {
        ws.send(Message::Text(bad.into())).await.unwrap();
    }
    ws.send(Message::Text(r#"{"kind":"ping"}"#.into()))
        .await
        .unwrap();
    assert_eq!(next_text(&mut ws).await, r#"{"kind":"pong"}"#);

    server.stop().await;
}

#[tokio::test]
async fn a_ticket_is_single_use_and_a_bad_one_is_refused() {
    let core = core();
    let server = Server::start(Arc::clone(&core)).await;

    let ticket = server.mint_ticket().await;
    let (ws, _) = server.connect_ws(&ticket).await;
    drop(ws);

    // Replaying it fails, as does one that never existed and none at all.
    assert_eq!(server.try_connect_ws(Some(&ticket)).await, 401);
    assert_eq!(server.try_connect_ws(Some("not-real")).await, 401);
    assert_eq!(server.try_connect_ws(None).await, 401);

    server.stop().await;
}

#[tokio::test]
async fn revoking_a_client_cuts_its_live_socket() {
    let core = core();
    let server = Server::start(Arc::clone(&core)).await;
    let ticket = server.mint_ticket().await;
    let (mut ws, _) = server.connect_ws(&ticket).await;

    core.frames().close_client(
        CLIENT,
        crate::transport::ws::REVOKED_CLOSE_CODE,
        "client revoked",
    );

    let closed = loop {
        match ws.next().await {
            Some(Ok(Message::Close(frame))) => break frame,
            Some(Ok(_)) => continue,
            other => panic!("expected a close frame, got {other:?}"),
        }
    };
    let closed = closed.expect("a close code");
    assert_eq!(
        u16::from(closed.code),
        crate::transport::ws::REVOKED_CLOSE_CODE
    );
    assert_eq!(closed.reason.as_str(), "client revoked");

    server.stop().await;
}

// --- the heartbeat, end to end over a real socket -------------------------

/// The two tests below run the shipping heartbeat loop over a real TLS socket on
/// the real clock, scaled down; only the ratio to production is preserved.
///
/// **Not `start_paused = true`.** Tokio auto-advances virtual time whenever
/// every task is parked, and a task parked in epoll on bytes already in the
/// kernel looks idle: the clock jumps past the pong deadline while the pong is
/// in flight. A virtual clock is sound in `ws::tests`, which has no socket.
const HEARTBEAT: std::time::Duration = std::time::Duration::from_millis(500);

/// Shorter than [`HEARTBEAT`], as production's 10s is shorter than 25s. Load
/// bearing: [`answering_a_ping_keeps_the_socket_open`] proves the pong disarmed
/// the deadline by watching the next ping arrive, which is only evidence if an
/// armed deadline would have fired first.
const PONG_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(200);

/// A listener serving the WS routes alone, on the compressed heartbeat.
/// Mounted directly because `router::build` takes no heartbeat; the upgrade
/// through the assembled router is covered by
/// [`a_client_mints_a_ticket_upgrades_and_talks`].
async fn heartbeat_server(core: &Arc<Core>) -> Server {
    let router =
        crate::transport::ws::routes_with_heartbeat(core.context(), HEARTBEAT, PONG_TIMEOUT);
    Server::start_with_router(Arc::clone(core), router).await
}

#[tokio::test]
async fn a_client_that_never_answers_a_ping_is_closed() {
    let core = core();
    let server = heartbeat_server(&core).await;
    let ticket = server.mint_ticket().await;
    let (mut ws, _) = server.connect_ws(&ticket).await;

    // The heartbeat ping, one interval after the socket opened.
    assert_eq!(next_text(&mut ws).await, r#"{"kind":"ping"}"#);

    // Ignored, so one timeout later the socket is closed with 4002.
    let closed = loop {
        match ws.next().await {
            Some(Ok(Message::Close(frame))) => break frame.expect("a close code"),
            Some(Ok(_)) => continue,
            other => panic!("expected a close frame, got {other:?}"),
        }
    };
    assert_eq!(u16::from(closed.code), 4002);
    assert_eq!(closed.reason.as_str(), "pong timeout");

    server.stop().await;
}

#[tokio::test]
async fn answering_a_ping_keeps_the_socket_open() {
    let core = core();
    let server = heartbeat_server(&core).await;
    let ticket = server.mint_ticket().await;
    let (mut ws, _) = server.connect_ws(&ticket).await;

    assert_eq!(next_text(&mut ws).await, r#"{"kind":"ping"}"#);
    ws.send(Message::Text(r#"{"kind":"pong"}"#.into()))
        .await
        .unwrap();

    // The pong disarmed the deadline, so the next arrival is the following
    // ping. The close arm is named so the regression reads as itself rather
    // than as "expected text, got Err(BrokenPipe)".
    match ws.next().await {
        Some(Ok(Message::Text(text))) => assert_eq!(text.as_str(), r#"{"kind":"ping"}"#),
        Some(Ok(Message::Close(frame))) => panic!(
            "the pong did not disarm the deadline: closed with {:?}",
            frame.map(|f| (u16::from(f.code), f.reason.to_string()))
        ),
        other => panic!("expected the next ping, got {other:?}"),
    }

    server.stop().await;
}

/// A connection registered before any socket exists is what the mobile frontend
/// does, and the bus cannot tell the two apart.
#[tokio::test]
async fn an_in_process_connection_receives_the_same_broadcasts() {
    let core = core();
    let mut conn = core.connect(CLIENT);

    core.frames().broadcast_to_client(
        CLIENT,
        &crate::contract::api::ServerToClientFrame::UpdateStaged {
            version: "1.2.3".to_string(),
        },
    );

    match conn.next().await.unwrap() {
        crate::transport::Outbound::Frame(json) => {
            let value: serde_json::Value = serde_json::from_str(&json).unwrap();
            assert_eq!(value["kind"], "update.staged");
            assert_eq!(value["version"], "1.2.3");
        }
        other => panic!("expected a frame, got {other:?}"),
    }
}

// --- TLS ------------------------------------------------------------------

/// The pin computed the way the client computes it: parse the presented
/// certificate, take its SubjectPublicKeyInfo, hash, encode.
#[tokio::test]
async fn the_served_certificate_hashes_to_the_pin_the_core_reports() {
    use base64::engine::general_purpose::STANDARD as B64;
    use base64::Engine;
    use sha2::{Digest, Sha256};

    let core = core();
    let server = Server::start(Arc::clone(&core)).await;

    // The capture happens during a handshake, so there has to have been one.
    let (status, _, _) = server
        .request("GET", "/api/v1/health", HashMap::new(), Vec::new())
        .await;
    assert_eq!(status, 200);

    let presented = server.captured_cert.lock().unwrap().clone();
    let presented = presented.expect("a certificate was presented during the handshake");
    let (_, parsed) = x509_parser::prelude::parse_x509_certificate(&presented).unwrap();
    let spki = parsed.tbs_certificate.subject_pki.raw;
    let pin = B64.encode(Sha256::digest(spki));

    assert_eq!(
        pin, server.pin,
        "the pin a client derives from the served certificate must equal the one core reports",
    );
    server.stop().await;
}

// --- the harness ----------------------------------------------------------

/// A real core on a real TLS socket, with a client that captures the presented
/// certificate the way the Tauri client's capture mode does.
struct Server {
    core: Arc<Core>,
    addr: std::net::SocketAddr,
    pin: String,
    captured_cert: Arc<std::sync::Mutex<Option<Vec<u8>>>>,
    shutdown: watch::Sender<bool>,
    joined: Option<tokio::task::JoinHandle<()>>,
}

impl Server {
    async fn start(core: Arc<Core>) -> Self {
        let router = core.router();
        Self::start_with_router(core, router).await
    }

    /// The same listener over a caller-supplied router.
    async fn start_with_router(core: Arc<Core>, router: axum::Router) -> Self {
        let (material, _) = tls::ensure(None, "127.0.0.1").unwrap();
        let pin = material.pin.clone();
        let listener = core.bind(&material, "127.0.0.1", 0).await.unwrap();
        let addr = listener.local_addr();

        let (shutdown, rx) = watch::channel(false);
        let joined = tokio::spawn(async move { listener.serve(router, rx).await });

        Self {
            core,
            addr,
            pin,
            captured_cert: Arc::new(std::sync::Mutex::new(None)),
            shutdown,
            joined: Some(joined),
        }
    }

    async fn stop(mut self) {
        self.core.shutdown();
        let _ = self.shutdown.send(true);
        if let Some(handle) = self.joined.take() {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), handle).await;
        }
    }

    fn client_config(&self) -> rustls::ClientConfig {
        let provider = crate::transport::server::provider();
        rustls::ClientConfig::builder_with_provider(Arc::clone(&provider))
            .with_safe_default_protocol_versions()
            .unwrap()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(CaptureVerifier {
                captured: Arc::clone(&self.captured_cert),
                provider,
            }))
            .with_no_client_auth()
    }

    /// One HTTP/1.1 request, hand-written so the test depends on nothing but
    /// the TLS stack the core actually serves.
    async fn request(
        &self,
        method: &str,
        path: &str,
        headers: HashMap<String, String>,
        body: Vec<u8>,
    ) -> (u16, HashMap<String, String>, Vec<u8>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let stream = tokio::net::TcpStream::connect(self.addr).await.unwrap();
        let connector = tokio_rustls::TlsConnector::from(Arc::new(self.client_config()));
        let server_name = rustls::pki_types::ServerName::try_from("localhost").unwrap();
        let mut tls = connector.connect(server_name, stream).await.unwrap();

        let mut request =
            format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n");
        for (k, v) in &headers {
            request.push_str(&format!("{k}: {v}\r\n"));
        }
        request.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
        tls.write_all(request.as_bytes()).await.unwrap();
        tls.write_all(&body).await.unwrap();
        tls.flush().await.unwrap();

        // Read to EOF, tolerating a `Connection: close` response ending
        // without a TLS close_notify, which rustls reports as
        // `UnexpectedEof`.
        let mut raw = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            match tls.read(&mut chunk).await {
                Ok(0) => break,
                Ok(n) => raw.extend_from_slice(&chunk[..n]),
                Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(err) => panic!("read failed: {err}"),
            }
        }
        parse_response(&raw)
    }

    async fn mint_ticket(&self) -> String {
        let mut headers = HashMap::new();
        headers.insert("authorization".to_string(), format!("Bearer {TOKEN}"));
        let (status, _, body) = self
            .request("POST", "/api/v1/ws/ticket", headers, Vec::new())
            .await;
        assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        value["ticket"].as_str().unwrap().to_string()
    }

    async fn connect_ws(
        &self,
        ticket: &str,
    ) -> (
        TestWs,
        tokio_tungstenite::tungstenite::handshake::client::Response,
    ) {
        let url = format!("wss://localhost:{}/ws/v1?ticket={ticket}", self.addr.port());
        tokio_tungstenite::connect_async_tls_with_config(
            url,
            None,
            false,
            Some(tokio_tungstenite::Connector::Rustls(Arc::new(
                self.client_config(),
            ))),
        )
        .await
        .expect("upgrade")
    }

    /// The status a refused upgrade came back with.
    async fn try_connect_ws(&self, ticket: Option<&str>) -> u16 {
        let path = match ticket {
            Some(t) => format!("/ws/v1?ticket={t}"),
            None => "/ws/v1".to_string(),
        };
        let mut headers = HashMap::new();
        headers.insert("upgrade".to_string(), "websocket".to_string());
        let (status, _, _) = self.request("GET", &path, headers, Vec::new()).await;
        status
    }
}

fn parse_response(raw: &[u8]) -> (u16, HashMap<String, String>, Vec<u8>) {
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("a header terminator");
    let head = String::from_utf8_lossy(&raw[..split]).to_string();
    let mut lines = head.lines();
    let status: u16 = lines
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .expect("a status line");
    let headers: HashMap<String, String> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_lowercase(), v.trim().to_string()))
        .collect();
    let body = &raw[split + 4..];
    let body = if headers
        .get("transfer-encoding")
        .is_some_and(|v| v.contains("chunked"))
    {
        dechunk(body)
    } else {
        body.to_vec()
    };
    (status, headers, body)
}

/// Minimal chunked decoding: which framing hyper picks is its choice, and a
/// test comparing a chunk header against JSON would fail confusingly.
fn dechunk(mut body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(eol) = body.windows(2).position(|w| w == b"\r\n") {
        let size = usize::from_str_radix(
            String::from_utf8_lossy(&body[..eol])
                .split(';')
                .next()
                .unwrap_or("0")
                .trim(),
            16,
        )
        .unwrap_or(0);
        if size == 0 {
            break;
        }
        let start = eol + 2;
        out.extend_from_slice(&body[start..start + size]);
        body = &body[start + size + 2..];
    }
    out
}

type TestWs =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn next_text(ws: &mut TestWs) -> String {
    loop {
        match ws.next().await {
            Some(Ok(Message::Text(text))) => return text.to_string(),
            Some(Ok(_)) => continue,
            other => panic!("expected a text frame, got {other:?}"),
        }
    }
}

/// Accepts any certificate and records it, as the client's pairing probe does
/// before the PAKE binds the pin.
#[derive(Debug)]
struct CaptureVerifier {
    captured: Arc<std::sync::Mutex<Option<Vec<u8>>>>,
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl rustls::client::danger::ServerCertVerifier for CaptureVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        *self.captured.lock().unwrap() = Some(end_entity.to_vec());
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}
