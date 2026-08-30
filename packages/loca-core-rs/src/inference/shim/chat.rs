//! Chat template application and output parsing: the part of llama.cpp a
//! `libllama`-only binding cannot reach. Jinja application, per-format
//! tool-call extraction, GBNF constraint and the reasoning/`<think>` split all
//! live in `common/chat.cpp`.
//!
//! The turn invariant: `common_chat_params` is per-turn state, so one
//! [`ChatTemplates::apply`] per turn and every parse of that turn's output goes
//! through the [`ChatTurn`] it returned. Parsing against another turn's handle
//! is undetectable at the ABI and fails quietly, so [`ChatTurn`] has no
//! `Clone`, no `Default` and no public constructor.

use std::ptr::NonNull;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::error::{internal, Result};

use super::ffi;
use super::model::Model;
use super::owned::{c_string, last_error, ShimString};

/// How the model's reasoning output should be treated. An APPLY-time setting:
/// it selects the parser [`ChatTemplates::apply`] builds, and that parser
/// decides the split, so `ChatTurn::parse_*` takes no format argument.
/// `common_chat_parser_params` carries a `reasoning_format` field that nothing
/// reads at the pinned commit, so exposing it would be an inert knob.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningFormat {
    /// Split `<think>` blocks out into `reasoning_content`. Pinned, so the
    /// stream reader always finds the thoughts in the same field.
    Deepseek,
    /// Let llama.cpp choose per template. Splits, for every template tried.
    Auto,
    /// Leave reasoning inline, tags and all, in `content`.
    None,
}

impl ReasoningFormat {
    fn as_str(self) -> &'static str {
        match self {
            Self::Deepseek => "deepseek",
            Self::Auto => "auto",
            Self::None => "none",
        }
    }
}

