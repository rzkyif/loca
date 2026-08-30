//! What the settings say the engine should be holding: turning
//! `{modelType, modelFiles}` into a set of resolved, existing file paths, or
//! into "not loaded" when anything is missing.
//!
//! Not loaded is a normal state rather than an error. An enabled engine whose
//! weights have not finished downloading resolves to `None` so the OTHER engine
//! keeps serving, and the download-completion hook re-derives once the files
//! land.
//!
//! Role validation is containment: handing sherpa a config with a role missing
//! is one of the inputs that can terminate the process rather than fail, so the
//! required roles are checked here, where a mismatch is a log line.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use indexmap::IndexMap;

use crate::contract::inbound::{SttFamily, TtsFamily};
use crate::models::required::{model_files_map, stt_uses_local, tts_uses_local};
use crate::settings::access::{num_setting, str_setting};
use crate::settings::SettingsRecord;

/// Threads for the ONNX session. Shared by both engines because they share the
/// process, and keyed on the STT knob because transcription is the heavier
/// workload. Matches `speechDesiredState`.
const THREADS_KEY: &str = "stt.threads";
const DEFAULT_THREADS: f64 = 4.0;

/// The settings that change what the engine should hold. A change to anything
/// outside this set does not touch the models. Both the STT and the TTS model
/// pairs belong here: omitting one leaves the running engine on the old weights
/// until something unrelated reconciles.
pub const SPEECH_KEYS: &[&str] = &[
    "stt.enabled",
    "stt.provider",
    "stt.modelType",
    "stt.modelPath",
    "stt.modelFiles",
    "stt.threads",
    "tts.enabled",
    "tts.provider",
    "tts.modelType",
    "tts.modelPath",
    "tts.modelFiles",
];

/// One engine's resolved on-disk state. `roles` maps a sherpa role name
/// (`encoder`, `tokens`, `voices`, ...) to an existing file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SttConfig {
    pub family: SttFamily,
    pub roles: BTreeMap<String, PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TtsConfig {
    pub family: TtsFamily,
    pub roles: BTreeMap<String, PathBuf>,
    /// The bundled espeak-ng phonemizer directory. Not a model download, and
    /// required by every family that phonemizes through espeak-ng.
    pub data_dir: PathBuf,
}

/// The full desired state of the local engine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Desired {
    pub stt: Option<SttConfig>,
    pub tts: Option<TtsConfig>,
    pub threads: i32,
}

impl Desired {
    /// True when neither engine is wanted, which is when the subsystem row
    /// disappears from the core status entirely.
    pub fn is_off(&self) -> bool {
        self.stt.is_none() && self.tts.is_none()
    }
}

/// The file roles a family cannot load without.
fn stt_required_roles(family: SttFamily) -> &'static [&'static str] {
    match family {
        SttFamily::Whisper => &["encoder", "decoder", "tokens"],
        SttFamily::SenseVoice => &["model", "tokens"],
        SttFamily::Moonshine => &[
            "preprocessor",
            "encoder",
            "uncached_decoder",
            "cached_decoder",
            "tokens",
        ],
        SttFamily::Paraformer => &["model", "tokens"],
        SttFamily::Transducer => &["encoder", "decoder", "joiner", "tokens"],
        SttFamily::NemoCtc => &["model", "tokens"],
        SttFamily::Dolphin => &["model", "tokens"],
        SttFamily::TelespeechCtc => &["model", "tokens"],
    }
}

fn tts_required_roles(family: TtsFamily) -> &'static [&'static str] {
    match family {
        TtsFamily::Kokoro => &["model", "voices", "tokens"],
        TtsFamily::Kitten => &["model", "voices", "tokens"],
        TtsFamily::Vits => &["model", "tokens"],
        TtsFamily::Matcha => &["acoustic_model", "vocoder", "tokens"],
    }
}

/// Parse a `*.modelType` setting into a family.
fn stt_family(raw: &str) -> Option<SttFamily> {
    serde_json::from_value(serde_json::Value::String(raw.to_string())).ok()
}

fn tts_family(raw: &str) -> Option<TtsFamily> {
    serde_json::from_value(serde_json::Value::String(raw.to_string())).ok()
}

/// Resolve the desired engine state from `settings`. `resolve` turns an
/// `@user/repo/branch/file` spec into an on-disk path and `exists` answers
/// whether it is a readable file; both are injected so this module never
/// touches the filesystem.
pub fn desired(
    settings: &SettingsRecord,
    espeak_data_dir: &Path,
    resolve: &dyn Fn(&str) -> PathBuf,
    exists: &dyn Fn(&Path) -> bool,
) -> Desired {
    let threads = num_setting(settings, THREADS_KEY, DEFAULT_THREADS);
    // Clamped rather than trusted: sherpa takes this straight into the ONNX
    // session, which does not handle a zero or negative thread count.
    let threads = (threads as i64).clamp(1, 64) as i32;

    let stt = stt_uses_local(settings)
        .then(|| {
            let family = stt_family(&str_setting(settings, "stt.modelType", ""))?;
            let roles = resolve_roles(
                settings.get("stt.modelFiles"),
                stt_required_roles(family),
                resolve,
                exists,
            )?;
            Some(SttConfig { family, roles })
        })
        .flatten();

    let tts = tts_uses_local(settings)
        .then(|| {
            let family = tts_family(&str_setting(settings, "tts.modelType", ""))?;
            let roles = resolve_roles(
                settings.get("tts.modelFiles"),
                tts_required_roles(family),
                resolve,
                exists,
            )?;
            Some(TtsConfig {
                family,
                roles,
                data_dir: espeak_data_dir.to_path_buf(),
            })
        })
        .flatten();

    Desired { stt, tts, threads }
}

