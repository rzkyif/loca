// Build as a Windows GUI-subsystem app in release so launching it never
// allocates a console window. loca-core spawns it with piped stdout/stderr, so
// it keeps logging normally; it only ever wrote to the console when Windows gave
// a console-subsystem child its own (visible) console. DO NOT REMOVE.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

// Resident HTTP speech sidecar for loca-core.
//
// One self-contained binary (statically-linked sherpa-onnx) that serves
// speech-to-text and text-to-speech over a small loopback HTTP API, across
// several sherpa-onnx model families (STT: whisper, sense-voice, moonshine,
// paraformer, transducer, nemo-ctc, dolphin, telespeech; TTS: kokoro, kitten,
// vits/piper, matcha). The loaded family is chosen by the `family` tag on each
// engine's config. It replaces the whisper-server sidecar and the kokoro-js TTS
// worker.
//
// A SINGLE instance holds each engine in a slot that can be loaded or dropped at
// runtime via `POST /configure`. Dropping an engine runs sherpa's C destructor,
// freeing that model's memory -- so disabling TTS (or STT) frees its model while
// the other stays resident, with no process restart. The core only stops the
// whole process when both modules are off. This preserves the per-module
// unload-on-disable memory behaviour the separate whisper-server / tts-worker had.
//
// Endpoints:
//   GET  /health      -> "ok" (process up; engines load eagerly from start flags).
//   POST /configure    -> body {stt:{family,...}|null, tts:{family,...}|null}: the
//                         full desired state. Each engine is (re)loaded when its
//                         config changes and dropped when null. Idempotent.
//   POST /transcribe  -> body is WAV bytes; returns {"text": "..."} (503 if no STT).
//   POST /speak       -> JSON {text, voice|sid, speed}; returns audio/wav (503 if no TTS).
//                         The voice selects the speaker AND the phonemizer
//                         language; a cross-language voice change reloads the
//                         multilingual engine in place (see voices::voice_lang).

use std::env;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::thread;

// Request-body size caps. The server is loopback-only and core-internal (see the
// bind below), so this is an OOM guard against a wedged/buggy local caller, not a
// security boundary. Each read takes cap+1 bytes so an over-limit body is
// detected and answered 413 rather than silently truncated.
const MAX_CONFIGURE_BODY: u64 = 1 << 20; // 1 MiB: configure JSON (model paths)
const MAX_SPEAK_BODY: u64 = 8 << 20; // 8 MiB: synthesis text (core chunks it)
const MAX_TRANSCRIBE_BODY: u64 = 256 << 20; // 256 MiB: audio WAV input

use sherpa_onnx::{
    GeneratedAudio, GenerationConfig, OfflineDolphinModelConfig, OfflineMoonshineModelConfig,
    OfflineNemoEncDecCtcModelConfig, OfflineParaformerModelConfig, OfflineRecognizer,
    OfflineRecognizerConfig, OfflineSenseVoiceModelConfig, OfflineTransducerModelConfig,
    OfflineTts, OfflineTtsConfig, OfflineTtsKittenModelConfig, OfflineTtsKokoroModelConfig,
    OfflineTtsMatchaModelConfig, OfflineTtsModelConfig, OfflineTtsVitsModelConfig,
    OfflineWhisperModelConfig, Wave,
};
use tempfile::NamedTempFile;

mod voices;

fn default_true() -> bool {
    true
}

/// A speech-to-text engine config, tagged by `family`. The role fields are the
/// resolved on-disk paths the core baked from the model bundle; their names match
/// the sherpa-onnx config struct fields one-for-one. Unknown fields are ignored,
/// so the core can send extras (e.g. a stray `data_dir`) without breaking.
#[derive(Clone, PartialEq, serde::Deserialize)]
#[serde(tag = "family", rename_all = "kebab-case")]
enum SttConfig {
    Whisper {
        encoder: String,
        decoder: String,
        tokens: String,
        #[serde(default)]
        language: Option<String>,
    },
    SenseVoice {
        model: String,
        tokens: String,
        #[serde(default)]
        language: Option<String>,
        #[serde(default = "default_true")]
        use_itn: bool,
    },
    Moonshine {
        preprocessor: String,
        encoder: String,
        uncached_decoder: String,
        cached_decoder: String,
        tokens: String,
    },
    Paraformer {
        model: String,
        tokens: String,
    },
    Transducer {
        encoder: String,
        decoder: String,
        joiner: String,
        tokens: String,
    },
    NemoCtc {
        model: String,
        tokens: String,
    },
    Dolphin {
        model: String,
        tokens: String,
    },
    TelespeechCtc {
        model: String,
        tokens: String,
    },
}

