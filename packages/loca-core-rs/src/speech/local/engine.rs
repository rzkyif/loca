//! The sherpa-onnx slots: building the engines, decoding, and synthesising.
//!
//! The only file in this package that names a sherpa type, and the only one
//! behind `#[cfg(feature = "local-speech")]`. Everything that decides anything
//! (the queue, the config derivation, the reconciler's decision table, the WAV
//! codec, the voice tables) is beside it in the default build, so it is tested
//! and mutation-proven without the C++ library.
//!
//! # What can still terminate the process
//!
//! This is the residual of the containment work described in [`super`], and it
//! is here because the next person to read `LocalSpeech` will assume the
//! `Result` covers it. It does not.
//!
//! ## How this was established, so the next reader can judge it rather than trust it
//!
//! By enumerating undefined-symbol imports (`nm -o`) across **all 15 static
//! archives** of the pinned `sherpa-onnx-v1.13.3-osx-arm64-static-lib`,
//! including the 65 MB `libonnxruntime.a`, for six termination classes: `exit`,
//! `_Exit`, POSIX `_exit`, `abort`, `std::terminate` and `__cxa_throw`.
//!
//! **This header has been wrong twice, both times from an incomplete
//! enumeration, so the method matters as much as the conclusion.** The first
//! version reasoned from upstream behaviour without checking the archive. The
//! second checked 6 archives of 15 and grepped only for `_exit`, and that grep
//! is the trap: Mach-O spells C `_Exit()` as `__Exit`, which a `_exit` pattern
//! does not match. `_Exit` is sherpa-onnx's dominant die-on-bad-input idiom, so
//! the narrow grep produced a confident "this path is clean" for paths that are
//! not.
//!
//! An import proves only that some code path in that object CAN call the symbol.
//! It cannot show the call is reachable from our inputs. Everything below is
//! possibility, not certainty, and is deliberately stated that way.
//!
//! ## The finding: the exposure is broad, not a short list
//!
//! **116 of `libsherpa-onnx-core.a`'s 228 members import `_Exit()`**, and they
//! are not an obscure corner: they include `offline-stream.cc.o`,
//! `features.cc.o`, `resample.cc.o`, `file-utils.cc.o`, `text-utils.cc.o`,
//! `offline-whisper-model.cc.o`, `offline-sense-voice-model.cc.o`,
//! `offline-tts-kokoro-model.cc.o`, `piper-phonemize-lexicon.cc.o` and
//! `kokoro-multi-lang-lexicon.cc.o`. That is the whole path a transcription or a
//! synthesis takes. `offline-recognizer-impl.cc.o` and `offline-tts-impl.cc.o`
//! are in the harshest class, importing both `exit()` and `_Exit()`.
//!
//! `_Exit` is harsher than `exit`: it skips `atexit` handlers, static
//! destructors and stream flushing. There is no hook, and nothing to catch.
//!
//! Two more, both on live paths:
//!
//! - **The STT feature extractor can `exit()`**: `feature-window.cc.o` and
//!   `rfft.cc.o` in `libkaldi-native-fbank-core.a`. This is what makes the
//!   sample-rate bound in [`super::wav`] a containment control rather than
//!   tidiness, since the rate is client-controlled and reaches that code.
//! - **espeak-ng can `exit()`**, in exactly one of its 27 members
//!   (`espeak_api.c.o`, the top-level API), which is where `espeak_Initialize`
//!   lives. A corrupt or version-mismatched phonemizer tree is therefore a
//!   plausible kill, which is why [`super::espeak`] validates the same four
//!   files sherpa validates and why `build_tts` re-checks the directory.
//!
//! ## What that means for the design
//!
//! **The accepted residual is wider than "a mis-specified model file", and
//! naming it precisely is the point of this section.** Once bytes reach sherpa,
//! most of the library can terminate the process on data it dislikes. In-process
//! linkage is what makes that fatal to the whole Core rather than to a sidecar,
//! and that trade was made deliberately (the sidecar cannot be linked into a
//! mobile app at all). What this module can do, and does, is keep everything it
//! CAN validate away from that boundary:
//!
//! - audio never crosses as a container, only as `f32` samples this code
//!   produced, **with a bounded sample rate** ([`super::wav`]);
//! - the espeak language comes from a closed set with bundled dictionaries
//!   ([`super::voices`]);
//! - every model role a family needs is resolved and confirmed present before a
//!   config is built ([`super::config`]);
//! - the phonemizer tree is content-checked, not stamp-checked
//!   ([`super::espeak`]).
//!
//! What remains uncontained is the content of a model file that exists and has
//! the right role but is not the model it claims to be, and, more broadly, any
//! malformed data inside a well-formed model. A subprocess probe would close it
//! and would reintroduce exactly the boundary this package removes, so it is
//! recorded rather than engineered around. If a crash report ever traces here,
//! this is the decision to revisit.
//!
//! ## Corrections to earlier versions of this comment
//!
//! - "`Wave::read` terminates on a malformed container" was asserted, then
//!   retracted on the strength of "`wave.cc` imports no `_exit`". **Both were
//!   wrong.** There is no `wave.cc.o`: the members are `wave-reader.cc.o` and
//!   `wave-writer.cc.o`, and both import `std::terminate` and `__cxa_throw`
//!   though neither imports any exit. So that path can die by unhandled
//!   exception across the FFI boundary rather than by exit, which is a different
//!   class and not a clean one. **Decoding in Rust stands**, and its
//!   justification is the [`super::wav`] list: a `CString::new().unwrap()` panic
//!   in the wrapper, keeping attacker-controlled bytes out of the C++, and
//!   avoiding a 25 MB temp-file round trip.
//! - "A missing model file terminates" does not hold: each config's `Validate()`
//!   returns false and the wrapper turns that into `None`. The existence checks
//!   in [`super::config`] stay, for a different job: they make "not loaded" a
//!   normal state rather than a failed load.
//! - The FST `exit` sites (`fst-utils`, `lodr-fst`, the two `*-ctc-fst-decoder`)
//!   need `rule_fsts`, `rule_fars` or a CTC decoding graph, all left empty here.
//!   Note `libsherpa-onnx-fst.a` is also the only `abort()` source in the stack
//!   and has no exception machinery at all, so an FST error either returns or
//!   dies depending on `FST_FLAGS_fst_error_fatal`, whose runtime value is not
//!   readable from the archive.
//! - No archive imports `assert()` at all: this is an `NDEBUG` build, so nothing
//!   here dies by assertion.

