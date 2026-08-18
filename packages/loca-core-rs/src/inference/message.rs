//! What a parse of the model's output produced, in this module's own shape.
//!
//! This mirrors llama.cpp's `common_chat_msg` after `to_json_oaicompat`, but it
//! is declared here rather than reused from [`super::shim`] for one reason: the
//! shim is behind the `local-inference` feature and this is not. Everything that
//! DECIDES (the diff engine, the delta emitter, the stop handling) works on this
//! type, so it compiles and is tested in the default gate, with no llama.cpp
//! anywhere. One feature-gated conversion in the engine absorbs the shim's
//! shape.
//!
//! The field names and the `Deserialize` are deliberately the ones llama.cpp
//! emits, so the conversion is a deserialization rather than a hand-mapping.
//! llama.cpp already converts OpenAI JSON in both directions (D-002 finding 5);
//! re-deriving those shapes here would be re-deriving something upstream
//! maintains.

use serde::{Deserialize, Serialize};

/// One parse of the generated text so far.
///
/// Every field defaults, because a partial parse routinely produces a message
/// with content and nothing else, and because a format with no PEG parser
/// produces content only for the whole generation.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct ParsedMessage {
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub content: String,
    /// The `<think>` block, split out because the turn was applied with
    /// `ReasoningFormat::Deepseek`. The client renders this as its own channel,
    /// so it must never end up inside [`Self::content`].
    #[serde(default)]
    pub reasoning_content: String,
    #[serde(default)]
    pub tool_calls: Vec<ParsedToolCall>,
}

/// One tool call, as far as the parser has got with it.
///
/// `arguments` is a JSON STRING rather than an object: that is the OpenAI wire
/// shape and what the whole core already consumes
/// (`llm::types::ToolCallFunction`). During a partial parse it is routinely
/// half-written JSON.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct ParsedToolCall {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub arguments: String,
    /// Absent for most templates, and that is not an edge case.
    ///
    /// `common_chat_msg::to_json_oaicompat` omits `id` when the model produced
    /// none, and the line that would generate one is COMMENTED OUT
    /// (`vendor/llama.cpp/common/chat.cpp:252-258`). The generator itself,
    /// `gen_tool_call_id`, lives in `tools/server/server-common.cpp:113`, which
    /// is upstream's server, and we do not build it. So the empty id is the
    /// normal case here where it never was behind `llama-server`, and
    /// [`super::ids`] is what fills it. An empty `tool_call_id` reaching the
    /// transcript renders as an orphaned tool response and garbles every hop
    /// after it (`llm::types::ToolCallRef`).
    #[serde(default)]
    pub id: String,
}

impl ParsedMessage {
    /// Whether this parse found anything at all. A generation that has produced
    /// only whitespace the parser is still holding produces an empty message,
    /// and emitting a delta for it would put a token on the stream that was
    /// never generated.
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
        // The shape `to_json_oaicompat` produces for a plain answer: no
        // `tool_calls` key at all, no `reasoning_content` key at all.
        let msg: ParsedMessage =
            serde_json::from_str(r#"{"role":"assistant","content":"It is 31 degrees."}"#).unwrap();
        assert_eq!(msg.content, "It is 31 degrees.");
        assert!(msg.reasoning_content.is_empty());
        assert!(msg.tool_calls.is_empty());
        assert!(!msg.is_empty());
    }

    #[test]
    fn a_tool_call_without_an_id_deserialises_rather_than_failing() {
        // The normal case, not an edge case: the id-generating line upstream is
        // commented out and its generator lives in the server we do not build.
        // A `#[serde(default)]` missing here would fail the parse of every
        // Qwen-style tool call, which is most of the catalog.
        let msg: ParsedMessage = serde_json::from_str(
            r#"{"content":"","tool_calls":[{"name":"get_weather","arguments":"{\"city\":\"Jakarta\"}"}]}"#,
        )
        .unwrap();
        assert_eq!(msg.tool_calls.len(), 1);
        assert_eq!(msg.tool_calls[0].name, "get_weather");
        assert_eq!(msg.tool_calls[0].arguments, r#"{"city":"Jakarta"}"#);
        assert!(
            msg.tool_calls[0].id.is_empty(),
            "an absent id must read as empty, which is what `ids` then fills",
        );
        assert!(
            !msg.is_empty(),
            "a tool call is output even with no content"
        );
    }

    #[test]
    fn reasoning_stays_in_its_own_field() {
        let msg: ParsedMessage = serde_json::from_str(
            r#"{"content":"It is 31 degrees.","reasoning_content":"The user wants weather."}"#,
        )
        .unwrap();
        assert_eq!(msg.reasoning_content, "The user wants weather.");
        assert_eq!(
            msg.content, "It is 31 degrees.",
            "the split is what --reasoning-format deepseek buys, and merging the \
             two would put the model's thoughts in the answer",
        );
    }

    #[test]
    fn an_empty_parse_is_empty_and_a_whitespace_one_is_not() {
        assert!(ParsedMessage::default().is_empty());
        // Whitespace IS content: the model generated it and the client renders
        // it. Trimming here would silently eat the space between two words that
        // arrived in separate tokens.
        let space = ParsedMessage {
            content: " ".to_string(),
            ..ParsedMessage::default()
        };
        assert!(!space.is_empty());
    }
}