/// A text-to-speech engine config, tagged by `family`. `data_dir` is the bundled
/// espeak-ng phonemizer dir (not a model download). Only Kokoro bakes a
/// phonemizer language in at load time; the other families derive it from the
/// model, so only Kokoro reloads on a cross-language voice change.
#[derive(Clone, PartialEq, serde::Deserialize)]
#[serde(tag = "family", rename_all = "kebab-case")]
enum TtsConfig {
    Kokoro {
        model: String,
        voices: String,
        tokens: String,
        data_dir: String,
    },
    Kitten {
        model: String,
        voices: String,
        tokens: String,
        data_dir: String,
    },
    Vits {
        model: String,
        tokens: String,
        #[serde(default)]
        data_dir: Option<String>,
    },
    Matcha {
        acoustic_model: String,
        vocoder: String,
        tokens: String,
        #[serde(default)]
        data_dir: Option<String>,
    },
}

impl TtsConfig {
    /// Kokoro is the only family whose phonemizer language is set at load time,
    /// so it is the only one that reloads when a voice crosses languages.
    fn is_kokoro(&self) -> bool {
        matches!(self, TtsConfig::Kokoro { .. })
    }
}

/// Full desired engine state. The core sends both fields every time; a null (or
/// absent) field means "unload that engine".
#[derive(serde::Deserialize)]
struct ConfigureReq {
    #[serde(default)]
    stt: Option<SttConfig>,
    #[serde(default)]
    tts: Option<TtsConfig>,
}

/// One engine and the paths it was loaded from (so reconfigure is a no-op when
/// unchanged and a reload when the paths differ).
struct Slot<P, E> {
    paths: Option<P>,
    engine: Option<E>,
}

impl<P, E> Slot<P, E> {
    fn empty() -> Self {
        Self {
            paths: None,
            engine: None,
        }
    }
}

/// The TTS engine plus the paths AND espeak language it was loaded with.
/// Separate from the generic `Slot` because multilingual Kokoro fixes its
/// phonemizer language at load time, so the server tracks the resident language
/// to know when a voice change requires a reload.
struct TtsSlot {
    paths: Option<TtsConfig>,
    lang: String,
    engine: Option<OfflineTts>,
}

impl TtsSlot {
    fn empty() -> Self {
        Self {
            paths: None,
            lang: String::new(),
            engine: None,
        }
    }
}

struct State {
    stt: Mutex<Slot<SttConfig, OfflineRecognizer>>,
    tts: Mutex<TtsSlot>,
    threads: i32,
    // ONNX Runtime execution provider ("cpu", "cuda", "directml", "coreml").
    // Fixed at process start from --provider; a GPU value only takes effect when
    // this binary was built against a GPU-enabled sherpa-onnx/onnxruntime (the
    // GPU build variant), otherwise onnxruntime falls back to CPU at load.
    provider: String,
}

#[derive(serde::Deserialize)]
struct SpeakReq {
    text: String,
    #[serde(default)]
    voice: Option<String>,
    #[serde(default)]
    sid: Option<i32>,
    #[serde(default = "default_speed")]
    speed: f32,
}

fn default_speed() -> f32 {
    1.0
}

#[derive(serde::Serialize)]
struct TextResp {
    text: String,
}

