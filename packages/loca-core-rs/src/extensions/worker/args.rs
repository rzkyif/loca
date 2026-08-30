//! Host-side validation of the LLM's tool arguments against the tool author's
//! declared JSON Schema, with default-filling.
//!
//! Without this step `argumentsJson` reaches the worker verbatim, so a tool
//! never sees the defaults its author declared and malformed model output
//! reaches tool code uncaught.
//!
//! # The fail-open is deliberate
//!
//! A schema the validator cannot compile means NO validation, and the raw
//! arguments pass through: refusing to run a tool whose author wrote slightly
//! wrong JSON Schema turns an author's mistake into a user's broken tool, and
//! the sandbox rather than the schema is what contains a tool.
//!
//! # A tool author's `pattern` cannot pin a thread
//!
//! Two separate mechanisms, easy to mistake for one, both measured:
//!
//! - **A pattern with no lookaround and no backreference is DELEGATED to the
//!   `regex` crate**, a linear-time automaton. That covers the textbook
//!   exponential cases, `(a+)+$` included, in microseconds, and is what makes
//!   the common case safe.
//! - **A pattern that cannot be delegated runs on the backtracking engine**,
//!   where `PATTERN_BACKTRACK_LIMIT` bounds it: `^(a|a)+$(?<!q)` against 30
//!   `a`s spends 80ms and then ERRORS, against about a minute unbounded.
//!
//! Hitting the limit FAILS the arguments rather than dropping the constraint,
//! which is the safe direction, so both halves have a test.

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, MutexGuard};

use serde_json::{Map, Value};

use crate::error::{AppError, ErrorCode, Result};

/// How many compiled validators are kept. FIFO, because a validator is cheap to
/// rebuild and the cost of a wrong eviction is one recompile.
const MAX_VALIDATORS: usize = 256;

/// Backtrack steps one `pattern` may spend on one string before the match is
/// abandoned. `fancy_regex`'s own default, set explicitly so a change to it is
/// a decision made here.
const PATTERN_BACKTRACK_LIMIT: usize = 1_000_000;

/// The compiled-validator cache. A handle rather than a static, so two cores in
/// one process cannot share one.
#[derive(Default)]
pub struct ArgValidators {
    inner: Mutex<Cache>,
}

#[derive(Default)]
struct Cache {
    /// Insertion order, for FIFO eviction.
    order: VecDeque<String>,
    entries: HashMap<String, Entry>,
}

struct Entry {
    /// A tool's parameters can change across a re-install under the same id,
    /// and a cache that did not notice would use the old contract.
    schema_json: String,
    /// `None` for a schema that would not compile: the fail-open above, cached
    /// so it is diagnosed once per install rather than once per call.
    validator: Option<jsonschema::Validator>,
}

impl ArgValidators {
    pub fn new() -> Self {
        Self::default()
    }

    /// Validate `arguments_json` against `parameters` and return a normalized
    /// JSON string with the schema's defaults applied. A schema that will not
    /// compile yields the raw arguments unchanged; see the module header.
    pub fn validate_and_normalize(
        &self,
        tool_id: &str,
        tool_name: &str,
        parameters: &Map<String, Value>,
        arguments_json: &str,
    ) -> Result<String> {
        let mut parsed: Value = if arguments_json.trim().is_empty() {
            Value::Object(Map::new())
        } else {
            serde_json::from_str(arguments_json).map_err(|_| {
                AppError::new(
                    ErrorCode::ValidationError,
                    format!("tool {tool_name}: arguments are not valid JSON"),
                )
            })?
        };

        let schema = Value::Object(parameters.clone());
        let schema_json = schema.to_string();

        let mut cache = lock(&self.inner);
        let entry = cache.entry_for(tool_id, &schema_json, &schema);
        let Some(validator) = entry.validator.as_ref() else {
            // Unusable schema: no validation, and no defaults either, because
            // the defaults live in a schema nothing has vouched for.
            return Ok(arguments_json.to_string());
        };

        // BEFORE validation, so a property with a default satisfies `required`.
        apply_defaults(&schema, &mut parsed);

        let errors: Vec<String> = validator
            .iter_errors(&parsed)
            .map(|err| {
                let path = err.instance_path.to_string();
                format!("args{path} {err}")
            })
            .collect();
        if !errors.is_empty() {
            return Err(AppError::new(
                ErrorCode::ValidationError,
                format!("tool {tool_name}: invalid arguments: {}", errors.join("; ")),
            ));
        }
        Ok(parsed.to_string())
    }

