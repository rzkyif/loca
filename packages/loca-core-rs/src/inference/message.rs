//! What a parse of the model's output produced. Mirrors llama.cpp's
//! `common_chat_msg` after `to_json_oaicompat` (same field names, so the
//! conversion is a deserialization), declared here so the ungated decision
//! logic never names a `shim` type.

use serde::{Deserialize, Serialize};

/// One parse of the generated text so far. Every field defaults: a partial
/// parse routinely has content and nothing else.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct ParsedMessage {
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub content: String,
    /// The `<think>` block; rendered as its own channel, never part of `content`.
    #[serde(default)]
    pub reasoning_content: String,
    #[serde(default)]
    pub tool_calls: Vec<ParsedToolCall>,
}

/// One tool call. `arguments` is a JSON string (the OpenAI wire shape), and
/// routinely half-written during a partial parse.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct ParsedToolCall {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub arguments: String,
    /// Empty for most templates: upstream's id generator lives in the server we
    /// do not build. [`super::ids`] fills it.
    #[serde(default)]
    pub id: String,
}

impl ParsedMessage {
    /// Whether this parse found anything; an empty parse must not emit a delta.
    pub fn is_empty(&self) -> bool {
        self.content.is_empty() && self.reasoning_content.is_empty() && self.tool_calls.is_empty()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn a_content_only_parse_deserialises_from_what_llama_cpp_emits() {
        let msg: ParsedMessage =
            serde_json::from_str(r#"{"role":"assistant","content":"It is 31 degrees."}"#).unwrap();
        assert_eq!(msg.content, "It is 31 degrees.");
        assert!(msg.reasoning_content.is_empty());
        assert!(msg.tool_calls.is_empty());
        assert!(!msg.is_empty());
    }

    #[test]
    fn a_tool_call_without_an_id_deserialises_rather_than_failing() {
        let msg: ParsedMessage = serde_json::from_str(
            r#"{"content":"","tool_calls":[{"name":"get_weather","arguments":"{\"city\":\"Jakarta\"}"}]}"#,
        )
        .unwrap();
        assert_eq!(msg.tool_calls.len(), 1);
        assert_eq!(msg.tool_calls[0].name, "get_weather");
        assert_eq!(msg.tool_calls[0].arguments, r#"{"city":"Jakarta"}"#);
        assert!(msg.tool_calls[0].id.is_empty());
        assert!(!msg.is_empty());
    }

    #[test]
    fn reasoning_stays_in_its_own_field() {
        let msg: ParsedMessage = serde_json::from_str(
            r#"{"content":"It is 31 degrees.","reasoning_content":"The user wants weather."}"#,
        )
        .unwrap();
        assert_eq!(msg.reasoning_content, "The user wants weather.");
        assert_eq!(msg.content, "It is 31 degrees.");
    }

    #[test]
    fn an_empty_parse_is_empty_and_a_whitespace_one_is_not() {
        assert!(ParsedMessage::default().is_empty());
        // Whitespace is content: the model generated it.
        let space = ParsedMessage {
            content: " ".to_string(),
            ..ParsedMessage::default()
        };
        assert!(!space.is_empty());
    }
}
