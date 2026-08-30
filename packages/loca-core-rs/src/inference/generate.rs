//! One turn: prompt, prefill, sample, parse, emit.
//!
//! Blocking from top to bottom. A [`shim::Context`] is `Send` but never `Sync`,
//! so a turn owns it for its whole duration on a blocking thread
//! [`super::runtime`] provides. Deltas go to a [`DeltaSink`], which is also
//! what makes the loop testable against a `Vec`.
//!
//! Cancellation is the sink answering `false`, and that is the only signal: a
//! second one could disagree with the first. The obligation is that it is
//! checked often enough, which is why prefill is chunked: a single-shot prefill
//! would hold the context for seconds after the user pressed stop.
//!
//! Every token, the accumulated text is parsed with `is_partial = true` and
//! diffed against the previous parse ([`super::diff`]), which is what makes
//! tool-call arguments and reasoning stream as they are produced. The text
//! handed to the parser is the STOP-SAFE prefix, never the raw accumulation: a
//! half-emitted stop marker must not be emitted and then retracted.

use std::sync::Arc;

use tokio::sync::Mutex;

use crate::llm::{LlmDelta, LlmRequest, ProviderError};

use super::diff::diff_messages;
use super::engine::{ContextState, Ready};
use super::ids::ToolCallIds;
use super::message::{ParsedMessage, ParsedToolCall};
use super::options::LoadSpec;
use super::prompt::{self, TurnPrompt};
use super::sampling::{self, SamplerPlan};
use super::shim;
use super::stop::{self, StopScan};
use super::text::Utf8Accumulator;

/// Where deltas go, and how the loop learns the consumer is gone.
pub trait DeltaSink {
    /// Hand one delta on. `false` means the consumer has gone away and the turn
    /// should stop.
    fn emit(&mut self, delta: LlmDelta) -> bool;

    /// Whether the consumer is still there, asked where there is nothing to
    /// emit: between prefill chunks, and before each sampled token.
    fn wanted(&self) -> bool;
}

/// Run one turn to completion, to cancellation, or to failure. `Ok(())` covers
/// a cancelled turn too: a consumer that has gone away is not an error.
pub fn run_turn(
    ready: &Ready,
    state: &mut ContextState,
    req: &LlmRequest,
    sink: &mut dyn DeltaSink,
) -> Result<(), ProviderError> {
    // Before the template runs: it renders text, and libmtmd needs a marker at
    // each image's position in the rendered prompt.
    let split = image_split(ready.mtmd.as_ref(), req)?;
    let transcript = split.as_ref().map(|s| s.messages.as_slice());

    let prompt = match transcript {
        Some(messages) => prompt::turn_prompt_with(req, messages),
        None => prompt::turn_prompt(req),
    }
    .map_err(|err| ProviderError::transport(err.message))?;
    let turn = ready
        .templates
        .apply(&chat_inputs(&prompt))
        .map_err(|err| ProviderError::transport(err.message))?;
    let applied = turn.applied().clone();

    // `parse_special = true`: tokenizing the template's control tokens as
    // literal text would feed the model `<|im_start|>` as characters.
    let tokens = ready
        .model
        .tokenize(&applied.prompt, true, true)
        .map_err(|err| ProviderError::transport(err.message))?;

    // llama.cpp's own answer, not the setting: the context may have been created
    // at a different size than the record now says.
    let n_ctx = state.context.n_ctx() as usize;
    if tokens.len() >= n_ctx {
        return Err(context_window_exceeded(tokens.len(), n_ctx));
    }

    match &split {
        Some(split) => prefill_multimodal(ready, state, &applied.prompt, split)?,
        None => prefill(ready, state, &tokens, sink)?,
    }
    if !sink.wanted() {
        return Ok(());
    }

    let plan = sampling::sampler_plan(req, prompt.thinking);
    let mut sampler =
        shim::SamplerChain::new(Arc::clone(&ready.model), &sampler_spec(&plan, &applied))
            .map_err(|err| ProviderError::transport(err.message))?;

    // The WHOLE prompt, reused prefix included, with `is_generated = false`:
    // this primes the penalty ring buffer and advances the grammar past the
    // prompt. Skipping it is invisible, and the repeat penalty silently stops
    // seeing the conversation it is meant to penalise against.
    for token in &tokens {
        sampler.accept(*token, false);
    }

    generate(
        ready,
        state,
        req,
        &turn,
        &applied,
        &mut sampler,
        sink,
        tokens.len(),
    )
}