/// What `apply` produced besides the opaque handle: everything the token loop
/// and the sampler need.
#[derive(Debug, Clone, Deserialize)]
pub struct AppliedChat {
    pub prompt: String,
    pub generation_prompt: String,
    pub grammar: String,
    pub grammar_lazy: bool,
    /// Without these a lazy grammar either constrains from the first token or
    /// never constrains at all, so they are carried through uninterpreted.
    pub grammar_triggers: Vec<GrammarTrigger>,
    pub format: i32,
    pub supports_thinking: bool,
    pub thinking_start_tag: String,
    pub thinking_end_tags: Vec<String>,
    pub additional_stops: Vec<String>,
    pub preserved_tokens: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GrammarTrigger {
    #[serde(rename = "type")]
    pub kind: i32,
    pub value: String,
    pub token: i32,
}

/// An OpenAI-shaped assistant message, as llama.cpp itself renders it: it
/// converts OpenAI JSON in both directions already, so this is its output
/// verbatim rather than a hand-mapped shape.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct AssistantMessage {
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub reasoning_content: String,
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ToolCall {
    #[serde(rename = "type", default)]
    pub kind: String,
    pub function: ToolCallFunction,
    #[serde(default)]
    pub id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ToolCallFunction {
    pub name: String,
    /// A JSON string, not an object: the OpenAI wire shape.
    pub arguments: String,
}

/// A parsed chat template, per model. Building one parses Jinja, so it is
/// shared across turns behind an `Arc` rather than rebuilt.
pub struct ChatTemplates {
    ptr: NonNull<ffi::LocaChatTemplates>,
    /// Kept alive because the templates borrow the model when built from one.
    _model: Option<Arc<Model>>,
}

// SAFETY, and what this claim actually rests on.
//
// The previous justification here was "llama.cpp takes it by const pointer".
// That is not a thread-safety argument: C++ `const` permits `mutable` members,
// function-local statics and shared caches, all of which are data races under
// concurrent reads. Replaced with what was established by inspection at the pin:
//
// - `common_chat_templates` holds a parsed `jinja::program` AST plus three
//   plain values, and `common_chat_templates_apply` takes it by const pointer
//   and only walks it.
// - `common/jinja/**` contains no `mutable` member, no function-local `static`
//   holding evaluation state and no `thread_local`. Rendering builds its own
//   per-call state.
// - The shim's only mutable global is a `thread_local` error string.
//
// So concurrent `apply` on one `ChatTemplates` reads shared immutable data and
// writes only per-call and per-thread state. This is inspection of the pinned
// tree, not a guarantee upstream documents, so a llama.cpp bump that adds a
// cache inside the jinja runtime invalidates it. That is recorded here rather
// than left implicit because it is exactly the kind of assumption a bump breaks
// quietly.
unsafe impl Send for ChatTemplates {}
unsafe impl Sync for ChatTemplates {}

impl ChatTemplates {
    /// Builds from the model's own template metadata, which is the production
    /// path.
    pub fn from_model(model: Arc<Model>) -> Result<Self> {
        Self::build(Some(model), "", "", "")
    }

    /// Builds from a template source string with no model.
    ///
    /// This exists so the parse path can be exercised without a GGUF on disk;
    /// `common_chat_templates_init` accepts a null model and drives Jinja from
    /// the source alone.
    pub fn from_source(source: &str, bos: &str, eos: &str) -> Result<Self> {
        Self::build(None, source, bos, eos)
    }

    fn build(model: Option<Arc<Model>>, source: &str, bos: &str, eos: &str) -> Result<Self> {
        let source = c_string(source, "chat template")?;
        let bos = c_string(bos, "bos token")?;
        let eos = c_string(eos, "eos token")?;
        let model_ptr = model.as_ref().map_or(std::ptr::null(), |m| m.as_ptr());

        // SAFETY: all four pointers are valid for the call: the CStrings
        // outlive it, and `model_ptr` is either null (accepted, documented) or
        // borrowed from an `Arc<Model>` this struct then keeps alive.
        let ptr = unsafe {
            ffi::loca_chat_templates_init(model_ptr, source.as_ptr(), bos.as_ptr(), eos.as_ptr())
        };
        let ptr = NonNull::new(ptr).ok_or_else(|| last_error("chat templates init"))?;
        Ok(Self { ptr, _model: model })
    }

    /// Applies the template for one turn.
    ///
    /// The returned [`ChatTurn`] must be kept for the whole turn: it owns the
    /// parser state that makes tool-call and reasoning extraction work.
    pub fn apply(self: &Arc<Self>, inputs: &ChatInputs) -> Result<ChatTurn> {
        let json = serde_json::to_string(inputs)
            .map_err(|e| internal(format!("chat inputs are not serialisable: {e}")))?;
        let json = c_string(&json, "chat inputs")?;
        let mut out_json: *mut std::ffi::c_char = std::ptr::null_mut();

        // SAFETY: `self.ptr` is valid for the life of `self`; `json` outlives
        // the call; `out_json` is a live out-parameter that the shim either
        // leaves null or fills with a malloc'd string we take ownership of
        // immediately below.
        let params =
            unsafe { ffi::loca_chat_apply(self.ptr.as_ptr(), json.as_ptr(), &mut out_json) };

        // Both outputs are taken into owning wrappers BEFORE anything can fail.
        //
        // The string was already handled this way; the handle was not, and sat
        // as a bare `NonNull` across three `?` operators. Any of them returning
        // early leaked a whole `common_chat_params`, serialized PEG parser and
        // all, while the request still reported a clean error. The reachable
        // trigger is a llama.cpp bump that renames or retypes an `AppliedChat`
        // field: the deserialization below starts failing and the leak is once
        // per turn, unbounded.
        // SAFETY: `out_json` is null or a fresh allocation from this ABI.
        let applied = unsafe { ShimString::from_raw(out_json) };
        // SAFETY: `params` is null or a handle from this ABI, and `OwnedParams`
        // frees it exactly once on drop.
        let params = unsafe { OwnedParams::from_raw(params) };

        let params = params.ok_or_else(|| last_error("chat apply"))?;
        let applied = applied.ok_or_else(|| internal("chat apply returned no metadata"))?;
        let applied: AppliedChat = serde_json::from_str(&applied.to_str()?)
            .map_err(|e| internal(format!("chat apply metadata unreadable: {e}")))?;

        Ok(ChatTurn {
            params,
            applied,
            _templates: Arc::clone(self),
        })
    }
}

impl Drop for ChatTemplates {
    fn drop(&mut self) {
        // SAFETY: freed exactly once; the handle is not `Copy`/`Clone` and no
        // `ChatTurn` can outlive this, because each holds an `Arc` to it.
        unsafe { ffi::loca_chat_templates_free(self.ptr.as_ptr()) }
    }
}

/// The inputs to one template application.
#[derive(Debug, Clone, Serialize)]
pub struct ChatInputs {
    pub messages: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<serde_json::Value>,
    pub tool_choice: String,
    pub add_generation_prompt: bool,
    pub enable_thinking: bool,
    pub parallel_tool_calls: bool,
    #[serde(serialize_with = "serialize_reasoning")]
    pub reasoning_format: ReasoningFormat,
}

fn serialize_reasoning<S: serde::Serializer>(
    v: &ReasoningFormat,
    s: S,
) -> std::result::Result<S::Ok, S::Error> {
    s.serialize_str(v.as_str())
}

impl ChatInputs {
    /// Defaults matching what the shipping core sends: generation prompt on,
    /// thinking on, tools optional, reasoning split into its own field.
    pub fn new(messages: serde_json::Value) -> Self {
        Self {
            messages,
            tools: None,
            tool_choice: "auto".to_owned(),
            add_generation_prompt: true,
            enable_thinking: true,
            parallel_tool_calls: false,
            reasoning_format: ReasoningFormat::Deepseek,
        }
    }

    pub fn with_tools(mut self, tools: serde_json::Value) -> Self {
        self.tools = Some(tools);
        self
    }
}

/// A `loca_chat_params` handle, freed exactly once on drop.
///
/// This exists so the handle is owned from the instant it crosses the boundary
/// rather than from the instant [`ChatTurn`] is constructed. The gap between the
/// two was a leak on every early return in between.
struct OwnedParams(NonNull<ffi::LocaChatParams>);

impl OwnedParams {
    /// # Safety
    ///
    /// `ptr` must be null, or a handle returned by this ABI and not yet freed.
    /// Ownership transfers here.
    unsafe fn from_raw(ptr: *mut ffi::LocaChatParams) -> Option<Self> {
        NonNull::new(ptr).map(Self)
    }

    fn as_ptr(&self) -> *const ffi::LocaChatParams {
        self.0.as_ptr()
    }
}

impl Drop for OwnedParams {
    fn drop(&mut self) {
        // SAFETY: the handle came from `loca_chat_apply` and is freed exactly
        // once, here, because this type is neither `Copy` nor `Clone`.
        unsafe { ffi::loca_chat_params_free(self.0.as_ptr()) }
    }
}

/// One turn's applied template plus the parser state that goes with it.
pub struct ChatTurn {
    params: OwnedParams,
    applied: AppliedChat,
    /// Keeps the templates alive for the turn. `common_chat_params` does not
    /// point back into them today, but relying on that across a version bump is
    /// exactly the kind of assumption this port should not be making.
    _templates: Arc<ChatTemplates>,
}

// SAFETY: `common_chat_params` is plain data after `apply`: strings, vectors and
// a serialized parser, with no interior mutability. Each parse builds a fresh
// `common_chat_parser_params` from it, loads the parser into that fresh copy,
// and runs `common_chat_peg_parse` against local state; the original is only
// read. The same inspection and the same caveat as `ChatTemplates` above apply.
unsafe impl Send for ChatTurn {}
unsafe impl Sync for ChatTurn {}

impl ChatTurn {
    /// The rendered prompt, grammar, stops and preserved tokens.
    pub fn applied(&self) -> &AppliedChat {
        &self.applied
    }

    /// Whether APPLY produced a serialized PEG parser for this turn.
    ///
    /// Necessary but not sufficient for tool-call extraction: this is the apply
    /// side, and the D-002 trap is on the parse side. Use
    /// [`ChatTurn::parser_survives_revival`] for the property that actually
    /// decides whether tool calls come out.
    pub fn has_parser(&self) -> bool {
        // SAFETY: `self.params` is valid for the life of `self`.
        unsafe { ffi::loca_chat_params_has_parser(self.params.as_ptr()) }
    }

    /// Whether the serialized parser survives into the parser params that
    /// [`ChatTurn::parse_final`] actually uses.
    ///
    /// This is the D-002 trap stated as a property. `common_chat_parser_params`'
    /// converting constructor carries only `format` and `generation_prompt`, so
    /// without an explicit `parser.load` the parse silently degrades to
    /// content-only and still reports success. The shim runs the same revival
    /// here that it runs in `loca_chat_parse`, so this cannot stay true after
    /// the real one is removed.
    pub fn parser_survives_revival(&self) -> bool {
        // SAFETY: `self.params` is valid for the life of `self`.
        unsafe { ffi::loca_chat_parser_survives_revival(self.params.as_ptr()) }
    }

    /// Parses a partial generation, for streaming.
    ///
    /// Takes no reasoning format: whether reasoning is split out was decided by
    /// the [`ChatInputs::reasoning_format`] this turn was applied with, and is
    /// baked into the parser. See [`ReasoningFormat`].
    pub fn parse_partial(&self, text: &str) -> Result<AssistantMessage> {
        self.parse(text, true)
    }

    /// Parses the final generation.
    pub fn parse_final(&self, text: &str) -> Result<AssistantMessage> {
        self.parse(text, false)
    }

    fn parse(&self, text: &str, is_partial: bool) -> Result<AssistantMessage> {
        let text = c_string(text, "model output")?;

        // SAFETY: `self.params` is valid for the life of `self`; the CString
        // outlives the call; the return is null or a fresh allocation we take.
        let raw = unsafe { ffi::loca_chat_parse(self.params.as_ptr(), text.as_ptr(), is_partial) };
        // SAFETY: `raw` is null or a fresh allocation from this ABI.
        let raw = unsafe { ShimString::from_raw(raw) }.ok_or_else(|| last_error("chat parse"))?;

        serde_json::from_str(&raw.to_str()?)
            .map_err(|e| internal(format!("parsed message unreadable: {e}")))
    }
}

impl std::fmt::Debug for ChatTurn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChatTurn")
            .field("format", &self.applied.format)
            .field("has_parser", &self.has_parser())
            .finish()
    }
}