use std::sync::{Arc, Mutex};

use sherpa_onnx::{
    GenerationConfig, OfflineDolphinModelConfig, OfflineMoonshineModelConfig,
    OfflineNemoEncDecCtcModelConfig, OfflineParaformerModelConfig, OfflineRecognizer,
    OfflineRecognizerConfig, OfflineSenseVoiceModelConfig, OfflineTransducerModelConfig,
    OfflineTts, OfflineTtsConfig, OfflineTtsKittenModelConfig, OfflineTtsKokoroModelConfig,
    OfflineTtsMatchaModelConfig, OfflineTtsModelConfig, OfflineTtsVitsModelConfig,
    OfflineWhisperModelConfig,
};

use crate::contract::inbound::{SttFamily, TtsFamily};

use super::config::{Desired, SttConfig, TtsConfig};
use super::reconcile::Slots;
use super::voices;

/// The ONNX Runtime execution provider.
///
/// Always CPU. GPU speech was one downloadable binary flavour (Linux x64 CUDA
/// only) and does not survive absorption: in process the provider is fixed by
/// what the core binary linked, and ONNX Runtime has no equivalent of
/// llama.cpp's loadable backends. Recorded as a deliberate capability loss that
/// degrades to CPU, which is what every Mac, Windows and non-NVIDIA Linux user
/// already gets.
const PROVIDER: &str = "cpu";