/// Lock a mutex, recovering a poisoned guard rather than panicking (`-D warnings`
/// forbids `.unwrap()`; a panic in a locked section cannot happen here).
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match m.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn flag(args: &[String], key: &str) -> Option<String> {
    args.iter()
        .position(|a| a == key)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn build_recognizer(
    cfg: &SttConfig,
    threads: i32,
    provider: &str,
) -> Result<OfflineRecognizer, String> {
    let mut config = OfflineRecognizerConfig::default();
    // Each arm populates exactly one model-family sub-config and yields the
    // shared tokens path; sherpa picks the family from whichever sub-config is set.
    let tokens = match cfg {
        SttConfig::Whisper {
            encoder,
            decoder,
            tokens,
            language,
        } => {
            config.model_config.whisper = OfflineWhisperModelConfig {
                encoder: Some(encoder.clone()),
                decoder: Some(decoder.clone()),
                // Absent language lets Whisper auto-detect per utterance.
                language: language.clone(),
                ..Default::default()
            };
            tokens
        }
        SttConfig::SenseVoice {
            model,
            tokens,
            language,
            use_itn,
        } => {
            config.model_config.sense_voice = OfflineSenseVoiceModelConfig {
                model: Some(model.clone()),
                language: language.clone(),
                use_itn: *use_itn,
            };
            tokens
        }
        SttConfig::Moonshine {
            preprocessor,
            encoder,
            uncached_decoder,
            cached_decoder,
            tokens,
        } => {
            config.model_config.moonshine = OfflineMoonshineModelConfig {
                preprocessor: Some(preprocessor.clone()),
                encoder: Some(encoder.clone()),
                uncached_decoder: Some(uncached_decoder.clone()),
                cached_decoder: Some(cached_decoder.clone()),
                ..Default::default()
            };
            tokens
        }
        SttConfig::Paraformer { model, tokens } => {
            config.model_config.paraformer = OfflineParaformerModelConfig {
                model: Some(model.clone()),
            };
            tokens
        }
        SttConfig::Transducer {
            encoder,
            decoder,
            joiner,
            tokens,
        } => {
            config.model_config.transducer = OfflineTransducerModelConfig {
                encoder: Some(encoder.clone()),
                decoder: Some(decoder.clone()),
                joiner: Some(joiner.clone()),
            };
            tokens
        }
        SttConfig::NemoCtc { model, tokens } => {
            config.model_config.nemo_ctc = OfflineNemoEncDecCtcModelConfig {
                model: Some(model.clone()),
            };
            tokens
        }
        SttConfig::Dolphin { model, tokens } => {
            config.model_config.dolphin = OfflineDolphinModelConfig {
                model: Some(model.clone()),
            };
            tokens
        }
        SttConfig::TelespeechCtc { model, tokens } => {
            // TeleSpeech has no sub-struct; it is a bare path on the shared config.
            config.model_config.telespeech_ctc = Some(model.clone());
            tokens
        }
    };
    config.model_config.tokens = Some(tokens.clone());
    config.model_config.num_threads = threads.max(1);
    config.model_config.provider = Some(provider.to_string());
    OfflineRecognizer::create(&config).ok_or_else(|| "failed to load stt model".to_string())
}

fn build_tts(
    cfg: &TtsConfig,
    threads: i32,
    lang: &str,
    provider: &str,
) -> Result<OfflineTts, String> {
    let mut model = OfflineTtsModelConfig {
        num_threads: threads.max(1),
        provider: Some(provider.to_string()),
        ..Default::default()
    };
    match cfg {
        TtsConfig::Kokoro {
            model: m,
            voices,
            tokens,
            data_dir,
        } => {
            model.kokoro = OfflineTtsKokoroModelConfig {
                model: Some(m.clone()),
                voices: Some(voices.clone()),
                tokens: Some(tokens.clone()),
                // espeak-ng phonemizer data plus the language to phonemize in.
                // Multilingual Kokoro (v1.0+) refuses to load (it exits the
                // process) without a language or a lexicon, so `lang` is
                // required here, not optional.
                data_dir: Some(data_dir.clone()),
                lang: Some(lang.to_string()),
                ..Default::default()
            };
        }
        TtsConfig::Kitten {
            model: m,
            voices,
            tokens,
            data_dir,
        } => {
            model.kitten = OfflineTtsKittenModelConfig {
                model: Some(m.clone()),
                voices: Some(voices.clone()),
                tokens: Some(tokens.clone()),
                data_dir: Some(data_dir.clone()),
                ..Default::default()
            };
        }
        TtsConfig::Vits {
            model: m,
            tokens,
            data_dir,
        } => {
            model.vits = OfflineTtsVitsModelConfig {
                model: Some(m.clone()),
                tokens: Some(tokens.clone()),
                data_dir: data_dir.clone(),
                ..Default::default()
            };
        }
        TtsConfig::Matcha {
            acoustic_model,
            vocoder,
            tokens,
            data_dir,
        } => {
            model.matcha = OfflineTtsMatchaModelConfig {
                acoustic_model: Some(acoustic_model.clone()),
                vocoder: Some(vocoder.clone()),
                tokens: Some(tokens.clone()),
                data_dir: data_dir.clone(),
                ..Default::default()
            };
        }
    }
    let config = OfflineTtsConfig {
        model,
        ..Default::default()
    };
    OfflineTts::create(&config).ok_or_else(|| "failed to load tts model".to_string())
}

/// Reconcile the STT slot to `want`. Builds the replacement BEFORE dropping the
/// current engine, so a failed load (bad or unreadable model path) leaves the
/// working engine intact instead of tearing it down. Swapping the slot then
/// drops the old engine via sherpa's C destructor, freeing its model's memory.
fn set_stt(state: &State, want: Option<SttConfig>) -> Result<(), String> {
    let mut slot = lock(&state.stt);
    if slot.paths == want {
        return Ok(());
    }
    match want {
        Some(p) => {
            let engine = build_recognizer(&p, state.threads, &state.provider)?;
            slot.engine = Some(engine);
            slot.paths = Some(p);
            eprintln!("loca-core-speech: STT loaded");
        }
        None => {
            slot.engine = None;
            slot.paths = None;
            eprintln!("loca-core-speech: STT unloaded");
        }
    }
    Ok(())
}

fn set_tts(state: &State, want: Option<TtsConfig>) -> Result<(), String> {
    let mut slot = lock(&state.tts);
    if slot.paths == want {
        return Ok(());
    }
    match want {
        Some(p) => {
            // Load in the default language; a /speak for a voice in another
            // language reloads the engine (see the /speak handler). Build before
            // swapping so a failed load keeps the current engine.
            let lang = voices::DEFAULT_LANG;
            let engine = build_tts(&p, state.threads, lang, &state.provider)?;
            eprintln!(
                "loca-core-speech: TTS loaded (lang={}, sample_rate={}, speakers={})",
                lang,
                engine.sample_rate(),
                engine.num_speakers()
            );
            slot.engine = Some(engine);
            slot.lang = lang.to_string();
            slot.paths = Some(p);
        }
        None => {
            slot.engine = None;
            slot.lang = String::new();
            slot.paths = None;
            eprintln!("loca-core-speech: TTS unloaded");
        }
    }
    Ok(())
}

// sherpa's `Wave::read` only decodes from a file path, so the request bytes go
// to a private temp file first. `NamedTempFile` picks an unpredictable name and
// creates it with O_EXCL (no symlink or predictable-path race), and the
// `TempPath` removes it on drop. The fd is closed (into_temp_path) before
// `Wave::read` reopens the path, so this is also safe on Windows.
fn transcribe(rec: &OfflineRecognizer, wav_bytes: &[u8]) -> Result<String, String> {
    let mut tmp = NamedTempFile::new().map_err(|e| format!("temp file: {e}"))?;
    tmp.write_all(wav_bytes)
        .map_err(|e| format!("write temp: {e}"))?;
    tmp.flush().map_err(|e| format!("flush temp: {e}"))?;
    let path = tmp.into_temp_path();
    let wave = Wave::read(&path.to_string_lossy());
    drop(path);
    let wave = wave.ok_or_else(|| "could not decode WAV input".to_string())?;
    let stream = rec.create_stream();
    stream.accept_waveform(wave.sample_rate(), wave.samples());
    rec.decode(&stream);
    Ok(stream.get_result().map(|r| r.text).unwrap_or_default())
}

/// Remove SenseVoice's inline metadata tokens (`<|en|>`, `<|EMO_UNKNOWN|>`,
/// `<|Speech|>`, `<|woitn|>`, ...) that it prepends to the transcript, leaving
/// just the spoken text. A no-op for text with no `<|...|>` runs, so it is only
/// applied for the SenseVoice family.
fn strip_special_tokens(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("<|") {
        out.push_str(&rest[..start]);
        match rest[start..].find("|>") {
            Some(end) => rest = &rest[start + end + 2..],
            // Unterminated marker: keep the remainder verbatim and stop.
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

/// The espeak language a /speak request needs, mirroring `synthesize`'s speaker
/// resolution: an explicit numeric sid wins over a voice name. Falls back to the
/// default language when neither resolves to a known voice.
fn req_lang(req: &SpeakReq) -> &'static str {
    if let Some(name) = req.sid.and_then(voices::name_for_sid) {
        return voices::voice_lang(name);
    }
    match req.voice.as_deref() {
        Some(voice) => voices::voice_lang(voice),
        None => voices::DEFAULT_LANG,
    }
}

fn synthesize(tts: &OfflineTts, req: &SpeakReq) -> Result<GeneratedAudio, String> {
    let sid = req
        .sid
        .or_else(|| req.voice.as_deref().map(voices::resolve_sid))
        .unwrap_or(0);
    let gen = GenerationConfig {
        sid,
        speed: if req.speed > 0.0 { req.speed } else { 1.0 },
        ..Default::default()
    };
    tts.generate_with_config(&req.text, &gen, None::<fn(&[f32], f32) -> bool>)
        .ok_or_else(|| "synthesis failed".to_string())
}

/// Encode mono f32 PCM samples as a 16-bit WAV byte buffer (canonical RIFF
/// layout, sample rate at byte offset 24). Builds the response in memory rather
/// than round-tripping through `GeneratedAudio::save` and a temp file.
fn wav_from_samples(samples: &[f32], sample_rate: i32) -> Vec<u8> {
    let data_len = samples.len() * 2;
    let sr = sample_rate.max(1) as u32;
    let mut buf = Vec::with_capacity(44 + data_len);
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&((36 + data_len) as u32).to_le_bytes());
    buf.extend_from_slice(b"WAVE");
    buf.extend_from_slice(b"fmt ");
    buf.extend_from_slice(&16u32.to_le_bytes()); // PCM fmt chunk size
    buf.extend_from_slice(&1u16.to_le_bytes()); // format = PCM
    buf.extend_from_slice(&1u16.to_le_bytes()); // channels = mono
    buf.extend_from_slice(&sr.to_le_bytes());
    buf.extend_from_slice(&(sr * 2).to_le_bytes()); // byte rate (mono, 2 bytes/sample)
    buf.extend_from_slice(&2u16.to_le_bytes()); // block align
    buf.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&(data_len as u32).to_le_bytes());
    for &s in samples {
        buf.extend_from_slice(&((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
    }
    buf
}

fn respond_text(request: tiny_http::Request, code: u16, body: &str) {
    let _ = request.respond(tiny_http::Response::from_string(body).with_status_code(code));
}

fn respond_json(request: tiny_http::Request, code: u16, body: String) {
    match tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]) {
        Ok(h) => {
            let _ = request.respond(
                tiny_http::Response::from_string(body)
                    .with_status_code(code)
                    .with_header(h),
            );
        }
        Err(()) => respond_text(request, code, "header error"),
    }
}

fn handle(mut request: tiny_http::Request, state: &State) {
    let method = request.method().as_str().to_string();
    let path = request.url().split('?').next().unwrap_or("").to_string();

    match (method.as_str(), path.as_str()) {
        ("GET", "/health") => respond_text(request, 200, "ok"),
        ("POST", "/configure") => {
            let mut body = String::new();
            if request
                .as_reader()
                .take(MAX_CONFIGURE_BODY + 1)
                .read_to_string(&mut body)
                .is_err()
            {
                return respond_text(request, 400, "read error");
            }
            if body.len() as u64 > MAX_CONFIGURE_BODY {
                return respond_text(request, 413, "configure body too large");
            }
            let req: ConfigureReq = match serde_json::from_str(&body) {
                Ok(r) => r,
                Err(e) => return respond_text(request, 400, &format!("bad json: {e}")),
            };
            if let Err(e) = set_stt(state, req.stt) {
                return respond_text(request, 500, &e);
            }
            if let Err(e) = set_tts(state, req.tts) {
                return respond_text(request, 500, &e);
            }
            respond_text(request, 200, "ok");
        }
        ("POST", "/transcribe") => {
            let mut body = Vec::new();
            if request
                .as_reader()
                .take(MAX_TRANSCRIBE_BODY + 1)
                .read_to_end(&mut body)
                .is_err()
            {
                return respond_text(request, 400, "read error");
            }
            if body.len() as u64 > MAX_TRANSCRIBE_BODY {
                return respond_text(request, 413, "transcribe body too large");
            }
            let slot = lock(&state.stt);
            let Some(rec) = slot.engine.as_ref() else {
                return respond_text(request, 503, "stt not loaded");
            };
            // SenseVoice prepends inline metadata tokens to its transcript; the
            // other families return plain text.
            let strip = matches!(slot.paths, Some(SttConfig::SenseVoice { .. }));
            match transcribe(rec, &body) {
                Ok(text) => {
                    let text = if strip {
                        strip_special_tokens(&text)
                    } else {
                        text
                    };
                    let json = serde_json::to_string(&TextResp { text })
                        .unwrap_or_else(|_| "{\"text\":\"\"}".to_string());
                    respond_json(request, 200, json);
                }
                Err(e) => respond_text(request, 500, &e),
            }
        }
        ("POST", "/speak") => {
            let mut body = String::new();
            if request
                .as_reader()
                .take(MAX_SPEAK_BODY + 1)
                .read_to_string(&mut body)
                .is_err()
            {
                return respond_text(request, 400, "read error");
            }
            if body.len() as u64 > MAX_SPEAK_BODY {
                return respond_text(request, 413, "speak body too large");
            }
            let req: SpeakReq = match serde_json::from_str(&body) {
                Ok(r) => r,
                Err(e) => return respond_text(request, 400, &format!("bad json: {e}")),
            };
            let mut slot = lock(&state.tts);
            if slot.engine.is_none() {
                return respond_text(request, 503, "tts not loaded");
            }
            // Multilingual Kokoro bakes its phonemizer language in at load time,
            // so when the requested voice belongs to another language, rebuild
            // the engine for it (build-before-swap keeps the working one if the
            // rebuild fails). espeak covers every Kokoro language, so a reload
            // only happens on a cross-language voice change. Other families fix
            // their language by model, so they never reload here.
            let is_kokoro = slot.paths.as_ref().is_some_and(TtsConfig::is_kokoro);
            let want_lang = req_lang(&req);
            if is_kokoro && slot.lang != want_lang {
                let Some(paths) = slot.paths.clone() else {
                    return respond_text(request, 503, "tts not loaded");
                };
                match build_tts(&paths, state.threads, want_lang, &state.provider) {
                    Ok(engine) => {
                        eprintln!(
                            "loca-core-speech: TTS reloaded ({} -> {want_lang})",
                            slot.lang
                        );
                        slot.engine = Some(engine);
                        slot.lang = want_lang.to_string();
                    }
                    Err(e) => {
                        return respond_text(
                            request,
                            500,
                            &format!("tts reload ({want_lang}): {e}"),
                        )
                    }
                }
            }
            let Some(tts) = slot.engine.as_ref() else {
                return respond_text(request, 503, "tts not loaded");
            };
            match synthesize(tts, &req) {
                Ok(audio) => {
                    let bytes = wav_from_samples(audio.samples(), audio.sample_rate());
                    match tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"audio/wav"[..]) {
                        Ok(h) => {
                            let _ = request
                                .respond(tiny_http::Response::from_data(bytes).with_header(h));
                        }
                        Err(()) => respond_text(request, 500, "header error"),
                    }
                }
                Err(e) => respond_text(request, 500, &e),
            }
        }
        _ => respond_text(request, 404, "not found"),
    }
}

/// Build the initial desired state from the `--stt-config` / `--tts-config` JSON
/// flags so a freshly started instance loads its models before serving
/// (readiness == loaded). Each flag carries the same tagged config object POST
/// /configure accepts, so startup and runtime reconfigure share one code path.
/// The core may later change this at runtime via `/configure`.
fn initial_config(args: &[String]) -> ConfigureReq {
    ConfigureReq {
        stt: parse_config_flag(args, "--stt-config"),
        tts: parse_config_flag(args, "--tts-config"),
    }
}

/// Parse a `--*-config <json>` flag, logging and ignoring a malformed value
/// (the engine then stays unloaded rather than crashing the process at boot).
fn parse_config_flag<T: serde::de::DeserializeOwned>(args: &[String], key: &str) -> Option<T> {
    let raw = flag(args, key)?;
    match serde_json::from_str(&raw) {
        Ok(v) => Some(v),
        Err(e) => {
            eprintln!("loca-core-speech: ignoring {key}: {e}");
            None
        }
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = env::args().collect();
    let host = flag(&args, "--host").unwrap_or_else(|| "127.0.0.1".to_string());
    let port: u16 = flag(&args, "--port")
        .and_then(|s| s.parse().ok())
        .unwrap_or(7702);
    let threads: i32 = flag(&args, "--threads")
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| {
            thread::available_parallelism()
                .map(|n| n.get() as i32)
                .unwrap_or(4)
                .min(4)
        });
    let provider = flag(&args, "--provider").unwrap_or_else(|| "cpu".to_string());

    let state = Arc::new(State {
        stt: Mutex::new(Slot::empty()),
        tts: Mutex::new(TtsSlot::empty()),
        threads,
        provider,
    });

    let initial = initial_config(&args);
    set_stt(&state, initial.stt)?;
    set_tts(&state, initial.tts)?;

    // No per-request auth on the endpoints by design: core spawns this sidecar
    // with a FIXED loopback host (see sidecars/speech.ts; --host defaults to
    // 127.0.0.1) and never exposes it off-box, so it is reachable only by local
    // processes of the same user. If a non-loopback bind is ever introduced, add a
    // spawn-time shared-secret header check here before relying on that.
    let server = tiny_http::Server::http((host.as_str(), port))
        .map_err(|e| format!("bind {host}:{port}: {e}"))?;
    let server = Arc::new(server);
    eprintln!("loca-core-speech: listening on {host}:{port}");

    let n_workers = (threads as usize).clamp(1, 4);
    let mut handles = Vec::new();
    for _ in 0..n_workers {
        let server = Arc::clone(&server);
        let state = Arc::clone(&state);
        handles.push(thread::spawn(move || {
            while let Ok(request) = server.recv() {
                handle(request, &state);
            }
        }));
    }
    for h in handles {
        let _ = h.join();
    }
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("loca-core-speech: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_sensevoice_metadata_tokens() {
        assert_eq!(
            strip_special_tokens("<|en|><|EMO_UNKNOWN|><|Speech|><|woitn|>hello there"),
            "hello there"
        );
        // Tokens interleaved with text are all removed.
        assert_eq!(strip_special_tokens("<|en|>one <|x|>two"), "one two");
    }

    #[test]
    fn strip_special_tokens_is_a_noop_for_plain_text() {
        assert_eq!(strip_special_tokens("just plain text"), "just plain text");
    }

    #[test]
    fn strip_special_tokens_keeps_an_unterminated_marker() {
        // A lone "<|" with no closing "|>" is kept verbatim (and trimmed).
        assert_eq!(strip_special_tokens("hi <| oops"), "hi <| oops");
    }

    #[test]
    fn sense_voice_config_defaults_use_itn_on() {
        // Absent use_itn defaults to true (inverse text normalization on).
        let cfg: Option<SttConfig> =
            serde_json::from_str(r#"{"family":"sense-voice","model":"m","tokens":"t"}"#).ok();
        assert!(matches!(
            cfg,
            Some(SttConfig::SenseVoice { use_itn: true, .. })
        ));
    }

    #[test]
    fn tts_config_tags_resolve_to_families() {
        let kokoro: Option<TtsConfig> = serde_json::from_str(
            r#"{"family":"kokoro","model":"m","voices":"v","tokens":"t","data_dir":"d"}"#,
        )
        .ok();
        assert_eq!(kokoro.map(|c| c.is_kokoro()), Some(true));
        let matcha: Option<TtsConfig> = serde_json::from_str(
            r#"{"family":"matcha","acoustic_model":"a","vocoder":"v","tokens":"t"}"#,
        )
        .ok();
        assert_eq!(matcha.map(|c| c.is_kokoro()), Some(false));
    }
}
