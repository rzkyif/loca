//! Speech: the STT and TTS routes, the external speech providers, and the
//! trait a local engine implements.
//!
//! Work package **D15** (`docs/rust-port/registry.md`), which owns the routes,
//! the external providers, and the trait a local engine implements. The
//! sherpa-backed local engine is N3's, in [`local`].
//!
//! One service in front of two providers. Which one answers is a settings
//! question (`stt.provider`, `tts.provider`) that neither the HTTP handlers nor
//! the extension broker's speech ops have to ask, which is what lets both entry
//! points share one code path.
//!
//! # Where the bounds are enforced
//!
//! The two input bounds live on [`SpeechService`] rather than in the handlers.
//! The TypeScript checks them at every entry point (the routes and the broker's
//! stdio ops) and says so; putting them one layer down covers both by
//! construction, so a second entry point cannot arrive without them. The
//! handlers keep only what is genuinely about the HTTP request, which is the
//! shape of the multipart body.

pub mod engine;
pub mod external;
pub mod local;
pub mod routes;
pub mod seams;

use std::sync::Arc;

use crate::contract::api::{SidecarKind, SidecarStatus};
use crate::contract::api::{
    SttStatusResponse, SttStatusResponseProvider, TtsStatusResponse, TtsVoiceEntry,
};
use crate::error::{validation, Result};
use crate::external::resolve_external_api_key;
use crate::secrets::Vault;
use crate::settings::access::{bool_setting, str_setting};
use crate::settings::{Settings, SettingsRecord};
use crate::sidecars::Manager;
use crate::transport::state::AppContext;

pub use engine::{AudioClip, LocalSpeech, NoVoices, Synthesis, TtsVoiceSource, Unavailable};
pub use seams::ToolSpeechService;

use external::{Endpoint, ExternalSpeech};

/// Max characters per synthesis call: synthesis time scales with length, and
/// the audio is buffered whole before it is returned.
///
/// Counted in UTF-16 code units, which is what the TypeScript's
/// `text.length` measures. Bytes and `char`s both agree with it for ASCII and
/// disagree above the basic plane, so the unit is part of the bound rather than
/// an implementation detail.
pub const TTS_MAX_TEXT_UNITS: usize = 2_000;

/// Max bytes for a single audio upload: the file is read fully into memory (and
/// base64 encoded across the broker's stdio pipe).
///
/// The bound is exclusive, so a clip of exactly this size is accepted, and the
/// rejection message names the number rather than repeating it as a literal: a
/// wrong constant then reads as a wrong message instead of a right-looking one.
pub const STT_MAX_AUDIO_BYTES: usize = 25 * 1024 * 1024;

/// The clientId internal callers key the local queue's fairness with. The
/// extension broker's `stt.transcribe` and `tts.speak` ops are the callers that
/// have no client of their own.
pub const INTERNAL_CLIENT_ID: &str = "core";

/// Speech-to-text and text-to-speech, over whichever provider the settings
/// select.
///
/// Cheap to clone; every clone shares one HTTP client and therefore one
/// connection pool. It holds no state of its own: the state that has to be
/// single per core (the subsystem registry, and the local engine with its model
/// memory and its queue) is threaded in rather than constructed here.
#[derive(Clone)]
pub struct SpeechService {
    inner: Arc<Inner>,
}

struct Inner {
    settings: Settings,
    vault: Vault,
    sidecars: Arc<Manager>,
    local: Arc<dyn LocalSpeech>,
    /// Separate from `local` on purpose: voices are a catalog fact, not an
    /// engine fact, so the dropdown fills on a core with no local engine
    /// (D-039).
    voices: Arc<dyn TtsVoiceSource>,
    external: ExternalSpeech,
}

/// Which of the two providers a lookup is for. The settings keys and the
/// rejection messages differ only by this, and spelling it out once is what
/// keeps the two halves from drifting apart.
#[derive(Clone, Copy)]
enum Kind {
    Stt,
    Tts,
}

impl Kind {
    fn prefix(self) -> &'static str {
        match self {
            Kind::Stt => "stt",
            Kind::Tts => "tts",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Kind::Stt => "STT",
            Kind::Tts => "TTS",
        }
    }
}

