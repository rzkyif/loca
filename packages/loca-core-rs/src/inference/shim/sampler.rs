//! The sampler, built from a declarative spec.
//!
//! # Why this wraps `common_sampler` rather than assembling a chain (D-136)
//!
//! The first version of this file hand-assembled a `llama_sampler_chain` from
//! the raw `llama_sampler_init_*` calls. That cannot express four things the
//! product already ships:
//!
//! - a **lazy grammar with triggers**, so tool-call constraint starts when the
//!   model actually begins a tool call rather than at the first token,
//! - the **thinking-token budget**, which forces the closing think tag when a
//!   reasoning block runs long,
//! - the user-configured **sampler order** (`llm.samplers`), which is a setting
//!   and therefore has to be a list rather than a fixed sequence,
//! - **DRY**.
//!
//! All four are in `common/sampling.cpp`, inside the `common` target this crate
//! already links, which is the same argument that put `common/chat.cpp` here.
//! Two work packages reached that conclusion independently.
//!
//! It also removes two defects by construction rather than by patch:
//!
//! - `common_sampler_init` **throws** on a grammar it cannot parse. The
//!   hand-rolled chain added `llama_sampler_init_grammar`'s unchecked null
//!   return, so [`SamplerChain::new`] returned `Ok` and the first
//!   [`SamplerChain::sample`] was a SIGSEGV reachable from safe Rust.
//! - `common_sampler_sample` does **not** accept the token it returns, so
//!   [`SamplerChain::accept`] is now the single advance. `llama_sampler_sample`
//!   accepts internally, which made the old pairing a double-advance of the
//!   grammar stack and the penalty ring that no test could see.
//!
//! # What is still N2's
//!
//! Which samplers, in what order, with what parameters. This builds what it is
//! told to build and takes no position on generation policy; it only refuses
//! inputs that llama.cpp would `abort()` on.

use std::ptr::NonNull;
use std::sync::Arc;

use serde::Serialize;

use crate::error::{internal, Result};

use super::ffi;
use super::model::{Context, Model};
use super::owned::{c_string, last_error, ShimString};

/// One of llama.cpp's samplers, for a chain named in Rust code.
///
/// [`SamplerSpec::samplers`] takes plain strings, because that field carries a
/// user setting and llama.cpp owns the name table it is matched against. This
/// enum is the other case: a chain this crate writes itself, where naming a
/// sampler wrongly should be a compile error rather than a rejected turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SamplerKind {
    Penalties,
    Dry,
    TopNSigma,
    TopK,
    TypicalP,
    TopP,
    MinP,
    Xtc,
    Temperature,
    Infill,
    AdaptiveP,
}

impl SamplerKind {
    /// llama.cpp's own canonical name, as `common_sampler_type_to_str` spells
    /// it (`common/sampling.cpp:812-823`).
    ///
    /// **A lookup table, deliberately, and not a derivation.** This was a
    /// `#[serde(rename_all = "snake_case")]` derive, which agrees with llama.cpp
    /// everywhere the C++ name is a mechanical snake_case of the concept and
    /// diverges at the one point where llama.cpp abbreviated: typical-p is
    /// `typ_p`, never `typical_p`. A rule that is right ten times out of eleven
    /// reads as a rule; the eleventh name is a fact about someone else's code
    /// and has to be written down as one.
    ///
    /// Whether these strings still MEAN anything to llama.cpp is not decidable
    /// here, so it is not asserted here either: `every_sampler_name_is_one_llama_cpp_accepts`
    /// under `local-inference` puts each of them through the real mapper.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Penalties => "penalties",
            Self::Dry => "dry",
            Self::TopNSigma => "top_n_sigma",
            Self::TopK => "top_k",
            Self::TypicalP => "typ_p",
            Self::TopP => "top_p",
            Self::MinP => "min_p",
            Self::Xtc => "xtc",
            Self::Temperature => "temperature",
            Self::Infill => "infill",
            Self::AdaptiveP => "adaptive_p",
        }
    }

    /// Every variant, so a test can walk the whole table rather than the subset
    /// somebody remembered to list.
    pub const ALL: [SamplerKind; 11] = [
        Self::Penalties,
        Self::Dry,
        Self::TopNSigma,
        Self::TopK,
        Self::TypicalP,
        Self::TopP,
        Self::MinP,
        Self::Xtc,
        Self::Temperature,
        Self::Infill,
        Self::AdaptiveP,
    ];
}