/// Lift the images out of the request, or `None` for a text-only turn.
///
/// A turn carrying images against a model with no projector FAILS, naming the
/// reason, rather than answering as though the user had sent text alone: the
/// model would describe something it never saw with nothing reporting a
/// problem. Takes the projector rather than the whole [`Ready`] so that branch
/// is reachable from a test with no model on disk.
fn image_split(
    mtmd: Option<&Arc<Mutex<shim::Mtmd>>>,
    req: &LlmRequest,
) -> Result<Option<super::vision::SplitPrompt>, ProviderError> {
    if super::vision::image_count(req) == 0 {
        return Ok(None);
    }
    let Some(mtmd) = mtmd else {
        return Err(ProviderError::transport(
            "this message contains an image, but the loaded model has no multimodal projector.              Turn on image support and pick a vision model in settings, or send the message              without the image.",
        ));
    };
    // The projector's own marker, never a constant: it is model-family
    // specific, and the wrong one silently detaches the image from its position.
    let marker = {
        let guard = mtmd.blocking_lock();
        guard.marker().to_string()
    };
    super::vision::split(req, &marker)
        .map(Some)
        .map_err(|err| ProviderError::transport(err.message))
}

/// Evaluate an image-bearing prompt into the context through libmtmd.
///
/// Replaces [`prefill`] and resets the KV cache first: the cache will hold
/// image embeddings, which have no token ids, so `state.tokens` cannot
/// represent what it holds. Leaving `state.tokens` empty makes the next turn's
/// `reusable_prefix` see `n_past` disagree, answer 0, and reset.
fn prefill_multimodal(
    ready: &Ready,
    state: &mut ContextState,
    prompt: &str,
    split: &super::vision::SplitPrompt,
) -> Result<(), ProviderError> {
    let Some(mtmd) = ready.mtmd.as_ref() else {
        // Unreachable: `image_split` answers `None` when there is no projector.
        return Err(ProviderError::transport(
            "internal: a multimodal prefill was attempted with no projector loaded",
        ));
    };

    state.context.reset();
    state.tokens.clear();
    state.reused = 0;

    let mut guard = mtmd.blocking_lock();
    guard
        .eval(
            &mut state.context,
            prompt,
            // `add_special` true, matching the text path: the BOS the template
            // expects is added once, here.
            true,
            &split.images,
            ready.spec.context.n_batch as i32,
        )
        .map_err(|err| ProviderError::transport(err.message))?;
    Ok(())
}

/// Decode the part of the prompt the context does not already hold, in
/// `n_batch` chunks, and return how many tokens were reused.
fn prefill(
    ready: &Ready,
    state: &mut ContextState,
    tokens: &[i32],
    sink: &dyn DeltaSink,
) -> Result<(), ProviderError> {
    let reuse = reusable_prefix(state, &ready.spec, tokens);
    if reuse == 0 {
        state.context.reset();
        state.tokens.clear();
    } else {
        state.tokens.truncate(reuse);
    }

    // What the reuse saved. Not observable from the delta stream, so it is
    // recorded here rather than inferred.
    state.reused = reuse;

    let batch = ready.spec.context.n_batch.max(1) as usize;
    for chunk in tokens[reuse..].chunks(batch) {
        if !sink.wanted() {
            return Ok(());
        }
        state
            .context
            .decode(chunk)
            .map_err(|err| ProviderError::transport(err.message))?;
        state.tokens.extend_from_slice(chunk);
    }
    Ok(())
}

/// How much of the cached prompt this turn can keep.
///
/// Extension only: the shim exposes `Context::reset` and no partial removal, so
/// a prefix that DIVERGES costs the whole cache. Appending to a conversation
/// reuses everything; a regenerate or an edit re-prefills.
///
/// The cached tokens are compared together with the [`LoadSpec`] they belong
/// to: reusing a cache across a spec change would condition this turn on
/// another configuration's state with nothing failing.
fn reusable_prefix(state: &ContextState, spec: &LoadSpec, tokens: &[i32]) -> usize {
    if state.spec != *spec {
        return 0;
    }
    // llama.cpp's own count. A disagreement means the cache is not what this
    // module thinks it is, so starting over is the only safe reading.
    let held = usize::try_from(state.context.n_past()).unwrap_or(0);
    if held != state.tokens.len() {
        return 0;
    }
    // Keep one token back: a prompt equal to the cache would otherwise leave
    // nothing to decode, which llama.cpp rejects.
    if tokens.len() <= state.tokens.len() || !tokens.starts_with(&state.tokens) {
        return 0;
    }
    state.tokens.len()
}

