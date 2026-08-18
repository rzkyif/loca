//! One turn: prompt, prefill, sample, parse, emit.
//!
//! Blocking from top to bottom. llama.cpp decode is CPU work and a
//! [`shim::Context`] is `Send` but never `Sync`, so a turn owns the context for
//! its whole duration and runs on a dedicated blocking thread that
//! [`super::runtime`] provides. Nothing here is `async`, and nothing here knows
//! about channels: deltas go to a [`DeltaSink`], which is also what makes the
//! loop testable against a `Vec`.
//!
//! # Cancellation is the sink saying no
//!
//! A dropped [`crate::llm::DeltaStream`] closes the channel, and the sink then
//! answers `false`. That is the ONLY cancellation signal, deliberately: a second
//! one (a token, a flag) could disagree with the first, and D3's scheduler
//! already ties the turn's permit to the stream's lifetime. The obligation this
//! module carries is that the answer is CHECKED often enough, which is why
//! prefill is chunked rather than decoded in one call: a single-shot prefill of
//! a full context would hold the context for seconds after the user pressed
//! stop, and the next turn would queue behind it invisibly.
//!
//! # What is parsed, and what is emitted
//!
//! Every token, the accumulated text is parsed with `is_partial = true` and
//! diffed against the previous parse ([`super::diff`], a port of upstream's
//! `common_chat_msg_diff::compute_diffs`). That is what makes tool-call
//! arguments and reasoning stream as they are produced rather than arriving in
//! one lump at the end, which is what the client renders live.
//!
//! The text handed to the parser is the STOP-SAFE prefix, never the raw
//! accumulation: a half-emitted stop marker must not reach the parser, be
//! emitted as content, and then have to be retracted when it completes.

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