/// What a grammar is FOR, which decides whether the generation prompt is
/// prefilled into the grammar sampler.
///
/// Not decoration. Only [`GrammarKind::ToolCalls`] and
/// [`GrammarKind::OutputFormat`] are prefilled, and without that prefill a
/// tool-capable template constrains from the very first token, which forces the
/// model to emit a tool call on every single turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GrammarKind {
    /// A grammar the user supplied. Never prefilled.
    User,
    /// Generated from a JSON schema.
    OutputFormat,
    /// Generated by the chat template for function calling. This is what
    /// `ChatTurn::applied().grammar` is.
    ToolCalls,
}

/// A grammar trigger carried straight through from `AppliedChat`.
///
/// The `kind` is llama.cpp's own enum value. It is validated in the shim rather
/// than cast, because an unknown value reaches a `GGML_ASSERT`, and a
/// `GGML_ASSERT` is `abort()`.
#[derive(Debug, Clone, Serialize)]
pub struct SamplerTrigger {
    #[serde(rename = "type")]
    pub kind: i32,
    pub value: String,
    pub token: i32,
}

/// The thinking-token budget: force the closing think tag once a reasoning block
/// has run past `tokens`.
#[derive(Debug, Clone, Serialize)]
pub struct ReasoningBudget {
    /// Token budget for the reasoning block. `-1` disables the limit while still
    /// building the sampler, which a lazy grammar needs for think-block
    /// suppression.
    #[serde(rename = "reasoning_budget_tokens")]
    pub tokens: i32,
    /// `AppliedChat::thinking_start_tag`.
    #[serde(rename = "reasoning_budget_start_tag")]
    pub start_tag: String,
    /// `AppliedChat::thinking_end_tags`. The first is the one forced.
    #[serde(rename = "reasoning_budget_end_tags")]
    pub end_tags: Vec<String>,
    /// Injected before the forced end tag, so the transcript says why the
    /// reasoning stopped instead of ending mid-sentence.
    #[serde(
        rename = "reasoning_budget_message",
        skip_serializing_if = "String::is_empty"
    )]
    pub message: String,
}

/// A sampler description. Absent fields keep llama.cpp's own default, so "not
/// set" and "set to the default value" stay distinct; the shipping core relies
/// on that when it sends only the knobs the user actually changed.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SamplerSpec {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub n_prev: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_keep: Option<i32>,

    /// The ORDER. This is a user setting (`llm.samplers`), which is why it is a
    /// list rather than a set of booleans, and why it is a list of STRINGS.
    ///
    /// **Passed through to llama.cpp's own name table, not to a table on this
    /// side.** That table maps eleven canonical names and derives from each of
    /// them a kebab-case and a no-dash alias, then adds `nucleus`, `temp` and
    /// `typ` (`common/sampling.cpp:829-874`). Anything narrower here would
    /// reject spellings `llama-server` accepts today, and the setting is free
    /// text, so those spellings are reachable by anyone typing rather than
    /// pasting. Use [`SamplerKind::as_str`] when the chain is named in code.
    ///
    /// An unrecognised name is an error from [`SamplerChain::new`], not a
    /// warning, and the error names the offender: llama.cpp's own mapper
    /// `LOG_WRN`s and DROPS it, which would turn a typo into a quietly shorter
    /// chain and a generation that is quietly different.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub samplers: Option<Vec<String>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dynatemp_range: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dynatemp_exponent: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_k: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub typical_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_n_sigma: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub xtc_probability: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub xtc_threshold: Option<f32>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub penalty_last_n: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub penalty_repeat: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub penalty_freq: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub penalty_present: Option<f32>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub dry_multiplier: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dry_base: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dry_allowed_length: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dry_penalty_last_n: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dry_sequence_breakers: Option<Vec<String>>,

    /// 0 disabled, 1 mirostat, 2 mirostat 2.0. Anything else is rejected rather
    /// than passed through, because llama.cpp `abort()`s on it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mirostat: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mirostat_tau: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mirostat_eta: Option<f32>,

    /// GBNF. This is how a tool call is constrained to the format the template
    /// declared; it comes from `ChatTurn::applied().grammar`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grammar: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grammar_kind: Option<GrammarKind>,
    /// From `ChatTurn::applied().grammar_lazy`. A lazy grammar starts
    /// constraining only once a trigger fires.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub grammar_lazy: bool,
    /// From `ChatTurn::applied().grammar_triggers`. A lazy grammar without its
    /// triggers either constrains from the first token or never constrains.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grammar_triggers: Option<Vec<SamplerTrigger>>,
    /// From `ChatTurn::applied().preserved_tokens`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preserved_tokens: Option<Vec<String>>,
    /// From `ChatTurn::applied().generation_prompt`. Fed to the grammar sampler
    /// so it advances past the tokens the template already placed in the prompt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation_prompt: Option<String>,

    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    pub reasoning_budget: Option<ReasoningBudget>,
}