/// Resolve a `*.modelFiles` bundle to role to path, or `None` when it is
/// malformed, incomplete for the family, or not fully downloaded.
fn resolve_roles(
    model_files: Option<&serde_json::Value>,
    required: &[&str],
    resolve: &dyn Fn(&str) -> PathBuf,
    exists: &dyn Fn(&Path) -> bool,
) -> Option<BTreeMap<String, PathBuf>> {
    let map: IndexMap<String, String> = model_files_map(model_files)?;
    let mut resolved = BTreeMap::new();
    for (role, spec) in &map {
        let path = resolve(spec);
        if !exists(&path) {
            return None;
        }
        resolved.insert(role.clone(), path);
    }
    // A bundle missing a role the family needs would be handed to sherpa with a
    // null path, which is one of the inputs that terminates rather than fails.
    if required.iter().any(|role| !resolved.contains_key(*role)) {
        return None;
    }
    Some(resolved)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use serde_json::json;

    use super::*;

    fn espeak() -> PathBuf {
        PathBuf::from("/root/espeak-ng-data")
    }

    /// The two seams `desired` takes, named so the fixture signature stays
    /// readable: a spec-to-path resolver and a does-this-file-exist check.
    type Resolver = Box<dyn Fn(&str) -> PathBuf>;
    type Exists = Box<dyn Fn(&Path) -> bool>;

    /// Every spec resolves under /models, and everything resolved exists.
    fn present() -> (Resolver, Exists) {
        (
            Box::new(|spec: &str| PathBuf::from("/models").join(spec.trim_start_matches('@'))),
            Box::new(|_: &Path| true),
        )
    }

    fn whisper_bundle() -> String {
        // Deliberately encoder-first, which is catalog file order rather than
        // alphabetical: `model_files_map` preserves it and a sorted parse would
        // be a different string on disk.
        r#"{"encoder":"@a/b/main/enc.onnx","decoder":"@a/b/main/dec.onnx","tokens":"@a/b/main/tok.txt"}"#
            .to_string()
    }

    fn settings(pairs: Vec<(&str, serde_json::Value)>) -> SettingsRecord {
        let mut record = SettingsRecord::new();
        for (key, value) in pairs {
            record.insert(key.to_string(), value);
        }
        record
    }

    #[test]
    fn a_fresh_install_wants_no_engine_at_all() {
        // Both speech flags default to false, so a core nobody has configured
        // holds no model and the subsystem row does not appear.
        let (resolve, exists) = present();
        let state = desired(&SettingsRecord::new(), &espeak(), &resolve, &exists);
        assert!(state.is_off());
        assert_eq!(state.threads, 4);
    }

    #[test]
    fn stt_loads_when_enabled_local_and_downloaded() {
        let (resolve, exists) = present();
        let state = desired(
            &settings(vec![
                ("stt.enabled", json!(true)),
                ("stt.modelType", json!("whisper")),
                ("stt.modelFiles", json!(whisper_bundle())),
            ]),
            &espeak(),
            &resolve,
            &exists,
        );
        let stt = state.stt.expect("stt is wanted");
        assert_eq!(stt.family, SttFamily::Whisper);
        assert_eq!(stt.roles.len(), 3);
        assert_eq!(
            stt.roles.get("encoder"),
            Some(&PathBuf::from("/models/a/b/main/enc.onnx")),
        );
        assert!(state.tts.is_none(), "tts was never enabled");
    }

    #[test]
    fn an_external_provider_leaves_the_local_engine_unloaded() {
        // The engine is for LOCAL speech only. External transcription must not
        // hold a multi-gigabyte model resident for nothing.
        let (resolve, exists) = present();
        let state = desired(
            &settings(vec![
                ("stt.enabled", json!(true)),
                ("stt.provider", json!("external")),
                ("stt.modelType", json!("whisper")),
                ("stt.modelFiles", json!(whisper_bundle())),
            ]),
            &espeak(),
            &resolve,
            &exists,
        );
        assert!(state.stt.is_none());
    }

    #[test]
    fn a_model_still_downloading_resolves_to_not_loaded_and_leaves_the_other_engine_serving() {
        // The property that makes "not loaded" a state rather than an error:
        // TTS is mid-download and STT keeps working.
        let resolve = |spec: &str| PathBuf::from("/models").join(spec.trim_start_matches('@'));
        let exists = |path: &Path| !path.to_string_lossy().contains("kokoro");
        let state = desired(
            &settings(vec![
                ("stt.enabled", json!(true)),
                ("stt.modelType", json!("whisper")),
                ("stt.modelFiles", json!(whisper_bundle())),
                ("tts.enabled", json!(true)),
                ("tts.modelType", json!("kokoro")),
                (
                    "tts.modelFiles",
                    json!(
                        r#"{"model":"@k/kokoro/main/m.onnx","voices":"@k/kokoro/main/v.bin","tokens":"@k/kokoro/main/t.txt"}"#
                    ),
                ),
            ]),
            &espeak(),
            &resolve,
            &exists,
        );
        assert!(state.stt.is_some(), "stt keeps serving");
        assert!(
            state.tts.is_none(),
            "tts is not loaded until its files land"
        );
        assert!(!state.is_off(), "the engine still runs for stt");
    }

    #[test]
    fn a_bundle_missing_a_role_the_family_needs_does_not_load() {
        // Whisper without its decoder would be handed to sherpa with a null
        // path, which is a terminate-the-process input rather than an error.
        let (resolve, exists) = present();
        let state = desired(
            &settings(vec![
                ("stt.enabled", json!(true)),
                ("stt.modelType", json!("whisper")),
                (
                    "stt.modelFiles",
                    json!(r#"{"encoder":"@a/b/main/enc.onnx","tokens":"@a/b/main/tok.txt"}"#),
                ),
            ]),
            &espeak(),
            &resolve,
            &exists,
        );
        assert!(state.stt.is_none());
    }

    #[test]
    fn every_family_declares_the_roles_its_catalog_bundles_carry() {
        // Checked against the catalog rather than against itself: a family
        // whose required roles are wrong refuses a model the user can select.
        const DEV_CATALOG: &str = include_str!("../../models/catalog.dev.json");
        let file: serde_json::Value =
            serde_json::from_str(DEV_CATALOG).expect("the dev catalog parses");
        let payload: crate::contract::inbound::CatalogPayload =
            serde_json::from_value(file["payload"].clone()).expect("the payload deserialises");

        for model in &payload.stt.models {
            for quant in &model.quants {
                let roles: Vec<&str> = quant.files.iter().map(|f| f.role.as_str()).collect();
                for required in stt_required_roles(model.family) {
                    assert!(
                        roles.contains(required),
                        "stt model {} lacks the required role {required}, roles are {roles:?}",
                        model.id,
                    );
                }
            }
        }
        for model in &payload.tts.models {
            for quant in &model.quants {
                let roles: Vec<&str> = quant.files.iter().map(|f| f.role.as_str()).collect();
                for required in tts_required_roles(model.family) {
                    assert!(
                        roles.contains(required),
                        "tts model {} lacks the required role {required}, roles are {roles:?}",
                        model.id,
                    );
                }
            }
        }
    }

    #[test]
    fn a_malformed_or_unknown_selection_does_not_load() {
        let (resolve, exists) = present();
        for (family, files) in [
            ("not-a-family", whisper_bundle()),
            ("whisper", "not json at all".to_string()),
            ("whisper", "{}".to_string()),
            ("", whisper_bundle()),
        ] {
            let state = desired(
                &settings(vec![
                    ("stt.enabled", json!(true)),
                    ("stt.modelType", json!(family)),
                    ("stt.modelFiles", json!(files)),
                ]),
                &espeak(),
                &resolve,
                &exists,
            );
            assert!(
                state.stt.is_none(),
                "family {family:?} with {files:?} loaded"
            );
        }
    }

    #[test]
    fn the_thread_count_is_clamped_rather_than_passed_through() {
        let (resolve, exists) = present();
        for (raw, want) in [(0.0, 1), (-8.0, 1), (2.0, 2), (4096.0, 64)] {
            let state = desired(
                &settings(vec![("stt.threads", json!(raw))]),
                &espeak(),
                &resolve,
                &exists,
            );
            assert_eq!(state.threads, want, "threads {raw}");
        }
    }

    #[test]
    fn tts_carries_the_bundled_phonemizer_directory() {
        let (resolve, exists) = present();
        let state = desired(
            &settings(vec![
                ("tts.enabled", json!(true)),
                ("tts.modelType", json!("kokoro")),
                (
                    "tts.modelFiles",
                    json!(
                        r#"{"model":"@k/k/main/m.onnx","voices":"@k/k/main/v.bin","tokens":"@k/k/main/t.txt"}"#
                    ),
                ),
            ]),
            &espeak(),
            &resolve,
            &exists,
        );
        let tts = state.tts.expect("tts is wanted");
        assert_eq!(tts.data_dir, espeak());
        assert_eq!(tts.family, TtsFamily::Kokoro);
    }

    #[test]
    fn the_watched_key_set_covers_every_key_the_derivation_reads() {
        // A key the derivation reads but the watcher ignores means a user's
        // choice does nothing until an unrelated change reconciles.
        for key in [
            "stt.enabled",
            "stt.provider",
            "stt.modelType",
            "stt.modelFiles",
            "stt.threads",
            "tts.enabled",
            "tts.provider",
            "tts.modelType",
            "tts.modelFiles",
        ] {
            assert!(SPEECH_KEYS.contains(&key), "{key} is read but not watched");
        }
    }
}