/// Run one turn to completion, to cancellation, or to failure.
///
/// `Ok(())` covers both a finished generation and a cancelled one: a consumer
/// that has gone away is not an error, and there is nobody left to tell.
pub fn run_turn(
    ready: &Ready,
    state: &mut ContextState,
    req: &LlmRequest,
    sink: &mut dyn DeltaSink,
) -> Result<(), ProviderError> {
    // Images are lifted out BEFORE the template runs, because the template
    // renders text and libmtmd needs a marker sitting at each image's position
    // in the rendered prompt. `None` is every ordinary turn.
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

    // `parse_special = true`: the rendered prompt is full of control tokens the
    // template placed there, and tokenizing them as literal text would feed the
    // model the characters `<|im_start|>` instead of the token that means it.
    let tokens = ready
        .model
        .tokenize(&applied.prompt, true, true)
        .map_err(|err| ProviderError::transport(err.message))?;

    // llama.cpp's own answer, not the setting: the context may have been created
    // with a different size than the record now says.
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

    // The WHOLE prompt, including the part that was already in the KV cache, and
    // with `is_generated = false`. This is what primes the penalty ring buffer
    // and advances the grammar past the prompt, and upstream does exactly the
    // same in `server_slot::init_sampler` (`server-context.cpp:376-383`).
    // Skipping it is invisible: generation still works, and the repeat penalty
    // silently stops seeing the conversation it is supposed to penalise against.
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
/// The refusal here is deliberate and is the point of the whole change: a turn
/// that carries images against a model with no projector loaded FAILS, naming
/// the reason, rather than silently answering as though the user had sent text
/// alone. That silent answer is what the port did before libmtmd was linked, and
/// it is the worst available outcome: the model describes something it never
/// saw, fluently, with nothing anywhere reporting a problem.
/// Takes the projector rather than the whole [`Ready`] so this is reachable from
/// a test with `None` and no model on disk. The refusal below is the branch that
/// matters and it needs no llama.cpp to exercise; requiring a `Ready` would have
/// made the one dangerous path testable only with a real vision model, which is
/// how a branch like this ends up with no test at all.
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
    // The marker is the projector's own, never a constant: it is model-family
    // specific, and the wrong one leaves the image attached to no position in
    // the prompt rather than failing.
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
/// Replaces [`prefill`] for the turn rather than extending it, and the KV cache
/// is always reset first. Both follow from the same fact: the cache will hold
/// IMAGE EMBEDDINGS, which have no token ids. `state.tokens` is the record of
/// what the cache holds and it cannot represent them, so any prefix reuse
/// computed from it afterwards would be a claim about content that is not there.
///
/// Leaving `state.tokens` empty is what makes that safe rather than merely
/// avoided: the next turn's `reusable_prefix` finds `n_past` disagreeing with an
/// empty token list, answers 0, and resets. The cache self-heals instead of
/// being trusted.
fn prefill_multimodal(
    ready: &Ready,
    state: &mut ContextState,
    prompt: &str,
    split: &super::vision::SplitPrompt,
) -> Result<(), ProviderError> {
    let Some(mtmd) = ready.mtmd.as_ref() else {
        // Unreachable: `image_split` returns `None` when there is no projector,
        // and this is only called for a `Some`. Stated rather than unwrapped.
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
            // `add_special` true, matching the text path's tokenize: the BOS the
            // template expects is added once, here.
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

    // What the reuse actually saved, for the turn that follows and for the test
    // that proves the second turn decodes less than the first. Production state
    // carrying a test-visible number, rather than a test driving its own copy of
    // this loop (D-084).
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
/// **Extension only**, which is Q3's approved fallback: the shim exposes
/// `Context::reset` and no partial removal, so a prefix that DIVERGES cannot be
/// trimmed out of the KV cache and the whole cache has to go. Appending to a
/// conversation (the common case) reuses everything; a regenerate or an edit
/// re-prefills. Adding `llama_memory_seq_rm` to the shim would close that gap
/// and is a performance change rather than a correctness one.
///
/// The cached tokens are compared together with the [`LoadSpec`] they belong to.
/// Reusing a cache across a spec change would condition this turn on another
/// configuration's state, and the output of that is fluent, confident and wrong
/// with nothing failing.
fn reusable_prefix(state: &ContextState, spec: &LoadSpec, tokens: &[i32]) -> usize {
    if state.spec != *spec {
        return 0;
    }
    // llama.cpp's own count, not a Rust shadow of it. If the two disagree the
    // cache is not what this module thinks it is, and starting over is the only
    // safe reading.
    let held = usize::try_from(state.context.n_past()).unwrap_or(0);
    if held != state.tokens.len() {
        return 0;
    }
    // A prompt that only EXTENDS what is held. The last cached token is left in
    // place rather than re-decoded, so a prompt equal to the cache would leave
    // nothing to decode at all, which llama.cpp rejects: keep one token back.
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
        // Exactly one advance per token, and it belongs here rather than inside
        // `sample`: `common_sampler_sample` deliberately does not accept what it
        // returns. Skipping this is how a grammar-constrained generation quietly
        // stops being constrained.
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

    // The final parse is the one that completes a tool call: a partial parse
    // leaves a call it has not finished reading out of the message entirely.
    let final_message = parse(turn, acc.text(), false)?;
    for delta in diff_messages(&previous, &final_message, &mut ids)
        .map_err(|err| ProviderError::decode(err.0))?
    {
        if !sink.emit(delta) {
            return Ok(());
        }
    }

    // Its own delta, carrying nothing else, which is what the SSE path produces
    // and what the consumer counts on.
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

    // The full prompt length including anything reused, because that is what
    // `llama-server` reports and what the consumer's token accounting expects.
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
                // Absent is the NORMAL case: llama.cpp's id-generating line is
                // commented out and its generator lives in the server we do not
                // build, so most templates hand back no id at all. `ids` fills
                // one in, stably per index.
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
        // A real field rather than an omission, and it must be SENT as false:
        // a thinking-by-default model keeps thinking unless the template is told
        // not to, and a test covering only the "on" case passes either way.
        enable_thinking: prompt.thinking.enabled,
        parallel_tool_calls: false,
        // Pinned, exactly as `--reasoning-format deepseek` is pinned today, so
        // the thoughts always arrive in `reasoning_content` whatever llama.cpp's
        // default becomes.
        reasoning_format: shim::ReasoningFormat::Deepseek,
    }
}

/// [`SamplerPlan`] plus the applied turn's grammar to the shim's sampler spec.
///
/// The grammar half is pass-through data, and two decisions ride with it:
///
/// - an EMPTY grammar means no grammar sampler at all, which is the ordinary
///   case for a turn with no tools;
/// - a template-produced grammar is [`shim::GrammarKind::ToolCalls`], which is
///   what makes llama.cpp prefill the generation prompt into the grammar
///   sampler. Without that prefill a tool-capable template constrains from the
///   first token, and the model is forced to emit a tool call on every turn.
///
/// `penalty_last_n` is deliberately absent: llama.cpp's own default is 64
/// (`common/common.h:238`) and `llama-server` is launched without
/// `--repeat-last-n` today, so leaving it unset reproduces what ships. Setting
/// it to 64 here would say the same thing in a place that has to be kept in
/// sync with someone else's default.
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
        // Built whenever the template carries the tags, whatever the budget is.
        // The budget sampler is also what suppresses a think block for a lazy
        // grammar, so omitting it when there is no cap would silently drop that
        // suppression; `-1` is how "no cap" is spelled to llama.cpp.
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

/// The refusal the client has a path for.
///
/// `status` is `None` (there is no HTTP here), so `code` carries the
/// classification: `classify_provider_error` maps `context_length_exceeded` to
/// `ErrorCode::ContextWindowExceeded` before it ever looks at the prose.
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
        /// Stop wanting anything after this many deltas, which is what a dropped
        /// stream looks like from in here.
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

    /// The Qwen3.5 template, applied with one tool, and no weights anywhere.
    ///
    /// `ChatTemplates::from_source` accepts a null model, so the whole
    /// apply-parse-diff chain runs against a REAL tool-calling grammar and a real
    /// PEG parser without a GGUF. The tiny stories model cannot exercise any of
    /// this: it has no chat template and no tool support.
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
        // The defect this guards is the one D3's `ToolCallRef` describes: the
        // `tool` message that follows references the call by id, and an id that
        // changes between fragments (or is absent, which is what llama.cpp
        // returns for this template) renders as an orphaned response and garbles
        // every hop after it.
        let turn = qwen_turn();
        let output = concat!(
            "The user is asking about weather. I should call the tool.\n</think>\n\n",
            "<tool_call>\n<function=get_weather>\n<parameter=city>\nJakarta\n",
            "</parameter>\n</function>\n</tool_call>",
        );

        let mut previous = ParsedMessage::default();
        let mut ids = ToolCallIds::new();
        let mut sink = VecSink::new();
        // One delta pass per character, which is more granular than a token loop
        // and therefore strictly harder: every intermediate parse state the real
        // loop can reach is visited here.
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

        // And the reasoning arrived on its own channel rather than inside the
        // answer, which is what `--reasoning-format deepseek` buys.
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
        // The shape of the stream, which is the whole of what this engine owes
        // its consumer: D3's `Streaming` counts deltas, keys on the finish
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

        // Ordering, not merely presence: a finish reason emitted before the last
        // content delta would truncate the answer for a consumer that stops
        // reading when it sees one.
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
        // Determinism is what makes every other assertion here meaningful: a
        // sampler wired up wrongly (the double-advance N1's redesign removed, a
        // missing `accept`) shows up as output that will not reproduce.
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
        // The D15-class obligation. `spawn_blocking` cannot be cancelled by the
        // runtime, so if the loop did not notice, a cancelled turn would hold the
        // one context for its full length and the next turn would queue behind it
        // invisibly.
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
        // A real error the client has a path for, and it must arrive as a
        // refusal rather than as a truncation: silently dropping the front of a
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
        // What the llama-server slot was quietly paying for. Without it every
        // turn re-prefills the whole transcript, which on CPU is seconds per turn
        // and grows with the session, and nothing in the delta stream would say
        // so.
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

        // Usage reports the WHOLE prompt, reused prefix included, which is what
        // `llama-server` reports and what the token accounting above this
        // expects. Counting only what was decoded would make a long conversation
        // look cheaper every turn.
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
        // The cache is keyed on the spec because the alternative is conditioning
        // a turn on another configuration's KV state, and that does not fail: it
        // produces fluent, confident, wrong output. The engine also replaces a
        // stale context before the turn starts, so this is the second of two
        // guards, and the one that survives someone hand-building a context.
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

        // A second turn that WOULD reuse: the test above proves this exact pair
        // reuses. The only difference here is that the cache is now labelled as
        // another configuration's, so a zero can only come from the spec check.
        //
        // Written the obvious way first (the same request twice), where the
        // reuse was zero anyway because the cache is longer than the prompt, and
        // the test passed with the spec check deleted.
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
        // **The failure this exists to prevent is a SILENT one.** Before libmtmd
        // was linked, an image part simply did not survive into the rendered
        // prompt, so the model answered a question about a picture it had never
        // been shown: fluently, confidently, with nothing reporting a problem.
        // Refusing is the only honest answer, and it is the branch that would go
        // unnoticed if it regressed, because a wrong answer still looks like an
        // answer.
        //
        // `None` is a model loaded without a projector, which is every text
        // model and every vision model with `llm.supportImages` off.
        let err = image_split(None, &with_image())
            .expect_err("an image turn with no projector must not be silently accepted");
        assert!(
            err.to_string().contains("multimodal projector"),
            "the refusal must say why and what to do about it, got: {err}",
        );
    }

    #[test]
    fn a_text_turn_takes_the_text_path_and_never_asks_for_a_projector() {
        // The other half, and it is what stops the refusal above from being
        // implemented as "always refuse": an ordinary turn must reach the plain
        // decode path with no projector involved, which is what `None` here
        // means to the caller.
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
