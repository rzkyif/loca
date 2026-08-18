//! The local speech engine: Whisper STT and Kokoro TTS over a statically linked
//! sherpa-onnx, in-process.
//!
//! Work package **N3** (`docs/rust-port/registry.md`). This module is that
//! package's owned path; nothing outside it writes here. It implements the
//! provider trait defined by its parent, which D15 owns, so the routes serve
//! external and local speech through one interface.
//!
//! # Absorbing sherpa removed a fault boundary, and that is this module's
//! central design constraint
//!
//! sherpa-onnx's C++ answers some bad inputs by logging and calling `exit()`
//! rather than by returning an error. As a separate process that killed the
//! speech sidecar, the core noticed, and the user saw a provider error. In
//! process the same call takes **the whole Core down**: chat, sessions,
//! scheduled prompts, every paired client. The repo already documented one
//! instance before this port existed (`loca-core-speech/src/main.rs:370-373`:
//! multilingual Kokoro "refuses to load (it exits the process)" without a
//! language).
//!
//! There is no way to catch `exit()`. The containment is to never hand sherpa an
//! input that can reach one, which is why several things here look
//! over-careful and are not:
//!
//! | risk                          | closed by                                                              |
//! | ----------------------------- | ---------------------------------------------------------------------- |
//! | a malformed audio container   | [`wav::decode`]; `Wave::read` is never called, only samples cross over |
//! | an unknown espeak language    | [`voices::voice_lang`]'s closed set, all present in the bundled data   |
//! | a missing model file or role  | [`config::desired`] refuses to build a config unless every role exists |
//! | a missing phonemizer dir      | checked again in `engine::build_tts`, immediately before the load      |
//! | a half-extracted espeak tree  | [`espeak::ensure`] verifies content, not just its own stamp            |
//!
//! **What remains open, deliberately** (see `engine.rs` for the full note): a
//! file that exists, carries the right role, and is not the model it claims to
//! be. A subprocess probe would close it and would reintroduce exactly the
//! process boundary this package exists to remove, so for v1 the risk is
//! recorded rather than engineered around.
//!
//! # Layout, and why only one file needs the feature
//!
//! Everything that DECIDES is in the default build and is tested there. Only
//! model loading and inference need sherpa:
//!
//! - [`config`]: settings to a resolved, existing set of model files.
//! - [`queue`]: the per-client fair-share slot in front of the engine.
//! - [`worker`]: the one thread blocking decodes run on.
//! - [`wav`]: decoding an upload and framing a synthesis answer.
//! - [`voices`]: voice name to speaker id, and the espeak language.
//! - [`espeak`]: the embedded phonemizer data and its extraction.
//! - [`reconcile`]: the decision table that keeps the models equal to the
//!   settings, and publishes the subsystem row.
//! - `speech`: the `LocalSpeech` implementation tying the three together.
//! - `engine`: **the only file that names a sherpa type**, behind
//!   `#[cfg(feature = "local-speech")]`.
//!
//! # The two subsystem routes this module does NOT implement
//!
//! `POST /api/v1/sidecars/speech/{start,stop,restart}` stay inert. They were
//! process control for a child that no longer exists, and the engine's lifecycle
//! follows `stt.enabled` / `tts.enabled` and the model files, never an explicit
//! client call. Making them load or drop a model would be a product decision
//! (a second, hidden way to change engine state that the settings screen would
//! not reflect), not a port. Same reasoning as D-035's ruling on
//! `POST /api/v1/tts/{load,unload}`.

pub mod config;
pub mod espeak;
pub mod queue;
pub mod reconcile;
pub mod speech;
pub mod voices;
pub mod wav;
pub mod worker;

/// The sherpa slots. The ONLY module in this package that needs the feature, so
/// everything above is compiled, tested and mutation-proven in the default gate.
#[cfg(feature = "local-speech")]
pub mod engine;

use std::sync::Arc;

use crate::settings::Settings;
use crate::sidecars::Manager;
use crate::speech::engine::LocalSpeech;
use crate::transport::status::CoreStatusService;

/// Build the local speech engine, or say there is not one.
///
/// The single entry point boot calls. With `local-speech` off it answers
/// [`Unavailable`] and `None`, which is not a stub: it is the correct behaviour
/// of a host without local speech, and the one every mobile build and every
/// default-feature desktop build has today.
///
/// The returned [`reconcile::Reconciler`] is what boot drives from the settings
/// subscription and the download queue. It is `None` in the same case, so boot
/// needs no `#[cfg]` of its own.
///
/// **The `Arc<dyn LocalSpeech>` must be shared, never rebuilt** for the second
/// consumer. Boot hands the same one to the route slice and to the extension
/// broker's speech seam; a second would be a second resident model and a second
/// fair-share queue, so two callers would each think they had the engine to
/// themselves.
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
