//! Core-side validation for `PATCH /api/v1/settings`.
//!
//! Strict on purpose. Only known schema keys the core stores, with well-typed
//! in-range values, are accepted: secrets belong in the encrypted vault,
//! `client-on-client` keys belong in the Client's own file, and a wrong-typed
//! value would otherwise reach a sidecar's argv. It also keeps the store
//! schema-only, which is what lets `GET` hand a client the whole record.
//!
//! Returns a list of human-readable errors rather than failing on the first, so
//! a client saving a form gets every problem at once. An empty list means the
//! patch is acceptable.

use serde_json::{Map, Value};

use super::schema::{schema, Destination, Field, FieldType, RegexValidation};

/// Validate a patch body against the schema.
///
/// `allow` is the set of destinations this path accepts: the shared store alone
/// for an internal caller, or both core-stored destinations for the settings
/// route, which partitions the body afterwards.
pub fn validate_settings_patch(patch: &Map<String, Value>, allow: &[Destination]) -> Vec<String> {
    let schema = schema();
    let mut errors = Vec::new();
    for (key, value) in patch {
        let Some(field) = schema.field(key) else {
            errors.push(format!("\"{key}\" is not a known setting"));
            continue;
        };
        if field.kind == FieldType::Password {
            errors.push(format!(
                "\"{key}\" is a secret and must be set via the secrets endpoint, not settings"
            ));
            continue;
        }
        if !allow.contains(&field.destination) {
            errors.push(format!("\"{key}\" is not accepted on this settings path"));
            continue;
        }
        if field.kind.is_render_only() {
            errors.push(format!(
                "\"{key}\" is a render-only field and holds no persisted value"
            ));
            continue;
        }
        // A null is a deletion (reset to the default) and is acceptable on any
        // accepted key.
        if value.is_null() {
            continue;
        }
        if let Some(error) = field_error(field, value) {
            errors.push(error);
        }
    }
    errors
}

/// The per-value checks, in the order a failing one short-circuits the rest.
fn field_error(field: &Field, value: &Value) -> Option<String> {
    let key = &field.id;
    if !value_type_ok(field.kind, value) {
        return Some(format!(
            "\"{key}\" has the wrong type for a \"{}\" setting",
            field.kind.as_str()
        ));
    }
    // A select value must be one of the declared options. A select backed by a
    // runtime `optionsSource` (monitors, fonts, voices) has none here and is
    // skipped, because the core cannot know them.
    if field.kind == FieldType::Select && !field.options.is_empty() {
        let listed = field
            .options
            .iter()
            .any(|option| same_scalar(option, value));
        if !listed {
            let allowed: Vec<String> = field.options.iter().map(js_string).collect();
            return Some(format!("\"{key}\" must be one of: {}", allowed.join(", ")));
        }
    }
    if let Some(number) = value.as_f64() {
        // A slider outside its track, or a plain number outside a declared
        // bound, must not reach the core and flow into a sidecar argv.
        if field.kind == FieldType::NumberSlider {
            if let (Some(min), Some(max)) = (field.min, field.max) {
                if number < min || number > max {
                    return Some(format!("\"{key}\" must be between {min} and {max}"));
                }
            }
        }
        if matches!(field.kind, FieldType::Number | FieldType::Float) {
            if let Some(min) = field.min.filter(|min| number < *min) {
                return Some(format!("\"{key}\" must be at least {min}"));
            }
            if let Some(max) = field.max.filter(|max| number > *max) {
                return Some(format!("\"{key}\" must be at most {max}"));
            }
        }
    }
    if field.kind.is_text_like() || matches!(field.kind, FieldType::Number | FieldType::Float) {
        if let Some(rules) = field.regex.as_ref() {
            if let Some(error) = validation_error(rules, value) {
                return Some(format!("\"{key}\": {error}"));
            }
        }
    }
    // The one pair of keys whose string value has a structure of its own: a JSON
    // object mapping each sherpa file role to an `@user/repo/branch/file` spec.
    // Guarded here so a malformed hand edit is rejected at PATCH time rather
    // than silently producing a bundle the speech engine cannot load.
    if key == "stt.modelFiles" || key == "tts.modelFiles" {
        if let Some(error) = model_files_error(value) {
            return Some(format!("\"{key}\": {error}"));
        }
    }
    None
}

