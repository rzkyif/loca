//! Which model files the current settings require.
//!
//! Three consumers read it: this module's `/ensure` route and its `ModelLocks`
//! seam, the requirements snapshot, and the storage locks. One answer to "is
//! this file required", because two would be a delete the user should not have
//! been offered.
//!
//! Everything here reads RESOLVED settings, with the schema defaults underneath.
//! The sparse file omits an unchanged default, so a core whose `llm.modelPath`
//! is still the default would otherwise require nothing at all.

use indexmap::IndexMap;
use serde_json::{Map, Value};

use crate::contract::api::RequirementGroup;

/// The local embedding model for tool relevance and memory retrieval: an
/// all-MiniLM-L6-v2 GGUF. Its identity is folded into the stored-vector
/// staleness hash, so changing it forces a re-embed of every cached vector.
///
/// Hosted in process by `inference::embed`, which reads this same constant.
pub const EMBED_MODEL_FILE: &str =
    "@second-state/All-MiniLM-L6-v2-Embedding-GGUF/main/all-MiniLM-L6-v2-Q8_0.gguf";

/// One file the current configuration needs on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequiredModelRef {
    pub source: String,
    pub group: RequirementGroup,
}

/// JavaScript truthiness. A settings value is normally a real boolean, but the
/// file is hand-editable and `"false"` is a truthy string, so guessing here
/// would silently change what a device downloads.
fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

/// `s[key] ?? default`, then truthiness. `??` only falls through on null and
/// undefined, so a stored `false` stays false rather than taking the default.
fn enabled_with_default(settings: &Map<String, Value>, key: &str, default: bool) -> bool {
    match settings.get(key) {
        None | Some(Value::Null) => default,
        other => truthy(other),
    }
}

fn is_external(settings: &Map<String, Value>, key: &str) -> bool {
    settings.get(key).and_then(Value::as_str) == Some("external")
}

/// True when speech-to-text runs on the local engine: enabled, and not pointed
/// at an external service.
///
/// Both speech flags default to false, so a fresh install needs no speech model
/// to send a text message. Voice is opt-in.
pub fn stt_uses_local(settings: &Map<String, Value>) -> bool {
    enabled_with_default(settings, "stt.enabled", false) && !is_external(settings, "stt.provider")
}

/// Mirror of [`stt_uses_local`] for the text-to-speech gates.
pub fn tts_uses_local(settings: &Map<String, Value>) -> bool {
    enabled_with_default(settings, "tts.enabled", false) && !is_external(settings, "tts.provider")
}

/// An HF spec: a non-empty string starting with `@`.
fn is_hf_spec(value: Option<&Value>) -> Option<&str> {
    let spec = value?.as_str()?;
    if spec.starts_with('@') && spec.len() > 1 {
        Some(spec)
    } else {
        None
    }
}

/// The role to spec map a `*.modelFiles` setting stores, or `None` when the
/// value is absent or malformed.
///
/// A speech model is a bundle of role-tagged files and the roles differ by
/// family, so selection bakes the whole chosen bundle into the setting as JSON
/// rather than guessing sibling filenames from one spec. Nothing downstream
/// assumes a naming convention.
pub fn model_files_map(value: Option<&Value>) -> Option<IndexMap<String, String>> {
    let raw = value?.as_str()?;
    if raw.trim().is_empty() {
        return None;
    }
    // Through `OrderedBundle`, NOT `serde_json::Value`, whose map is a
    // `BTreeMap` here: sorting the roles would show a whisper bundle
    // decoder-first in the download queue and the requirements list.
    let bundle: OrderedBundle = serde_json::from_str(raw).ok()?;
    Some(bundle.0.into_iter().collect())
}

/// A JSON object of string values, in the order the document lists them.
///
/// `serde`'s `MapAccess` yields entries in input order, which is the whole reason
/// this exists rather than a `serde_json::Value`. A non-string value fails the
/// whole parse: a half-read bundle would download some of a model's files and
/// leave the engine unable to load any of them.
struct OrderedBundle(Vec<(String, String)>);

impl<'de> serde::Deserialize<'de> for OrderedBundle {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Entries;

        impl<'de> serde::de::Visitor<'de> for Entries {
            type Value = OrderedBundle;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("an object mapping each file role to an @user/repo/branch/file spec")
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Self::Value, A::Error> {
                let mut out = Vec::new();
                while let Some(entry) = map.next_entry::<String, String>()? {
                    out.push(entry);
                }
                Ok(OrderedBundle(out))
            }
        }

        deserializer.deserialize_map(Entries)
    }
}

/// The HF specs a `*.modelFiles` setting references. Empty for a missing or
/// malformed value; the loud-failure guard for a hand edit is the settings
/// route's validator, not this.
pub fn parse_model_files(value: Option<&Value>) -> Vec<String> {
    model_files_map(value)
        .map(|map| {
            map.into_values()
                .filter(|spec| spec.starts_with('@') && spec.len() > 1)
                .collect()
        })
        .unwrap_or_default()
}

