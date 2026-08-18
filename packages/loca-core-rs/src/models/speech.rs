//! Speech-to-Text and Text-to-Speech catalog resolution.
//!
//! Neither has a fit engine. STT bundles run from about 31 MB to 3.1 GB and TTS
//! bundles are small, so everything fits on every device and selection is a
//! direct lookup. What these do is the job `fit.rs` does for the LLM picker:
//! turn a `{presetId | modelId | modelSpec}` choice into the `stt.*` or `tts.*`
//! settings to write.
//!
//! # The `modelFiles` string is a wire format, and two things about it are traps
//!
//! A speech model is a bundle of role-tagged files, and selection bakes the
//! chosen bundle into `stt.modelFiles` / `tts.modelFiles` as JSON. That string
//! is persisted, read back by the speech engine and by the download flow, and
//! compared byte for byte by the conformance suite. Two properties of it are
//! easy to "tidy" into a divergence:
//!
//! 1. **Key order is the catalog's file order, not sorted.** `modelFilesObject`
//!    iterates `quant.files` and `JSON.stringify` preserves insertion order.
//!    `serde_json::Map` is a `BTreeMap` in this crate, so serialising a map
//!    would emit `decoder` before `encoder` where the TypeScript emits
//!    `encoder` first. The string is therefore built in file order by hand.
//! 2. **STT writes compact JSON and TTS writes two-space pretty JSON.**
//!    `stt.ts:64` is `JSON.stringify(obj)`; `tts.ts:48` is
//!    `JSON.stringify(obj, null, 2)`. The asymmetry reads as an accident and is
//!    the specification: both values are already on disk in real installs.

use indexmap::IndexMap;

use crate::contract::api::{
    AppliedSttSettings, AppliedTtsSettings, HardwareInfo, SttPresetView, TtsPresetView,
    TtsPresetViewVoicesItem,
};
use crate::contract::inbound::{
    CatalogPayload, SpeechQuant, SttCatalogModel, SttFamily, TtsCatalogModel, TtsFamily, TtsVoice,
};
use crate::error::{not_found, validation, Result};

use super::fit::threads_for;
use crate::contract::inbound::SpeechSelectBody;

/// The file role that identifies an STT family's main weights file. It is the
/// `modelPath` identity: which model is selected, and what a custom pick is
/// matched against.
fn stt_primary_role(family: SttFamily) -> &'static str {
    match family {
        SttFamily::Whisper | SttFamily::Moonshine | SttFamily::Transducer => "encoder",
        SttFamily::SenseVoice
        | SttFamily::Paraformer
        | SttFamily::NemoCtc
        | SttFamily::Dolphin
        | SttFamily::TelespeechCtc => "model",
    }
}

/// Mirror of [`stt_primary_role`]. Matcha splits its acoustic model from its
/// vocoder, so the identity file is the acoustic one.
fn tts_primary_role(family: TtsFamily) -> &'static str {
    match family {
        TtsFamily::Kokoro | TtsFamily::Kitten | TtsFamily::Vits => "model",
        TtsFamily::Matcha => "acoustic_model",
    }
}

/// The catalog's family enum and the response's are two generated types over
/// the same wire strings: the catalog is validated on the way IN and the
/// applied settings are produced on the way OUT, and `contract` partitions by
/// direction of travel (D-003). The mapping is total in both directions and is
/// written out so a variant added on one side cannot compile without the other.
fn stt_family_out(family: SttFamily) -> crate::contract::api::SttFamily {
    use crate::contract::api::SttFamily as Out;
    match family {
        SttFamily::Whisper => Out::Whisper,
        SttFamily::SenseVoice => Out::SenseVoice,
        SttFamily::Moonshine => Out::Moonshine,
        SttFamily::Paraformer => Out::Paraformer,
        SttFamily::Transducer => Out::Transducer,
        SttFamily::NemoCtc => Out::NemoCtc,
        SttFamily::Dolphin => Out::Dolphin,
        SttFamily::TelespeechCtc => Out::TelespeechCtc,
    }
}

