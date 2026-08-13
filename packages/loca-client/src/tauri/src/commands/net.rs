//! Trust-scoped network access to a paired core.
//!
//! The webview can't control certificate trust (browser fetch/WebSocket expose
//! no pinning or verifier API), so all core traffic is terminated here in Rust
//! with a per-request trust mode (`TlsTrust`). HTTP uses reqwest; the WebSocket
//! uses tokio-tungstenite. Both share `build_client_config`.
//!
//! The three modes, one per paired-core trust posture:
//!   - `Pin`: enforce the exact SubjectPublicKeyInfo SHA-256 pin captured at
//!     pairing (the default; a self-signed core the Client secures itself).
//!   - `WebPki`: standard public-CA validation (chain + hostname + expiry)
//!     against the Mozilla root set, for a core served over HTTPS behind a
//!     terminating reverse proxy. No pin.
//!   - `Capture`: accept whatever cert is presented and report its pin, used
//!     ONLY by the unpaired pairing/discovery probe. The PAKE key confirmation
//!     then binds that pin, so a MITM is caught at the application layer.
//!
//! There is deliberately no "no verifier / accept-any" resting state: a paired
//! connection is always `Pin` or `WebPki`, never `Capture`.

use crate::error::{AppError, AppResult};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::client::WebPkiServerVerifier;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use tauri::{AppHandle, Emitter};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async_tls_with_config, Connector};

// One shared crypto provider (rustls' default: aws-lc-rs). The pin verifier
// delegates signature checks to it, so the TLS handshake still proves the
// server holds the cert's private key; we only override chain/name validation.
static PROVIDER: LazyLock<Arc<rustls::crypto::CryptoProvider>> =
    LazyLock::new(|| Arc::new(rustls::crypto::aws_lc_rs::default_provider()));

// Live WebSocket senders, keyed by the JS-generated wsId.
static WS_HANDLES: LazyLock<Mutex<HashMap<String, mpsc::UnboundedSender<WsOut>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

enum WsOut {
    Text(String),
    Close,
}

// --- pin verifier ---------------------------------------------------------