/// Every model file the current settings require, tagged with its group.
///
/// Local LLM contributes its model path, plus the projector when image support
/// is on. Local speech contributes every file of its bundle. The embedding
/// model is required unless embeddings are served by an external relevance
/// model.
pub fn required_model_refs(settings: &Map<String, Value>) -> Vec<RequiredModelRef> {
    let mut out = Vec::new();
    let llm_local = !is_external(settings, "llm.provider");
    let images_on = truthy(settings.get("llm.supportImages"));

    if llm_local {
        if let Some(spec) = is_hf_spec(settings.get("llm.modelPath")) {
            out.push(RequiredModelRef {
                source: spec.to_string(),
                group: RequirementGroup::Llm,
            });
        }
        // Required only when the user turned images ON: selecting a
        // vision-capable model writes `llm.mmprojPath` and never
        // `llm.supportImages`, so picking one adds no download.
        if images_on {
            if let Some(spec) = is_hf_spec(settings.get("llm.mmprojPath")) {
                out.push(RequiredModelRef {
                    source: spec.to_string(),
                    group: RequirementGroup::Llm,
                });
            }
        }
    }
    if stt_uses_local(settings) {
        for spec in parse_model_files(settings.get("stt.modelFiles")) {
            out.push(RequiredModelRef {
                source: spec,
                group: RequirementGroup::Stt,
            });
        }
    }
    if tts_uses_local(settings) {
        for spec in parse_model_files(settings.get("tts.modelFiles")) {
            out.push(RequiredModelRef {
                source: spec,
                group: RequirementGroup::Tts,
            });
        }
    }
    // With the LLM external AND a relevance model configured, embeddings reuse
    // that provider and the local GGUF is not needed. Either condition alone
    // still requires it.
    let external_embed = is_external(settings, "llm.provider")
        && settings
            .get("llm.external.embedModel")
            .and_then(Value::as_str)
            .is_some_and(|m| !m.trim().is_empty());
    if !external_embed {
        out.push(RequiredModelRef {
            source: EMBED_MODEL_FILE.to_string(),
            group: RequirementGroup::Embed,
        });
    }
    out
}