/// Mirror of [`stt_family_out`].
fn tts_family_out(family: TtsFamily) -> crate::contract::api::TtsFamily {
    use crate::contract::api::TtsFamily as Out;
    match family {
        TtsFamily::Kokoro => Out::Kokoro,
        TtsFamily::Kitten => Out::Kitten,
        TtsFamily::Vits => Out::Vits,
        TtsFamily::Matcha => Out::Matcha,
    }
}

/// The role to spec map a `*.modelFiles` setting stores, in the catalog's file
/// order. A duplicate role keeps its first position and its last value, which is
/// what assigning twice into a JavaScript object does.
fn model_files_map(quant: &SpeechQuant) -> IndexMap<&str, &str> {
    let mut out = IndexMap::new();
    for file in &quant.files {
        out.insert(file.role.as_str(), file.model_spec.as_str());
    }
    out
}

/// `JSON.stringify(modelFilesObject(quant))`, or its two-space form. Written
/// out rather than serialised: see the module docs for why the obvious way is
/// wrong on both counts.
fn model_files_json(quant: &SpeechQuant, indent: bool) -> String {
    let map = model_files_map(quant);
    if map.is_empty() {
        return "{}".to_string();
    }
    let mut out = String::from("{");
    for (i, (role, spec)) in map.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        if indent {
            out.push_str("\n  ");
        }
        out.push_str(&json_string(role));
        out.push(':');
        if indent {
            out.push(' ');
        }
        out.push_str(&json_string(spec));
    }
    if indent {
        out.push('\n');
    }
    out.push('}');
    out
}

fn json_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| format!("\"{value}\""))
}

/// The primary (identity) file spec of a quant for a family's primary role,
/// falling back to the first file so a caller always gets a spec.
fn primary_file_spec(quant: &SpeechQuant, primary_role: &str) -> String {
    quant
        .files
        .iter()
        .find(|f| f.role == primary_role)
        .or_else(|| quant.files.first())
        .map(|f| f.model_spec.clone())
        .unwrap_or_default()
}

// --- Speech-to-Text ---------------------------------------------------------

/// Resolve the curated cards against the catalog, for the client's badges.
///
/// Presets are validated against the models at catalog build time, so a
/// dangling reference here is a bug rather than bad data. It is skipped
/// defensively: dropping one card beats failing the whole picker.
pub fn build_stt_preset_views(catalog: &CatalogPayload) -> Vec<SttPresetView> {
    let mut views = Vec::new();
    for preset in &catalog.stt.presets {
        let Some(model) = catalog.stt.models.iter().find(|m| m.id == preset.model_id) else {
            continue;
        };
        let Some(quant) = model.quants.iter().find(|q| q.quant == preset.quant) else {
            continue;
        };
        views.push(SttPresetView {
            id: preset.id.clone(),
            model_id: model.id.clone(),
            name: model.name.clone(),
            family: stt_family_out(model.family),
            english: model.english,
            quant: quant.quant.clone(),
            model_spec: primary_file_spec(quant, stt_primary_role(model.family)),
            file_size_bytes: quant.file_size_bytes.get() as i64,
        });
    }
    views
}

/// The default quant when a model is picked without one: int8, which is
/// near-lossless and the only quant the catalog ships, else the first listed.
fn default_stt_quant(model: &SttCatalogModel) -> Option<&SpeechQuant> {
    model
        .quants
        .iter()
        .find(|q| q.quant == "int8")
        .or_else(|| model.quants.first())
}

fn applied_stt(
    model: &SttCatalogModel,
    quant: &SpeechQuant,
    hw: &HardwareInfo,
) -> AppliedSttSettings {
    AppliedSttSettings {
        model_type: stt_family_out(model.family),
        model_path: primary_file_spec(quant, stt_primary_role(model.family)),
        model_files: model_files_json(quant, false),
        // One thread per physical core, capped: the same policy the LLM fit
        // engine applies, read from the same function so the two cannot drift.
        threads: threads_for(hw),
    }
}