impl SamplerSpec {
    /// A deterministic sampler: argmax, nothing else.
    ///
    /// `temperature = 0` is llama.cpp's own spelling of greedy: the temperature
    /// sampler collapses the distribution to one-hot (`llama-sampler.cpp:270`)
    /// and the terminating `dist` then has exactly one candidate to pick. The
    /// sampler list is narrowed to just temperature so nothing else can perturb
    /// it, which is what makes the linkage test's assertion exact rather than
    /// probabilistic.
    pub fn greedy() -> Self {
        Self {
            temperature: Some(0.0),
            samplers: Some(vec![SamplerKind::Temperature.as_str().to_owned()]),
            seed: Some(0),
            ..Self::default()
        }
    }
}

/// A built llama.cpp sampler.
pub struct SamplerChain {
    ptr: NonNull<ffi::LocaSampler>,
    _model: Arc<Model>,
}

// SAFETY: a `common_sampler` carries mutable state (the penalty ring buffer, the
// grammar stack, the reasoning-budget state machine, the RNG) and llama.cpp does
// not lock it. Movable between threads, never shared: `Sync` is deliberately not
// implemented, and every mutating method takes `&mut self`.
unsafe impl Send for SamplerChain {}

impl SamplerChain {
    pub fn new(model: Arc<Model>, spec: &SamplerSpec) -> Result<Self> {
        let json = serde_json::to_string(spec)
            .map_err(|e| internal(format!("sampler spec is not serialisable: {e}")))?;
        let c_json = c_string(&json, "sampler spec")?;
        // SAFETY: the model outlives the sampler (kept in `_model`) and `c_json`
        // outlives the call.
        let ptr = unsafe { ffi::loca_sampler_new(model.as_ptr(), c_json.as_ptr()) };
        let ptr = NonNull::new(ptr).ok_or_else(|| last_error("sampler create"))?;
        Ok(Self { ptr, _model: model })
    }

    /// Builds from a raw spec string, so a test can send something
    /// [`SamplerSpec`] cannot express.
    ///
    /// Test-only on purpose: the typed spec is the whole point of the safe
    /// surface, and the only reason to bypass it is to prove that a malformed
    /// input (a misspelled sampler name, say) is rejected rather than absorbed.
    #[cfg(test)]
    pub(super) fn new_from_json(model: Arc<Model>, spec_json: &str) -> Result<Self> {
        let c_json = c_string(spec_json, "sampler spec")?;
        // SAFETY: the model outlives the sampler (kept in `_model`) and `c_json`
        // outlives the call.
        let ptr = unsafe { ffi::loca_sampler_new(model.as_ptr(), c_json.as_ptr()) };
        let ptr = NonNull::new(ptr).ok_or_else(|| last_error("sampler create"))?;
        Ok(Self { ptr, _model: model })
    }

    /// Samples one token from the logits at `idx` (`-1` is the last position).
    ///
    /// Does **not** accept the token. Call [`SamplerChain::accept`] with
    /// `is_generated = true` next; that split is `common_sampler`'s documented
    /// contract and is why the two can no longer double-advance each other.
    pub fn sample(&mut self, ctx: &mut Context, idx: i32) -> Result<i32> {
        // SAFETY: `&mut` on both means neither is in use elsewhere; llama.cpp
        // reads the context's logits and advances the sampler's own state.
        let token = unsafe { ffi::loca_sampler_sample(self.ptr.as_ptr(), ctx.as_ptr(), idx) };
        if token < 0 {
            return Err(last_error("sample"));
        }
        Ok(token)
    }

    /// Feeds a token back so the grammar, the reasoning budget and the penalties
    /// all advance by exactly one.
    ///
    /// `is_generated` is `true` for a token the model just produced and `false`
    /// for one that was already in the prompt. Skipping this is how a
    /// grammar-constrained generation stops being constrained; calling it twice
    /// for the same token is how it constrains against the wrong position, which
    /// is what the previous pairing did.
    pub fn accept(&mut self, token: i32, is_generated: bool) {
        // SAFETY: `&mut self` guarantees exclusive use.
        unsafe { ffi::loca_sampler_accept(self.ptr.as_ptr(), token, is_generated) }
    }

    /// Clears per-sequence state so the sampler can serve another generation.
    pub fn reset(&mut self) {
        // SAFETY: `&mut self` guarantees exclusive use.
        unsafe { ffi::loca_sampler_reset(self.ptr.as_ptr()) }
    }