    #[cfg(test)]
    fn cached(&self) -> usize {
        lock(&self.inner).entries.len()
    }
}

impl Cache {
    fn entry_for(&mut self, tool_id: &str, schema_json: &str, schema: &Value) -> &Entry {
        let stale = self
            .entries
            .get(tool_id)
            .is_none_or(|entry| entry.schema_json != schema_json);
        if stale {
            if self.entries.len() >= MAX_VALIDATORS && !self.entries.contains_key(tool_id) {
                if let Some(oldest) = self.order.pop_front() {
                    self.entries.remove(&oldest);
                }
            }
            let validator = compile(schema);
            if validator.is_none() {
                tracing::warn!(
                    target: "loca::extensions",
                    tool = tool_id,
                    "unusable parameters schema, skipping argument validation"
                );
            }
            if self
                .entries
                .insert(
                    tool_id.to_string(),
                    Entry {
                        schema_json: schema_json.to_string(),
                        validator,
                    },
                )
                .is_none()
            {
                self.order.push_back(tool_id.to_string());
            }
        }
        self.entries
            .get(tool_id)
            .unwrap_or_else(|| unreachable!("just inserted"))
    }
}

fn compile(schema: &Value) -> Option<jsonschema::Validator> {
    jsonschema::options()
        .with_pattern_options(
            jsonschema::PatternOptions::fancy_regex().backtrack_limit(PATTERN_BACKTRACK_LIMIT),
        )
        // `format` is annotation-only in modern JSON Schema and authors use it
        // loosely; asserting it would reject arguments their runtime accepts.
        .should_validate_formats(false)
        .build(schema)
        .ok()
}

