//! The speech route slice: `/api/v1/stt` and `/api/v1/tts`.
//!
//! Both prefixes in one slice because both are served by one service over one
//! pair of providers, and splitting them would mean two mounts and two copies
//! of the same state.
//!
//! Every route requires a paired client and none has an admin tier. The
//! handlers do as little as possible: shape the request, hand it to
//! [`SpeechService`], render the answer. Every bound and every provider choice
//! is the service's, so the extension broker's speech ops get the same
//! behaviour without going through HTTP.

use std::sync::Arc;

use axum::extract::multipart::MultipartRejection;
use axum::extract::{FromRequest, Multipart, Request, State};
use axum::response::Response;
use axum::routing::{get, post};
use axum::Router;

use crate::contract::api::{
    SttStatusResponse, SttTranscribeResponse, TtsStatusResponse, TtsSynthesizeRequest,
    TtsVoicesResponse,
};
use crate::error::{validation, AppError, Result};
use crate::sidecars::Manager;
use crate::transport::http::{bytes, no_content, Json};
use crate::transport::middleware::{from_fn_with_state, require_client, AuthedClient};
use crate::transport::state::AppContext;

use super::engine::{AudioClip, LocalSpeech, Synthesis};
use super::SpeechService;

/// `sidecars` and `local` are threaded in rather than built here: the registry
/// is the core's one subsystem table, and the local engine owns its model
/// memory and its request queue, so a second instance of either would answer
/// from state the rest of the core cannot see.
pub fn routes(
    ctx: &AppContext,
    sidecars: Arc<Manager>,
    local: Arc<dyn LocalSpeech>,
    voice_source: Arc<dyn super::TtsVoiceSource>,
) -> Router {
    let service = SpeechService::new(ctx, sidecars, local, voice_source);
    Router::new()
        .route("/api/v1/stt/transcribe", post(transcribe))
        .route("/api/v1/stt/status", get(stt_status))
        .route("/api/v1/tts/load", post(load))
        .route("/api/v1/tts/unload", post(unload))
        .route("/api/v1/tts/synthesize", post(synthesize))
        .route("/api/v1/tts/voices", get(voices))
        .route("/api/v1/tts/status", get(tts_status))
        .layer(from_fn_with_state(ctx.clone(), require_client))
        .with_state(service)
}

// --- stt ---------------------------------------------------------------------

/// `Multipart`, rejected in the wire envelope.
///
/// axum's own extractor answers a request that is not `multipart/form-data`
/// with a plain-text 400, which would be the one response in this slice a client
/// cannot read as `{error:{code,message}}`. Same shape, and the same reason, as
/// [`crate::transport::http::Json`].
struct Form(Multipart);

impl<S: Send + Sync> FromRequest<S> for Form {
    type Rejection = AppError;

    async fn from_request(req: Request, state: &S) -> Result<Self> {
        Multipart::from_request(req, state)
            .await
            .map(Form)
            .map_err(|rejection: MultipartRejection| validation(rejection.body_text()))
    }
}

/// `multipart/form-data` with an `audio` file and an optional `language` hint.
///
/// The upload is read into memory before its size is checked, bounded until
/// then by the router-wide body limit. The engine reads the whole clip anyway,
/// so streaming it to disk first would buy nothing.
async fn transcribe(
    State(service): State<SpeechService>,
    client: AuthedClient,
    Form(mut form): Form,
) -> Result<Json<SttTranscribeResponse>> {
    let mut audio: Option<AudioClip> = None;
    let mut audio_seen = false;
    let mut language: Option<String> = None;
    // Fields are read in arrival order, so both are collected in one pass
    // rather than assuming the client sends them in a particular order.
    while let Some(field) = form
        .next_field()
        .await
        .map_err(|err| validation(format!("malformed multipart body: {err}")))?
    {
        match field.name() {
            // The FIRST part named `audio` is the upload, whatever it turns out
            // to be, because that is the one `form.get("audio")` returns on the
            // other side. A part with no filename is a text field rather than a
            // file, which is the distinction `audio instanceof File` draws, and
            // a later file part does not rescue it: without both halves,
            // `audio=hello` followed by a real clip reads as a valid upload
            // where the specification calls it a bad request.
            Some("audio") if !audio_seen => {
                audio_seen = true;
                let Some(filename) = field.file_name().map(str::to_string) else {
                    continue;
                };
                let mime = field.content_type().map(str::to_string);
                let bytes = field.bytes().await.map_err(|err| {
                    validation(format!("could not read the uploaded audio: {err}"))
                })?;
                audio = Some(AudioClip {
                    bytes: bytes.to_vec(),
                    filename,
                    mime,
                });
            }
            // A part that cannot be read is a broken request, not an absent
            // hint: reading the form is one operation on the other side and it
            // fails whole.
            Some("language") if language.is_none() => {
                language = Some(field.text().await.map_err(|err| {
                    validation(format!("could not read the language field: {err}"))
                })?);
            }
            _ => {}
        }
    }

    let Some(audio) = audio else {
        return Err(validation("audio file required"));
    };
    let language = language.filter(|hint| !hint.is_empty());
    let text = service.transcribe(audio, language, &client.id).await?;
    Ok(Json(SttTranscribeResponse { text }))
}

async fn stt_status(State(service): State<SpeechService>) -> Result<Json<SttStatusResponse>> {
    Ok(Json(service.stt_status().await?))
}

// --- tts ---------------------------------------------------------------------

/// Synthesis loads and unloads with the engine, which follows `tts.enabled` and
/// the model files on disk. These two exist for the calls the client already
/// makes; making them do something would be a product change rather than a
/// port, and it would take a lifecycle pair on the local engine trait.
async fn load() -> Response {
    no_content()
}

async fn unload() -> Response {
    no_content()
}

async fn synthesize(
    State(service): State<SpeechService>,
    client: AuthedClient,
    Json(body): Json<TtsSynthesizeRequest>,
) -> Result<Response> {
    let wav = service
        .synthesize(
            Synthesis {
                text: body.text,
                voice: body.voice,
                speed: body.speed,
            },
            &client.id,
        )
        .await?;
    Ok(bytes("audio/wav", wav))
}

/// The voices of the currently selected local model, so the client's dropdown
/// matches the model in use. A core with no local engine has no catalog and
/// returns none.
async fn voices(State(service): State<SpeechService>) -> Result<Json<TtsVoicesResponse>> {
    Ok(Json(TtsVoicesResponse(service.voices().await?)))
}

async fn tts_status(State(service): State<SpeechService>) -> Result<Json<TtsStatusResponse>> {
    Ok(Json(service.tts_status().await?))
}