/// True when `value` is type-compatible with the field's declared type.
fn value_type_ok(kind: FieldType, value: &Value) -> bool {
    match kind {
        FieldType::Boolean => value.is_boolean(),
        FieldType::Number | FieldType::Float | FieldType::NumberSlider => {
            value.as_f64().is_some_and(f64::is_finite)
        }
        FieldType::Select => value.is_string() || value.is_number(),
        FieldType::CommandPreview => value.is_string() || value.is_boolean(),
        FieldType::String
        | FieldType::Password
        | FieldType::Multiline
        | FieldType::Color
        | FieldType::Shortcut
        | FieldType::Preset
        | FieldType::ModelPreset
        | FieldType::SttPreset
        | FieldType::TtsPreset => value.is_string(),
        // Render-only: no persisted scalar to check. Unreachable through
        // `validate_settings_patch`, which rejects these before they get here.
        FieldType::Services | FieldType::Storage | FieldType::ObjectManagement => true,
    }
}

/// Scalar equality that does not care whether a number arrived as `1` or `1.0`.
///
/// `serde_json::Value` says those differ, since one is an integer and the other
/// a float, where the wire and the schema treat them as one number. Used for the
/// select-options check and for change detection in the store.
pub(super) fn same_scalar(a: &Value, b: &Value) -> bool {
    match (a.as_f64(), b.as_f64()) {
        (Some(a), Some(b)) => a == b,
        _ => a == b,
    }
}

/// The message for the first rule `value` fails, or `None` when it passes.
///
/// An empty value is exempt: an optional field left blank is not a format error,
/// and `optional` is what decides whether blank is allowed at all.
fn validation_error(validation: &RegexValidation, value: &Value) -> Option<String> {
    if value.is_null() || value.as_str() == Some("") {
        return None;
    }
    let text = js_string(value);
    match validation {
        RegexValidation::Pattern(pattern) => match regress::Regex::new(pattern) {
            Err(_) => Some("Invalid regex pattern".to_string()),
            Ok(re) if re.find(&text).is_none() => Some("Invalid format".to_string()),
            Ok(_) => None,
        },
        RegexValidation::Rules(rules) => rules.iter().find_map(|rule| {
            match regress::Regex::new(&rule.regex) {
                Err(_) => Some("Invalid regex pattern".to_string()),
                // `RegExp.test` is an unanchored search, and so is `find`.
                Ok(re) if re.find(&text).is_none() => Some(rule.error_message.clone()),
                Ok(_) => None,
            }
        }),
    }
}

/// A value as JavaScript's `String()` would render it, which is what the schema's
/// patterns were written against.
///
/// `serde_json`'s own `Display` renders `1.0` for a whole float where JavaScript
/// renders `1`, and a pattern like `^\d+$` tells the two apart.
fn js_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Number(n) => match n.as_f64() {
            Some(f) if f.fract() == 0.0 && f.abs() < 1e15 => format!("{}", f as i64),
            _ => n.to_string(),
        },
        Value::Bool(b) => b.to_string(),
        // Unreachable from the validation path, which type-checks first.
        other => other.to_string(),
    }
}

/// `null` when the value is a well-formed `*.modelFiles` bundle map or is empty.
fn model_files_error(value: &Value) -> Option<String> {
    let text = match value {
        Value::String(s) if !s.trim().is_empty() => s,
        Value::Null => return None,
        Value::String(_) => return None,
        _ => return Some(MALFORMED_MODEL_FILES.to_string()),
    };
    let Ok(Value::Object(map)) = serde_json::from_str::<Value>(text) else {
        return Some(MALFORMED_MODEL_FILES.to_string());
    };
    for (role, spec) in &map {
        if !spec.as_str().is_some_and(is_hf_spec) {
            return Some(format!(
                "role \"{role}\" must be an @user/repo/branch/file spec"
            ));
        }
    }
    None
}

