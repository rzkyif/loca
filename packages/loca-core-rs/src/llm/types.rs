//! The shapes a completion is asked for and answered in.
//!
//! The request half serialises straight onto the OpenAI-compatible wire, so the
//! serde attributes here ARE the wire format; the response half
//! ([`LlmDelta`]) is the normalised form every consumer reads, whether the
//! completion came over HTTP or, after Stage 3, from the in-process engine.

use serde::Serialize;
use serde_json::Value;

use crate::contract::api::TokenUsage;

/// Which model a turn is routed to.
///
/// The secondary route is the dual-model setup's fast responder: a separate
/// external endpoint, deliberately with no reasoning and no sampling overrides.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LlmRoute {
    #[default]
    Default,
    Secondary,
}

impl LlmRoute {
    pub fn as_str(self) -> &'static str {
        match self {
            LlmRoute::Default => "default",
            LlmRoute::Secondary => "secondary",
        }
    }
}

/// Whether the model should think before answering.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reasoning {
    Off,
    On,
}

/// The OpenAI-style effort level, for endpoints that take no token budget.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReasoningEffort {
    Minimal,
    Low,
    Medium,
    High,
}

impl ReasoningEffort {
    pub fn as_str(self) -> &'static str {
        match self {
            ReasoningEffort::Minimal => "minimal",
            ReasoningEffort::Low => "low",
            ReasoningEffort::Medium => "medium",
            ReasoningEffort::High => "high",
        }
    }

    /// Parse a settings value. `llm.reasoningEffort` is a select over exactly
    /// these four, so PATCH cannot store anything else and only a hand-edited
    /// file reaches the fallback; sending the schema default beats forwarding a
    /// typo for the provider to reject.
    pub fn from_setting(raw: &str) -> Self {
        match raw {
            "minimal" => ReasoningEffort::Minimal,
            "low" => ReasoningEffort::Low,
            "medium" => ReasoningEffort::Medium,
            _ => ReasoningEffort::High,
        }
    }
}

/// Everything about the upstream a completion goes to.
///
/// Resolved from settings by [`super::resolver::resolve_endpoint`], or built by
/// hand in a test. Every sampler is `Option`, and absent means "do not put the
/// field on the wire" rather than "use zero", which is also what makes the
/// derived `Default` the right empty endpoint rather than a set of zeroed
/// samplers.
#[derive(Clone, Debug, Default)]
pub struct LlmEndpointConfig {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub reasoning: Option<Reasoning>,
    /// External endpoints only. Local ones take [`Self::reasoning_budget`].
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Local endpoints only: the per-turn `<think>` token cap. `None` is
    /// unrestricted.
    pub reasoning_budget: Option<u32>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub max_tokens: Option<u32>,
    pub top_k: Option<u32>,
    pub min_p: Option<f64>,
    pub repeat_penalty: Option<f64>,
    pub dry_multiplier: Option<f64>,
    pub presence_penalty: Option<f64>,
    /// llama.cpp sampler-chain order, by canonical name. `None` leaves the
    /// server's default chain alone.
    pub samplers: Option<Vec<String>>,
    /// Whether the llama.cpp-only body fields may be sent, derived from the base
    /// URL being loopback.
    ///
    /// **This is not the same question as [`Self::is_local_provider`], and the
    /// two disagree for a real configuration.** An external provider pointed at
    /// a loopback gateway (LM Studio, Ollama, a hand-run llama-server) has this
    /// set and `is_local_provider` clear: it receives the llama samplers and
    /// does not queue. Folding them into one flag would start queueing external
    /// traffic behind one slot AND stop sending samplers to a hand-run
    /// llama-server, in one edit, with nothing failing to say so.
    pub llama_body_extras: bool,
    /// Whether this is the core's own local model, from `llm.provider`. Gates
    /// the scheduler's semaphore, and after Stage 3 selects the in-process
    /// engine over HTTP. See the note on [`Self::llama_body_extras`].
    pub is_local_provider: bool,
}

/// Per-call sampling, merged over the endpoint's own values.
#[derive(Clone, Debug, Default)]
pub struct Overrides {
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub max_tokens: Option<u32>,
    /// Per-call thinking budget, in tokens. `Some(0)` turns thinking OFF for
    /// this call and `Some(n)` turns it on capped at `n`; either overrides the
    /// endpoint's reasoning mode. `None` leaves the endpoint in charge, which is
    /// what the chat path wants. The single-shot utility calls each carry their
    /// own budget setting, which is why this exists at all.
    pub reasoning_budget: Option<u32>,
}

