//! Typed readers for a sparse settings record.
//!
//! A key absent from the record, or stored with the wrong type, falls back to
//! the caller's default. Every consumer of a settings map reads through these
//! rather than matching on `Value` itself, so "absent" and "junk" behave
//! identically everywhere.

use serde_json::{Map, Value};

pub fn str_setting(settings: &Map<String, Value>, key: &str, def: &str) -> String {
    match settings.get(key) {
        Some(Value::String(s)) => s.clone(),
        _ => def.to_string(),
    }
}

/// Also accepts a non-empty numeric string, because hand-edited `settings.json`
/// values and some form inputs arrive as strings. That is not a nicety: the port
/// number is a `string` field and `llm.gpuLayers` is a `number` field whose
/// default is `""`.
///
/// Two JavaScript warts are deliberately not carried: `Number("0x10")` is 16 and
/// `Number("  ")` is 0, where this returns the default for both.
pub fn num_setting(settings: &Map<String, Value>, key: &str, def: f64) -> f64 {
    match settings.get(key) {
        Some(Value::Number(n)) => n.as_f64().filter(|v| v.is_finite()).unwrap_or(def),
        Some(Value::String(s)) if !s.is_empty() => s
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|v| v.is_finite())
            .unwrap_or(def),
        _ => def,
    }
}

pub fn bool_setting(settings: &Map<String, Value>, key: &str, def: bool) -> bool {
    match settings.get(key) {
        Some(Value::Bool(b)) => *b,
        _ => def,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use serde_json::json;

    fn record() -> Map<String, Value> {
        match json!({
            "str": "value",
            "num": 42,
            "float": 1.5,
            "numeric-string": "7701",
            "empty-string": "",
            "junk-string": "12abc",
            "bool": true,
            "wrong-type": ["nope"],
        }) {
            Value::Object(map) => map,
            _ => unreachable!(),
        }
    }

    #[test]
    fn a_reader_returns_the_stored_value_when_the_type_matches() {
        let s = record();
        assert_eq!(str_setting(&s, "str", "def"), "value");
        assert_eq!(num_setting(&s, "num", 0.0), 42.0);
        assert_eq!(num_setting(&s, "float", 0.0), 1.5);
        assert!(bool_setting(&s, "bool", false));
    }

    #[test]
    fn absent_wrong_typed_and_unparseable_values_all_fall_back() {
        let s = record();
        assert_eq!(str_setting(&s, "absent", "def"), "def");
        assert_eq!(str_setting(&s, "num", "def"), "def");
        assert_eq!(num_setting(&s, "absent", 7.0), 7.0);
        assert_eq!(num_setting(&s, "wrong-type", 7.0), 7.0);
        assert_eq!(num_setting(&s, "empty-string", 7.0), 7.0);
        assert_eq!(num_setting(&s, "junk-string", 7.0), 7.0);
        assert!(bool_setting(&s, "absent", true));
        assert!(bool_setting(&s, "str", true));
    }

    #[test]
    fn a_numeric_string_is_coerced() {
        assert_eq!(num_setting(&record(), "numeric-string", 0.0), 7701.0);
    }
}