/// Resolve a `{presetId | modelId | modelSpec}` selection into the `stt.*`
/// settings to apply. A preset keeps its id; a manual model or quant pick is
/// `custom`. A `modelSpec` matches a model's primary (identity) file.
pub fn resolve_stt_selection(
    catalog: &CatalogPayload,
    hw: &HardwareInfo,
    body: &SpeechSelectBody,
) -> Result<(String, AppliedSttSettings)> {
    let stt = &catalog.stt;
    if let Some(preset_id) = body.preset_id() {
        let found = stt
            .presets
            .iter()
            .find(|p| p.id == preset_id)
            .and_then(|p| {
                let model = stt.models.iter().find(|m| m.id == p.model_id)?;
                let quant = model.quants.iter().find(|q| q.quant == p.quant)?;
                Some((p, model, quant))
            });
        let Some((preset, model, quant)) = found else {
            return Err(not_found(format!("preset not in catalog: {preset_id}")));
        };
        return Ok((preset.id.clone(), applied_stt(model, quant, hw)));
    }
    if let Some(model_spec) = body.model_spec() {
        for model in &stt.models {
            let role = stt_primary_role(model.family);
            if let Some(quant) = model
                .quants
                .iter()
                .find(|q| primary_file_spec(q, role) == model_spec)
            {
                return Ok(("custom".to_string(), applied_stt(model, quant, hw)));
            }
        }
        return Err(not_found(format!("model not in catalog: {model_spec}")));
    }
    if let Some(model_id) = body.model_id() {
        let Some(model) = stt.models.iter().find(|m| m.id == model_id) else {
            return Err(not_found(format!("model not in catalog: {model_id}")));
        };
        let Some(quant) = default_stt_quant(model) else {
            return Err(not_found(format!("model has no quants: {model_id}")));
        };
        return Ok(("custom".to_string(), applied_stt(model, quant, hw)));
    }
    Err(validation("presetId, modelId, or modelSpec required"))
}

// --- Text-to-Speech ---------------------------------------------------------

/// Resolve the curated cards, with each model's voices so the voice dropdown
/// populates from the selection without a second round trip.
pub fn build_tts_preset_views(catalog: &CatalogPayload) -> Vec<TtsPresetView> {
    let mut views = Vec::new();
    for preset in &catalog.tts.presets {
        let Some(model) = catalog.tts.models.iter().find(|m| m.id == preset.model_id) else {
            continue;
        };
        let Some(quant) = model.quants.iter().find(|q| q.quant == preset.quant) else {
            continue;
        };
        views.push(TtsPresetView {
            id: preset.id.clone(),
            model_id: model.id.clone(),
            name: model.name.clone(),
            family: tts_family_out(model.family),
            quant: quant.quant.clone(),
            model_spec: primary_file_spec(quant, tts_primary_role(model.family)),
            file_size_bytes: quant.file_size_bytes.get() as i64,
            // `TtsPresetView.voices` is an inline object literal in
            // `domain/recommend.ts:189` rather than a reference to the catalog
            // voice type, so the generated view carries its own item type and
            // the catalog's voices are copied field by field into it.
            voices: model
                .voices
                .iter()
                .map(|v| TtsPresetViewVoicesItem {
                    id: v.id.clone(),
                    label: v.label.clone(),
                    lang: v.lang.clone(),
                })
                .collect(),
            default_voice: model.default_voice.clone(),
        });
    }
    views
}

fn applied_tts(model: &TtsCatalogModel, quant: &SpeechQuant) -> AppliedTtsSettings {
    AppliedTtsSettings {
        model_type: tts_family_out(model.family),
        model_path: primary_file_spec(quant, tts_primary_role(model.family)),
        // Two-space indented, unlike STT's compact form. See the module docs.
        model_files: model_files_json(quant, true),
        voice: model.default_voice.clone(),
    }
}

