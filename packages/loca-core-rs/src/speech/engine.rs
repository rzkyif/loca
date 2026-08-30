//! The local speech engine seam: the trait `speech/local/` implements, the two
//! values that cross it, and the answer a core with no local engine gives.
//!
//! No `status()` (the sidecar registry already publishes engine state, and a
//! second copy would let `/tts/status` and the status bar disagree) and no
//! `load()`/`unload()` (the engine's lifecycle follows `tts.enabled` and the
//! model files, so those routes are no-ops).

use async_trait::async_trait;

use crate::contract::api::TtsVoiceEntry;
use crate::error::{AppError, ErrorCode, Result};
use crate::settings::SettingsRecord;

/// An uploaded audio clip on its way to a transcriber. `filename` and `mime`
/// are carried because the external relay forwards both and an
/// OpenAI-compatible endpoint may sniff the extension to pick a decoder.
#[derive(Clone, Debug)]
pub struct AudioClip {
    pub bytes: Vec<u8>,
    pub filename: String,
    pub mime: Option<String>,
}

/// One synthesis request, as the route received it. `voice` is a LOCAL voice id
/// and means nothing to an external provider, which is why the external path
/// substitutes `tts.external.voice` rather than forwarding it.
#[derive(Clone, Debug)]
pub struct Synthesis {
    pub text: String,
    pub voice: Option<String>,
    pub speed: Option<f64>,
}

/// The local speech engine: Whisper transcription and Kokoro synthesis, in
/// process. The routes serve this and the external providers behind one
/// interface, so no caller knows which one answered.
#[async_trait]
pub trait LocalSpeech: Send + Sync {
    /// Audio in, transcript out. `client_id` keys the per-client fairness of
    /// the queue in front of the engine.
    ///
    /// Cancellation is by future drop: an implementation whose decode runs on
    /// `spawn_blocking` does not stop when this future is dropped, so it must
    /// release its queue slot on drop or the next caller waits behind work
    /// nobody is listening for.
    async fn transcribe(&self, audio: AudioClip, client_id: &str) -> Result<String>;

    /// Text in, complete WAV bytes out. Buffered rather than streamed: the
    /// route returns one `audio/wav` body, and both the client's playback queue
    /// and the broker's sample-rate probe assume whole WAV bytes.
    async fn synthesize(&self, req: Synthesis, client_id: &str) -> Result<Vec<u8>>;
}

/// The voices of the model `tts.modelType` and `tts.modelPath` currently select,
/// for the client's voice dropdown.
///
/// Deliberately NOT a method on [`LocalSpeech`]: it is a pure catalog lookup
/// with no sherpa, no model load and no audio, so the picker fills even on a
/// core with no local engine. An unknown selection answers an empty list rather
/// than failing: an empty dropdown is recoverable, a 500 on settings is not.
#[async_trait]
pub trait TtsVoiceSource: Send + Sync {
    async fn voices(&self, settings: &SettingsRecord) -> Result<Vec<TtsVoiceEntry>>;
}

/// A [`TtsVoiceSource`] for a core with no model catalog wired.
pub struct NoVoices;

#[async_trait]
impl TtsVoiceSource for NoVoices {
    async fn voices(&self, _settings: &SettingsRecord) -> Result<Vec<TtsVoiceEntry>> {
        Ok(Vec::new())
    }
}

/// The engine of a core that has none: transcription and synthesis report that
/// they are unavailable. Not a placeholder, but the behaviour of a host built
/// without local speech.
pub struct Unavailable;

#[async_trait]
impl LocalSpeech for Unavailable {
    async fn transcribe(&self, _audio: AudioClip, _client_id: &str) -> Result<String> {
        Err(AppError::new(
            ErrorCode::ServerUnavailable,
            "local STT is not available on this host",
        ))
    }

    async fn synthesize(&self, _req: Synthesis, _client_id: &str) -> Result<Vec<u8>> {
        Err(AppError::new(
            ErrorCode::ServerUnavailable,
            "local TTS is not available on this host",
        ))
    }
}