/// The token loop.
#[allow(clippy::too_many_arguments)]
fn generate(
    ready: &Ready,
    state: &mut ContextState,
    req: &LlmRequest,
    turn: &shim::ChatTurn,
    applied: &shim::AppliedChat,
    sampler: &mut shim::SamplerChain,
    sink: &mut dyn DeltaSink,
    prompt_len: usize,
) -> Result<(), ProviderError> {
    let n_ctx = state.context.n_ctx() as usize;
    let max_tokens = sampling::max_tokens(req);
    let stops = &applied.additional_stops;

    let mut acc = Utf8Accumulator::new();
    let mut previous = ParsedMessage::default();
    let mut ids = ToolCallIds::new();
    let mut generated: u32 = 0;
    let mut finish = FinishReason::Stop;

    loop {
        if !sink.wanted() {
            return Ok(());
        }
        let token = sampler
            .sample(&mut state.context, -1)
            .map_err(|err| ProviderError::transport(err.message))?;
        // Exactly one advance per token: `common_sampler_sample` deliberately
        // does not accept what it returns, and skipping this is how a
        // grammar-constrained generation quietly stops being constrained.
        sampler.accept(token, true);
        generated = generated.saturating_add(1);

        if ready.model.is_eog(token) {
            break;
        }

        acc.push(
            &ready
                .model
                .token_bytes(token, false)
                .map_err(|err| ProviderError::transport(err.message))?,
        );

        let hit = match stop::scan(acc.text(), stops) {
            StopScan::Hit { keep } => Some(keep),
            StopScan::Continue { safe } => {
                let visible = &acc.text()[..safe];
                if !emit_parse(turn, visible, &mut previous, &mut ids, sink)? {
                    return Ok(());
                }
                None
            }
        };
        if let Some(keep) = hit {
            acc.truncate(keep);
            break;
        }

        if max_tokens.is_some_and(|cap| generated >= cap) {
            finish = FinishReason::Length;
            break;
        }

        state
            .context
            .decode(&[token])
            .map_err(|err| ProviderError::transport(err.message))?;
        state.tokens.push(token);

        if state.tokens.len() >= n_ctx {
            finish = FinishReason::Length;
            break;
        }
    }

    // The final parse completes a tool call: a partial parse leaves a call it
    // has not finished reading out of the message entirely.
    let final_message = parse(turn, acc.text(), false)?;
    for delta in diff_messages(&previous, &final_message, &mut ids)
        .map_err(|err| ProviderError::decode(err.0))?
    {
        if !sink.emit(delta) {
            return Ok(());
        }
    }

    // Its own delta, carrying nothing else, as the SSE path produces.
    let finish_reason = if final_message.tool_calls.is_empty() {
        finish.as_str()
    } else {
        "tool_calls"
    };
    if !sink.emit(LlmDelta {
        finish_reason: Some(finish_reason.to_string()),
        ..LlmDelta::default()
    }) {
        return Ok(());
    }

    // The full prompt length, reused prefix included: what the consumer's token
    // accounting expects.
    let prompt_count = i64::try_from(prompt_len).unwrap_or(i64::MAX);
    let completion = i64::from(generated);
    sink.emit(LlmDelta {
        usage: Some(crate::contract::api::TokenUsage {
            prompt: prompt_count,
            completion,
            total: prompt_count.saturating_add(completion),
        }),
        ..LlmDelta::default()
    });
    Ok(())
}

#[derive(Clone, Copy)]
enum FinishReason {
    Stop,
    Length,
}

impl FinishReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::Stop => "stop",
            Self::Length => "length",
        }
    }
}