/// Mirrors [`resolve_stt_selection`]. Picking a model resets the voice to that
/// model's default.
pub fn resolve_tts_selection(
    catalog: &CatalogPayload,
    body: &SpeechSelectBody,
) -> Result<(String, AppliedTtsSettings)> {
    let tts = &catalog.tts;
    if let Some(preset_id) = body.preset_id() {
        let found = tts
            .presets
            .iter()
            .find(|p| p.id == preset_id)
            .and_then(|p| {
                let model = tts.models.iter().find(|m| m.id == p.model_id)?;
                let quant = model.quants.iter().find(|q| q.quant == p.quant)?;
                Some((p, model, quant))
            });
        let Some((preset, model, quant)) = found else {
            return Err(not_found(format!("preset not in catalog: {preset_id}")));
        };
        return Ok((preset.id.clone(), applied_tts(model, quant)));
    }
    if let Some(model_spec) = body.model_spec() {
        for model in &tts.models {
            let role = tts_primary_role(model.family);
            if let Some(quant) = model
                .quants
                .iter()
                .find(|q| primary_file_spec(q, role) == model_spec)
            {
                return Ok(("custom".to_string(), applied_tts(model, quant)));
            }
        }
        return Err(not_found(format!("model not in catalog: {model_spec}")));
    }
    if let Some(model_id) = body.model_id() {
        let Some(model) = tts.models.iter().find(|m| m.id == model_id) else {
            return Err(not_found(format!("model not in catalog: {model_id}")));
        };
        // The first quant is the best listed, which is the whole default rule
        // for TTS; there is no int8 special case as in STT.
        let Some(quant) = model.quants.first() else {
            return Err(not_found(format!("model has no quants: {model_id}")));
        };
        return Ok(("custom".to_string(), applied_tts(model, quant)));
    }
    Err(validation("presetId, modelId, or modelSpec required"))
}

/// The voices of the TTS model the settings currently select, for
/// `GET /tts/voices`. Falls back to the first model of the same family when the
/// settings do not match a catalog model, which is what a hand-typed custom
/// path produces.
pub fn selected_tts_voices(
    catalog: &CatalogPayload,
    model_type: &str,
    model_path: &str,
) -> Vec<TtsVoice> {
    let by_path = catalog.tts.models.iter().find(|m| {
        family_str(m) == model_type
            && m.quants
                .iter()
                .any(|q| primary_file_spec(q, tts_primary_role(m.family)) == model_path)
    });
    if let Some(model) = by_path {
        return model.voices.clone();
    }
    catalog
        .tts
        .models
        .iter()
        .find(|m| family_str(m) == model_type)
        .map(|m| m.voices.clone())
        .unwrap_or_default()
}

/// The wire spelling of a family, which is what the `tts.modelType` setting
/// stores.
fn family_str(model: &TtsCatalogModel) -> &'static str {
    match model.family {
        TtsFamily::Kokoro => "kokoro",
        TtsFamily::Kitten => "kitten",
        TtsFamily::Vits => "vits",
        TtsFamily::Matcha => "matcha",
    }
}

/// Answers the client's voice dropdown from the model catalog.
///
/// Implements `speech::TtsVoiceSource`, which the speech module declares and
/// deliberately keeps OFF its engine trait (D-039): voices are a catalog fact,
/// so the dropdown has to fill on a core with no local speech engine at all,
/// including every mobile build until N3 ships. The impl lives here because this
/// module owns the catalog, which is the same rule every other seam in the crate
/// follows, and the resulting `models -> speech` edge runs one way only.
///
/// The conversion is the whole of the work: `selected_tts_voices` returns the
/// catalog's `TtsVoice` and the wire wants `TtsVoiceEntry`. They are structurally
/// identical today and are still converted field by field rather than
/// transmuted, because one is a downloaded-catalog shape and the other is our
/// response shape, and nothing keeps them aligned.
pub struct CatalogVoices {
    models: crate::models::Models,
}

impl CatalogVoices {
    pub fn new(models: crate::models::Models) -> Self {
        Self { models }
    }
}

