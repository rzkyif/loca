//! The local speech engine: Whisper STT and Kokoro TTS over a statically linked
//! sherpa-onnx, in-process.
//!
//! sherpa-onnx answers some bad inputs by logging and calling `exit()`, which
//! in-process takes the whole Core down (known instance: multilingual Kokoro
//! loaded without a language). There is no way to catch that, so several things
//! here look over-careful and are not: [`wav::decode`] replaces `Wave::read` so
//! only samples cross over, [`voices::voice_lang`] is a closed set,
//! [`config::desired`] refuses a config unless every model role exists, and
//! [`espeak::ensure`] verifies tree content rather than its own stamp. What
//! stays open deliberately: a file that exists, carries the right role, and is
//! not the model it claims to be.
//!
//! Everything that decides is in the default build; only `engine` names a
//! sherpa type, behind `#[cfg(feature = "local-speech")]`.
//!
//! `POST /api/v1/sidecars/speech/{start,stop,restart}` stay inert: the engine's
//! lifecycle follows `stt.enabled` / `tts.enabled` and the model files, never an
//! explicit client call.

pub mod config;
pub mod espeak;
pub mod queue;
pub mod reconcile;
pub mod speech;
pub mod voices;
pub mod wav;
pub mod worker;

/// The sherpa slots, and the only module here that needs the feature.
#[cfg(feature = "local-speech")]
pub mod engine;

use std::sync::Arc;

use crate::settings::Settings;
use crate::sidecars::Manager;
use crate::speech::engine::LocalSpeech;
use crate::transport::status::CoreStatusService;

/// Build the local speech engine, or say there is not one. With `local-speech`
/// off both answers are the empty ones, so boot needs no `#[cfg]` of its own.
///
/// The returned `Arc<dyn LocalSpeech>` must be shared, never rebuilt for the
/// second consumer: a second would mean a second resident model and a second
/// fair-share queue.
pub fn build(
    settings: Settings,
    root: std::path::PathBuf,
    models_dir: std::path::PathBuf,
    sidecars: Arc<Manager>,
    status: CoreStatusService,
) -> (Arc<dyn LocalSpeech>, Option<Arc<reconcile::Reconciler>>) {
    #[cfg(not(feature = "local-speech"))]
    {
        // Named so the signature reads the same in both builds.
        let _ = (settings, root, models_dir, sidecars, status);
        (Arc::new(crate::speech::engine::Unavailable), None)
    }

    #[cfg(feature = "local-speech")]
    {
        let engines = Arc::new(engine::Engines::new(root.clone()));
        let queue = Arc::new(queue::Queue::new(Arc::new(StatusQueue { status })));
        let speech = Arc::new(speech::SherpaSpeech::new(
            queue,
            Arc::new(worker::Worker::start()),
            Arc::clone(&engines) as Arc<dyn speech::EngineOps>,
        ));
        let reconciler = Arc::new(reconcile::Reconciler::new(
            settings,
            sidecars,
            engines as Arc<dyn reconcile::Slots>,
            espeak::data_dir(&root),
            models_dir,
        ));
        (speech as Arc<dyn LocalSpeech>, Some(reconciler))
    }
}

/// Pushes the queue's depth into the aggregate core status, which is what the
/// client's status bar renders while transcriptions are waiting.
#[cfg(feature = "local-speech")]
struct StatusQueue {
    status: CoreStatusService,
}

#[cfg(feature = "local-speech")]
impl queue::QueueObserver for StatusQueue {
    fn note(&self, active: u32, queued: u32) {
        self.status.note_speech_queue(active, queued);
    }
}
