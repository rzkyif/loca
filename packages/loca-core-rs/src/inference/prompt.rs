//! One completion request, turned into the inputs a chat template is applied
//! with.
//!
//! Pure: no shim types, no llama.cpp, no settings reads, so these decisions are
//! tested in the DEFAULT gate; the gated half converts [`TurnPrompt`] into
//! `shim::ChatInputs` field for field.
//!
//! Two things are decided: which JSON the messages and tools become, and
//! whether the model is asked to think. Everything else `ChatInputs` carries is
//! one of its own constructor's defaults, so the conversion adds no third
//! opinion. The reasoning format in particular is PINNED rather than
//! configured, so the stream reader always finds thoughts in
//! `reasoning_content`; `llm.showReasoning` is a client-side display setting
//! and never reaches here.
//!
//! `enable_thinking` (a template input) and the reasoning-budget sampler are
//! two consumers of ONE decision with a three-level precedence, so [`thinking`]
//! resolves it once and [`super::sampling`] takes the answer as an argument
//! rather than re-reading the request.

use serde_json::Value;

use crate::error::{internal, Result};
use crate::llm::{LlmRequest, Reasoning};

/// Whether the model thinks this turn, and for how long.
///
/// `budget` is a token cap on the `<think>` block; `None` is unrestricted, which
/// is what the shipping core sends when `llm.reasoningBudget` is 0 or unset.
/// A budget with [`Self::enabled`] false is not representable, which is the
/// point: a cap on a block that will not be generated is a knob that does
/// nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Thinking {
    pub enabled: bool,
    pub budget: Option<u32>,
}

/// The inputs to one template application.
#[derive(Clone, Debug, PartialEq)]
pub struct TurnPrompt {
    /// The transcript as OpenAI-shaped JSON.
    pub messages: Value,
    /// The tool offer, or `None` for no offer at all. Never `[]`.
    pub tools: Option<Value>,
    pub thinking: Thinking,
}

/// Build the template inputs for one turn.
///
/// The messages and tools are a SERIALIZATION, not a re-mapping. llama.cpp
/// converts OpenAI JSON in both directions itself (D-002 finding 5) and D3's
/// types already serialise to exactly those shapes, with tests pinning them
/// (`llm/types.rs:367-453`). Hand-mapping here would be a second, drifting copy
/// of a format upstream maintains.
pub fn turn_prompt(req: &LlmRequest) -> Result<TurnPrompt> {
    turn_prompt_with(req, &req.messages)
}

/// [`turn_prompt`], over a transcript that is not the request's own.
///
/// The one caller that needs this is the multimodal path, which replaces each
/// image part with the projector's marker before the template runs
/// ([`super::vision::split`]). Everything else about the turn (the tools, the
/// thinking decision) still comes from the request, which is why this takes both
/// rather than being a second entry point that re-derives them.
pub fn turn_prompt_with(
    req: &LlmRequest,
    transcript: &[crate::llm::ChatMessage],
) -> Result<TurnPrompt> {
    let messages = serde_json::to_value(transcript)
        .map_err(|e| internal(format!("transcript is not serialisable: {e}")))?;

    // An empty tool list stays ABSENT rather than becoming `[]`. D3's body
    // builder makes the same distinction and its test says why: an empty tool
    // list is not an offer of no tools, it is no offer. A template that sees an
    // empty array can still take the tool-calling branch and render the
    // tool-call preamble for a turn that has no tools.
    let tools = if req.tools.is_empty() {
        None
    } else {
        Some(
            serde_json::to_value(&req.tools)
                .map_err(|e| internal(format!("tool offer is not serialisable: {e}")))?,
        )
    };

    Ok(TurnPrompt {
        messages,
        tools,
        thinking: thinking(req),
    })
}