/// Fill in `default` for properties the model omitted. Recurses only into
/// subschemas whose value is PRESENT, so a missing object with defaulted
/// children stays missing unless it has a `default` of its own.
fn apply_defaults(schema: &Value, instance: &mut Value) {
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return;
    };
    let Some(object) = instance.as_object_mut() else {
        return;
    };
    for (name, subschema) in properties {
        if !object.contains_key(name) {
            if let Some(default) = subschema.get("default") {
                object.insert(name.clone(), default.clone());
            }
            continue;
        }
        if let Some(value) = object.get_mut(name) {
            apply_defaults(subschema, value);
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    // A poisoned cache is still a cache: a compiled schema and a string,
    // neither of which a panicking validator can leave half-updated.
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn schema(json: Value) -> Map<String, Value> {
        json.as_object().expect("an object schema").clone()
    }

    fn run(v: &ArgValidators, params: Value, args: &str) -> Result<String> {
        v.validate_and_normalize("ext/tool", "search", &schema(params), args)
    }

    fn object(json: &str) -> Value {
        serde_json::from_str(json).expect("valid json")
    }

    #[test]
    fn a_declared_default_reaches_the_tool() {
        let v = ArgValidators::new();
        let out = run(
            &v,
            serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "limit": { "type": "integer", "default": 10 },
                },
            }),
            r#"{"query":"cats"}"#,
        )
        .unwrap();
        assert_eq!(
            object(&out),
            serde_json::json!({ "query": "cats", "limit": 10 })
        );
    }

    #[test]
    fn a_default_satisfies_required_so_the_model_need_not_repeat_it() {
        // Validating first would reject arguments the author made optional by
        // giving them a default.
        let v = ArgValidators::new();
        let out = run(
            &v,
            serde_json::json!({
                "type": "object",
                "required": ["limit"],
                "properties": { "limit": { "type": "integer", "default": 10 } },
            }),
            "{}",
        )
        .unwrap();
        assert_eq!(object(&out), serde_json::json!({ "limit": 10 }));
    }

    #[test]
    fn a_value_the_model_supplied_is_never_overwritten_by_a_default() {
        let v = ArgValidators::new();
        let out = run(
            &v,
            serde_json::json!({
                "type": "object",
                "properties": { "limit": { "type": "integer", "default": 10 } },
            }),
            r#"{"limit":3}"#,
        )
        .unwrap();
        assert_eq!(object(&out), serde_json::json!({ "limit": 3 }));
    }

    #[test]
    fn defaults_reach_a_nested_object_the_model_did_supply() {
        let v = ArgValidators::new();
        let params = serde_json::json!({
            "type": "object",
            "properties": {
                "opts": {
                    "type": "object",
                    "properties": { "深": { "type": "integer", "default": 2 } },
                },
            },
        });
        let out = run(&v, params.clone(), r#"{"opts":{}}"#).unwrap();
        assert_eq!(object(&out), serde_json::json!({ "opts": { "深": 2 } }));

        // An absent parent stays absent: filling it in reads to the tool as an
        // explicit choice its caller never made.
        let out = run(&v, params, "{}").unwrap();
        assert_eq!(object(&out), serde_json::json!({}));
    }

    #[test]
    fn arguments_that_violate_the_schema_are_refused() {
        let v = ArgValidators::new();
        let err = run(
            &v,
            serde_json::json!({
                "type": "object",
                "required": ["query"],
                "properties": { "query": { "type": "string" } },
            }),
            r#"{"query":42}"#,
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::ValidationError);
        assert!(
            err.message.contains("search"),
            "no tool name: {}",
            err.message
        );
        assert!(err.message.contains("args"), "no arg path: {}", err.message);
    }

    #[test]
    fn arguments_that_are_not_json_are_refused_before_any_schema_work() {
        let v = ArgValidators::new();
        let err = run(&v, serde_json::json!({ "type": "object" }), "{not json").unwrap_err();
        assert_eq!(err.code, ErrorCode::ValidationError);
        assert!(err.message.contains("not valid JSON"));
    }

    #[test]
    fn an_empty_argument_string_is_an_empty_object_not_an_error() {
        // Models emit `""` for a no-argument tool call routinely.
        let v = ArgValidators::new();
        for args in ["", "   ", "\n"] {
            let out = run(
                &v,
                serde_json::json!({
                    "type": "object",
                    "properties": { "n": { "type": "integer", "default": 1 } },
                }),
                args,
            )
            .unwrap();
            assert_eq!(object(&out), serde_json::json!({ "n": 1 }));
        }
    }

    #[test]
    fn an_unusable_author_schema_passes_the_arguments_through_untouched() {
        // The raw string, not a re-serialization: nothing vouched for the
        // defaults in a schema nothing could compile.
        let v = ArgValidators::new();
        let raw = r#"{ "query" :  "cats" }"#;
        let out = run(
            &v,
            serde_json::json!({ "type": "object", "properties": { "q": { "type": 17 } } }),
            raw,
        )
        .unwrap();
        assert_eq!(out, raw);
    }

    #[test]
    fn a_schema_pointing_at_a_remote_ref_neither_fetches_nor_validates() {
        // A tool author's schema is not a user action. `resolve-http` is off,
        // so the compile fails and the call takes the fail-open path.
        let v = ArgValidators::new();
        let raw = r#"{"anything":true}"#;
        let out = run(
            &v,
            serde_json::json!({
                "type": "object",
                "properties": { "q": { "$ref": "https://example.invalid/schema.json" } },
            }),
            raw,
        )
        .unwrap();
        assert_eq!(out, raw);
    }

    /// Validate on its own thread and refuse to wait past `secs`. A stopwatch
    /// AROUND the call cannot see a hang: it reports only once the call
    /// returns, so the suite hangs rather than fails.
    fn bounded(params: Value, args: String, secs: u64) -> Result<String> {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let v = ArgValidators::new();
            let _ = tx.send(run(&v, params, &args));
        });
        match rx.recv_timeout(std::time::Duration::from_secs(secs)) {
            Ok(result) => result,
            Err(_) => panic!("a tool author's regex ran for over {secs} seconds"),
        }
    }

    #[test]
    fn a_textbook_exponential_pattern_is_answered_by_the_linear_time_engine() {
        // `(a+)+$` has no lookaround and no backreference, so it goes to the
        // `regex` crate: microseconds, and a CORRECT answer rather than an
        // error. Separate from the bounded case below, which is a different
        // mechanism.
        let params = serde_json::json!({
            "type": "object",
            "properties": { "q": { "type": "string", "pattern": "(a+)+$" } },
        });
        let evil = "a".repeat(60) + "!";
        let args = serde_json::json!({ "q": evil }).to_string();
        let err = bounded(params, args, 5).unwrap_err();
        assert_eq!(err.code, ErrorCode::ValidationError);
        assert!(
            err.message.contains("invalid arguments"),
            "the pattern was not evaluated at all: {}",
            err.message
        );
    }

    #[test]
    fn a_pattern_the_linear_engine_cannot_take_is_bounded_and_then_refused() {
        // The lookbehind forces the backtracking engine, the only shape where
        // PATTERN_BACKTRACK_LIMIT is reachable: 80ms to hit the limit against
        // about a minute unbounded, so a lifted limit shows up here as a hang.
        // The outcome must be a REFUSAL rather than a dropped constraint.
        let params = serde_json::json!({
            "type": "object",
            "properties": { "q": { "type": "string", "pattern": "^(a|a)+$(?<!q)" } },
        });
        let evil = "a".repeat(30) + "!";
        let args = serde_json::json!({ "q": evil }).to_string();
        assert!(
            bounded(params, args, 5).is_err(),
            "an unevaluatable pattern let the arguments through"
        );
    }

    #[test]
    fn a_reinstall_that_changes_the_schema_invalidates_the_cached_validator() {
        // A cache keyed on id alone answers against the old schema, which is
        // silently wrong rather than visibly failing.
        let v = ArgValidators::new();
        let v1 = serde_json::json!({
            "type": "object",
            "properties": { "n": { "type": "integer" } },
        });
        assert!(run(&v, v1, r#"{"n":1}"#).is_ok());

        let v2 = serde_json::json!({
            "type": "object",
            "properties": { "n": { "type": "string" } },
        });
        assert!(
            run(&v, v2, r#"{"n":1}"#).is_err(),
            "the stale validator was reused across a schema change"
        );
        assert_eq!(
            v.cached(),
            1,
            "the replacement should not have grown the cache"
        );
    }

    #[test]
    fn the_validator_cache_is_bounded_at_two_hundred_and_fifty_six() {
        // Literal counts, not arithmetic on MAX_VALIDATORS, which would move
        // with it.
        let v = ArgValidators::new();
        let params = schema(serde_json::json!({ "type": "object" }));
        for i in 0..256 {
            v.validate_and_normalize(&format!("tool-{i}"), "t", &params, "{}")
                .unwrap();
        }
        assert_eq!(v.cached(), 256);
        for i in 256..300 {
            v.validate_and_normalize(&format!("tool-{i}"), "t", &params, "{}")
                .unwrap();
        }
        assert_eq!(v.cached(), 256, "the cache grew past its bound");

        // FIFO: without this the test passes for a cache that evicts at random
        // or clears itself wholesale at the bound.
        let inner = lock(&v.inner);
        assert!(!inner.entries.contains_key("tool-0"));
        assert!(inner.entries.contains_key("tool-299"));
    }

    #[test]
    fn an_empty_parameters_schema_accepts_anything() {
        // `{}` must mean "no constraints" rather than "no properties".
        let v = ArgValidators::new();
        let out = run(&v, serde_json::json!({}), r#"{"whatever":[1,2]}"#).unwrap();
        assert_eq!(object(&out), serde_json::json!({ "whatever": [1, 2] }));
    }
}