#[async_trait::async_trait]
impl crate::speech::TtsVoiceSource for CatalogVoices {
    async fn voices(
        &self,
        settings: &crate::settings::SettingsRecord,
    ) -> crate::error::Result<Vec<crate::contract::api::TtsVoiceEntry>> {
        // `false`: the cached catalog is what the settings screen should read.
        // Forcing a refresh here would put a network fetch behind a dropdown,
        // which the no-non-consented-network rule forbids outright.
        let catalog = self.models.catalog(false).await?;
        // The same two defaults the oracle applies at
        // `host/deno-local-endpoints.ts:50-52`. A core whose settings have never
        // been written still fills the dropdown.
        let model_type = settings
            .get("tts.modelType")
            .and_then(|v| v.as_str())
            .unwrap_or("kokoro");
        let model_path = settings
            .get("tts.modelPath")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        Ok(selected_tts_voices(&catalog, model_type, model_path)
            .into_iter()
            .map(|voice| crate::contract::api::TtsVoiceEntry {
                id: voice.id,
                label: voice.label,
                lang: voice.lang,
            })
            .collect())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::contract::api::{GpuBackend, GpuInfo};
    use crate::models::catalog::{dev_payload, validate};

    /// `TtsVoice` is generated and typify derives no `PartialEq`, so two voice
    /// lists are compared field by field rather than by hanging an impl on a
    /// generated type (D-067).
    fn voice_keys(voices: &[TtsVoice]) -> Vec<(&str, &str, Option<&str>)> {
        voices
            .iter()
            .map(|v| (v.id.as_str(), v.label.as_str(), v.lang.as_deref()))
            .collect()
    }

    fn hw() -> HardwareInfo {
        HardwareInfo {
            total_ram_bytes: 16 * 1024 * 1024 * 1024,
            available_ram_bytes: 8 * 1024 * 1024 * 1024,
            cpu_cores_physical: 10,
            gpu: GpuInfo {
                backend: GpuBackend::Metal,
                name: "test".into(),
                vram_bytes: 0,
            },
            unified_memory: false,
        }
    }

    fn req(preset: Option<&str>, id: Option<&str>, spec: Option<&str>) -> SpeechSelectBody {
        SpeechSelectBody {
            preset_id: preset.map(str::to_string),
            model_id: id.map(str::to_string),
            model_spec: spec.map(str::to_string),
        }
    }

    /// The exact string a `stt.modelFiles` setting stores. Compact, and in the
    /// catalog's file order rather than sorted.
    ///
    /// The order is checked against the raw STRING rather than by parsing it,
    /// because `serde_json::Value`'s map is a `BTreeMap` here: reading the keys
    /// back through it sorts them and the assertion passes whatever the code
    /// emitted. That mistake was made once while writing this test.
    #[test]
    fn stt_model_files_is_compact_and_in_catalog_order() {
        let catalog = dev_payload().expect("the dev catalog loads");
        let (preset, applied) = resolve_stt_selection(
            &catalog,
            &hw(),
            &req(Some("light-multilingual"), None, None),
        )
        .expect("the preset resolves");
        assert_eq!(preset, "light-multilingual");
        assert!(
            !applied.model_files.contains('\n'),
            "STT is compact: {}",
            applied.model_files
        );
        serde_json::from_str::<serde_json::Value>(&applied.model_files).expect("valid JSON");

        let quant = quant_behind(&catalog, &applied.model_path);
        let roles: Vec<&str> = quant.files.iter().map(|f| f.role.as_str()).collect();
        assert!(roles.len() > 1, "a bundle has several roles");
        // whisper is encoder, decoder, tokens: not sorted order, so this
        // fixture can tell the two apart.
        let mut sorted = roles.clone();
        sorted.sort_unstable();
        assert_ne!(
            roles, sorted,
            "this only proves anything while the catalog order differs from sorted order"
        );
        assert_eq!(applied.model_files, expected_json(quant, false));
        assert_eq!(key_order(&applied.model_files), roles);
    }

    /// The roles in the order they appear in the emitted string.
    fn key_order(json: &str) -> Vec<&str> {
        let mut out = Vec::new();
        let mut rest = json;
        while let Some(open) = rest.find('"') {
            let after = &rest[open + 1..];
            let Some(close) = after.find('"') else { break };
            let key = &after[..close];
            let tail = &after[close + 1..];
            // A key is followed by a colon; a value is followed by a comma or
            // the closing brace.
            if tail.trim_start().starts_with(':') {
                out.push(key);
            }
            rest = tail;
        }
        out
    }

    /// The map rendered independently of the code under test, so the assertion
    /// is not the implementation restated.
    fn expected_json(quant: &SpeechQuant, indent: bool) -> String {
        let parts: Vec<String> = quant
            .files
            .iter()
            .map(|f| {
                let key = serde_json::to_string(&f.role).expect("a string");
                let value = serde_json::to_string(&f.model_spec).expect("a string");
                if indent {
                    format!("\n  {key}: {value}")
                } else {
                    format!("{key}:{value}")
                }
            })
            .collect();
        if indent {
            format!("{{{}\n}}", parts.join(","))
        } else {
            format!("{{{}}}", parts.join(","))
        }
    }

    fn quant_behind<'a>(catalog: &'a CatalogPayload, model_path: &str) -> &'a SpeechQuant {
        for model in &catalog.stt.models {
            for quant in &model.quants {
                if primary_file_spec(quant, stt_primary_role(model.family)) == model_path {
                    return quant;
                }
            }
        }
        panic!("the applied spec is not a catalog model: {model_path}");
    }

    /// TTS pretty-prints where STT does not. Both are persisted, so the
    /// asymmetry is the specification rather than a style choice.
    #[test]
    fn tts_model_files_is_two_space_pretty_printed() {
        let catalog = dev_payload().expect("the dev catalog loads");
        let (preset, applied) = resolve_tts_selection(&catalog, &req(Some("kokoro"), None, None))
            .expect("the preset resolves");
        assert_eq!(preset, "kokoro");
        assert!(applied.model_files.starts_with("{\n  \""));
        assert!(applied.model_files.ends_with("\n}"));
        assert!(
            applied.model_files.contains("\": \""),
            "a space after the colon, which the compact STT form does not have"
        );
        serde_json::from_str::<serde_json::Value>(&applied.model_files).expect("valid JSON");

        let model = catalog
            .tts
            .models
            .iter()
            .find(|m| m.id == "kokoro")
            .expect("the kokoro model");
        let quant = model
            .quants
            .iter()
            .find(|q| primary_file_spec(q, tts_primary_role(model.family)) == applied.model_path)
            .expect("the quant");
        assert_eq!(applied.model_files, expected_json(quant, true));
    }

    #[test]
    fn the_two_renderings_carry_the_same_map() {
        let catalog = dev_payload().expect("the dev catalog loads");
        let quant = &catalog.tts.models[0].quants[0];
        let compact: serde_json::Value =
            serde_json::from_str(&model_files_json(quant, false)).unwrap();
        let pretty: serde_json::Value =
            serde_json::from_str(&model_files_json(quant, true)).unwrap();
        assert_eq!(compact, pretty);
        assert_ne!(
            model_files_json(quant, false),
            model_files_json(quant, true)
        );
    }

    /// A catalog whose speech presets do not all resolve is SERVED, not
    /// refused. The Zod schema checks no referential integrity (`domain/catalog.ts`
    /// has no `.superRefine`), and the specification degrades on purpose:
    /// `buildSttPresetViews` skips a card whose model or quant is missing and
    /// keeps the rest. Rejecting the payload would turn a repointed card into
    /// `manifest_fetch_failed` on `/recommend`, `/catalog`, `/select` and both
    /// speech pickers, so the blast radius of a release mistake would be the
    /// whole models surface rather than one missing card (D-044).
    #[test]
    fn a_dangling_preset_is_skipped_rather_than_failing_the_whole_catalog() {
        let good = dev_payload().expect("the dev catalog loads");
        let mut document = serde_json::to_value(&good).expect("serialises");
        let stt_cards = good.stt.presets.len();
        let tts_cards = good.tts.presets.len();

        // Three shapes Zod accepts: a preset naming a model that is not there, a
        // preset naming a quant the model does not ship, and a duplicated id.
        let unknown_model = serde_json::json!({
            "id": "ghost-model",
            "modelId": "no-such-model",
            "quant": "int8",
        });
        let unknown_quant = serde_json::json!({
            "id": "ghost-quant",
            "modelId": good.stt.models[0].id,
            "quant": "no-such-quant",
        });
        for (branch, dup_model) in [
            ("stt", good.stt.models[0].id.clone()),
            ("tts", good.tts.models[0].id.clone()),
        ] {
            let presets = document[branch]["presets"]
                .as_array_mut()
                .expect("presets is an array");
            presets.push(unknown_model.clone());
            let mut quant_ref = unknown_quant.clone();
            quant_ref["modelId"] = serde_json::json!(dup_model);
            presets.push(quant_ref);
            let first = presets[0].clone();
            presets.push(first);
        }

        let bad: CatalogPayload =
            serde_json::from_value(document).expect("the shape is still the schema's");
        validate(&bad).expect("a dangling reference is not a schema violation");

        // The two dangling cards vanish; the duplicate resolves like its twin,
        // so each picker serves one more card than the catalog shipped.
        assert_eq!(build_stt_preset_views(&bad).len(), stt_cards + 1);
        assert_eq!(build_tts_preset_views(&bad).len(), tts_cards + 1);

        // And selecting a dangling card is a 404 for that card alone.
        assert_eq!(
            resolve_stt_selection(&bad, &hw(), &req(Some("ghost-model"), None, None))
                .expect_err("no such model")
                .code,
            crate::error::ErrorCode::NotFound
        );
        assert_eq!(
            resolve_tts_selection(&bad, &req(Some("ghost-quant"), None, None))
                .expect_err("no such quant")
                .code,
            crate::error::ErrorCode::NotFound
        );
    }

    #[test]
    fn a_preset_keeps_its_id_and_a_manual_pick_is_custom() {
        let catalog = dev_payload().expect("the dev catalog loads");
        let preset = &catalog.stt.presets[0];
        let (id, _) = resolve_stt_selection(&catalog, &hw(), &req(Some(&preset.id), None, None))
            .expect("resolves");
        assert_eq!(id, preset.id);

        let model = &catalog.stt.models[0];
        let (id, applied) =
            resolve_stt_selection(&catalog, &hw(), &req(None, Some(&model.id), None))
                .expect("resolves");
        assert_eq!(id, "custom");
        assert_eq!(applied.model_type, stt_family_out(model.family));

        let spec = primary_file_spec(&model.quants[0], stt_primary_role(model.family));
        let (id, applied) = resolve_stt_selection(&catalog, &hw(), &req(None, None, Some(&spec)))
            .expect("resolves");
        assert_eq!(id, "custom");
        assert_eq!(applied.model_path, spec);
    }

    /// Precedence is presetId, then modelSpec, then modelId. A body carrying
    /// two of them must not resolve by the wrong one.
    #[test]
    fn preset_wins_over_model_spec_which_wins_over_model_id() {
        let catalog = dev_payload().expect("the dev catalog loads");
        let preset = &catalog.stt.presets[0];
        let other = catalog
            .stt
            .models
            .iter()
            .find(|m| m.id != preset.model_id)
            .expect("a second model");
        let spec = primary_file_spec(&other.quants[0], stt_primary_role(other.family));

        let (id, _) = resolve_stt_selection(
            &catalog,
            &hw(),
            &req(Some(&preset.id), Some(&other.id), Some(&spec)),
        )
        .expect("resolves");
        assert_eq!(id, preset.id, "presetId wins");

        let (_, applied) =
            resolve_stt_selection(&catalog, &hw(), &req(None, Some(&other.id), Some(&spec)))
                .expect("resolves");
        assert_eq!(applied.model_path, spec, "modelSpec wins over modelId");
    }

    /// An empty string is not a choice. The Zod schema is
    /// `z.string().optional()` with no `.min(1)`, so `""` reaches the handler,
    /// and the TypeScript's `if (body.presetId)` is falsy for it: the field
    /// falls through to the next discriminator rather than being looked up and
    /// missed. A client that clears a field instead of omitting it reaches this
    /// (D-043).
    #[test]
    fn an_empty_discriminator_falls_through_like_an_omitted_one() {
        let catalog = dev_payload().expect("the dev catalog loads");
        let model = &catalog.stt.models[0];
        let (preset, applied) =
            resolve_stt_selection(&catalog, &hw(), &req(Some(""), Some(&model.id), None))
                .expect("the empty presetId falls through to modelId");
        assert_eq!(preset, "custom");
        assert_eq!(applied.model_type, stt_family_out(model.family));

        let spec = primary_file_spec(&model.quants[0], stt_primary_role(model.family));
        let (preset, applied) =
            resolve_stt_selection(&catalog, &hw(), &req(Some(""), None, Some(&spec)))
                .expect("and to modelSpec");
        assert_eq!(preset, "custom");
        assert_eq!(applied.model_path, spec);

        let tts_model = &catalog.tts.models[0];
        let (preset, _) =
            resolve_tts_selection(&catalog, &req(Some(""), Some(&tts_model.id), None))
                .expect("the same on the TTS side");
        assert_eq!(preset, "custom");
        let tts_spec = primary_file_spec(&tts_model.quants[0], tts_primary_role(tts_model.family));
        let (preset, _) =
            resolve_tts_selection(&catalog, &req(Some(""), Some(""), Some(&tts_spec)))
                .expect("an empty modelId falls through too");
        assert_eq!(preset, "custom");

        // All three empty is the same body as all three absent: nothing was
        // chosen, which is a validation error rather than a lookup miss.
        let blank = req(Some(""), Some(""), Some(""));
        assert_eq!(
            resolve_stt_selection(&catalog, &hw(), &blank)
                .expect_err("rejected")
                .code,
            crate::error::ErrorCode::ValidationError
        );
        assert_eq!(
            resolve_tts_selection(&catalog, &blank)
                .expect_err("rejected")
                .code,
            crate::error::ErrorCode::ValidationError
        );
    }

    #[test]
    fn an_unknown_selection_is_not_found_and_an_empty_one_is_a_validation_error() {
        let catalog = dev_payload().expect("the dev catalog loads");
        for body in [
            req(Some("no-such-preset"), None, None),
            req(None, Some("no-such-model"), None),
            req(None, None, Some("@nobody/nothing/main/x.onnx")),
        ] {
            let err = resolve_stt_selection(&catalog, &hw(), &body).expect_err("rejected");
            assert_eq!(err.code, crate::error::ErrorCode::NotFound);
            let err = resolve_tts_selection(&catalog, &body).expect_err("rejected");
            assert_eq!(err.code, crate::error::ErrorCode::NotFound);
        }
        let empty = req(None, None, None);
        assert_eq!(
            resolve_stt_selection(&catalog, &hw(), &empty)
                .expect_err("rejected")
                .code,
            crate::error::ErrorCode::ValidationError
        );
        assert_eq!(
            resolve_tts_selection(&catalog, &empty)
                .expect_err("rejected")
                .code,
            crate::error::ErrorCode::ValidationError
        );
    }

    /// STT prefers int8 when a model is picked without a quant; TTS takes the
    /// first listed. Different rules, and the catalog only ships int8 for STT,
    /// so this pins the intent rather than the observation.
    #[test]
    fn the_default_quant_rules_differ_between_stt_and_tts() {
        let catalog = dev_payload().expect("the dev catalog loads");
        for model in &catalog.stt.models {
            let chosen = default_stt_quant(model).expect("a quant");
            if model.quants.iter().any(|q| q.quant == "int8") {
                assert_eq!(chosen.quant, "int8", "{} should default to int8", model.id);
            } else {
                assert_eq!(chosen.quant, model.quants[0].quant);
            }
        }
    }

    #[test]
    fn every_curated_card_resolves_against_the_shipped_catalog() {
        let catalog = dev_payload().expect("the dev catalog loads");
        let stt = build_stt_preset_views(&catalog);
        assert_eq!(
            stt.len(),
            catalog.stt.presets.len(),
            "a dangling preset would be silently dropped"
        );
        let tts = build_tts_preset_views(&catalog);
        assert_eq!(tts.len(), catalog.tts.presets.len());
        for view in &tts {
            assert!(!view.voices.is_empty(), "{} has voices", view.id);
            assert!(
                view.voices.iter().any(|v| v.id == view.default_voice),
                "{}'s default voice is one of its voices",
                view.id
            );
        }
    }

    #[test]
    fn the_selected_voices_fall_back_from_path_to_family_to_nothing() {
        let catalog = dev_payload().expect("the dev catalog loads");
        let model = &catalog.tts.models[0];
        let spec = primary_file_spec(&model.quants[0], tts_primary_role(model.family));
        let family = family_str(model);

        assert_eq!(
            voice_keys(&selected_tts_voices(&catalog, family, &spec)),
            voice_keys(&model.voices)
        );
        // A hand-typed path still resolves, by family.
        assert_eq!(
            voice_keys(&selected_tts_voices(
                &catalog,
                family,
                "/somewhere/custom.onnx"
            )),
            voice_keys(&model.voices)
        );
        assert!(selected_tts_voices(&catalog, "nosuchfamily", &spec).is_empty());
    }
}
