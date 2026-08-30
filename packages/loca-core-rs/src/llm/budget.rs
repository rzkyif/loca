//! The per-use-case thinking budget for single-shot utility calls.
//!
//! Each use case (title generation, transcription cleanup and merge, memory
//! summary, complexity check, tool-filter refinement) has its own
//! `*ThinkingBudget` setting. This reads and normalises one of them; callers
//! pass it through [`super::Overrides::reasoning_budget`] and add it to their
//! `max_tokens` so the answer still has room after the thinking. The `0` case
//! is load-bearing: [`super::provider`] encodes it as "thinking off".

use crate::settings::access::num_setting;
use crate::settings::SettingsRecord;

/// The budget at `key`, in tokens. `0` means thinking is off for this call.
///
/// A non-integer is floored and anything unreadable, absent, negative or zero
/// comes back as 0, so a caller never has to distinguish "unset" from "off":
/// they are the same instruction.
pub fn thinking_budget(settings: &SettingsRecord, key: &str) -> u32 {
    let value = num_setting(settings, key, 0.0);
    if value > 0.0 {
        // `as` saturates at the bounds, and the sign is already checked.
        value.floor() as u32
    } else {
        0
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn record(pairs: Value) -> SettingsRecord {
        match pairs {
            Value::Object(map) => map,
            _ => unreachable!("test records are objects"),
        }
    }

    #[test]
    fn a_positive_budget_is_floored() {
        let settings = record(json!({ "a": 256, "b": 2.7, "c": "512" }));
        assert_eq!(thinking_budget(&settings, "a"), 256);
        assert_eq!(thinking_budget(&settings, "b"), 2);
        assert_eq!(thinking_budget(&settings, "c"), 512);
    }

    #[test]
    fn absent_zero_negative_and_unreadable_all_mean_thinking_off() {
        let settings = record(json!({
            "zero": 0,
            "negative": -1,
            "junk": "nope",
            "empty": "",
            "wrong-type": true,
        }));
        for key in ["missing", "zero", "negative", "junk", "empty", "wrong-type"] {
            assert_eq!(
                thinking_budget(&settings, key),
                0,
                "{key} must read as thinking off, not as an unbounded budget",
            );
        }
    }
}