#[derive(Debug)]
struct SpkiPinVerifier {
    /// Expected SHA-256 of the cert's SPKI. `None` = capture mode (accept any).
    pin: Option<[u8; 32]>,
    /// Filled with the presented cert's SPKI hash on every handshake.
    captured: Arc<Mutex<Option<[u8; 32]>>>,
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl ServerCertVerifier for SpkiPinVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let hash = spki_sha256(end_entity)
            .ok_or_else(|| rustls::Error::General("certificate parse failed".into()))?;
        if let Ok(mut g) = self.captured.lock() {
            *g = Some(hash);
        }
        match self.pin {
            Some(expected) if constant_time_eq(&expected, &hash) => {
                Ok(ServerCertVerified::assertion())
            }
            Some(_) => Err(rustls::Error::General("certificate pin mismatch".into())),
            None => Ok(ServerCertVerified::assertion()), // TOFU capture mode
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
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
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

fn spki_sha256(cert: &CertificateDer<'_>) -> Option<[u8; 32]> {
    use x509_parser::prelude::*;
    let (_, parsed) = X509Certificate::from_der(cert.as_ref()).ok()?;
    let mut h = Sha256::new();
    h.update(parsed.public_key().raw);
    Some(h.finalize().into())
}

fn constant_time_eq(a: &[u8; 32], b: &[u8; 32]) -> bool {
    let mut diff = 0u8;
    for i in 0..32 {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

fn parse_pin(pin: Option<String>) -> AppResult<Option<[u8; 32]>> {
    match pin {
        None => Ok(None),
        Some(s) => {
            let bytes = B64
                .decode(s.as_bytes())
                .map_err(|e| AppError::validation(format!("bad pin: {e}")))?;
            let arr: [u8; 32] = bytes
                .try_into()
                .map_err(|_| AppError::validation("pin must be 32 bytes (SHA-256)"))?;
            Ok(Some(arr))
        }
    }
}

/// The trust posture for one connection. `Pin` and `WebPki` are the two
/// paired-core modes; `Capture` (accept-any + report the SPKI) is reachable only
/// from the unpaired probe/discovery paths. There is intentionally no variant
/// that accepts any cert without reporting - a paired request that failed to
/// carry a mode is an error, not a silent accept-any.
enum TlsTrust {
    Pin([u8; 32]),
    WebPki,
    Capture(Arc<Mutex<Option<[u8; 32]>>>),
}

/// Map the IPC `(mode, pin)` pair to a `TlsTrust`. `pin` mode requires a pin;
/// an unknown mode is rejected. `captured` is the per-connection cell the
/// capture verifier writes the presented SPKI into.
fn trust_from(
    mode: &str,
    pin: Option<String>,
    captured: &Arc<Mutex<Option<[u8; 32]>>>,
) -> AppResult<TlsTrust> {
    match mode {
        "pin" => {
            let hash =
                parse_pin(pin)?.ok_or_else(|| AppError::validation("pin mode requires a pin"))?;
            Ok(TlsTrust::Pin(hash))
        }
        "webpki" => Ok(TlsTrust::WebPki),
        "capture" => Ok(TlsTrust::Capture(captured.clone())),
        other => Err(AppError::validation(format!("unknown tls mode: {other}"))),
    }
}

/// Standard public-CA verifier: full chain + hostname + expiry validation
/// against the Mozilla root set. Restricting to public roots (not the OS store)
/// keeps `webpki` mode to genuinely public HTTPS and avoids trusting an
/// OS-installed interception root.
fn webpki_verifier() -> AppResult<Arc<dyn ServerCertVerifier>> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let verifier: Arc<dyn ServerCertVerifier> =
        WebPkiServerVerifier::builder_with_provider(Arc::new(roots), PROVIDER.clone())
            .build()
            .map_err(|e| AppError::external(format!("webpki verifier: {e}")))?;
    Ok(verifier)
}

fn build_client_config(trust: TlsTrust) -> AppResult<ClientConfig> {
    let verifier: Arc<dyn ServerCertVerifier> = match trust {
        TlsTrust::Pin(hash) => Arc::new(SpkiPinVerifier {
            pin: Some(hash),
            captured: Arc::new(Mutex::new(None)),
            provider: PROVIDER.clone(),
        }),
        TlsTrust::Capture(captured) => Arc::new(SpkiPinVerifier {
            pin: None,
            captured,
            provider: PROVIDER.clone(),
        }),
        TlsTrust::WebPki => webpki_verifier()?,
    };
    Ok(ClientConfig::builder_with_provider(PROVIDER.clone())
        .with_safe_default_protocol_versions()
        .map_err(|e| AppError::external(format!("tls config: {e}")))?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth())
}

// --- HTTP -----------------------------------------------------------------

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetFetchReply {
    status: u16,
    headers: HashMap<String, String>,
    body_b64: String,
    captured_pin: Option<String>,
}

/// One HTTP request under a trust `mode` ("pin" | "webpki" | "capture").
/// `body_b64`/the reply body are base64 to cross IPC. `captured_pin` is filled
/// only in capture mode (the unpaired probe).
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn net_fetch(
    url: String,
    method: String,
    headers: HashMap<String, String>,
    body_b64: Option<String>,
    mode: String,
    pin: Option<String>,
) -> AppResult<NetFetchReply> {
    let captured = Arc::new(Mutex::new(None));
    let trust = trust_from(&mode, pin, &captured)?;
    let is_capture = matches!(trust, TlsTrust::Capture(_));
    let tls = build_client_config(trust)?;
    let client = reqwest::Client::builder()
        .use_preconfigured_tls(tls)
        // Without explicit timeouts a stalled core (e.g. a wedged TLS handshake
        // or a peer that accepts but never replies) hangs `send().await`
        // forever. On the boot path this awaited request gates the window-show
        // and first render (see +page.svelte onMount), so an unbounded hang
        // leaves the app running with no window. Bound both phases.
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|e| AppError::external(format!("http client: {e}")))?;

    let method = reqwest::Method::from_bytes(method.as_bytes())
        .map_err(|e| AppError::validation(format!("bad method: {e}")))?;
    let mut header_map = reqwest::header::HeaderMap::new();
    for (k, v) in &headers {
        // Fail the call on an unparseable header rather than silently dropping it:
        // a dropped header could omit an intended Authorization/content-type, so
        // the caller must see the error instead of a request that quietly lacks it.
        let name = reqwest::header::HeaderName::from_bytes(k.as_bytes())
            .map_err(|e| AppError::validation(format!("bad header name {k:?}: {e}")))?;
        let val = reqwest::header::HeaderValue::from_str(v)
            .map_err(|e| AppError::validation(format!("bad header value for {k:?}: {e}")))?;
        header_map.insert(name, val);
    }
    let mut req = client.request(method, &url).headers(header_map);
    if let Some(b64) = body_b64 {
        req = req.body(
            B64.decode(b64.as_bytes())
                .map_err(|e| AppError::validation(format!("bad body: {e}")))?,
        );
    }

    let resp = req
        .send()
        .await
        .map_err(|e| AppError::external(format!("request failed: {e}")))?;
    let status = resp.status().as_u16();
    let mut out_headers = HashMap::new();
    for (k, v) in resp.headers() {
        if let Ok(s) = v.to_str() {
            out_headers.insert(k.as_str().to_lowercase(), s.to_string());
        }
    }
    let body = resp
        .bytes()
        .await
        .map_err(|e| AppError::external(format!("read body: {e}")))?;

    let captured_pin = if is_capture {
        captured.lock().ok().and_then(|g| *g).map(|h| B64.encode(h))
    } else {
        None
    };

    Ok(NetFetchReply {
        status,
        headers: out_headers,
        body_b64: B64.encode(&body),
        captured_pin,
    })
}

// --- LAN discovery --------------------------------------------------------
//
// The "ping" button on the remote-pairing screen sweeps the local network for
// reachable cores. Each candidate host is probed at the unauthenticated
// `GET /api/v1/health` endpoint over a TOFU TLS connection (capture mode, same
// as the pairing handshake): a 2xx means a core is there. We read its reported
// version and capture its cert SPKI pin for display only. Nothing is trusted
// here: pairing still runs the full PAKE + pin-binding flow afterwards, so this
// adds no new trust surface, only a convenience that finds the address.
//
// Desktop-only: it relies on interface enumeration (local-ip-address), which is
// a desktop dependency. The mobile Platform impl returns an empty list.

#[cfg(desktop)]
use std::net::Ipv4Addr;

/// The known per-channel core ports (stable / latest / dev). We probe all three
/// so a client on one channel still finds a core running on another; pairing
/// itself is channel-agnostic. Mirrors channel.rs `core_port_for()`.
#[cfg(desktop)]
const KNOWN_CORE_PORTS: [u16; 3] = [7800, 7810, 7820];

/// Bounded in-flight probes for the sweep. A /24 has 254 hosts * 3 ports;
/// most are dead and fail the short-timeout connect fast, so this keeps the
/// whole sweep to a couple of seconds without flooding the network at once.
#[cfg(desktop)]
const DISCOVERY_CONCURRENCY: usize = 128;

/// Per-host probe timeout. Short so a /24 full of dead hosts still finishes
/// quickly; a live core on the LAN answers well within this.
#[cfg(desktop)]
const DISCOVERY_TIMEOUT_MS: u64 = 400;

#[cfg(desktop)]
#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredCore {
    base_url: String,
    version: String,
    pin: String,
}

#[cfg(desktop)]
#[derive(serde::Deserialize)]
struct HealthBody {
    version: Option<String>,
}

/// Sweep the local network for reachable cores. Probes this machine's /24(s)
/// plus loopback at the known core ports and returns one entry per distinct
/// core (deduped by cert pin).
#[cfg(desktop)]
#[tauri::command]
pub async fn discover_lan_cores() -> AppResult<Vec<DiscoveredCore>> {
    use futures_util::stream::{self, StreamExt};
    let targets = discovery_targets(local_ipv4_addrs());
    let found: Vec<DiscoveredCore> = stream::iter(targets)
        .map(|(ip, port)| async move { probe_core_health(ip, port).await })
        .buffer_unordered(DISCOVERY_CONCURRENCY)
        .filter_map(|r| async move { r })
        .collect()
        .await;
    Ok(dedupe_by_pin(found))
}

/// This machine's non-loopback IPv4 interface addresses. Empty on enumeration
/// failure (the sweep then still covers loopback for the local/dev core).
#[cfg(desktop)]
fn local_ipv4_addrs() -> Vec<Ipv4Addr> {
    local_ip_address::list_afinet_netifas()
        .map(|ifaces| {
            ifaces
                .into_iter()
                .filter_map(|(_, ip)| match ip {
                    std::net::IpAddr::V4(v4)
                        if !v4.is_loopback() && !v4.is_unspecified() && !v4.is_link_local() =>
                    {
                        Some(v4)
                    }
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Build the (host, port) candidate list: loopback plus every host in the /24
/// of each local interface, crossed with the known core ports. Pure so the
/// host math can be unit-tested without touching the network.
#[cfg(desktop)]
fn discovery_targets(local: Vec<Ipv4Addr>) -> Vec<(Ipv4Addr, u16)> {
    let mut hosts: Vec<Ipv4Addr> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    // Loopback always, so the local (and dev) core shows even with no LAN.
    push_host(&mut hosts, &mut seen, Ipv4Addr::LOCALHOST);
    for ip in local {
        let o = ip.octets();
        for h in 1u8..=254 {
            push_host(&mut hosts, &mut seen, Ipv4Addr::new(o[0], o[1], o[2], h));
        }
    }
    let mut targets = Vec::with_capacity(hosts.len() * KNOWN_CORE_PORTS.len());
    for ip in hosts {
        for &port in &KNOWN_CORE_PORTS {
            targets.push((ip, port));
        }
    }
    targets
}

#[cfg(desktop)]
fn push_host(
    hosts: &mut Vec<Ipv4Addr>,
    seen: &mut std::collections::HashSet<Ipv4Addr>,
    ip: Ipv4Addr,
) {
    if seen.insert(ip) {
        hosts.push(ip);
    }
}

/// Probe one host:port at /api/v1/health over a TOFU TLS connection. Returns a
/// `DiscoveredCore` only on a 2xx whose cert pin we could capture.
#[cfg(desktop)]
async fn probe_core_health(ip: Ipv4Addr, port: u16) -> Option<DiscoveredCore> {
    let captured = Arc::new(Mutex::new(None));
    // A fresh client per probe: the capture cell is per-connection, so reusing
    // one client across hosts would race their pins onto the same cell.
    let tls = build_client_config(TlsTrust::Capture(captured.clone())).ok()?;
    let client = reqwest::Client::builder()
        .use_preconfigured_tls(tls)
        .connect_timeout(std::time::Duration::from_millis(DISCOVERY_TIMEOUT_MS))
        .timeout(std::time::Duration::from_millis(DISCOVERY_TIMEOUT_MS))
        .build()
        .ok()?;
    let base_url = format!("https://{ip}:{port}");
    let resp = client
        .get(format!("{base_url}/api/v1/health"))
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body = resp.bytes().await.ok()?;
    let version = serde_json::from_slice::<HealthBody>(&body)
        .ok()
        .and_then(|b| b.version)
        .unwrap_or_else(|| "unknown".to_string());
    let pin = captured
        .lock()
        .ok()
        .and_then(|g| *g)
        .map(|h| B64.encode(h))?;
    Some(DiscoveredCore {
        base_url,
        version,
        pin,
    })
}

/// Collapse responders that share a cert pin (the local core answers on both
/// loopback and its own LAN IP) into one entry, preferring a non-loopback
/// address so the user pairs with a reachable "remote" URL. Sorted for a
/// stable display order.
#[cfg(desktop)]
fn dedupe_by_pin(found: Vec<DiscoveredCore>) -> Vec<DiscoveredCore> {
    let mut by_pin: HashMap<String, DiscoveredCore> = HashMap::new();
    for core in found {
        match by_pin.get(&core.pin) {
            Some(existing) if !is_loopback_url(&existing.base_url) => {}
            _ => {
                by_pin.insert(core.pin.clone(), core);
            }
        }
    }
    let mut out: Vec<DiscoveredCore> = by_pin.into_values().collect();
    out.sort_by(|a, b| a.base_url.cmp(&b.base_url));
    out
}

#[cfg(desktop)]
fn is_loopback_url(base_url: &str) -> bool {
    base_url.contains("127.0.0.1")
}

// --- WebSocket ------------------------------------------------------------

fn ev(ws_id: &str, kind: &str) -> String {
    format!("net://ws/{ws_id}/{kind}")
}

/// Open a pinned WebSocket. Frames are forwarded to JS as per-id Tauri events
/// (`net://ws/<id>/{open,message,close,error}`); send/close go through the
/// channel stored under `ws_id`. Returns immediately; connect runs in the task.
#[tauri::command]
pub async fn net_ws_open(
    app: AppHandle,
    ws_id: String,
    url: String,
    mode: String,
    pin: Option<String>,
) -> AppResult<()> {
    let captured = Arc::new(Mutex::new(None));
    let trust = trust_from(&mode, pin, &captured)?;
    // The WebSocket carries live traffic and the bearer token; it must never run
    // in accept-any capture mode (that is only the unpaired probe).
    if matches!(trust, TlsTrust::Capture(_)) {
        return Err(AppError::validation("websocket cannot use capture mode"));
    }
    let tls = build_client_config(trust)?;
    let (tx, mut rx) = mpsc::unbounded_channel::<WsOut>();
    if let Ok(mut m) = WS_HANDLES.lock() {
        m.insert(ws_id.clone(), tx);
    }

    tokio::spawn(async move {
        let connector = Connector::Rustls(Arc::new(tls));
        let stream = match connect_async_tls_with_config(&url, None, false, Some(connector)).await {
            Ok((s, _)) => s,
            Err(e) => {
                // Surface the connect failure reason so the client can show it in
                // the reconnect banner instead of a generic message. For an HTTP
                // handshake rejection (e.g. 401 when the core no longer recognizes
                // our bearer token) include the status code: the client uses it to
                // halt the otherwise-futile reconnect loop and prompt a re-pair.
                let reason = match &e {
                    tokio_tungstenite::tungstenite::Error::Http(resp) => {
                        format!(
                            "server rejected connection: HTTP {}",
                            resp.status().as_u16()
                        )
                    }
                    other => other.to_string(),
                };
                let _ = app.emit(&ev(&ws_id, "error"), reason);
                let _ = app.emit(&ev(&ws_id, "close"), ());
                remove_handle(&ws_id);
                return;
            }
        };
        let _ = app.emit(&ev(&ws_id, "open"), ());
        let (mut write, mut read) = stream.split();
        loop {
            tokio::select! {
                out = rx.recv() => match out {
                    Some(WsOut::Text(s)) => {
                        if write.send(Message::Text(s.into())).await.is_err() { break; }
                    }
                    Some(WsOut::Close) | None => {
                        let _ = write.send(Message::Close(None)).await;
                        break;
                    }
                },
                msg = read.next() => match msg {
                    Some(Ok(Message::Text(t))) => {
                        let _ = app.emit(&ev(&ws_id, "message"), t.to_string());
                    }
                    Some(Ok(Message::Ping(p))) => {
                        let _ = write.send(Message::Pong(p)).await;
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {}
                    Some(Err(_)) => break,
                },
            }
        }
        remove_handle(&ws_id);
        let _ = app.emit(&ev(&ws_id, "close"), ());
    });

    Ok(())
}

#[tauri::command]
pub fn net_ws_send(ws_id: String, data: String) -> AppResult<()> {
    if let Ok(m) = WS_HANDLES.lock() {
        if let Some(tx) = m.get(&ws_id) {
            let _ = tx.send(WsOut::Text(data));
        }
    }
    Ok(())
}

#[tauri::command]
pub fn net_ws_close(ws_id: String) -> AppResult<()> {
    if let Ok(m) = WS_HANDLES.lock() {
        if let Some(tx) = m.get(&ws_id) {
            let _ = tx.send(WsOut::Close);
        }
    }
    Ok(())
}

fn remove_handle(ws_id: &str) {
    if let Ok(mut m) = WS_HANDLES.lock() {
        m.remove(ws_id);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use rustls::pki_types::{ServerName, UnixTime};

    // A fresh self-signed ECDSA cert (rcgen), as the wire `CertificateDer`.
    fn self_signed() -> CertificateDer<'static> {
        rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_string()])
            .unwrap()
            .cert
            .der()
            .clone()
    }

    fn make_verifier(
        pin: Option<[u8; 32]>,
        captured: Arc<Mutex<Option<[u8; 32]>>>,
    ) -> SpkiPinVerifier {
        SpkiPinVerifier {
            pin,
            captured,
            provider: PROVIDER.clone(),
        }
    }

    fn run_verify(v: &SpkiPinVerifier, der: &CertificateDer<'_>) -> Result<(), rustls::Error> {
        let name = ServerName::try_from("127.0.0.1").unwrap();
        v.verify_server_cert(der, &[], &name, &[], UnixTime::now())
            .map(|_| ())
    }

    #[test]
    fn capture_mode_records_the_presented_spki_pin() {
        let der = self_signed();
        let captured = Arc::new(Mutex::new(None));
        let v = make_verifier(None, captured.clone());
        assert!(run_verify(&v, &der).is_ok());
        assert_eq!(
            captured.lock().unwrap().unwrap(),
            spki_sha256(&der).unwrap()
        );
    }

    #[test]
    fn pin_mode_accepts_match_and_rejects_mismatch() {
        let der = self_signed();
        let pin = spki_sha256(&der).unwrap();
        // Matching pin → accepted.
        let good = make_verifier(Some(pin), Arc::new(Mutex::new(None)));
        assert!(run_verify(&good, &der).is_ok());
        // One flipped byte (the MITM-presents-a-different-cert case) → rejected.
        let mut wrong = pin;
        wrong[0] ^= 0xff;
        let bad = make_verifier(Some(wrong), Arc::new(Mutex::new(None)));
        assert!(run_verify(&bad, &der).is_err());
    }

    #[test]
    fn webpki_rejects_a_self_signed_cert() {
        // A self-signed core cert chains to no public root, so webpki mode (used
        // only behind a real HTTPS proxy) must reject it. This is the guard that
        // a self-signed core can never be silently reached in webpki mode.
        let der = self_signed();
        let verifier = webpki_verifier().unwrap();
        let name = ServerName::try_from("127.0.0.1").unwrap();
        let res = verifier.verify_server_cert(&der, &[], &name, &[], UnixTime::now());
        assert!(res.is_err());
    }

    #[test]
    fn trust_from_maps_modes_and_requires_a_pin() {
        let cell = Arc::new(Mutex::new(None));
        // pin mode without a pin is an error (never a silent accept-any).
        assert!(trust_from("pin", None, &cell).is_err());
        let pin = B64.encode([9u8; 32]);
        assert!(matches!(
            trust_from("pin", Some(pin), &cell).unwrap(),
            TlsTrust::Pin(_)
        ));
        assert!(matches!(
            trust_from("webpki", None, &cell).unwrap(),
            TlsTrust::WebPki
        ));
        assert!(matches!(
            trust_from("capture", None, &cell).unwrap(),
            TlsTrust::Capture(_)
        ));
        // An unknown/empty mode is rejected outright.
        assert!(trust_from("bogus", None, &cell).is_err());
        assert!(trust_from("", None, &cell).is_err());
    }

    #[test]
    fn parse_pin_roundtrips_a_32_byte_pin() {
        let raw = [7u8; 32];
        assert_eq!(parse_pin(Some(B64.encode(raw))).unwrap(), Some(raw));
    }

    #[test]
    fn parse_pin_none_is_none() {
        assert_eq!(parse_pin(None).unwrap(), None);
    }

    #[test]
    fn parse_pin_rejects_wrong_length() {
        // 31 bytes is not a SHA-256 digest.
        assert!(parse_pin(Some(B64.encode([0u8; 31]))).is_err());
    }

    #[test]
    fn parse_pin_rejects_non_base64() {
        assert!(parse_pin(Some("not base64!!".to_string())).is_err());
    }

    #[test]
    fn constant_time_eq_distinguishes_pins() {
        let a = [1u8; 32];
        let mut b = [1u8; 32];
        assert!(constant_time_eq(&a, &b));
        b[31] = 2;
        assert!(!constant_time_eq(&a, &b));
    }

    #[cfg(desktop)]
    fn core(base_url: &str, pin: &str) -> DiscoveredCore {
        DiscoveredCore {
            base_url: base_url.to_string(),
            version: "0.1.0".to_string(),
            pin: pin.to_string(),
        }
    }

    #[cfg(desktop)]
    #[test]
    fn discovery_targets_cover_loopback_and_each_interface_slash_24() {
        let local = vec![Ipv4Addr::new(192, 168, 1, 50)];
        let targets = discovery_targets(local);
        // (1 loopback + 254 LAN hosts) * 3 ports.
        assert_eq!(targets.len(), (1 + 254) * KNOWN_CORE_PORTS.len());
        // Loopback is probed at every known port.
        for &port in &KNOWN_CORE_PORTS {
            assert!(targets.contains(&(Ipv4Addr::LOCALHOST, port)));
        }
        // The /24 spans .1..=.254, excluding the network (.0) and broadcast (.255).
        assert!(targets.contains(&(Ipv4Addr::new(192, 168, 1, 1), 7800)));
        assert!(targets.contains(&(Ipv4Addr::new(192, 168, 1, 254), 7800)));
        assert!(!targets
            .iter()
            .any(|(ip, _)| *ip == Ipv4Addr::new(192, 168, 1, 0)));
        assert!(!targets
            .iter()
            .any(|(ip, _)| *ip == Ipv4Addr::new(192, 168, 1, 255)));
    }

    #[cfg(desktop)]
    #[test]
    fn discovery_targets_dedupe_overlapping_interfaces() {
        // Two interfaces in the same /24 must not double-probe the same host.
        let local = vec![Ipv4Addr::new(10, 0, 0, 5), Ipv4Addr::new(10, 0, 0, 9)];
        let targets = discovery_targets(local);
        assert_eq!(targets.len(), (1 + 254) * KNOWN_CORE_PORTS.len());
    }

    #[cfg(desktop)]
    #[test]
    fn dedupe_by_pin_prefers_lan_address_over_loopback() {
        let found = vec![
            core("https://127.0.0.1:7820", "PIN_A"),
            core("https://192.168.1.50:7820", "PIN_A"),
            core("https://192.168.1.77:7800", "PIN_B"),
        ];
        let out = dedupe_by_pin(found);
        assert_eq!(out.len(), 2);
        let a = out.iter().find(|c| c.pin == "PIN_A").unwrap();
        assert_eq!(a.base_url, "https://192.168.1.50:7820"); // LAN wins over loopback
    }

    #[cfg(desktop)]
    #[test]
    fn dedupe_by_pin_keeps_loopback_when_thats_all_there_is() {
        let out = dedupe_by_pin(vec![core("https://127.0.0.1:7820", "PIN_A")]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].base_url, "https://127.0.0.1:7820");
    }
}