/// The bundled espeak-ng phonemizer data, repacked as a gzipped tar.
///
/// 8.6 MB compressed, 18 MB and 355 files unpacked, sha256
/// `2b78a08e0c46d49e2d6379bcc71842b4c61f89ee36247706ea2968ed92ba87b7`, repacked
/// from the bz2 asset `scripts/release/core.ts` already pins by hash. Embedded
/// rather than downloaded because the user has already consented to speech by
/// enabling it, and fetching a required resource afterwards would be a
/// background network call, which the repo's one absolute rule forbids. Only
/// linked in under `local-speech`, so a default build pays nothing for it.
const ESPEAK_ARCHIVE: &[u8] = include_bytes!("../../../data/espeak-ng-data.tar.gz");

/// The resident engines.
///
/// Each slot holds the config it was built from, so reconciliation is an
/// equality check and an unchanged settings PATCH does not reload a model.
pub struct Engines {
    stt: Mutex<Option<(SttConfig, OfflineRecognizer)>>,
    tts: Mutex<Option<TtsSlot>>,
    /// The channel root, for extracting the phonemizer data on first use.
    root: std::path::PathBuf,
    /// The thread count the resident models were built with, carried so a
    /// mid-synthesis Kokoro language reload rebuilds with the same one rather
    /// than with a default that would silently differ from the loaded model.
    threads: std::sync::atomic::AtomicI32,
}

/// The TTS engine plus the espeak language it was built with.
///
/// Separate from the STT slot because multilingual Kokoro fixes its phonemizer
/// language at load time, so a synthesis for a voice in another language has to
/// rebuild it. That is the one piece of engine state a request can mutate.
struct TtsSlot {
    config: TtsConfig,
    lang: String,
    engine: OfflineTts,
}

impl Engines {
    pub fn new(root: std::path::PathBuf) -> Self {
        Self {
            stt: Mutex::new(None),
            tts: Mutex::new(None),
            root,
            threads: std::sync::atomic::AtomicI32::new(1),
        }
    }

    /// Transcribe mono samples. Blocking: called on the speech worker thread.
    pub fn transcribe(&self, samples: &[f32], sample_rate: u32) -> Result<String, String> {
        let slot = lock(&self.stt);
        let Some((config, recognizer)) = slot.as_ref() else {
            return Err("local STT is not loaded".to_string());
        };
        let stream = recognizer.create_stream();
        // Samples, not a path. sherpa resamples internally when this rate is not
        // the model's own.
        stream.accept_waveform(sample_rate as i32, samples);
        recognizer.decode(&stream);
        let text = stream.get_result().map(|r| r.text).unwrap_or_default();
        Ok(match config.family {
            // SenseVoice prepends inline metadata tokens to its transcript; the
            // other families return plain text.
            SttFamily::SenseVoice => strip_special_tokens(&text),
            _ => text,
        })
    }