const MALFORMED_MODEL_FILES: &str =
    "must be a JSON object mapping each file role to an @user/repo/branch/file spec";

fn is_hf_spec(spec: &str) -> bool {
    spec.starts_with('@') && spec.len() > 1
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use serde_json::json;

    const CORE_PATH: &[Destination] = &[Destination::Core, Destination::ClientOnCore];

    fn errors(patch: Value) -> Vec<String> {
        let Value::Object(map) = patch else {
            unreachable!("a patch is an object")
        };
        validate_settings_patch(&map, CORE_PATH)
    }

    fn one_error(patch: Value) -> String {
        let mut errors = errors(patch);
        assert_eq!(errors.len(), 1, "expected exactly one error: {errors:?}");
        errors.remove(0)
    }

    #[test]
    fn a_well_formed_patch_of_every_shape_passes() {
        assert_eq!(
            errors(json!({
                "llm.host": "127.0.0.1",
                "llm.contextSize": 8192,
                "llm.temperature": 0.7,
                "llm.provider": "external",
                "llm.flashAttn": true,
                "memories.maxRelevant": 5,
                "prompts.defaultSystemPrompt": "be brief",
                "llm.modelPath": null,
            })),
            Vec::<String>::new()
        );
    }

    #[test]
    fn an_unknown_key_is_rejected() {
        assert_eq!(
            one_error(json!({ "ui.theme": "dark" })),
            "\"ui.theme\" is not a known setting"
        );
        // The two boot-only keys are deliberately not schema fields, so they are
        // unknown here. That is what stops a paired client widening the bind
        // host or clearing the pairing posture over the API.
        assert_eq!(
            one_error(json!({ "server.bindHost": "0.0.0.0" })),
            "\"server.bindHost\" is not a known setting"
        );
        assert_eq!(
            one_error(json!({ "server.behindProxy": true })),
            "\"server.behindProxy\" is not a known setting"
        );
    }

    #[test]
    fn a_secret_key_is_rejected_even_though_its_destination_is_allowed() {
        assert_eq!(
            one_error(json!({ "llm.external.apiKey": "sk-nope" })),
            "\"llm.external.apiKey\" is a secret and must be set via the secrets endpoint, not settings"
        );
    }

    #[test]
    fn a_client_on_client_key_is_rejected_on_the_core_path() {
        assert_eq!(
            one_error(json!({ "appearance.theme": "dark" })),
            "\"appearance.theme\" is not accepted on this settings path"
        );
    }

    #[test]
    fn a_client_on_core_key_is_rejected_when_only_core_is_allowed() {
        let Value::Object(patch) = json!({ "llm.temperature": 0.5 }) else {
            unreachable!()
        };
        assert_eq!(
            validate_settings_patch(&patch, &[Destination::Core]),
            vec!["\"llm.temperature\" is not accepted on this settings path"]
        );
    }

    #[test]
    fn a_render_only_field_holds_no_value() {
        assert_eq!(
            one_error(json!({ "memories.list": "x" })),
            "\"memories.list\" is a render-only field and holds no persisted value"
        );
    }

    #[test]
    fn a_wrong_typed_value_is_rejected_per_field_type() {
        assert_eq!(
            one_error(json!({ "llm.host": 1234 })),
            "\"llm.host\" has the wrong type for a \"string\" setting"
        );
        assert_eq!(
            one_error(json!({ "llm.contextSize": "8192" })),
            "\"llm.contextSize\" has the wrong type for a \"number\" setting"
        );
        assert_eq!(
            one_error(json!({ "llm.flashAttn": "true" })),
            "\"llm.flashAttn\" has the wrong type for a \"boolean\" setting"
        );
    }

    #[test]
    fn a_select_value_must_be_one_of_the_declared_options() {
        assert_eq!(
            one_error(json!({ "llm.provider": "banana" })),
            "\"llm.provider\" must be one of: local, external"
        );
    }

    #[test]
    fn a_value_outside_its_declared_range_is_rejected() {
        assert_eq!(
            one_error(json!({ "memories.maxRelevant": 42 })),
            "\"memories.maxRelevant\" must be between 1 and 10"
        );
        assert_eq!(
            one_error(json!({ "memories.maxRelevant": 0 })),
            "\"memories.maxRelevant\" must be between 1 and 10"
        );
        assert_eq!(
            one_error(json!({ "llm.external.contextSize": 0 })),
            "\"llm.external.contextSize\" must be at least 1"
        );
    }

    #[test]
    fn a_regex_rule_reports_its_own_message() {
        assert_eq!(
            one_error(json!({ "llm.host": "not-an-ip" })),
            "\"llm.host\": Must be a valid IPv4 address"
        );
        // Every rule is evaluated in order and the first failure wins.
        assert_eq!(
            one_error(json!({ "llm.port": "abc" })),
            "\"llm.port\": Port must be a number"
        );
        // Six digits. Not 70000, which the schema's own pattern accepts: its
        // first alternative is `[1-9]\d{0,4}`, so anything up to five digits
        // passes and the range rule only bites from six. That is the shared
        // schema's behaviour, matched here rather than corrected here.
        assert_eq!(
            one_error(json!({ "llm.port": "123456" })),
            "\"llm.port\": Port must be 1-65535"
        );
        assert!(errors(json!({ "llm.port": "7701" })).is_empty());
        // An empty value is exempt: blank is not a format error.
        assert!(errors(json!({ "llm.external.baseUrl": "" })).is_empty());
        assert_eq!(
            one_error(json!({ "llm.external.baseUrl": "http://example.com" })),
            "\"llm.external.baseUrl\": URL must use HTTPS, or HTTP with localhost / 127.0.0.1 only."
        );
    }

    #[test]
    fn the_model_files_bundle_must_map_roles_to_specs() {
        assert!(errors(json!({
            "stt.modelFiles": "{\"encoder\":\"@u/r/main/e.onnx\",\"tokens\":\"@u/r/main/t.txt\"}"
        }))
        .is_empty());
        assert!(errors(json!({ "stt.modelFiles": "" })).is_empty());
        assert_eq!(
            one_error(json!({ "tts.modelFiles": "[1,2]" })),
            format!("\"tts.modelFiles\": {MALFORMED_MODEL_FILES}")
        );
        assert_eq!(
            one_error(json!({ "tts.modelFiles": "not json" })),
            format!("\"tts.modelFiles\": {MALFORMED_MODEL_FILES}")
        );
        assert_eq!(
            one_error(json!({ "tts.modelFiles": "{\"model\":\"kokoro.onnx\"}" })),
            "\"tts.modelFiles\": role \"model\" must be an @user/repo/branch/file spec"
        );
    }

    #[test]
    fn every_bad_key_is_reported_rather_than_only_the_first() {
        assert_eq!(
            errors(json!({ "ui.theme": "x", "nope.at.all": 1 })).len(),
            2
        );
    }

    #[test]
    fn a_whole_float_renders_the_way_javascript_renders_it() {
        // The port pattern is `^\d+$`. A value of 7701.0 must not be rendered
        // "7701.0", which serde_json's own Display would do and which the
        // pattern would then reject.
        assert_eq!(js_string(&json!(7701.0)), "7701");
        assert_eq!(js_string(&json!(7701)), "7701");
        assert_eq!(js_string(&json!(0.7)), "0.7");
        assert_eq!(js_string(&json!(true)), "true");
    }

    #[test]
    fn numbers_compare_by_value_not_by_json_representation() {
        assert!(same_scalar(&json!(1), &json!(1.0)));
        assert!(same_scalar(&json!("a"), &json!("a")));
        assert!(!same_scalar(&json!(1), &json!(2)));
        assert!(!same_scalar(&json!("1"), &json!(1)));
    }
}
