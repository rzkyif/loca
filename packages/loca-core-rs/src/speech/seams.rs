//! What the extension supervisor reaches speech through.
//!
//! [`ToolSpeech`] is DECLARED by `extensions::broker` (the consumer declares, so
//! nothing under `extensions/` imports a `speech::` type) and implemented here,
//! over the same [`SpeechService`] the STT and TTS routes call.
//!
//! # The two caps are not restated here
//!
//! 25 MB of audio and 2,000 characters of text are enforced inside
//! [`SpeechService::transcribe`] and [`SpeechService::synthesize`], one layer
//! below both entry points. A second copy at this seam would be a constant that
//! could drift from the one the routes enforce, and the divergence would show up
//! as a tool that accepts a clip the HTTP route rejects.

use async_trait::async_trait;

use crate::error::Result;
use crate::extensions::broker::ToolSpeech;

use super::engine::{AudioClip, Synthesis};
use super::{SpeechService, INTERNAL_CLIENT_ID};

/// Synthesis and transcription on behalf of an extension's tool.
#[derive(Clone)]
pub struct ToolSpeechService {
    speech: SpeechService,
}

impl ToolSpeechService {
    pub fn new(speech: SpeechService) -> Self {
        Self { speech }
    }
}

#[async_trait]
impl ToolSpeech for ToolSpeechService {
    async fn synthesize(&self, text: &str, voice: &str) -> Result<Vec<u8>> {
        self.speech
            .synthesize(
                Synthesis {
                    text: text.to_string(),
                    voice: Some(voice.to_string()),
                    // No speed: the broker has no client whose preference to
                    // carry, so the engine's own default applies.
                    speed: None,
                },
                INTERNAL_CLIENT_ID,
            )
            .await
    }

    async fn transcribe(
        &self,
        audio: Vec<u8>,
        mime: &str,
        language: Option<&str>,
    ) -> Result<String> {
        self.speech
            .transcribe(
                AudioClip {
                    bytes: audio,
                    // The name an external provider's multipart part carries.
                    // Nothing reads it back, but a part with no filename is
                    // rejected by some gateways.
                    filename: "audio".to_string(),
                    mime: Some(mime.to_string()),
                },
                language.map(str::to_string),
                INTERNAL_CLIENT_ID,
            )
            .await
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::sync::{Arc, Mutex};

    use crate::error::Result;
    use crate::sidecars::Manager;
    use crate::speech::engine::LocalSpeech;
    use crate::speech::{SpeechService, STT_MAX_AUDIO_BYTES, TTS_MAX_TEXT_UNITS};
    use crate::transport::testing;

    use super::*;

    /// A local engine that answers, and remembers exactly what it was handed.
    #[derive(Default)]
    struct Recording {
        heard: Mutex<Vec<AudioClip>>,
        spoken: Mutex<Vec<Synthesis>>,
        clients: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl LocalSpeech for Recording {
        async fn transcribe(&self, audio: AudioClip, client_id: &str) -> Result<String> {
            self.clients
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(client_id.to_string());
            self.heard
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(audio);
            Ok("transcript".to_string())
        }

        async fn synthesize(&self, req: Synthesis, client_id: &str) -> Result<Vec<u8>> {
            self.clients
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(client_id.to_string());
            self.spoken
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(req);
            Ok(b"RIFF".to_vec())
        }
    }

    fn seam() -> (ToolSpeechService, Arc<Recording>) {
        let ctx = testing::context();
        let local = Arc::new(Recording::default());
        let service = SpeechService::new(
            &ctx,
            Arc::new(Manager::new()),
            Arc::clone(&local) as Arc<dyn LocalSpeech>,
            // The broker never asks for voices; `NoVoices` says so rather than
            // implying this seam has a catalog behind it.
            Arc::new(crate::speech::NoVoices),
        );
        (ToolSpeechService::new(service), local)
    }

    #[tokio::test]
    async fn a_tool_call_carries_its_voice_mime_and_language_hint_through() {
        let (seam, local) = seam();

        assert_eq!(seam.synthesize("hello", "af_bella").await.unwrap(), b"RIFF");
        let spoken = local
            .spoken
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        assert_eq!(spoken.len(), 1);
        assert_eq!(spoken[0].text, "hello");
        assert_eq!(spoken[0].voice.as_deref(), Some("af_bella"));

        assert_eq!(
            seam.transcribe(vec![1, 2, 3], "audio/mpeg", Some("id"))
                .await
                .unwrap(),
            "transcript",
        );
        let heard = local
            .heard
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        assert_eq!(heard.len(), 1);
        assert_eq!(heard[0].bytes, vec![1, 2, 3]);
        assert_eq!(heard[0].mime.as_deref(), Some("audio/mpeg"));
        assert_eq!(heard[0].filename, "audio");

        // Both calls key the local queue as the core itself: a tool has no
        // client of its own, and borrowing one would let an extension's work
        // consume that client's fair share.
        assert_eq!(
            local
                .clients
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
            vec![
                INTERNAL_CLIENT_ID.to_string(),
                INTERNAL_CLIENT_ID.to_string()
            ],
        );
    }

    /// The claim this seam is built on: the caps are enforced BELOW it, so a
    /// tool cannot reach past what the routes allow. If a second copy of either
    /// bound is ever added at the seam, this still passes; if the seam ever
    /// stops going through `SpeechService`, it does not.
    #[tokio::test]
    async fn the_route_bounds_apply_to_a_tool_call_too() {
        let (seam, local) = seam();

        let err = seam
            .transcribe(vec![0u8; STT_MAX_AUDIO_BYTES + 1], "audio/wav", None)
            .await
            .unwrap_err();
        assert_eq!(err.message, "stt audio exceeds the 25 MB limit");

        let err = seam
            .synthesize(&"a".repeat(TTS_MAX_TEXT_UNITS + 1), "af_bella")
            .await
            .unwrap_err();
        assert_eq!(
            err.message,
            format!("tts text exceeds {TTS_MAX_TEXT_UNITS} characters"),
        );

        assert!(
            local
                .heard
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_empty()
                && local
                    .spoken
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .is_empty(),
            "neither over-sized call reached the engine",
        );
    }
}