/// Parse the visible text, diff it against the last parse, and emit what moved.
///
/// Returns `false` when the consumer has gone.
fn emit_parse(
    turn: &shim::ChatTurn,
    visible: &str,
    previous: &mut ParsedMessage,
    ids: &mut ToolCallIds,
    sink: &mut dyn DeltaSink,
) -> Result<bool, ProviderError> {
    let current = parse(turn, visible, true)?;
    let deltas =
        diff_messages(previous, &current, ids).map_err(|err| ProviderError::decode(err.0))?;
    *previous = current;
    for delta in deltas {
        if !sink.emit(delta) {
            return Ok(false);
        }
    }
    Ok(true)
}

fn parse(turn: &shim::ChatTurn, text: &str, partial: bool) -> Result<ParsedMessage, ProviderError> {
    let parsed = if partial {
        turn.parse_partial(text)
    } else {
        turn.parse_final(text)
    }
    .map_err(|err| ProviderError::decode(err.message))?;
    Ok(parsed_message(parsed))
}

/// llama.cpp's assistant message in this module's own shape.
fn parsed_message(message: shim::AssistantMessage) -> ParsedMessage {
    ParsedMessage {
        role: message.role,
        content: message.content,
        reasoning_content: message.reasoning_content,
        tool_calls: message
            .tool_calls
            .into_iter()
            .map(|call| ParsedToolCall {
                name: call.function.name,
                arguments: call.function.arguments,
                // Absent is the normal case: llama.cpp's id generator lives in
                // the server we do not build. `ids` fills one in, stably per
                // index.
                id: call.id.unwrap_or_default(),
            })
            .collect(),
    }
}

/// [`TurnPrompt`] to the shim's chat inputs.
fn chat_inputs(prompt: &TurnPrompt) -> shim::ChatInputs {
    shim::ChatInputs {
        messages: prompt.messages.clone(),
        tools: prompt.tools.clone(),
        tool_choice: "auto".to_string(),
        add_generation_prompt: true,
        // Must be SENT as false: a thinking-by-default model keeps thinking
        // unless the template is told not to.
        enable_thinking: prompt.thinking.enabled,
        parallel_tool_calls: false,
        // Pinned so thoughts always arrive in `reasoning_content`, whatever
        // llama.cpp's default becomes.
        reasoning_format: shim::ReasoningFormat::Deepseek,
    }
}

/// [`SamplerPlan`] plus the applied turn's grammar to the shim's sampler spec.
///
/// An EMPTY grammar means no grammar sampler at all. A template-produced
/// grammar must be [`shim::GrammarKind::ToolCalls`], which is what makes
/// llama.cpp prefill the generation prompt into the grammar sampler; without
/// that a tool-capable template constrains from the first token and forces a
/// tool call on every turn.
///
/// `penalty_last_n` is deliberately unset, so llama.cpp's own default of 64
/// applies rather than a copy of it that has to be kept in sync.
fn sampler_spec(plan: &SamplerPlan, applied: &shim::AppliedChat) -> shim::SamplerSpec {
    let grammar = (!applied.grammar.is_empty()).then(|| applied.grammar.clone());
    shim::SamplerSpec {
        samplers: plan.samplers.clone(),
        temperature: plan.temperature,
        top_k: plan.top_k,
        top_p: plan.top_p,
        min_p: plan.min_p,
        penalty_repeat: plan.penalty_repeat,
        penalty_present: plan.penalty_present,
        dry_multiplier: plan.dry_multiplier,
        grammar_kind: grammar.as_ref().map(|_| shim::GrammarKind::ToolCalls),
        grammar_lazy: applied.grammar_lazy,
        grammar_triggers: (!applied.grammar_triggers.is_empty()).then(|| {
            applied
                .grammar_triggers
                .iter()
                .map(|trigger| shim::SamplerTrigger {
                    kind: trigger.kind,
                    value: trigger.value.clone(),
                    token: trigger.token,
                })
                .collect()
        }),
        preserved_tokens: (!applied.preserved_tokens.is_empty())
            .then(|| applied.preserved_tokens.clone()),
        generation_prompt: (!applied.generation_prompt.is_empty())
            .then(|| applied.generation_prompt.clone()),
        // Built whenever the template carries the tags, whatever the budget:
        // the budget sampler also suppresses a think block for a lazy grammar,
        // and `-1` is how "no cap" is spelled to llama.cpp.
        reasoning_budget: (!applied.thinking_end_tags.is_empty()).then(|| shim::ReasoningBudget {
            tokens: plan.reasoning_budget_tokens,
            start_tag: applied.thinking_start_tag.clone(),
            end_tags: applied.thinking_end_tags.clone(),
            message: String::new(),
        }),
        grammar,
        ..shim::SamplerSpec::default()
    }
}