/// One completion request.
#[derive(Clone, Debug)]
pub struct LlmRequest {
    pub endpoint: LlmEndpointConfig,
    pub messages: Vec<ChatMessage>,
    pub tools: Vec<ToolDef>,
    pub overrides: Overrides,
}

impl LlmRequest {
    /// A request with no tools and no overrides.
    pub fn new(endpoint: LlmEndpointConfig, messages: Vec<ChatMessage>) -> Self {
        Self {
            endpoint,
            messages,
            tools: Vec::new(),
            overrides: Overrides::default(),
        }
    }

    pub fn with_overrides(mut self, overrides: Overrides) -> Self {
        self.overrides = overrides;
        self
    }

    pub fn with_tools(mut self, tools: Vec<ToolDef>) -> Self {
        self.tools = tools;
        self
    }
}

/// One message in the transcript, in the shape the model API takes.
///
/// Reasoning traces are deliberately absent: they are not part of the message
/// protocol, and the TypeScript drops them when building a request too.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "role", rename_all = "lowercase")]
pub enum ChatMessage {
    System {
        content: String,
    },
    User {
        content: UserContent,
    },
    Assistant {
        content: String,
        /// The calls this turn emitted. They MUST be replayed: the `tool`
        /// messages that follow reference them by id, and an undeclared
        /// `tool_call_id` renders as an orphaned response in the chat template,
        /// garbling every hop after it.
        #[serde(skip_serializing_if = "Vec::is_empty")]
        tool_calls: Vec<ToolCallRef>,
    },
    Tool {
        tool_call_id: String,
        content: String,
    },
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        ChatMessage::System {
            content: content.into(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        ChatMessage::User {
            content: UserContent::Text(content.into()),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        ChatMessage::Assistant {
            content: content.into(),
            tool_calls: Vec::new(),
        }
    }
}

/// User content is either plain text or a multipart list.
///
/// The multipart form only earns its keep when there is an image: some providers
/// reject a parts array that carries no `image_url` sibling, so a caller with
/// only text collapses it to the string form.
#[derive(Clone, Debug, Serialize)]
#[serde(untagged)]
pub enum UserContent {
    Text(String),
    Parts(Vec<ContentPart>),
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    Text { text: String },
    ImageUrl { image_url: ImageUrl },
}

#[derive(Clone, Debug, Serialize)]
pub struct ImageUrl {
    /// An `https:` URL or a `data:` URI with the bytes inlined.
    pub url: String,
}

/// The only tool type the API has.
#[derive(Clone, Copy, Debug, Default, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolKind {
    #[default]
    Function,
}

/// A tool offered to the model.
#[derive(Clone, Debug, Serialize)]
pub struct ToolDef {
    #[serde(rename = "type")]
    pub kind: ToolKind,
    pub function: ToolFunction,
}

#[derive(Clone, Debug, Serialize)]
pub struct ToolFunction {
    pub name: String,
    pub description: String,
    /// JSON Schema for the arguments, passed through untouched.
    pub parameters: Value,
}

impl ToolDef {
    pub fn function(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: Value,
    ) -> Self {
        Self {
            kind: ToolKind::Function,
            function: ToolFunction {
                name: name.into(),
                description: description.into(),
                parameters,
            },
        }
    }
}

/// A tool call replayed on an assistant message.
#[derive(Clone, Debug, Serialize)]
pub struct ToolCallRef {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: ToolKind,
    pub function: ToolCallFunction,
}

#[derive(Clone, Debug, Serialize)]
pub struct ToolCallFunction {
    pub name: String,
    /// The arguments as the model emitted them: a JSON string, not an object.
    pub arguments: String,
}

impl ToolCallRef {
    pub fn function(
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            kind: ToolKind::Function,
            function: ToolCallFunction {
                name: name.into(),
                arguments: arguments.into(),
            },
        }
    }
}

/// One normalised chunk of a completion.
///
/// Every field is optional because a chunk carries whichever of them moved: a
/// usage-only chunk produces no output, and the finish reason arrives alone.
#[derive(Clone, Debug, Default)]
pub struct LlmDelta {
    pub content_delta: Option<String>,
    pub reasoning_delta: Option<String>,
    /// Fragments, not whole calls. The first fragment for an index carries `id`
    /// and `name`; later ones extend `arguments_delta`, and the caller
    /// reassembles by index.
    pub tool_calls: Vec<ToolCallDelta>,
    pub finish_reason: Option<String>,
    pub usage: Option<TokenUsage>,
}