/// The sources one sidecar kind needs. Empty when the kind is disabled or on an
/// external provider.
pub fn sources_for_kind(settings: &Map<String, Value>, kind: RequirementGroup) -> Vec<String> {
    required_model_refs(settings)
        .into_iter()
        .filter(|r| r.group == kind)
        .map(|r| r.source)
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use serde_json::json;

    fn settings(pairs: &[(&str, Value)]) -> Map<String, Value> {
        let mut map = Map::new();
        for (key, value) in pairs {
            map.insert((*key).to_string(), value.clone());
        }
        map
    }

    fn sources(settings: &Map<String, Value>) -> Vec<String> {
        required_model_refs(settings)
            .into_iter()
            .map(|r| r.source)
            .collect()
    }

    /// The default posture: local LLM, speech off, so the model path and the
    /// embedding GGUF and nothing else.
    #[test]
    fn a_local_llm_requires_its_weights_and_the_embedding_model() {
        let s = settings(&[("llm.modelPath", json!("@u/r/main/m.gguf"))]);
        assert_eq!(
            sources(&s),
            vec!["@u/r/main/m.gguf".to_string(), EMBED_MODEL_FILE.to_string()]
        );
        assert_eq!(required_model_refs(&s)[0].group, RequirementGroup::Llm);
        assert_eq!(required_model_refs(&s)[1].group, RequirementGroup::Embed);
    }

    /// The projector is gated on the user's image-support choice, NOT on the
    /// model shipping one, which is what stops a preset pick from adding a
    /// download nobody asked for.
    #[test]
    fn the_projector_is_required_only_when_images_are_on() {
        let base = [
            ("llm.modelPath", json!("@u/r/main/m.gguf")),
            ("llm.mmprojPath", json!("@u/r/main/mmproj.gguf")),
        ];
        let off = settings(&base);
        assert!(!sources(&off).contains(&"@u/r/main/mmproj.gguf".to_string()));

        let mut on = settings(&base);
        on.insert("llm.supportImages".into(), json!(true));
        assert!(sources(&on).contains(&"@u/r/main/mmproj.gguf".to_string()));
    }

    #[test]
    fn an_external_llm_requires_no_weights() {
        let s = settings(&[
            ("llm.provider", json!("external")),
            ("llm.modelPath", json!("@u/r/main/m.gguf")),
        ]);
        // Still the embedding model: an external chat provider does not imply
        // an external embedding provider.
        assert_eq!(sources(&s), vec![EMBED_MODEL_FILE.to_string()]);
    }

    /// Both halves are needed to drop the local embedding GGUF. Either alone
    /// still requires it, which is the branch a one-condition port gets wrong.
    #[test]
    fn the_embedding_model_is_dropped_only_for_an_external_relevance_model() {
        let both = settings(&[
            ("llm.provider", json!("external")),
            ("llm.external.embedModel", json!("text-embedding-3-small")),
        ]);
        assert!(sources(&both).is_empty());

        let external_only = settings(&[("llm.provider", json!("external"))]);
        assert_eq!(sources(&external_only), vec![EMBED_MODEL_FILE.to_string()]);

        let model_only = settings(&[("llm.external.embedModel", json!("text-embedding-3-small"))]);
        assert!(sources(&model_only).contains(&EMBED_MODEL_FILE.to_string()));

        // A whitespace-only model name is not a configured provider.
        let blank = settings(&[
            ("llm.provider", json!("external")),
            ("llm.external.embedModel", json!("   ")),
        ]);
        assert_eq!(sources(&blank), vec![EMBED_MODEL_FILE.to_string()]);
    }

    #[test]
    fn local_speech_requires_every_file_of_its_bundle() {
        let bundle = r#"{"encoder":"@o/r/main/e.onnx","decoder":"@o/r/main/d.onnx","tokens":"@o/r/main/t.txt"}"#;
        let s = settings(&[
            ("stt.enabled", json!(true)),
            ("stt.modelFiles", json!(bundle)),
        ]);
        let refs = required_model_refs(&s);
        let stt: Vec<&str> = refs
            .iter()
            .filter(|r| r.group == RequirementGroup::Stt)
            .map(|r| r.source.as_str())
            .collect();
        // Bundle order, not sorted: the same order the setting stores.
        assert_eq!(
            stt,
            vec!["@o/r/main/e.onnx", "@o/r/main/d.onnx", "@o/r/main/t.txt"]
        );
    }

    #[test]
    fn speech_gates_are_off_by_default_and_external_turns_them_off_again() {
        let bundle = r#"{"model":"@o/r/main/m.onnx"}"#;
        let disabled = settings(&[("stt.modelFiles", json!(bundle))]);
        assert!(!stt_uses_local(&disabled));
        assert!(!sources(&disabled).contains(&"@o/r/main/m.onnx".to_string()));

        let external = settings(&[
            ("stt.enabled", json!(true)),
            ("stt.provider", json!("external")),
            ("stt.modelFiles", json!(bundle)),
        ]);
        assert!(!stt_uses_local(&external));

        let local = settings(&[
            ("stt.enabled", json!(true)),
            ("stt.provider", json!("local")),
            ("stt.modelFiles", json!(bundle)),
        ]);
        assert!(stt_uses_local(&local));
        // An absent provider means local, not external.
        let absent = settings(&[("tts.enabled", json!(true))]);
        assert!(tts_uses_local(&absent));
    }

    /// A malformed bundle contributes nothing rather than contributing some of
    /// itself. Half a bundle is a download that completes and an engine that
    /// still cannot start.
    #[test]
    fn a_malformed_bundle_contributes_nothing() {
        for bad in [
            json!("not json"),
            json!("[]"),
            json!("null"),
            json!(r#"{"role": 42}"#),
            json!(""),
            json!("   "),
            json!(7),
        ] {
            let s = settings(&[
                ("stt.enabled", json!(true)),
                ("stt.modelFiles", bad.clone()),
            ]);
            assert_eq!(sources(&s), vec![EMBED_MODEL_FILE.to_string()], "{bad}");
        }
        // A well-formed bundle with a non-HF value drops that entry only.
        let s = settings(&[
            ("stt.enabled", json!(true)),
            (
                "stt.modelFiles",
                json!(r#"{"model":"@o/r/main/m.onnx","tokens":"/local/tokens.txt"}"#),
            ),
        ]);
        assert!(sources(&s).contains(&"@o/r/main/m.onnx".to_string()));
        assert!(!sources(&s).contains(&"/local/tokens.txt".to_string()));
    }

    #[test]
    fn a_non_hf_model_path_is_not_a_download() {
        let s = settings(&[("llm.modelPath", json!("/opt/models/local.gguf"))]);
        assert_eq!(sources(&s), vec![EMBED_MODEL_FILE.to_string()]);
        let empty = settings(&[("llm.modelPath", json!("@"))]);
        assert_eq!(sources(&empty), vec![EMBED_MODEL_FILE.to_string()]);
    }

    #[test]
    fn sources_for_kind_filters_to_one_group() {
        let s = settings(&[
            ("llm.modelPath", json!("@u/r/main/m.gguf")),
            ("tts.enabled", json!(true)),
            ("tts.modelFiles", json!(r#"{"model":"@o/r/main/v.onnx"}"#)),
        ]);
        assert_eq!(
            sources_for_kind(&s, RequirementGroup::Llm),
            vec!["@u/r/main/m.gguf".to_string()]
        );
        assert_eq!(
            sources_for_kind(&s, RequirementGroup::Tts),
            vec!["@o/r/main/v.onnx".to_string()]
        );
        assert_eq!(
            sources_for_kind(&s, RequirementGroup::Embed),
            vec![EMBED_MODEL_FILE.to_string()]
        );
        assert!(sources_for_kind(&s, RequirementGroup::Stt).is_empty());
    }
}