/// The refusal the client has a path for. `status` is `None` (no HTTP here), so
/// `code` carries the classification `classify_provider_error` reads.
fn context_window_exceeded(tokens: usize, n_ctx: usize) -> ProviderError {
    ProviderError {
        kind: crate::llm::ProviderErrorKind::Transport,
        status: None,
        code: Some("context_length_exceeded".to_string()),
        message: format!(
            "the prompt is {tokens} tokens and the context window is {n_ctx}. \
             Shorten the conversation or raise the context size in settings."
        ),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;

    use crate::llm::{ChatMessage, LlmEndpointConfig};

    use super::super::engine::LocalEngine;
    use super::*;

    /// Collects deltas, and can walk away mid-turn.
    struct VecSink {
        deltas: Vec<LlmDelta>,
        /// Stop wanting anything after this many deltas: what a dropped stream
        /// looks like from in here.
        leave_after: Option<usize>,
    }

    impl VecSink {
        fn new() -> Self {
            Self {
                deltas: Vec::new(),
                leave_after: None,
            }
        }

        fn leaving_after(n: usize) -> Self {
            Self {
                deltas: Vec::new(),
                leave_after: Some(n),
            }
        }

        fn content(&self) -> String {
            self.deltas
                .iter()
                .filter_map(|d| d.content_delta.clone())
                .collect()
        }
    }

    impl DeltaSink for VecSink {
        fn emit(&mut self, delta: LlmDelta) -> bool {
            self.deltas.push(delta);
            self.wanted()
        }

        fn wanted(&self) -> bool {
            self.leave_after.is_none_or(|n| self.deltas.len() < n)
        }
    }

    fn test_gguf() -> Option<PathBuf> {
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("models/test.gguf");
        p.is_file().then_some(p)
    }

    const NO_GGUF: &str = "SKIP: packages/loca-core-rs/models/test.gguf is absent, so the \
        token-loop tests did not run. See docs/rust-port/packages/N1/status.md for the one \
        command that fetches it.";

    /// A loaded engine over the tiny model, through the production path.
    async fn loaded(
        context_size: u32,
    ) -> Option<(LocalEngine, crate::transport::state::AppContext)> {
        let model = test_gguf()?;
        let ctx = crate::transport::testing::context();
        let engine = LocalEngine::with_install_dir(
            ctx.settings.clone(),
            (*ctx.paths).clone(),
            PathBuf::from(env!("LOCA_LLAMA_BACKEND_DIR")),
        );
        let settings = match json!({
            "llm.provider": "local",
            "llm.modelPath": model.to_string_lossy(),
            "llm.contextSize": context_size,
        }) {
            serde_json::Value::Object(map) => map,
            _ => panic!("a record is an object"),
        };
        engine.ensure_loaded(&settings).await.unwrap();
        Some((engine, ctx))
    }

    /// A request against the local endpoint, capped so a test is quick.
    fn request(cap: u32, messages: Vec<ChatMessage>) -> LlmRequest {
        LlmRequest::new(
            LlmEndpointConfig {
                is_local_provider: true,
                llama_body_extras: true,
                max_tokens: Some(cap),
                // Argmax, so two runs of the same prompt are comparable.
                temperature: Some(0.0),
                ..LlmEndpointConfig::default()
            },
            messages,
        )
    }

    /// The Qwen3.5 template applied with one tool and no weights anywhere:
    /// `ChatTemplates::from_source` accepts a null model, so the whole
    /// apply-parse-diff chain runs against a real grammar and parser.
    fn qwen_turn() -> shim::ChatTurn {
        let path =
            PathBuf::from(env!("LOCA_LLAMA_VENDOR_DIR")).join("models/templates/Qwen3.5-4B.jinja");
        let source = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
        let templates = Arc::new(shim::ChatTemplates::from_source(&source, "<s>", "</s>").unwrap());
        let inputs = shim::ChatInputs::new(json!([
            { "role": "user", "content": "What is the weather in Jakarta?" }
        ]))
        .with_tools(json!([{
            "type": "function",
            "function": {
                "name": "get_weather",
                "description": "Current weather for a city",
                "parameters": {
                    "type": "object",
                    "properties": { "city": { "type": "string" } },
                    "required": ["city"],
                },
            },
        }]));
        templates.apply(&inputs).unwrap()
    }

    #[test]
    fn streaming_a_tool_call_through_the_real_parser_emits_one_stable_id_and_one_name() {
        // The `tool` message that follows references the call by id, so an id
        // that changes between fragments (or is absent, which is what llama.cpp
        // returns here) renders as an orphaned response.
        let turn = qwen_turn();
        let output = concat!(
            "The user is asking about weather. I should call the tool.\n</think>\n\n",
            "<tool_call>\n<function=get_weather>\n<parameter=city>\nJakarta\n",
            "</parameter>\n</function>\n</tool_call>",
        );

        let mut previous = ParsedMessage::default();
        let mut ids = ToolCallIds::new();
        let mut sink = VecSink::new();
        // One pass per character: strictly harder than a token loop, since
        // every intermediate parse state the real loop can reach is visited.
        for (at, ch) in output.char_indices() {
            let end = at + ch.len_utf8();
            emit_parse(&turn, &output[..end], &mut previous, &mut ids, &mut sink).unwrap();
        }
        let final_message = parse(&turn, output, false).unwrap();
        for delta in diff_messages(&previous, &final_message, &mut ids).unwrap() {
            sink.emit(delta);
        }

        assert!(
            sink.deltas.iter().all(|d| !d.is_empty()),
            "an empty delta reads as a token that was never generated",
        );

        let fragments: Vec<_> = sink
            .deltas
            .iter()
            .flat_map(|d| d.tool_calls.clone())
            .collect();
        assert!(!fragments.is_empty(), "the tool call never streamed");

        let ids_seen: Vec<_> = fragments.iter().filter_map(|f| f.id.clone()).collect();
        assert_eq!(
            ids_seen.len(),
            1,
            "the id is sent once, with the first fragment"
        );
        assert!(
            !ids_seen[0].is_empty(),
            "llama.cpp sends no id, so one is minted"
        );

        let names: Vec<_> = fragments.iter().filter_map(|f| f.name.clone()).collect();
        assert_eq!(names, vec!["get_weather".to_string()]);

        let arguments: String = fragments
            .iter()
            .filter_map(|f| f.arguments_delta.clone())
            .collect();
        assert_eq!(
            arguments, final_message.tool_calls[0].arguments,
            "the fragments must reassemble into exactly the parsed arguments",
        );
        assert!(arguments.contains("Jakarta"), "{arguments}");

        // Reasoning arrives on its own channel rather than inside the answer.
        let reasoning: String = sink
            .deltas
            .iter()
            .filter_map(|d| d.reasoning_delta.clone())
            .collect();
        assert!(reasoning.contains("call the tool"), "{reasoning:?}");
        assert!(
            !sink.content().contains("call the tool"),
            "the model's thoughts leaked into the answer",
        );
    }

    #[tokio::test]
    async fn a_turn_emits_content_then_one_finish_reason_then_usage_and_nothing_empty() {
        // The shape of the stream: `Streaming` counts deltas, keys on the finish
        // reason arriving alone, and reads usage once.
        let Some((engine, _ctx)) = loaded(512).await else {
            eprintln!("{NO_GGUF}");
            return;
        };
        let ready = engine.ready().await.expect("the model loaded");
        let mut slot = ContextState::new(Arc::clone(&ready.model), &ready.spec).unwrap();
        let mut sink = VecSink::new();

        run_turn(
            &ready,
            &mut slot,
            &request(8, vec![ChatMessage::user("hi")]),
            &mut sink,
        )
        .unwrap();

        assert!(
            sink.deltas.iter().all(|d| !d.is_empty()),
            "an empty delta reads as a token that was never generated",
        );
        let finishes: Vec<_> = sink
            .deltas
            .iter()
            .filter_map(|d| d.finish_reason.clone())
            .collect();
        assert_eq!(finishes, vec!["length".to_string()], "{:?}", sink.deltas);

        let finish = sink
            .deltas
            .iter()
            .find(|d| d.finish_reason.is_some())
            .unwrap();
        assert!(
            finish.content_delta.is_none() && finish.usage.is_none(),
            "the finish reason arrives alone",
        );

        // Ordering, not merely presence: a finish reason before the last content
        // delta truncates the answer for a consumer that stops on it.
        let finish_at = sink
            .deltas
            .iter()
            .position(|d| d.finish_reason.is_some())
            .unwrap();
        let last_content = sink
            .deltas
            .iter()
            .rposition(|d| d.content_delta.is_some())
            .unwrap();
        assert!(
            last_content < finish_at,
            "the finish reason arrived before the text it finishes",
        );

        let last = sink.deltas.last().expect("a turn emits something");
        let usage = last.usage.as_ref().expect("usage is last");
        assert_eq!(usage.completion, 8, "the cap is what was generated");
        assert!(usage.prompt > 0);
        assert_eq!(usage.total, usage.prompt + usage.completion);
        assert!(!sink.content().is_empty(), "a turn produces text");
    }

    #[tokio::test]
    async fn the_same_prompt_at_temperature_zero_generates_the_same_text_twice() {
        // Determinism makes every other assertion here meaningful: a sampler
        // wired up wrongly shows up as output that will not reproduce.
        let Some((engine, _ctx)) = loaded(512).await else {
            eprintln!("{NO_GGUF}");
            return;
        };
        let ready = engine.ready().await.expect("the model loaded");
        let req = request(8, vec![ChatMessage::user("once upon a time")]);

        let mut first = VecSink::new();
        let mut slot = ContextState::new(Arc::clone(&ready.model), &ready.spec).unwrap();
        run_turn(&ready, &mut slot, &req, &mut first).unwrap();

        let mut second = VecSink::new();
        let mut slot = ContextState::new(Arc::clone(&ready.model), &ready.spec).unwrap();
        run_turn(&ready, &mut slot, &req, &mut second).unwrap();

        assert_eq!(first.content(), second.content());
    }

    #[tokio::test]
    async fn a_consumer_that_walks_away_stops_the_turn_rather_than_running_it_out() {
        // `spawn_blocking` cannot be cancelled by the runtime, so a loop that
        // did not notice would hold the one context for its full length.
        let Some((engine, _ctx)) = loaded(512).await else {
            eprintln!("{NO_GGUF}");
            return;
        };
        let ready = engine.ready().await.expect("the model loaded");
        let mut slot = ContextState::new(Arc::clone(&ready.model), &ready.spec).unwrap();
        let mut sink = VecSink::leaving_after(2);

        run_turn(
            &ready,
            &mut slot,
            &request(64, vec![ChatMessage::user("tell me a long story")]),
            &mut sink,
        )
        .unwrap();

        assert!(
            sink.deltas.len() <= 3,
            "the loop kept generating for a consumer that had gone: {} deltas",
            sink.deltas.len(),
        );
        assert!(
            sink.deltas.iter().all(|d| d.finish_reason.is_none()),
            "a cancelled turn has no finish reason: nobody is listening for one",
        );
    }

    #[tokio::test]
    async fn a_prompt_longer_than_the_window_is_refused_before_anything_is_decoded() {
        // A refusal rather than a truncation: silently dropping the front of a
        // conversation answers a question the user did not ask.
        let Some((engine, _ctx)) = loaded(64).await else {
            eprintln!("{NO_GGUF}");
            return;
        };
        let ready = engine.ready().await.expect("the model loaded");
        let mut slot = ContextState::new(Arc::clone(&ready.model), &ready.spec).unwrap();
        let mut sink = VecSink::new();

        let long = "word ".repeat(400);
        let err = run_turn(
            &ready,
            &mut slot,
            &request(8, vec![ChatMessage::user(long)]),
            &mut sink,
        )
        .expect_err("a prompt past the window cannot be answered");

        assert_eq!(err.code.as_deref(), Some("context_length_exceeded"));
        assert_eq!(
            crate::llm::classify_provider_error(&err).code,
            crate::error::ErrorCode::ContextWindowExceeded,
            "the code has to survive D3's classifier, not merely be set",
        );
        assert!(sink.deltas.is_empty(), "nothing was generated");
    }

    #[tokio::test]
    async fn a_second_turn_that_extends_the_first_reuses_the_prompt_it_already_decoded() {
        // Without reuse every turn re-prefills the whole transcript, which on
        // CPU is seconds per turn and nothing in the delta stream would say so.
        let Some((engine, _ctx)) = loaded(512).await else {
            eprintln!("{NO_GGUF}");
            return;
        };
        let ready = engine.ready().await.expect("the model loaded");
        let mut slot = ContextState::new(Arc::clone(&ready.model), &ready.spec).unwrap();

        let mut first = VecSink::new();
        run_turn(
            &ready,
            &mut slot,
            &request(8, vec![ChatMessage::user("hello")]),
            &mut first,
        )
        .unwrap();
        assert_eq!(slot.reused, 0, "the first turn has nothing to reuse");

        let mut second = VecSink::new();
        run_turn(
            &ready,
            &mut slot,
            &request(
                8,
                vec![
                    ChatMessage::user("hello"),
                    ChatMessage::assistant(first.content()),
                    ChatMessage::user("and then?"),
                ],
            ),
            &mut second,
        )
        .unwrap();

        assert!(
            slot.reused > 0,
            "the second turn re-decoded the whole transcript",
        );

        // Usage reports the WHOLE prompt, reused prefix included. Counting only
        // what was decoded would make a long conversation look cheaper.
        let usage = second
            .deltas
            .last()
            .and_then(|d| d.usage.clone())
            .expect("usage is last");
        assert!(
            usage.prompt > i64::try_from(slot.reused).unwrap(),
            "usage counted only the tokens this turn decoded: {} against {} reused",
            usage.prompt,
            slot.reused,
        );
    }

    #[tokio::test]
    async fn a_context_belonging_to_another_configuration_is_never_reused() {
        // Conditioning a turn on another configuration's KV state does not
        // fail, it produces confident wrong output. The second of two guards,
        // and the one that survives someone hand-building a context.
        let Some((engine, _ctx)) = loaded(512).await else {
            eprintln!("{NO_GGUF}");
            return;
        };
        let ready = engine.ready().await.expect("the model loaded");
        let mut slot = ContextState::new(Arc::clone(&ready.model), &ready.spec).unwrap();

        let mut first = VecSink::new();
        run_turn(
            &ready,
            &mut slot,
            &request(8, vec![ChatMessage::user("hello")]),
            &mut first,
        )
        .unwrap();

        // A pair the test above proves reuses, with the cache relabelled as
        // another configuration's, so a zero can only come from the spec check.
        slot.spec.n_gpu_layers += 1;
        let mut second = VecSink::new();
        run_turn(
            &ready,
            &mut slot,
            &request(
                8,
                vec![
                    ChatMessage::user("hello"),
                    ChatMessage::assistant(first.content()),
                    ChatMessage::user("and then?"),
                ],
            ),
            &mut second,
        )
        .unwrap();
        assert_eq!(
            slot.reused, 0,
            "a cache from another configuration was reused",
        );
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod vision_routing_tests {
    use super::*;
    use crate::llm::{ChatMessage, ContentPart, ImageUrl, LlmEndpointConfig, UserContent};

    fn with_image() -> LlmRequest {
        LlmRequest::new(
            LlmEndpointConfig::default(),
            vec![ChatMessage::User {
                content: UserContent::Parts(vec![
                    ContentPart::Text {
                        text: "what is in this picture?".into(),
                    },
                    ContentPart::ImageUrl {
                        image_url: ImageUrl {
                            // Any bytes; nothing decodes them on this path.
                            url: "data:image/png;base64,aGVsbG8=".into(),
                        },
                    },
                ]),
            }],
        )
    }

    #[test]
    fn an_image_turn_against_a_model_with_no_projector_is_refused() {
        // The failure this prevents is silent: an image part that does not
        // survive into the prompt leaves the model answering about a picture it
        // was never shown, and a wrong answer still looks like an answer.
        // `None` is a model loaded without a projector.
        let err = image_split(None, &with_image())
            .expect_err("an image turn with no projector must not be silently accepted");
        assert!(
            err.to_string().contains("multimodal projector"),
            "the refusal must say why and what to do about it, got: {err}",
        );
    }

    #[test]
    fn a_text_turn_takes_the_text_path_and_never_asks_for_a_projector() {
        // The other half, so the refusal above cannot be "always refuse": an
        // ordinary turn reaches the plain decode path.
        let text = LlmRequest::new(
            LlmEndpointConfig::default(),
            vec![ChatMessage::user("no pictures here")],
        );
        assert!(
            image_split(None, &text).unwrap().is_none(),
            "a text-only turn asked for the multimodal path",
        );
    }
}