    /// Synthesize to raw samples plus the engine's sample rate. Blocking.
    ///
    /// `cancelled` is polled by sherpa's generation callback, so an abandoned
    /// synthesis stops at the next chunk rather than running to completion for a
    /// client that has gone.
    pub fn synthesize(
        &self,
        text: &str,
        voice: Option<&str>,
        speed: Option<f64>,
        threads: i32,
        cancelled: Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<(Vec<f32>, i32), String> {
        let mut slot = lock(&self.tts);
        let Some(resident) = slot.as_mut() else {
            return Err("local TTS is not loaded".to_string());
        };

        // Multilingual Kokoro bakes its phonemizer language in at load time, so
        // a voice in another language needs the engine rebuilt for it. Build
        // before swapping, so a failed rebuild keeps the working engine. Other
        // families fix their language by model and never reload here.
        //
        // **This is the one path where the "every role file exists" invariant is
        // stale by construction.** `config::desired` checked those paths at
        // reconcile time; a model deleted from the storage screen since then
        // changes no watched setting, so nothing reconciled, and this reads them
        // again. Contained rather than guarded: `Validate()` returns a null
        // engine, so the caller gets a `provider_error` and the resident engine
        // survives the failed rebuild.
        let want_lang = voices::request_lang(voice);
        if resident.config.family == TtsFamily::Kokoro && resident.lang != want_lang {
            let rebuilt = build_tts(&resident.config, threads, want_lang)?;
            tracing::info!(
                target: "loca::tts",
                "reloaded the voice model ({} to {want_lang})", resident.lang,
            );
            resident.engine = rebuilt;
            resident.lang = want_lang.to_string();
        }

        let sid = voice.map(voices::resolve_sid).unwrap_or(0);
        let speed = speed.filter(|s| *s > 0.0).unwrap_or(1.0) as f32;
        let generation = GenerationConfig {
            sid,
            speed,
            ..Default::default()
        };
        let audio = resident
            .engine
            .generate_with_config(
                text,
                &generation,
                Some(move |_samples: &[f32], _progress: f32| {
                    // Returning false asks sherpa to stop generating.
                    !cancelled.load(std::sync::atomic::Ordering::Relaxed)
                }),
            )
            .ok_or_else(|| "synthesis failed".to_string())?;
        Ok((audio.samples().to_vec(), audio.sample_rate()))
    }

    /// The engine's sample rate, for a caller that needs it without generating.
    pub fn tts_loaded(&self) -> bool {
        lock(&self.tts).is_some()
    }

    pub fn stt_loaded(&self) -> bool {
        lock(&self.stt).is_some()
    }
}

/// The engine behind `SherpaSpeech`, with no sherpa type in the signature.
impl super::speech::EngineOps for Engines {
    fn transcribe(&self, samples: &[f32], sample_rate: u32) -> Result<String, String> {
        Engines::transcribe(self, samples, sample_rate)
    }