impl LlmDelta {
    /// True when nothing at all moved, which the SSE decoder uses to drop the
    /// role-only opening chunk rather than yield an empty delta.
    pub fn is_empty(&self) -> bool {
        self.content_delta.is_none()
            && self.reasoning_delta.is_none()
            && self.tool_calls.is_empty()
            && self.finish_reason.is_none()
            && self.usage.is_none()
    }
}

/// Hand-written because the generated [`TokenUsage`] carries no `PartialEq`, and
/// comparing a whole delta is what makes a provider test readable.
impl PartialEq for LlmDelta {
    fn eq(&self, other: &Self) -> bool {
        let usage = |u: &Option<TokenUsage>| u.as_ref().map(|u| (u.prompt, u.completion, u.total));
        self.content_delta == other.content_delta
            && self.reasoning_delta == other.reasoning_delta
            && self.tool_calls == other.tool_calls
            && self.finish_reason == other.finish_reason
            && usage(&self.usage) == usage(&other.usage)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ToolCallDelta {
    pub index: u32,
    pub id: Option<String>,
    pub name: Option<String>,
    pub arguments_delta: Option<String>,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn messages_serialise_to_the_openai_shapes() {
        let messages = vec![
            ChatMessage::system("be brief"),
            ChatMessage::user("hi"),
            ChatMessage::Assistant {
                content: "one moment".to_string(),
                tool_calls: vec![ToolCallRef::function("call_1", "get_weather", "{\"c\":1}")],
            },
            ChatMessage::Tool {
                tool_call_id: "call_1".to_string(),
                content: "{\"temp\":31}".to_string(),
            },
        ];
        assert_eq!(
            serde_json::to_value(&messages).unwrap(),
            json!([
                { "role": "system", "content": "be brief" },
                { "role": "user", "content": "hi" },
                {
                    "role": "assistant",
                    "content": "one moment",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": { "name": "get_weather", "arguments": "{\"c\":1}" }
                    }],
                },
                { "role": "tool", "tool_call_id": "call_1", "content": "{\"temp\":31}" },
            ])
        );
    }

    #[test]
    fn an_assistant_message_without_calls_omits_the_key() {
        let value = serde_json::to_value(ChatMessage::assistant("hello")).unwrap();
        assert_eq!(value, json!({ "role": "assistant", "content": "hello" }));
    }

    #[test]
    fn multipart_user_content_carries_images_beside_text() {
        let message = ChatMessage::User {
            content: UserContent::Parts(vec![
                ContentPart::Text {
                    text: "what is this".to_string(),
                },
                ContentPart::ImageUrl {
                    image_url: ImageUrl {
                        url: "data:image/png;base64,AAAA".to_string(),
                    },
                },
            ]),
        };
        assert_eq!(
            serde_json::to_value(&message).unwrap(),
            json!({
                "role": "user",
                "content": [
                    { "type": "text", "text": "what is this" },
                    { "type": "image_url", "image_url": { "url": "data:image/png;base64,AAAA" } },
                ],
            })
        );
    }

    #[test]
    fn a_tool_definition_serialises_with_its_schema_untouched() {
        let tool = ToolDef::function(
            "get_weather",
            "Look up the weather",
            json!({ "type": "object", "properties": { "city": { "type": "string" } } }),
        );
        assert_eq!(
            serde_json::to_value(&tool).unwrap(),
            json!({
                "type": "function",
                "function": {
                    "name": "get_weather",
                    "description": "Look up the weather",
                    "parameters": {
                        "type": "object",
                        "properties": { "city": { "type": "string" } },
                    },
                },
            })
        );
    }

    #[test]
    fn every_effort_level_survives_the_round_trip_to_the_wire() {
        // All four are real values of `llm.reasoningEffort` in the settings
        // schema, and each goes on the wire verbatim. A level with no arm here
        // does not fail: it falls through to the schema default and silently
        // sends "high", so the model thinks harder (and costs more) than the
        // user asked. Only asserting one level leaves the other three free.
        for level in [
            ReasoningEffort::Minimal,
            ReasoningEffort::Low,
            ReasoningEffort::Medium,
            ReasoningEffort::High,
        ] {
            assert_eq!(
                ReasoningEffort::from_setting(level.as_str()),
                level,
                "{} must not degrade on the way through settings",
                level.as_str(),
            );
        }
        assert_eq!(
            ReasoningEffort::from_setting("wat"),
            ReasoningEffort::High,
            "a hand-edited file falls back to the schema default",
        );
    }
}
