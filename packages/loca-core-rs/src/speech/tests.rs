//! The speech suite: the seven routes, the two providers behind them, and the
//! four bounds that are one keystroke from wrong. Each test's doc names the
//! change it defends against.
//!
//! The external provider is a real loopback HTTP server rather than a mocked
//! client, so the request that would go to a user's endpoint is asserted as
//! bytes on a socket. Nothing here reaches the network.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::body::{to_bytes, Body};
use axum::extract::State;
use axum::http::{header, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Router;
use serde_json::{json, Value};
use tower::ServiceExt;

use crate::contract::api::{SidecarKind, SidecarSnapshot, SidecarStatus, TtsVoiceEntry};
use crate::error::Result;
use crate::settings::{Settings, SettingsRecord};
use crate::sidecars::Manager;
use crate::transport::state::AppContext;
use crate::transport::testing;

use super::engine::{AudioClip, LocalSpeech, Synthesis, Unavailable};
use super::external::{classify, Endpoint, ExternalSpeech};

const TOKEN: &str = "t0ken";
const BOUNDARY: &str = "----locaconformanceboundary";

// --- the fake external provider ----------------------------------------------

/// One request the upstream received, kept whole so a test can assert on what
/// actually went over the wire rather than on what the caller meant to send.
#[derive(Clone, Debug)]
struct Seen {
    path: String,
    authorization: Option<String>,
    body: Vec<u8>,
}

impl Seen {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).expect("upstream body was not JSON")
    }

    /// The value of one multipart part, by name. Parsed rather than
    /// substring-matched: the generated boundary is random hex, so a `contains`
    /// assertion passes or fails by luck.
    fn part(&self, name: &str) -> Option<String> {
        let text = self.text();
        let at = text.find(&format!("name=\"{name}\""))?;
        let value_at = at + text[at..].find("\r\n\r\n")? + 4;
        let end = value_at + text[value_at..].find("\r\n--")?;
        Some(text[value_at..end].to_string())
    }

    /// The `filename` the part named `name` carried, which is what makes it a
    /// file rather than a text field on the far side too.
    fn filename(&self, name: &str) -> Option<String> {
        let text = self.text();
        let at = text.find(&format!("name=\"{name}\""))?;
        let rest = &text[at..];
        let key = rest.find("filename=\"")? + "filename=\"".len();
        let end = key + rest[key..].find('"')?;
        Some(rest[key..end].to_string())
    }
}

#[derive(Default)]
struct UpstreamState {
    seen: Mutex<Vec<Seen>>,
    /// Consumed by the next request, so a test can script one failure without
    /// breaking the requests around it.
    fail_next: Mutex<Option<(u16, String)>>,
    /// Never consumed: a provider that is down for every attempt, which is what
    /// distinguishes "retried" from "retried forever".
    fail_always: Mutex<Option<(u16, String)>>,
}

struct Upstream {
    base_url: String,
    state: Arc<UpstreamState>,
}

impl Upstream {
    async fn start() -> Self {
        let state = Arc::new(UpstreamState::default());
        let app = Router::new()
            .fallback(upstream_handler)
            .with_state(Arc::clone(&state));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Self {
            base_url: format!("http://127.0.0.1:{}/v1", addr.port()),
            state,
        }
    }

    fn seen(&self) -> Vec<Seen> {
        self.state
            .seen
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn only(&self) -> Seen {
        let seen = self.seen();
        assert_eq!(
            seen.len(),
            1,
            "expected exactly one upstream call: {seen:?}"
        );
        seen[0].clone()
    }

    fn fail_next(&self, status: u16, body: &str) {
        *self
            .state
            .fail_next
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some((status, body.to_string()));
    }

    fn fail_always(&self, status: u16, body: &str) {
        *self
            .state
            .fail_always
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some((status, body.to_string()));
    }
}

/// A listener that accepts a connection and answers nothing, ever: the wedged
/// provider the client's deadline exists for. The connections are held rather
/// than dropped, since a closed socket is a transport error instead.
async fn wedged_upstream() -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let held = tokio::spawn(async move {
        let mut open = Vec::new();
        while let Ok((socket, _)) = listener.accept().await {
            open.push(socket);
        }
    });
    (format!("http://127.0.0.1:{port}/v1"), held)
}

/// A port nothing is listening on, so connecting to it fails immediately.
async fn dead_port() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

