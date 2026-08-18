//! The local speech engine seam: the trait `speech/local/` implements, the two
//! values that cross it, and the answer a core with no local engine gives.
//!
//! Three methods, and what is deliberately absent matters as much as what is
//! here:
//!
//! - **No `status()`.** Whether the engine is up is read from the sidecar
//!   registry (`sidecars::Manager`), which is where an engine publishes its
//!   state anyway. A second copy here would let `/tts/status` and the core
//!   status bar disagree, and the client renders both at once.
//! - **No `load()` / `unload()`.** `POST /api/v1/tts/{load,unload}` are no-ops
//!   in the specification: the engine's lifecycle follows `tts.enabled` and the
//!   model files, never an explicit client call. If an in-process engine wants
//!   real prewarm, adding the pair here and calling it from those two handlers
//!   is the extension point, and it is a product decision rather than a port.
//! - **No cancellation token.** See [`LocalSpeech::transcribe`].

use async_trait::async_trait;

use crate::contract::api::TtsVoiceEntry;
use crate::error::{AppError, ErrorCode, Result};
use crate::settings::SettingsRecord;

/// An uploaded audio clip on its way to a transcriber.
///
/// `filename` and `mime` are carried rather than dropped because the external
/// relay forwards both, and an OpenAI-compatible endpoint may sniff the
/// extension to pick a decoder. Both are client supplied.
#[derive(Clone, Debug)]
pub struct AudioClip {
    pub bytes: Vec<u8>,
    pub filename: String,
    pub mime: Option<String>,
}

/// One synthesis request, as the route received it.
///
/// `voice` is a LOCAL voice id (the client always sends `tts.voice`, a Kokoro
/// speaker). It means nothing to an external provider, which is why the
/// external path substitutes `tts.external.voice` instead of forwarding this.
#[derive(Clone, Debug)]
pub struct Synthesis {
    pub text: String,
    pub voice: Option<String>,
    pub speed: Option<f64>,
}

/// The local speech engine: Whisper transcription and Kokoro synthesis, in
/// process.
///
/// Implemented by `speech/local/` (work package N3). The routes serve the
/// external providers and this behind one interface, so neither the handlers
/// nor the module broker's speech ops know which one answered.
#[async_trait]
pub trait LocalSpeech: Send + Sync {
    /// Audio in, transcript out. `client_id` keys the per-client fairness of
    /// the queue in front of the engine.
    ///
    /// **Cancellation is by future drop, and that is a real obligation.** axum
    /// drops the handler future when the client disconnects, which is how an
    /// abandoned transcription stops costing anything. An implementation that
    /// runs its decode on `spawn_blocking` does NOT stop when this future is
    /// dropped, so it must release its queue slot on drop (or grow an explicit
    /// token on this trait). Getting it wrong is silent: the next caller waits
    /// behind work nobody is listening for.
    async fn transcribe(&self, audio: AudioClip, client_id: &str) -> Result<String>;

    /// Text in, complete WAV bytes out.
    ///
    /// Buffered rather than streamed on purpose: the route returns one
    /// `audio/wav` body, and both the client's playback queue and the module
    /// broker's sample-rate probe assume whole WAV bytes. The 2000-character
    /// input bound is what keeps the buffer small.
    async fn synthesize(&self, req: Synthesis, client_id: &str) -> Result<Vec<u8>>;
}

/// The voices of the model `tts.modelType` and `tts.modelPath` currently select,
/// for the client's voice dropdown.
///
/// **Deliberately NOT a method on [`LocalSpeech`]** (D-039). It was one, and that
/// was the right shape before the models package existed. The answer does not
/// come from the speech engine at all: it is a pure catalog lookup over the
/// model the settings select, with no sherpa, no model load and no audio.
///
/// Leaving it on the engine trait had a consequence worth stating, because it is
/// the reason this is a separate trait rather than a default method: the voice
/// picker would be empty on every core without a local speech engine, including
/// every mobile build until N3 ships, and the emptiness would read as "no voices
/// available" rather than "this list was never wired". Users pick from that list.
///
/// Reads the sparse settings record it is handed. An unknown selection returns an
/// empty list rather than failing: an empty dropdown is a recoverable state and a
/// 500 on a settings screen is not.
#[async_trait]
pub trait TtsVoiceSource: Send + Sync {
    async fn voices(&self, settings: &SettingsRecord) -> Result<Vec<TtsVoiceEntry>>;
}

/// A [`TtsVoiceSource`] for a core with no model catalog wired.
///
/// Empty is the honest answer here, unlike on the engine: with no catalog there
/// is genuinely nothing to list.
pub struct NoVoices;

#[async_trait]
impl TtsVoiceSource for NoVoices {
    async fn voices(&self, _settings: &SettingsRecord) -> Result<Vec<TtsVoiceEntry>> {
        Ok(Vec::new())
    }
}

/// The engine of a core that has none.
///
/// Not a placeholder: this is the behaviour of a host without local inference,
/// which the TypeScript already supports and ships on mobile, where
/// `host.localEndpoints` is simply absent. Transcription and synthesis report
/// that they are unavailable. The voice list is NOT here: it moved to
/// [`TtsVoiceSource`] because it is a catalog fact rather than an engine one
/// (D-039), which is what lets the dropdown fill on exactly this core.
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