    /// The assembled chain as llama.cpp renders it, e.g.
    /// `"logits -> penalties -> dry -> top-k -> dist "`.
    ///
    /// This is the only way to observe that a configured order actually reached
    /// llama.cpp without resorting to a statistical test.
    pub fn chain_description(&self) -> Result<String> {
        // SAFETY: `self.ptr` is valid for the life of `self`.
        let raw = unsafe { ffi::loca_sampler_chain_str(self.ptr.as_ptr()) };
        // SAFETY: `raw` is null or a fresh allocation from this ABI.
        let raw =
            unsafe { ShimString::from_raw(raw) }.ok_or_else(|| last_error("sampler chain"))?;
        raw.to_str()
    }
}

impl Drop for SamplerChain {
    fn drop(&mut self) {
        // SAFETY: freed exactly once; no `Clone`, and the model outlives it.
        unsafe { ffi::loca_sampler_free(self.ptr.as_ptr()) }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn absent_knobs_are_omitted_rather_than_defaulted() {
        let json = serde_json::to_string(&SamplerSpec::default()).unwrap();
        assert_eq!(
            json, "{}",
            "an empty spec must leave every llama.cpp default alone"
        );
    }

    #[test]
    fn greedy_pins_temperature_and_narrows_the_chain() {
        let json = serde_json::to_string(&SamplerSpec::greedy()).unwrap();
        assert!(json.contains(r#""temperature":0.0"#), "{json}");
        assert!(json.contains(r#""samplers":["temperature"]"#), "{json}");
    }

    #[test]
    fn set_knobs_survive_serialisation() {
        let spec = SamplerSpec {
            temperature: Some(0.7),
            top_k: Some(40),
            ..SamplerSpec::default()
        };
        let json = serde_json::to_string(&spec).unwrap();
        assert!(json.contains(r#""temperature":0.7"#), "{json}");
        assert!(json.contains(r#""top_k":40"#), "{json}");
        assert!(
            !json.contains("top_p"),
            "unset knobs must stay absent: {json}"
        );
    }

    #[test]
    fn a_configured_order_reaches_the_spec_in_the_order_it_was_given() {
        // The names themselves are NOT asserted here, and that absence is
        // deliberate. The question "is `typ_p` what llama.cpp calls this" cannot
        // be answered by a string assertion about a string: the previous test
        // here asserted `typical_p` and passed for as long as it existed, while
        // the shim would have rejected every chain containing it. Only the C++
        // mapper can answer, so it is asked in
        // `every_sampler_name_is_one_llama_cpp_accepts` under `local-inference`.
        //
        // What IS this file's to check is that the ORDER survives, since the
        // whole reason the field is a list is that it is a user-configured
        // sequence.
        let spec = SamplerSpec {
            samplers: Some(vec!["temp".to_owned(), "top-k".to_owned()]),
            ..SamplerSpec::default()
        };
        let json = serde_json::to_string(&spec).unwrap();
        assert!(json.contains(r#""samplers":["temp","top-k"]"#), "{json}");
    }

    #[test]
    fn a_user_spelling_llama_cpp_accepts_is_not_narrowed_on_the_way_through() {
        // `temp`, `topk`, `top-k` and `nucleus` are all names llama-server takes
        // today, and `llm.samplers` is a free-text setting, so they arrive. The
        // field is `Vec<String>` precisely so that this side owns no table to
        // disagree with llama.cpp's; a typed enum here would reject them while
        // looking stricter and safer.
        let spec = SamplerSpec {
            samplers: Some(vec![
                "temp".to_owned(),
                "topk".to_owned(),
                "nucleus".to_owned(),
            ]),
            ..SamplerSpec::default()
        };
        let json = serde_json::to_string(&spec).unwrap();
        assert!(
            json.contains(r#""samplers":["temp","topk","nucleus"]"#),
            "{json}"
        );
    }

    #[test]
    fn the_reasoning_budget_flattens_into_the_spec() {
        let spec = SamplerSpec {
            reasoning_budget: Some(ReasoningBudget {
                tokens: 256,
                start_tag: "<think>".to_owned(),
                end_tags: vec!["</think>".to_owned()],
                message: "Reasoning cut short.".to_owned(),
            }),
            ..SamplerSpec::default()
        };
        let json = serde_json::to_string(&spec).unwrap();
        assert!(json.contains(r#""reasoning_budget_tokens":256"#), "{json}");
        assert!(
            json.contains(r#""reasoning_budget_start_tag":"<think>""#),
            "{json}"
        );
        assert!(
            json.contains(r#""reasoning_budget_end_tags":["</think>"]"#),
            "{json}"
        );
    }
}