    fn synthesize(
        &self,
        text: &str,
        voice: Option<&str>,
        speed: Option<f64>,
        cancelled: Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<(Vec<f32>, i32), String> {
        Engines::synthesize(
            self,
            text,
            voice,
            speed,
            self.threads.load(std::sync::atomic::Ordering::Relaxed),
            cancelled,
        )
    }
}

impl Slots for Engines {
    /// Make the resident models equal `desired`.
    ///
    /// Build before swap, per slot: a failed load leaves the working engine
    /// resident rather than tearing it down, so a bad model selection degrades
    /// to "the old voice is still here" instead of "speech is gone". Dropping a
    /// slot runs sherpa's C destructor and frees that model's memory with the
    /// other engine untouched, which is the whole reason the sidecar held both
    /// in one process.
    fn apply(&self, desired: &Desired) -> Result<(), String> {
        let mut failures = Vec::new();
        self.threads
            .store(desired.threads, std::sync::atomic::Ordering::Relaxed);

        {
            let mut slot = lock(&self.stt);
            let current = slot.as_ref().map(|(config, _)| config);
            if current != desired.stt.as_ref() {
                match &desired.stt {
                    Some(config) => match build_recognizer(config, desired.threads) {
                        Ok(engine) => *slot = Some((config.clone(), engine)),
                        Err(err) => failures.push(err),
                    },
                    None => *slot = None,
                }
            }
        }

        {
            let mut slot = lock(&self.tts);
            let current = slot.as_ref().map(|resident| &resident.config);
            if current != desired.tts.as_ref() {
                match &desired.tts {
                    Some(config) => {
                        // Extracted HERE rather than at boot: 18 MB of
                        // dictionaries should not be written by a core whose
                        // user never turns synthesis on. `ensure` is idempotent
                        // and verifies content, so this costs a stat on every
                        // later load.
                        if let Err(err) = super::espeak::ensure(&self.root, ESPEAK_ARCHIVE) {
                            failures.push(err.message);
                            return Err(failures.join("; "));
                        }
                        match build_tts(config, desired.threads, voices::DEFAULT_LANG) {
                            Ok(engine) => {
                                *slot = Some(TtsSlot {
                                    config: config.clone(),
                                    lang: voices::DEFAULT_LANG.to_string(),
                                    engine,
                                })
                            }
                            Err(err) => failures.push(err),
                        }
                    }
                    None => *slot = None,
                }
            }
        }

        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }
}

/// The path for a role the family requires.
///
/// `config` cannot be built without every required role, so a miss here is a
/// disagreement between this file and `config::stt_required_roles` rather than
/// anything a user can cause. It still returns an error rather than unwrapping,
/// because the failure mode of being wrong is a null path reaching sherpa.
fn role(config: &SttConfig, name: &str) -> Result<String, String> {
    config
        .roles
        .get(name)
        .map(|path| path.to_string_lossy().into_owned())
        .ok_or_else(|| format!("the selected STT model has no {name} file"))
}

fn tts_role(config: &TtsConfig, name: &str) -> Result<String, String> {
    config
        .roles
        .get(name)
        .map(|path| path.to_string_lossy().into_owned())
        .ok_or_else(|| format!("the selected TTS model has no {name} file"))
}

fn build_recognizer(config: &SttConfig, threads: i32) -> Result<OfflineRecognizer, String> {
    let mut recognizer = OfflineRecognizerConfig::default();
    match config.family {
        SttFamily::Whisper => {
            recognizer.model_config.whisper = OfflineWhisperModelConfig {
                encoder: Some(role(config, "encoder")?),
                decoder: Some(role(config, "decoder")?),
                // Absent language lets Whisper auto-detect per utterance, which
                // is why the STT route's language hint reaches only the external
                // provider.
                language: None,
                ..Default::default()
            };
        }
        SttFamily::SenseVoice => {
            recognizer.model_config.sense_voice = OfflineSenseVoiceModelConfig {
                model: Some(role(config, "model")?),
                language: None,
                use_itn: true,
            };
        }
        SttFamily::Moonshine => {
            recognizer.model_config.moonshine = OfflineMoonshineModelConfig {
                preprocessor: Some(role(config, "preprocessor")?),
                encoder: Some(role(config, "encoder")?),
                uncached_decoder: Some(role(config, "uncached_decoder")?),
                cached_decoder: Some(role(config, "cached_decoder")?),
                ..Default::default()
            };
        }
        SttFamily::Paraformer => {
            recognizer.model_config.paraformer = OfflineParaformerModelConfig {
                model: Some(role(config, "model")?),
            };
        }
        SttFamily::Transducer => {
            recognizer.model_config.transducer = OfflineTransducerModelConfig {
                encoder: Some(role(config, "encoder")?),
                decoder: Some(role(config, "decoder")?),
                joiner: Some(role(config, "joiner")?),
            };
        }
        SttFamily::NemoCtc => {
            recognizer.model_config.nemo_ctc = OfflineNemoEncDecCtcModelConfig {
                model: Some(role(config, "model")?),
            };
        }
        SttFamily::Dolphin => {
            recognizer.model_config.dolphin = OfflineDolphinModelConfig {
                model: Some(role(config, "model")?),
            };
        }
        SttFamily::TelespeechCtc => {
            // TeleSpeech has no sub-struct; it is a bare path on the shared
            // config.
            recognizer.model_config.telespeech_ctc = Some(role(config, "model")?);
        }
    }
    recognizer.model_config.tokens = Some(role(config, "tokens")?);
    recognizer.model_config.num_threads = threads.max(1);
    recognizer.model_config.provider = Some(PROVIDER.to_string());
    OfflineRecognizer::create(&recognizer)
        .ok_or_else(|| "the transcription model could not be loaded".to_string())
}

fn build_tts(config: &TtsConfig, threads: i32, lang: &str) -> Result<OfflineTts, String> {
    // Checked HERE rather than trusted from the config derivation, because the
    // one espeak-ng object that imports `exit()` is `espeak_api.c.o`, where
    // `espeak_Initialize` lives, and this is the argument that reaches it.
    if !config.data_dir.is_dir() {
        return Err(format!(
            "the phonemizer data is missing at {}",
            config.data_dir.display(),
        ));
    }
    let data_dir = config.data_dir.to_string_lossy().into_owned();

    let mut model = OfflineTtsModelConfig {
        num_threads: threads.max(1),
        provider: Some(PROVIDER.to_string()),
        ..Default::default()
    };
    match config.family {
        TtsFamily::Kokoro => {
            model.kokoro = OfflineTtsKokoroModelConfig {
                model: Some(tts_role(config, "model")?),
                voices: Some(tts_role(config, "voices")?),
                tokens: Some(tts_role(config, "tokens")?),
                data_dir: Some(data_dir),
                // Multilingual Kokoro refuses to load without a language, and
                // "refuses" means it exits the process, so this is required
                // rather than optional.
                lang: Some(lang.to_string()),
                ..Default::default()
            };
        }
        TtsFamily::Kitten => {
            model.kitten = OfflineTtsKittenModelConfig {
                model: Some(tts_role(config, "model")?),
                voices: Some(tts_role(config, "voices")?),
                tokens: Some(tts_role(config, "tokens")?),
                data_dir: Some(data_dir),
                ..Default::default()
            };
        }
        TtsFamily::Vits => {
            model.vits = OfflineTtsVitsModelConfig {
                model: Some(tts_role(config, "model")?),
                tokens: Some(tts_role(config, "tokens")?),
                data_dir: Some(data_dir),
                ..Default::default()
            };
        }
        TtsFamily::Matcha => {
            model.matcha = OfflineTtsMatchaModelConfig {
                acoustic_model: Some(tts_role(config, "acoustic_model")?),
                vocoder: Some(tts_role(config, "vocoder")?),
                tokens: Some(tts_role(config, "tokens")?),
                data_dir: Some(data_dir),
                ..Default::default()
            };
        }
    }
    OfflineTts::create(&OfflineTtsConfig {
        model,
        ..Default::default()
    })
    .ok_or_else(|| "the voice model could not be loaded".to_string())
}

/// Remove SenseVoice's inline metadata tokens (`<|en|>`, `<|EMO_UNKNOWN|>`,
/// `<|Speech|>`, `<|woitn|>`, ...) that it prepends to the transcript.
///
/// Lives here rather than in a shared module because it is one family's quirk,
/// and applying it to every family would silently eat any transcript that
/// legitimately contained `<|`.
pub fn strip_special_tokens(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("<|") {
        out.push_str(&rest[..start]);
        match rest[start..].find("|>") {
            Some(end) => rest = &rest[start + end + 2..],
            // An unterminated marker is kept verbatim: it is more likely to be
            // real transcript than a truncated token.
            None => {
                out.push_str(&rest[start..]);
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out.trim().to_string()
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn sensevoice_metadata_tokens_are_stripped() {
        assert_eq!(
            strip_special_tokens("<|en|><|EMO_UNKNOWN|><|Speech|><|woitn|>hello there"),
            "hello there",
        );
        assert_eq!(strip_special_tokens("<|en|>one <|x|>two"), "one two");
    }

    #[test]
    fn plain_text_survives_unchanged_and_an_unterminated_marker_is_kept() {
        assert_eq!(strip_special_tokens("just plain text"), "just plain text");
        assert_eq!(strip_special_tokens("hi <| oops"), "hi <| oops");
    }

    #[test]
    fn an_unloaded_engine_answers_rather_than_faulting() {
        let engines = Engines::new(std::path::PathBuf::from("/nonexistent"));
        assert!(!engines.stt_loaded());
        assert!(!engines.tts_loaded());
        assert!(engines.transcribe(&[0.0], 16_000).is_err());
    }
}