async fn upstream_handler(State(state): State<Arc<UpstreamState>>, req: Request<Body>) -> Response {
    let path = req.uri().path().to_string();
    let authorization = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let body = to_bytes(req.into_body(), 64 << 20)
        .await
        .map(|b| b.to_vec())
        .unwrap_or_default();
    state
        .seen
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(Seen {
            path: path.clone(),
            authorization,
            body,
        });

    let scripted = state
        .fail_next
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
        .or_else(|| {
            state
                .fail_always
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
        });
    if let Some((status, body)) = scripted {
        return (
            StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            [(header::CONTENT_TYPE, "application/json")],
            body,
        )
            .into_response();
    }

    match path.as_str() {
        "/v1/audio/transcriptions" => {
            axum::Json(json!({ "text": "mock transcription" })).into_response()
        }
        // A silent 44-byte WAV header is enough for a round trip.
        "/v1/audio/speech" => {
            ([(header::CONTENT_TYPE, "audio/wav")], vec![0u8; 44]).into_response()
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

// --- a local engine that answers -----------------------------------------------

/// Records what it was handed and answers with fixtures.
#[derive(Default)]
struct FakeLocal {
    calls: Mutex<Vec<String>>,
}

#[async_trait]
impl LocalSpeech for FakeLocal {
    async fn transcribe(&self, audio: AudioClip, client_id: &str) -> Result<String> {
        self.calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(format!("transcribe {} {client_id}", audio.bytes.len()));
        Ok("local transcript".to_string())
    }

    async fn synthesize(&self, req: Synthesis, client_id: &str) -> Result<Vec<u8>> {
        self.calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(format!("synthesize {:?} {client_id}", req.voice));
        Ok(vec![1, 2, 3])
    }
}

/// A voice source with one voice, deliberately a SEPARATE stub from the engine:
/// voices are a catalog fact, so the fixture wires this whether or not it wires
/// an engine.
struct StubVoices;

#[async_trait]
impl super::TtsVoiceSource for StubVoices {
    async fn voices(&self, _settings: &SettingsRecord) -> Result<Vec<TtsVoiceEntry>> {
        Ok(vec![TtsVoiceEntry {
            id: "af_bella".to_string(),
            label: "Bella".to_string(),
            lang: Some("en-us".to_string()),
        }])
    }
}

// --- fixtures ------------------------------------------------------------------

fn record(pairs: &[(&str, Value)]) -> SettingsRecord {
    let mut map = SettingsRecord::new();
    for (key, value) in pairs {
        map.insert((*key).to_string(), value.clone());
    }
    map
}

/// A context whose settings file is writable, since `Settings::patch` renames
/// into the root and nothing has created it yet.
async fn context(settings: &[(&str, Value)]) -> AppContext {
    let ctx = testing::context_with(Arc::new(testing::StubAuth::new().with_client(TOKEN, "c1")));
    if let Some(dir) = ctx.paths.settings_file().parent() {
        std::fs::create_dir_all(dir).unwrap();
    }
    ctx.settings.patch(record(settings)).await.unwrap();
    ctx
}

/// The slice under test, plus the handles a test needs to drive it.
struct Fixture {
    router: Router,
    manager: Arc<Manager>,
    upstream: Upstream,
    local: Arc<FakeLocal>,
    settings: Settings,
}

/// External providers at the loopback upstream, with no API key: a keyless
/// loopback gateway is what the resolver hands its placeholder to.
async fn external_fixture(extra: &[(&str, Value)]) -> Fixture {
    let upstream = Upstream::start().await;
    let mut settings = vec![
        ("stt.provider", json!("external")),
        ("stt.external.baseUrl", json!(upstream.base_url)),
        ("stt.external.model", json!("whisper-1")),
        ("tts.provider", json!("external")),
        ("tts.external.baseUrl", json!(upstream.base_url)),
        ("tts.external.model", json!("tts-1")),
    ];
    settings.extend(extra.iter().cloned());
    fixture(upstream, &settings).await
}

/// The local provider selected, with no engine behind it: the state of a host
/// built without local speech.
async fn local_fixture(extra: &[(&str, Value)]) -> Fixture {
    let upstream = Upstream::start().await;
    let mut settings = vec![
        ("stt.provider", json!("local")),
        ("tts.provider", json!("local")),
    ];
    settings.extend(extra.iter().cloned());
    fixture(upstream, &settings).await
}

/// The slice with the router-wide body limit around it, which is not
/// decoration: axum's `DefaultBodyLimit` is 2 MB, and the real router raises it
/// to 100 MiB, so a bare slice refuses a legitimate 20 MB upload with a
/// multipart parse error before this package's own 25 MB check is reached.
fn mounted(ctx: &AppContext, manager: &Arc<Manager>, local: Arc<dyn LocalSpeech>) -> Router {
    super::routes::routes(ctx, Arc::clone(manager), local, Arc::new(StubVoices))
        .layer(crate::transport::middleware::body_limit::limit())
}

async fn fixture(upstream: Upstream, settings: &[(&str, Value)]) -> Fixture {
    let ctx = context(settings).await;
    let manager = Arc::new(Manager::new());
    let local = Arc::new(FakeLocal::default());
    Fixture {
        router: mounted(&ctx, &manager, Arc::new(Unavailable)),
        manager,
        upstream,
        local,
        settings: ctx.settings.clone(),
    }
}

impl Fixture {
    /// The same slice with the fake local engine behind it instead of
    /// `Unavailable`.
    async fn with_local_engine(mut self, settings: &[(&str, Value)]) -> Self {
        let ctx = context(settings).await;
        self.settings = ctx.settings.clone();
        self.router = mounted(
            &ctx,
            &self.manager,
            Arc::clone(&self.local) as Arc<dyn LocalSpeech>,
        );
        self
    }

    async fn send(&self, req: Request<Body>) -> (StatusCode, Vec<u8>, Option<String>) {
        let response = self.router.clone().oneshot(req).await.unwrap();
        let status = response.status();
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let body = to_bytes(response.into_body(), 64 << 20)
            .await
            .unwrap()
            .to_vec();
        (status, body, content_type)
    }

    async fn get(&self, path: &str) -> (StatusCode, Value) {
        let (status, body, _) = self
            .send(
                Request::builder()
                    .uri(path)
                    .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        (status, parse(&body))
    }

    async fn post_json(&self, path: &str, body: Value) -> (StatusCode, Vec<u8>, Option<String>) {
        self.send(
            Request::builder()
                .method("POST")
                .uri(path)
                .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
    }

    async fn transcribe(&self, parts: Vec<Part>) -> (StatusCode, Value) {
        let (status, body, _) = self
            .send(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/stt/transcribe")
                    .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                    .header(
                        header::CONTENT_TYPE,
                        format!("multipart/form-data; boundary={BOUNDARY}"),
                    )
                    .body(Body::from(multipart(parts)))
                    .unwrap(),
            )
            .await;
        (status, parse(&body))
    }
}

fn parse(body: &[u8]) -> Value {
    serde_json::from_slice(body).unwrap_or(Value::Null)
}

/// One multipart part. A `filename` is what makes it a file rather than a text
/// field, which is the distinction the route turns on.
struct Part {
    name: &'static str,
    filename: Option<&'static str>,
    mime: Option<&'static str>,
    body: Vec<u8>,
}

fn field(name: &'static str, value: &str) -> Part {
    Part {
        name,
        filename: None,
        mime: None,
        body: value.as_bytes().to_vec(),
    }
}

/// A text field whose bytes are not text, which is what makes reading it fail.
fn raw_field(name: &'static str, body: Vec<u8>) -> Part {
    Part {
        name,
        filename: None,
        mime: None,
        body,
    }
}

fn file(name: &'static str, filename: &'static str, body: Vec<u8>) -> Part {
    Part {
        name,
        filename: Some(filename),
        mime: Some("audio/wav"),
        body,
    }
}

fn multipart(parts: Vec<Part>) -> Vec<u8> {
    let mut out = Vec::new();
    for part in parts {
        out.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
        let disposition = match part.filename {
            Some(filename) => format!(
                "Content-Disposition: form-data; name=\"{}\"; filename=\"{filename}\"\r\n",
                part.name
            ),
            None => format!("Content-Disposition: form-data; name=\"{}\"\r\n", part.name),
        };
        out.extend_from_slice(disposition.as_bytes());
        if let Some(mime) = part.mime {
            out.extend_from_slice(format!("Content-Type: {mime}\r\n").as_bytes());
        }
        out.extend_from_slice(b"\r\n");
        out.extend_from_slice(&part.body);
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
    out
}

fn wav() -> Vec<u8> {
    let mut bytes = b"RIFF....WAVEfmt ".to_vec();
    bytes.extend_from_slice(&[0u8; 28]);
    bytes
}

/// The service-level shape of an upload, for the bounds that would otherwise
/// cost a 25 MB multipart round trip to reach.
fn clip(bytes: Vec<u8>) -> AudioClip {
    AudioClip {
        bytes,
        filename: "clip.wav".to_string(),
        mime: Some("audio/wav".to_string()),
    }
}

// --- auth ----------------------------------------------------------------------

/// Mutation: drop the `require_client` layer. Both slices then transcribe and
/// synthesize for anyone who can reach the port, at the user's expense.
#[tokio::test]
async fn every_route_needs_a_bearer() {
    let fx = external_fixture(&[]).await;
    for (method, path) in [
        ("POST", "/api/v1/stt/transcribe"),
        ("GET", "/api/v1/stt/status"),
        ("POST", "/api/v1/tts/load"),
        ("POST", "/api/v1/tts/unload"),
        ("POST", "/api/v1/tts/synthesize"),
        ("GET", "/api/v1/tts/voices"),
        ("GET", "/api/v1/tts/status"),
    ] {
        let (status, body, _) = fx
            .send(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {path}");
        assert_eq!(
            parse(&body)["error"]["code"],
            "missing_token",
            "{method} {path}"
        );
    }
    assert!(
        fx.upstream.seen().is_empty(),
        "an unauthenticated request reached the provider"
    );
}

// --- transcription -------------------------------------------------------------

/// Mutation: send the bytes without the filename, or drop the bearer. The
/// endpoint is what a user pays for, so what leaves the process is asserted as
/// bytes rather than as an intention.
#[tokio::test]
async fn transcription_relays_the_upload_to_the_external_provider() {
    let fx = external_fixture(&[]).await;
    let (status, body) = fx.transcribe(vec![file("audio", "clip.wav", wav())]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({ "text": "mock transcription" }));

    let seen = fx.upstream.only();
    assert_eq!(seen.path, "/v1/audio/transcriptions");
    assert_eq!(seen.authorization.as_deref(), Some("Bearer sk-noauth"));
    assert_eq!(seen.part("model").as_deref(), Some("whisper-1"));
    assert_eq!(seen.part("response_format").as_deref(), Some("json"));
    assert_eq!(seen.filename("file").as_deref(), Some("clip.wav"));
    assert_eq!(
        seen.part("file"),
        Some(String::from_utf8_lossy(&wav()).into_owned()),
        "the audio bytes did not arrive intact",
    );
    assert_eq!(
        seen.part("language"),
        None,
        "nothing configured a language, so none should have been sent",
    );
}

/// Mutation: fall back to the configured default when a hint was sent. A
/// request-pinned language is the one the speaker just chose.
#[tokio::test]
async fn a_request_language_hint_wins_over_the_configured_default() {
    let fx = external_fixture(&[("stt.external.language", json!("de"))]).await;
    let (status, _) = fx
        .transcribe(vec![
            file("audio", "clip.wav", wav()),
            field("language", "es"),
        ])
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        fx.upstream.only().part("language").as_deref(),
        Some("es"),
        "the configured default overrode the request's own hint",
    );
}

/// Mutation: stop sending `stt.external.language`. Without a hint the setting
/// is the only thing that tells the provider what it is listening to.
#[tokio::test]
async fn the_configured_language_is_sent_when_no_hint_was_given() {
    let fx = external_fixture(&[("stt.external.language", json!("de"))]).await;
    let (status, _) = fx.transcribe(vec![file("audio", "clip.wav", wav())]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fx.upstream.only().part("language").as_deref(), Some("de"));
}

/// Mutation: treat a missing `audio` part as an empty clip. The provider would
/// then be paid to transcribe nothing.
#[tokio::test]
async fn an_upload_with_no_audio_part_is_refused() {
    let fx = external_fixture(&[]).await;
    let (status, body) = fx.transcribe(vec![field("language", "es")]).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body["error"],
        json!({ "code": "validation_error", "message": "audio file required" }),
    );
    assert!(fx.upstream.seen().is_empty());
}

/// Mutation: accept any part named `audio`, filename or not. `audio=hello` then
/// reads as a five-byte upload, which is the shape `audio instanceof File`
/// rejects on the other side.
#[tokio::test]
async fn a_text_field_named_audio_is_not_an_audio_file() {
    let fx = external_fixture(&[]).await;
    let (status, body) = fx.transcribe(vec![field("audio", "hello")]).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["message"], "audio file required");
    assert!(fx.upstream.seen().is_empty());
}

/// Mutation: take the first part named `audio` that happens to be a file. The
/// other side reads `form.get("audio")`, which is the FIRST part with that name
/// whatever it turns out to be, so a text field followed by a real upload is a
/// rejection there and a paid transcription here.
#[tokio::test]
async fn a_later_audio_file_does_not_rescue_a_non_file_audio_field() {
    let fx = external_fixture(&[]).await;
    let (status, body) = fx
        .transcribe(vec![
            field("audio", "hello"),
            file("audio", "clip.wav", wav()),
        ])
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["message"], "audio file required");
    assert!(
        fx.upstream.seen().is_empty(),
        "a shadowed audio field paid for a transcription",
    );
}

/// A `language` part that is not valid UTF-8 is decoded lossily, not rejected.
///
/// `multer::Field::text` replaces malformed sequences with U+FFFD rather than
/// failing, so bad bytes can never produce a read error here and a test
/// asserting a 400 would assert something the library cannot do. The `map_err`
/// on that branch still catches a part that genuinely cannot be read.
///
/// A second trap sits underneath: `text` decodes with BOM SNIFFING, so `ff fe`
/// is the UTF-16LE BOM and decodes to the empty string rather than to two
/// replacement characters. Both cases are pinned below.
#[tokio::test]
async fn a_language_field_that_is_not_utf8_is_decoded_lossily() {
    let fx = external_fixture(&[]).await;
    let (status, _) = fx
        .transcribe(vec![
            file("audio", "clip.wav", wav()),
            // Invalid UTF-8 and not a BOM, so it decodes to replacement
            // characters and is forwarded like any other hint.
            raw_field("language", vec![0x80, 0x81]),
        ])
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        fx.upstream.only().part("language").as_deref(),
        Some("\u{fffd}\u{fffd}"),
        "bad bytes reach the provider as replacement characters, as they do on \
         the other side, rather than being dropped or rejected",
    );
}

/// A BOM-only `language` part decodes to nothing, and an empty hint is an
/// absent hint on both sides: `routes.rs` filters it out and the resolver falls
/// back to `stt.external.language`, exactly as an omitted part would.
#[tokio::test]
async fn a_language_field_that_decodes_to_nothing_is_an_absent_hint() {
    let fx = external_fixture(&[
        ("stt.provider", json!("external")),
        ("stt.external.language", json!("de")),
    ])
    .await;
    let (status, _) = fx
        .transcribe(vec![
            file("audio", "clip.wav", wav()),
            raw_field("language", vec![0xff, 0xfe]),
        ])
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        fx.upstream.only().part("language").as_deref(),
        Some("de"),
        "a hint that decoded to nothing must not shadow the configured language",
    );
}

/// Taking `Multipart` directly in the handler would let axum's own extractor
/// reject a non-multipart request with a plain-text body. Every other failure
/// here is the `{error:{code,message}}` envelope, and a client calling
/// `res.json()` would get a parse failure instead of a code.
#[tokio::test]
async fn a_request_that_is_not_multipart_still_gets_the_error_envelope() {
    let fx = external_fixture(&[]).await;
    for content_type in [Some("application/json"), None] {
        let mut request = Request::builder()
            .method("POST")
            .uri("/api/v1/stt/transcribe")
            .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"));
        if let Some(content_type) = content_type {
            request = request.header(header::CONTENT_TYPE, content_type);
        }
        let (status, body, response_type) = fx
            .send(request.body(Body::from(r#"{"audio":"clip"}"#)).unwrap())
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{content_type:?}");
        assert_eq!(
            response_type.as_deref(),
            Some("application/json"),
            "{content_type:?}",
        );
        assert_eq!(
            parse(&body)["error"]["code"],
            "validation_error",
            "{content_type:?}: {}",
            String::from_utf8_lossy(&body),
        );
    }
    assert!(fx.upstream.seen().is_empty());
}

/// The bound is exclusive and the message names the number, so both the value
/// and which side of it is refused are part of the contract. Driven through the
/// service rather than the route: the at-limit half would otherwise cost a
/// 25 MB multipart round trip.
#[tokio::test]
async fn the_audio_bound_is_25_mb_and_the_limit_itself_is_allowed() {
    // Spelled out: every other assertion here follows the constant.
    assert_eq!(super::STT_MAX_AUDIO_BYTES, 26_214_400);

    let ctx = context(&[
        ("stt.provider", json!("local")),
        ("tts.provider", json!("local")),
    ])
    .await;
    let local = Arc::new(FakeLocal::default());
    let service = super::SpeechService::new(
        &ctx,
        Arc::new(Manager::new()),
        Arc::clone(&local) as Arc<dyn LocalSpeech>,
        Arc::new(StubVoices),
    );

    let at_limit = clip(vec![0u8; super::STT_MAX_AUDIO_BYTES]);
    assert_eq!(
        service.transcribe(at_limit, None, "c1").await.unwrap(),
        "local transcript",
        "a clip exactly at the limit is within it",
    );

    let over = clip(vec![0u8; super::STT_MAX_AUDIO_BYTES + 1]);
    let err = service.transcribe(over, None, "c1").await.unwrap_err();
    assert_eq!(err.message, "stt audio exceeds the 25 MB limit");
}

/// Mutation: drop the size check. The clip is read fully into memory and, on
/// the local path, base64 encoded across the broker's pipe.
#[tokio::test]
async fn an_upload_over_the_limit_is_refused() {
    let fx = external_fixture(&[]).await;
    let oversized = vec![0u8; super::STT_MAX_AUDIO_BYTES + 1];
    let (status, body) = fx
        .transcribe(vec![file("audio", "clip.wav", oversized)])
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body["error"]["message"],
        "stt audio exceeds the 25 MB limit"
    );
    assert!(
        fx.upstream.seen().is_empty(),
        "an oversized clip was relayed anyway"
    );
}

/// Mutation: fall through to the external provider when the local one is
/// selected. A user who chose local transcription must not have their audio
/// leave the machine because no engine was up.
#[tokio::test]
async fn local_transcription_says_so_when_there_is_no_local_engine() {
    let fx = local_fixture(&[]).await;
    let (status, body) = fx.transcribe(vec![file("audio", "clip.wav", wav())]).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        body["error"],
        json!({
            "code": "server_unavailable",
            "message": "local STT is not available on this host",
        }),
    );
    assert!(fx.upstream.seen().is_empty(), "audio left the machine");
}

/// Mutation: route the local provider through the external client. The engine
/// is what "local" means.
#[tokio::test]
async fn local_transcription_reaches_the_local_engine() {
    let settings = [
        ("stt.provider", json!("local")),
        ("tts.provider", json!("local")),
    ];
    let fx = local_fixture(&[]).await.with_local_engine(&settings).await;
    let (status, body) = fx.transcribe(vec![file("audio", "clip.wav", wav())]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({ "text": "local transcript" }));
    let calls = fx.local.calls.lock().unwrap().clone();
    assert_eq!(calls, vec![format!("transcribe {} c1", wav().len())]);
    assert!(fx.upstream.seen().is_empty());
}

// --- synthesis -----------------------------------------------------------------

/// Mutation: change the response content type, or the `response_format` sent
/// upstream. The client's playback queue and the broker's sample-rate probe
/// both assume WAV bytes.
#[tokio::test]
async fn synthesis_relays_to_the_external_provider_and_returns_wav() {
    let fx = external_fixture(&[]).await;
    let (status, body, content_type) = fx
        .post_json("/api/v1/tts/synthesize", json!({ "text": "hello" }))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(content_type.as_deref(), Some("audio/wav"));
    assert_eq!(body.len(), 44);

    let seen = fx.upstream.only();
    assert_eq!(seen.path, "/v1/audio/speech");
    assert_eq!(
        seen.json(),
        json!({
            "model": "tts-1",
            "voice": "alloy",
            "input": "hello",
            "response_format": "wav",
        }),
    );
}

/// Mutation: forward `req.voice`. The client always sends `tts.voice`, a local
/// Kokoro speaker id, and an external provider does not know what `af_bella`
/// is; the dedicated `tts.external.voice` setting exists for exactly this.
#[tokio::test]
async fn the_external_voice_is_sent_rather_than_the_local_one() {
    let fx = external_fixture(&[("tts.external.voice", json!("nova"))]).await;
    let (status, _, _) = fx
        .post_json(
            "/api/v1/tts/synthesize",
            json!({ "text": "hello", "voice": "af_bella", "speed": 1.5 }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let body = fx.upstream.only().json();
    assert_eq!(body["voice"], "nova");
    assert_eq!(body["speed"], 1.5);
}

/// Mutation: send `speed` unconditionally. A zero rate is not a rate, and the
/// field is optional upstream.
#[tokio::test]
async fn a_zero_speed_is_omitted_rather_than_sent() {
    let fx = external_fixture(&[]).await;
    let (status, _, _) = fx
        .post_json(
            "/api/v1/tts/synthesize",
            json!({ "text": "hello", "speed": 0 }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fx.upstream.only().json().get("speed"), None);
}

/// Mutation: rely on the generated type alone. It declares `text` a required
/// string and cannot say non-empty, so `""` deserializes happily and a provider
/// is paid to synthesize nothing.
#[tokio::test]
async fn empty_text_is_refused() {
    let fx = external_fixture(&[]).await;
    let (status, body, _) = fx
        .post_json("/api/v1/tts/synthesize", json!({ "text": "" }))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        parse(&body)["error"],
        json!({ "code": "validation_error", "message": "text required" }),
    );
    assert!(fx.upstream.seen().is_empty());
}

/// The bound is UTF-16 code units, not `chars()` or `len()`. Every count agrees
/// for ASCII, so only the astral case distinguishes them: 1001 astral
/// characters are 2002 UTF-16 units (over), 1001 chars (under) and 4004 bytes
/// (over), so the ASCII pair plus this one pins the unit exactly.
#[tokio::test]
async fn the_text_bound_is_measured_in_utf16_code_units() {
    let fx = external_fixture(&[]).await;

    let (status, _, _) = fx
        .post_json(
            "/api/v1/tts/synthesize",
            json!({ "text": "a".repeat(super::TTS_MAX_TEXT_UNITS) }),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "2000 ascii characters are within the bound"
    );

    let (status, body, _) = fx
        .post_json(
            "/api/v1/tts/synthesize",
            json!({ "text": "a".repeat(super::TTS_MAX_TEXT_UNITS + 1) }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        parse(&body)["error"]["message"],
        "tts text exceeds 2000 characters",
    );

    // 1001 emoji: over the bound in UTF-16 units, under it in `chars()`.
    let astral = "\u{1F600}".repeat(1_001);
    assert_eq!(astral.chars().count(), 1_001);
    assert_eq!(astral.encode_utf16().count(), 2_002);
    let (status, _, _) = fx
        .post_json("/api/v1/tts/synthesize", json!({ "text": astral }))
        .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "an astral text counted by chars() slips past the bound",
    );
}

/// Mutation: fall through to the external provider when the local one is
/// selected.
#[tokio::test]
async fn local_synthesis_says_so_when_there_is_no_local_engine() {
    let fx = local_fixture(&[]).await;
    let (status, body, _) = fx
        .post_json("/api/v1/tts/synthesize", json!({ "text": "hello" }))
        .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        parse(&body)["error"],
        json!({
            "code": "server_unavailable",
            "message": "local TTS is not available on this host",
        }),
    );
    assert!(fx.upstream.seen().is_empty());
}

/// Mutation: substitute `tts.external.voice` on the local path too. Locally the
/// body's voice IS the speaker id, and replacing it silently changes who
/// speaks.
#[tokio::test]
async fn local_synthesis_passes_the_requested_voice_to_the_engine() {
    let settings = [
        ("stt.provider", json!("local")),
        ("tts.provider", json!("local")),
        ("tts.external.voice", json!("nova")),
    ];
    let fx = local_fixture(&[]).await.with_local_engine(&settings).await;
    let (status, body, _) = fx
        .post_json(
            "/api/v1/tts/synthesize",
            json!({ "text": "hello", "voice": "af_bella" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, vec![1, 2, 3]);
    let calls = fx.local.calls.lock().unwrap().clone();
    assert_eq!(calls, vec!["synthesize Some(\"af_bella\") c1".to_string()]);
}

// --- configuration -------------------------------------------------------------

/// Mutation: build the request anyway and let the provider reject it. A missing
/// model is a settings problem, and the message has to say which key.
#[tokio::test]
async fn an_incomplete_external_endpoint_is_refused_before_any_request() {
    let fx = external_fixture(&[("stt.external.model", json!(""))]).await;
    let (status, body) = fx.transcribe(vec![file("audio", "clip.wav", wav())]).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body["error"]["message"],
        "external STT requires stt.external.baseUrl and stt.external.model",
    );
    assert!(fx.upstream.seen().is_empty());

    let (status, body, _) = fx
        .post_json("/api/v1/tts/synthesize", json!({ "text": "hello" }))
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the TTS half is configured and unaffected"
    );
    assert_eq!(body.len(), 44);
}

/// Mutation: hand the keyless placeholder to any host, not only loopback. The
/// placeholder exists for a local gateway that wants no key; sending it to a
/// remote endpoint turns a missing key into an upstream 401 nobody can read.
#[tokio::test]
async fn a_remote_endpoint_with_no_key_is_refused_before_any_request() {
    let fx =
        external_fixture(&[("tts.external.baseUrl", json!("https://api.example.com/v1"))]).await;
    let (status, body, _) = fx
        .post_json("/api/v1/tts/synthesize", json!({ "text": "hello" }))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        parse(&body)["error"]["message"],
        "external TTS requires an API key (tts.external.apiKey)",
    );
    assert!(fx.upstream.seen().is_empty());
}

/// Mutation: read the plaintext settings value first. A key that reached the
/// vault must never be shadowed by a stale one left in the file.
#[tokio::test]
async fn the_vault_key_is_what_reaches_the_provider() {
    let upstream = Upstream::start().await;
    let ctx = context(&[
        ("tts.provider", json!("external")),
        ("tts.external.baseUrl", json!(upstream.base_url.clone())),
        ("tts.external.model", json!("tts-1")),
        ("tts.external.apiKey", json!("sk-from-settings")),
    ])
    .await;
    ctx.vault
        .set("tts.external.apiKey", "sk-from-vault")
        .await
        .unwrap();
    let router = super::routes::routes(
        &ctx,
        Arc::new(Manager::new()),
        Arc::new(Unavailable),
        Arc::new(StubVoices),
    );
    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/tts/synthesize")
                .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({ "text": "hello" }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        upstream.only().authorization.as_deref(),
        Some("Bearer sk-from-vault"),
    );
}

// --- provider failures ---------------------------------------------------------

/// Mutation: report every provider failure as `provider_error`. The client
/// shows a different path for an expired key than for a broken endpoint, and a
/// log line that says which is the whole diagnostic.
#[tokio::test]
async fn a_provider_rejection_keeps_its_code_and_names_the_provider() {
    let fx = external_fixture(&[]).await;
    fx.upstream.fail_next(
        401,
        r#"{"error":{"code":"invalid_api_key","message":"Incorrect API key provided"}}"#,
    );
    let (status, body) = fx.transcribe(vec![file("audio", "clip.wav", wav())]).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], "provider_unauthorized");
    assert_eq!(
        body["error"]["message"],
        "external STT failed: Incorrect API key provided",
    );
    assert_eq!(
        fx.upstream.seen().len(),
        1,
        "a rejected key is not a transient failure, so it is not retried",
    );
}