/// Resolve whether this turn thinks, and under what cap.
///
/// The precedence is D3's, restated as the specification rather than
/// re-derived (`llm/provider.rs:406-455`). Only the LOCAL arm applies: if this
/// engine is running, the endpoint is the local provider, so the
/// `llama_body_extras` gate that `build_body` consults cannot be false here. It
/// is deliberately not re-tested; a fourth predicate that looks like "is this
/// local" is D-042 exactly.
///
/// 1. A per-call budget wins over the endpoint. `Some(0)` is thinking OFF,
///    `Some(n)` is thinking on capped at `n`. This is how the single-shot
///    utility calls each carry their own budget.
/// 2. Otherwise the endpoint's reasoning mode decides, capped by
///    `endpoint.reasoning_budget` when that is set and non-zero.
/// 3. An endpoint with no opinion leaves thinking ON.
///
/// Step 3 is the one place this differs in SHAPE from the HTTP path, and it is
/// worth naming. Over HTTP "no opinion" meant sending no `enable_thinking` at
/// all, which leaves a thinking-by-default model thinking. In process the field
/// is a plain `bool` with no absent state, so "leave it alone" has to be spelled
/// `true`. Spelling it `false` would silently stop every thinking model from
/// thinking on any endpoint that expressed no preference.
pub fn thinking(req: &LlmRequest) -> Thinking {
    if let Some(budget) = req.overrides.reasoning_budget {
        return Thinking {
            enabled: budget > 0,
            budget: (budget > 0).then_some(budget),
        };
    }
    match req.endpoint.reasoning {
        // Thinking-by-default models keep thinking unless the template is told
        // `false`, so "off" must be SENT rather than omitted. Deleting this arm
        // leaves every thinking-off turn thinking, and a test covering only the
        // "on" case still passes.
        Some(Reasoning::Off) => Thinking {
            enabled: false,
            budget: None,
        },
        Some(Reasoning::On) => Thinking {
            enabled: true,
            budget: req.endpoint.reasoning_budget.filter(|b| *b > 0),
        },
        None => Thinking {
            enabled: true,
            budget: None,
        },
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::llm::{ChatMessage, LlmEndpointConfig, Overrides, ToolCallRef, ToolDef};
    use serde_json::json;

    fn local_endpoint() -> LlmEndpointConfig {
        LlmEndpointConfig {
            model: "local".to_string(),
            is_local_provider: true,
            llama_body_extras: true,
            ..LlmEndpointConfig::default()
        }
    }

    fn request() -> LlmRequest {
        LlmRequest::new(local_endpoint(), vec![ChatMessage::user("hi")])
    }

    #[test]
    fn the_transcript_reaches_the_template_in_the_openai_shapes() {
        // llama.cpp's own OpenAI conversion is what reads this, so the shapes
        // are not ours to choose. A tool call and its response are included
        // because that pair is where a wrong shape does the most damage: an
        // undeclared `tool_call_id` renders as an orphaned response and garbles
        // every hop after it.
        let req = LlmRequest::new(
            local_endpoint(),
            vec![
                ChatMessage::system("be brief"),
                ChatMessage::user("weather?"),
                ChatMessage::Assistant {
                    content: String::new(),
                    tool_calls: vec![ToolCallRef::function(
                        "call_1",
                        "get_weather",
                        r#"{"city":"Jakarta"}"#,
                    )],
                },
                ChatMessage::Tool {
                    tool_call_id: "call_1".to_string(),
                    content: r#"{"temp":31}"#.to_string(),
                },
            ],
        );
        assert_eq!(
            turn_prompt(&req).unwrap().messages,
            json!([
                { "role": "system", "content": "be brief" },
                { "role": "user", "content": "weather?" },
                {
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": { "name": "get_weather", "arguments": "{\"city\":\"Jakarta\"}" }
                    }],
                },
                { "role": "tool", "tool_call_id": "call_1", "content": "{\"temp\":31}" },
            ]),
        );
    }

    #[test]
    fn no_tools_means_no_offer_and_not_an_empty_offer() {
        // `[]` and absent are different inputs to a Jinja template: `tools` is
        // routinely tested for truthiness, and an empty array can still take the
        // tool-calling branch, rendering a tool preamble for a turn with no
        // tools and biasing the model toward calling something that is not
        // there.
        assert_eq!(turn_prompt(&request()).unwrap().tools, None);
    }

    #[test]
    fn an_offered_tool_reaches_the_template_with_its_schema_untouched() {
        let req = request().with_tools(vec![ToolDef::function(
            "get_weather",
            "Look up the weather",
            json!({ "type": "object", "properties": { "city": { "type": "string" } } }),
        )]);
        assert_eq!(
            turn_prompt(&req).unwrap().tools,
            Some(json!([{
                "type": "function",
                "function": {
                    "name": "get_weather",
                    "description": "Look up the weather",
                    "parameters": {
                        "type": "object",
                        "properties": { "city": { "type": "string" } },
                    },
                },
            }])),
        );
    }

    #[test]
    fn an_endpoint_with_thinking_off_says_so_rather_than_staying_silent() {
        // The trap D3 documents, in the shape it takes here. Over HTTP the risk
        // was omitting the flag; in process `enabled` is a plain bool, so the
        // risk is answering `true` for an endpoint that said off. A
        // thinking-by-default model would then think on every single turn with
        // the setting visibly switched off.
        let mut req = request();
        req.endpoint.reasoning = Some(Reasoning::Off);
        assert_eq!(
            thinking(&req),
            Thinking {
                enabled: false,
                budget: None
            },
        );
    }

    #[test]
    fn an_endpoint_with_thinking_on_carries_its_budget_and_omits_a_zero_one() {
        let mut req = request();
        req.endpoint.reasoning = Some(Reasoning::On);
        req.endpoint.reasoning_budget = Some(512);
        assert_eq!(
            thinking(&req),
            Thinking {
                enabled: true,
                budget: Some(512)
            },
        );

        // Unset is unrestricted, and so is an explicit zero: `llm.reasoningBudget`
        // 0 means no cap, and sending it as a cap of zero would mean the exact
        // opposite (stop thinking immediately).
        req.endpoint.reasoning_budget = None;
        assert_eq!(thinking(&req).budget, None);
        req.endpoint.reasoning_budget = Some(0);
        assert_eq!(
            thinking(&req).budget,
            None,
            "a zero budget is unrestricted, not a cap of zero",
        );
        assert!(thinking(&req).enabled);
    }

    #[test]
    fn an_endpoint_with_no_opinion_leaves_thinking_on() {
        // "No opinion" over HTTP meant sending no flag, which leaves a
        // thinking-by-default model thinking. In process there is no absent
        // state, so the equivalent has to be spelled `true`. Answering `false`
        // here would silently disable reasoning for every endpoint built without
        // one, which includes every hand-built request in the crate.
        let req = request();
        assert_eq!(req.endpoint.reasoning, None, "the fixture states the case");
        assert!(thinking(&req).enabled);
    }

    #[test]
    fn a_per_call_budget_overrides_the_endpoint_in_both_directions() {
        // This is what the single-shot utility calls use, and it has to win both
        // ways round. Testing only the on-over-off direction would leave a
        // `Some(0)` silently thinking, which is the expensive half: a title
        // generation that reasons for 2000 tokens before answering.
        let mut off_endpoint = request();
        off_endpoint.endpoint.reasoning = Some(Reasoning::Off);
        off_endpoint.overrides = Overrides {
            reasoning_budget: Some(256),
            ..Overrides::default()
        };
        assert_eq!(
            thinking(&off_endpoint),
            Thinking {
                enabled: true,
                budget: Some(256)
            },
        );

        let mut on_endpoint = request();
        on_endpoint.endpoint.reasoning = Some(Reasoning::On);
        on_endpoint.endpoint.reasoning_budget = Some(512);
        on_endpoint.overrides = Overrides {
            reasoning_budget: Some(0),
            ..Overrides::default()
        };
        assert_eq!(
            thinking(&on_endpoint),
            Thinking {
                enabled: false,
                budget: None
            },
            "a per-call 0 turns thinking off and takes the endpoint's cap with it",
        );
    }

    #[test]
    fn the_whole_prompt_carries_the_thinking_decision_rather_than_dropping_it() {
        // `turn_prompt` is the only caller the engine has, so a `thinking` that
        // resolved correctly and was then not attached would be invisible.
        let mut req = request();
        req.endpoint.reasoning = Some(Reasoning::Off);
        assert!(!turn_prompt(&req).unwrap().thinking.enabled);
    }
}