impl SpeechService {
    pub fn new(
        ctx: &AppContext,
        sidecars: Arc<Manager>,
        local: Arc<dyn LocalSpeech>,
        voices: Arc<dyn TtsVoiceSource>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                settings: ctx.settings.clone(),
                vault: ctx.vault.clone(),
                sidecars,
                local,
                voices,
                external: ExternalSpeech::new(),
            }),
        }
    }

    // --- transcription -------------------------------------------------------

    /// Transcribe `audio` with the provider `stt.provider` selects.
    ///
    /// `language` is a request-pinned hint that overrides
    /// `stt.external.language`. It reaches the external provider only: the
    /// local model auto-detects the spoken language, so a hint there would be
    /// ignored anyway.
    pub async fn transcribe(
        &self,
        audio: AudioClip,
        language: Option<String>,
        client_id: &str,
    ) -> Result<String> {
        if audio.bytes.len() > STT_MAX_AUDIO_BYTES {
            return Err(validation(format!(
                "stt audio exceeds the {} MB limit",
                STT_MAX_AUDIO_BYTES / (1024 * 1024),
            )));
        }
        let settings = self.inner.settings.load().await?;
        if str_setting(&settings, "stt.provider", "local") == "external" {
            let endpoint = self.endpoint(&settings, Kind::Stt).await?;
            let language = language
                .filter(|hint| !hint.is_empty())
                .or_else(|| non_empty(str_setting(&settings, "stt.external.language", "")));
            return self
                .inner
                .external
                .transcribe(&endpoint, audio, language)
                .await;
        }

        let started = std::time::Instant::now();
        let size = audio.bytes.len();
        tracing::info!(target: "loca::stt", "local transcription starting (audio {size} bytes)");
        let text = self.inner.local.transcribe(audio, client_id).await?;
        tracing::info!(
            target: "loca::stt",
            "local transcription done in {}ms (audio {size} bytes, out {} chars)",
            started.elapsed().as_millis(), text.len(),
        );
        Ok(text)
    }

    // --- synthesis -----------------------------------------------------------

    /// Synthesize `req.text` to WAV bytes with the provider `tts.provider`
    /// selects.
    pub async fn synthesize(&self, req: Synthesis, client_id: &str) -> Result<Vec<u8>> {
        // Empty text is a missing field, not an empty job. The contract types
        // `text` as a required string and cannot express the bound, so this is
        // the only place it is checked.
        if req.text.is_empty() {
            return Err(validation("text required"));
        }
        if req.text.encode_utf16().count() > TTS_MAX_TEXT_UNITS {
            return Err(validation(format!(
                "tts text exceeds {TTS_MAX_TEXT_UNITS} characters"
            )));
        }
        let settings = self.inner.settings.load().await?;
        if str_setting(&settings, "tts.provider", "local") == "external" {
            let endpoint = self.endpoint(&settings, Kind::Tts).await?;
            // Never `req.voice`: that is a local Kokoro speaker id the client
            // sends every time, and it means nothing to an external provider.
            let voice = str_setting(&settings, "tts.external.voice", "alloy");
            return self
                .inner
                .external
                .synthesize(&endpoint, &req, &voice)
                .await;
        }
        self.inner.local.synthesize(req, client_id).await
    }

    // --- catalog and lifecycle ----------------------------------------------

    /// The voices of the currently selected local model. Empty when there is no
    /// local engine, which is what a client with no voices to choose from
    /// shows.
    pub async fn voices(&self) -> Result<Vec<TtsVoiceEntry>> {
        let settings = self.inner.settings.load().await?;
        self.inner.voices.voices(&settings).await
    }

    /// `{provider, running}` for the client's microphone affordance.
    ///
    /// External transcription has no local engine to poll, so it is running
    /// whenever it is the selected provider.
    pub async fn stt_status(&self) -> Result<SttStatusResponse> {
        let settings = self.inner.settings.load().await?;
        let external = str_setting(&settings, "stt.provider", "local") == "external";
        Ok(SttStatusResponse {
            provider: if external {
                SttStatusResponseProvider::External
            } else {
                SttStatusResponseProvider::Local
            },
            running: external || self.lifecycle().0,
        })
    }

    /// `{loaded, loading}` for the client's speech toggle.
    ///
    /// TTS is loaded once the engine is up AND `tts.enabled` is on, because the
    /// engine serves transcription too: it can be running with synthesis
    /// switched off.
    pub async fn tts_status(&self) -> Result<TtsStatusResponse> {
        let settings = self.inner.settings.load().await?;
        let enabled = bool_setting(&settings, "tts.enabled", false);
        let (running, loading) = self.lifecycle();
        Ok(TtsStatusResponse {
            loaded: enabled && running,
            loading: enabled && loading,
        })
    }

    /// `(running, loading)` from the subsystem registry, which is where the
    /// engine publishes its state.
    ///
    /// Read rather than duplicated: a second copy here could disagree with the
    /// core status bar, and the client renders both at once. A kind nobody has
    /// published reads as `Disabled`, which is the same answer the TypeScript
    /// gives for a sidecar that was never started.
    fn lifecycle(&self) -> (bool, bool) {
        let status = self.inner.sidecars.status(SidecarKind::Speech).status;
        (
            status == SidecarStatus::Running,
            status == SidecarStatus::Loading,
        )
    }

    /// Resolve the configured external endpoint, or say which part is missing.
    ///
    /// The vault is authoritative for the key and a plaintext value in
    /// `settings.json` is the fallback; a keyless loopback gateway resolves to a
    /// placeholder. Both speech paths treat a non-loopback host with no key as a
    /// misconfiguration and reject it up front, where the chat path reports the
    /// same state more gently.
    async fn endpoint(&self, settings: &SettingsRecord, kind: Kind) -> Result<Endpoint> {
        let (prefix, label) = (kind.prefix(), kind.label());
        let base_url = str_setting(settings, &format!("{prefix}.external.baseUrl"), "");
        let model = str_setting(settings, &format!("{prefix}.external.model"), "");
        if base_url.is_empty() || model.is_empty() {
            return Err(validation(format!(
                "external {label} requires {prefix}.external.baseUrl and {prefix}.external.model"
            )));
        }
        let key_id = format!("{prefix}.external.apiKey");
        let api_key =
            resolve_external_api_key(settings, &self.inner.vault, &key_id, &base_url).await?;
        if api_key.is_empty() {
            return Err(validation(format!(
                "external {label} requires an API key ({key_id})"
            )));
        }
        Ok(Endpoint {
            base_url,
            model,
            api_key,
        })
    }
}

fn non_empty(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}

#[cfg(test)]
mod tests;