/// Mutation: make exactly one attempt. A provider that answers 429 or 502 once
/// is absorbed by the SDK on the other side (`maxRetries: 2`), and the user
/// hears their transcript; without the retry the recording is simply lost.
#[tokio::test]
async fn a_transient_provider_failure_is_retried() {
    let fx = external_fixture(&[]).await;
    fx.upstream.fail_next(
        429,
        r#"{"error":{"code":"rate_limit_exceeded","message":"slow down"}}"#,
    );
    let (status, body) = fx.transcribe(vec![file("audio", "clip.wav", wav())]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({ "text": "mock transcription" }));
    assert_eq!(
        fx.upstream.seen().len(),
        2,
        "one transient rate limit lost the transcript",
    );
}

/// Mutation: retry forever, or retry a different number of times. Two retries
/// is what the SDK does on the other side, and an unbounded loop against a
/// provider that is down holds the user's request open indefinitely.
#[tokio::test]
async fn a_provider_that_stays_down_is_retried_twice_and_no_more() {
    let fx = external_fixture(&[]).await;
    fx.upstream
        .fail_always(502, r#"{"error":{"message":"bad gateway"}}"#);
    let (status, body) = fx.transcribe(vec![file("audio", "clip.wav", wav())]).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(body["error"]["message"], "external STT failed: bad gateway");
    assert_eq!(
        fx.upstream.seen().len(),
        3,
        "the initial attempt plus two retries, and nothing beyond",
    );
}

/// The client's deadline is the only bound on a provider that accepts the
/// connection and then says nothing. Time-bounded so a regression fails rather
/// than hangs, and driven with a short deadline; the real one is pinned
/// separately.
#[tokio::test]
async fn a_wedged_provider_is_cut_off_rather_than_waited_on() {
    assert_eq!(
        ExternalSpeech::new().timeout(),
        Duration::from_millis(crate::external::EXTERNAL_TIMEOUT_MS),
        "the production client is built with the shared 120s deadline",
    );

    let (base_url, _held) = wedged_upstream().await;
    let external = ExternalSpeech::with_timeout(Duration::from_millis(200));
    let endpoint = Endpoint {
        base_url,
        model: "whisper-1".to_string(),
        api_key: "sk-noauth".to_string(),
    };
    let call = external.transcribe(&endpoint, clip(wav()), None);
    let outcome = tokio::time::timeout(Duration::from_secs(10), call).await;
    let err = outcome
        .expect("the call is unbounded: a silent provider held it past every retry")
        .expect_err("the wedged provider cannot have answered");
    assert_eq!(err.message, "external STT failed: Request timed out.");
}

/// `reqwest::Error`'s `Display` appends " for url (...)" with no redaction, so
/// folding it into the message puts a base URL carrying credentials into the
/// response body. The log scrubber masks bearer-shaped and long values, and a
/// short password is neither.
#[tokio::test]
async fn a_transport_failure_does_not_echo_the_endpoint_url() {
    let port = dead_port().await;
    let fx = external_fixture(&[
        (
            "tts.external.baseUrl",
            json!(format!("http://gateway:hunter2@127.0.0.1:{port}/v1")),
        ),
        ("tts.external.apiKey", json!("sk-configured")),
    ])
    .await;
    let (status, body, _) = fx
        .post_json("/api/v1/tts/synthesize", json!({ "text": "hello" }))
        .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    let message = parse(&body)["error"]["message"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        message, "external TTS failed: Connection error.",
        "the client-visible message is the SDK's own generic one",
    );
    assert!(
        !message.contains("hunter2") && !message.contains("127.0.0.1"),
        "the endpoint reached the response body: {message}",
    );
}

/// Mutation: any single branch of the classifier. Structured fields are tried
/// before the message, because a message match is a heuristic and a status is
/// not.
#[test]
fn provider_failures_map_onto_stable_codes() {
    use crate::error::ErrorCode;
    let cases: [(Option<u16>, Option<&str>, &str, ErrorCode); 8] = [
        (Some(401), None, "nope", ErrorCode::ProviderUnauthorized),
        (
            None,
            Some("invalid_api_key"),
            "nope",
            ErrorCode::ProviderUnauthorized,
        ),
        (Some(429), None, "slow down", ErrorCode::ProviderRateLimited),
        (
            None,
            Some("rate_limit_exceeded"),
            "x",
            ErrorCode::ProviderRateLimited,
        ),
        (Some(503), None, "down", ErrorCode::ServerUnavailable),
        (Some(504), None, "gateway", ErrorCode::ServerUnavailable),
        (
            Some(400),
            None,
            "This model's maximum context length is 8192 tokens",
            ErrorCode::ContextWindowExceeded,
        ),
        (Some(500), None, "boom", ErrorCode::ProviderError),
    ];
    for (status, code, message, want) in cases {
        let err = classify(status, code, message);
        assert_eq!(err.code, want, "{status:?} {code:?} {message}");
        assert_eq!(err.message, message, "the provider's own words are kept");
    }
}

// --- lifecycle -----------------------------------------------------------------

/// Mutation: report `running: false` for the external provider, or poll the
/// registry for it. There is no local engine to poll: external transcription is
/// running whenever it is the selected provider, and a client that reads
/// otherwise disables its microphone.
#[tokio::test]
async fn stt_status_reports_the_external_provider_as_running() {
    let fx = external_fixture(&[]).await;
    let (status, body) = fx.get("/api/v1/stt/status").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({ "provider": "external", "running": true }));
}

/// Mutation: return a constant, or hold a copy of the engine state here. The
/// registry is where an engine publishes, and a second copy could disagree with
/// the core status bar the client renders beside this.
#[tokio::test]
async fn stt_status_follows_the_registry_for_the_local_provider() {
    let fx = local_fixture(&[]).await;

    let (_, body) = fx.get("/api/v1/stt/status").await;
    assert_eq!(
        body,
        json!({ "provider": "local", "running": false }),
        "an engine nobody published is not running",
    );

    fx.manager.publish(snapshot(SidecarStatus::Loading));
    let (_, body) = fx.get("/api/v1/stt/status").await;
    assert_eq!(body["running"], false, "loading is not running");

    fx.manager.publish(snapshot(SidecarStatus::Running));
    let (_, body) = fx.get("/api/v1/stt/status").await;
    assert_eq!(body["running"], true);
}

/// Mutation: drop the `tts.enabled` gate, or read a different subsystem. One
/// engine serves both directions, so it can be up with synthesis switched off,
/// and the toggle is what the user set.
#[tokio::test]
async fn tts_status_is_the_engine_state_gated_on_the_enabled_setting() {
    let fx = local_fixture(&[("tts.enabled", json!(false))]).await;

    fx.manager.publish(snapshot(SidecarStatus::Running));
    let (status, body) = fx.get("/api/v1/tts/status").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({ "loaded": false, "loading": false }),
        "a running engine with synthesis switched off is not loaded",
    );

    fx.settings
        .patch(record(&[("tts.enabled", json!(true))]))
        .await
        .unwrap();
    let (_, body) = fx.get("/api/v1/tts/status").await;
    assert_eq!(body, json!({ "loaded": true, "loading": false }));

    fx.manager.publish(snapshot(SidecarStatus::Loading));
    let (_, body) = fx.get("/api/v1/tts/status").await;
    assert_eq!(body, json!({ "loaded": false, "loading": true }));

    fx.manager.clear(SidecarKind::Speech);
    let (_, body) = fx.get("/api/v1/tts/status").await;
    assert_eq!(body, json!({ "loaded": false, "loading": false }));
}

/// Mutation: publish or read a different `SidecarKind`. `speech` is the row the
/// engine owns; reading `llama` would track the language model instead.
#[tokio::test]
async fn only_the_speech_subsystem_moves_the_speech_status() {
    let fx = local_fixture(&[("tts.enabled", json!(true))]).await;
    fx.manager
        .publish(kind_snapshot(SidecarKind::Llama, SidecarStatus::Running));
    let (_, body) = fx.get("/api/v1/tts/status").await;
    assert_eq!(body, json!({ "loaded": false, "loading": false }));
}

/// Mutation: make either route do something. They are no-ops in the
/// specification: the engine's lifecycle follows `tts.enabled` and the model
/// files, and these exist for the calls the client already makes.
#[tokio::test]
async fn load_and_unload_are_accepted_and_answer_with_no_body() {
    let fx = external_fixture(&[]).await;
    for path in ["/api/v1/tts/load", "/api/v1/tts/unload"] {
        let (status, body, _) = fx.post_json(path, json!({})).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{path}");
        assert!(body.is_empty(), "{path}");
    }
    assert!(fx.upstream.seen().is_empty());
}

// --- voices --------------------------------------------------------------------

/// Voices are a CATALOG fact, so they must resolve with no local engine at all,
/// or the dropdown is empty on every build without one. `Unavailable` is used
/// explicitly rather than incidentally in the first half; the second half stops
/// "the source is consulted only when an engine exists" from passing.
#[tokio::test]
async fn voices_come_from_the_catalog_not_the_engine() {
    let fx = local_fixture(&[]).await;
    let (status, body) = fx.get("/api/v1/tts/voices").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!([{ "id": "af_bella", "label": "Bella", "lang": "en-us" }]),
        "a bare array, with no envelope, and populated with NO local engine",
    );

    // And unchanged once an engine exists, because the engine was never the
    // source.
    let settings = [("tts.provider", json!("local"))];
    let fx = fx.with_local_engine(&settings).await;
    let (status, body) = fx.get("/api/v1/tts/voices").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!([{ "id": "af_bella", "label": "Bella", "lang": "en-us" }]),
    );
}

/// Built field by field rather than through the registry's own helper, which is
/// private to that package.
fn kind_snapshot(kind: SidecarKind, status: SidecarStatus) -> SidecarSnapshot {
    SidecarSnapshot {
        kind,
        status,
        message: None,
        progress: None,
        pid: None,
        cpu_pct: None,
        rss_mb: None,
    }
}

fn snapshot(status: SidecarStatus) -> SidecarSnapshot {
    kind_snapshot(SidecarKind::Speech, status)
}
