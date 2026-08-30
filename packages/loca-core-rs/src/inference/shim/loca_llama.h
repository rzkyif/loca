/* The C ABI over llama.cpp that `inference::shim` binds to.
 *
 * This header is the single source of truth for the boundary. Rust binds ONLY
 * what is declared here: no llama.cpp struct, enum or by-value parameter
 * crosses, every handle is opaque, and every compound value is JSON. The point
 * is that the ABI cannot drift silently. The shim is compiled against llama.cpp's
 * real headers, so a version bump that changes a struct is a C++ compile error
 * naming the field, where a hand-declared or bindgen-generated struct on the
 * Rust side would simply be misaligned at runtime.
 *
 * ---- Memory ownership, in full ------------------------------------------
 *
 * 1. Every `char *` RETURNED by this ABI is allocated with malloc() by the shim
 *    and is owned by the caller. Free it with loca_string_free(). Do not free it
 *    with any other allocator: the Rust side must not use CString::from_raw,
 *    which would hand a malloc block to Rust's allocator.
 *
 * 2. Every `const char *` PASSED IN is borrowed for the duration of the call
 *    only. The shim copies anything it keeps.
 *
 * 3. Every loca_* handle is created by its _new/_init/_load/_apply function and
 *    released by its matching _free. Passing a handle after freeing it, or
 *    freeing it twice, is undefined. Handles are hierarchical: a context and a
 *    templates handle borrow their model, so the model must outlive both.
 *
 * 4. loca_last_error() returns a pointer to a thread-local buffer that the next
 *    loca_* call ON THE SAME THREAD overwrites. Copy it before doing anything
 *    else. It is valid only immediately after a call that reported failure.
 *
 * 5. loca_get_logits() returns a BORROWED pointer into the context's own
 *    buffer, invalidated by the next loca_decode() on that context. It is the
 *    only borrowed return value here; everything else is copied or owned.
 *
 * ---- What is deliberately NOT here --------------------------------------
 *
 * Both of ggml's convenience loaders. `ggml_backend_load_all()` searches the
 * process working directory and dlopen()s anything matching libggml-*.so;
 * `ggml_backend_load_all_from_path(dir)` confines the directory search but then
 * dlopen()s whatever GGML_BACKEND_PATH names. For a service startable from any
 * directory, by an environment that is not always the operator's, both are
 * arbitrary code execution.
 *
 * So loca_backend_load_dir() enumerates the directory itself and loads each
 * module with ggml_backend_load(), which takes a path and reads no environment.
 * The shim poisons both identifiers, so no code path in this ABI opens a
 * library whose path did not come from the caller.
 */

#ifndef LOCA_LLAMA_H
#define LOCA_LLAMA_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* ---- errors and strings ------------------------------------------------ */

/** The last error on THIS thread. Valid until the next loca_* call. Never null.
 *
 *  Cleared on entry to every fallible function, so an empty string means "this
 *  call reported no message" rather than "some earlier call's message is still
 *  sitting here". */
const char * loca_last_error(void);

/** Frees any `char *` this ABI returned. Null is a no-op. */
void loca_string_free(char * s);

/* ---- backend registry -------------------------------------------------- */

/** Initialises ggml. Idempotent, and safe to call before any backend is loaded. */
void loca_backend_init(void);

/** Loads one backend module by explicit path. Returns false on failure. */
bool loca_backend_load(const char * path);

/** Loads every backend module found in ONE explicit directory.
 *
 *  Enumerates `dir` itself and loads each match by path. Never searches the
 *  working directory, never reads GGML_BACKEND_PATH, never opens a library
 *  outside `dir`. Returns the number of modules THIS CALL loaded, or -1 with an
 *  error (including when `dir` does not exist). */
int32_t loca_backend_load_dir(const char * dir);

/** Registered devices, as JSON: [{ "name", "description", "type" }]. `type` is
 *  one of "cpu", "gpu", "accel". Zero devices means inference cannot run, which
 *  in the desktop (shared) build means no backend module was found. */
char * loca_backend_devices_json(void);

/* ---- model and context ------------------------------------------------- */

typedef struct loca_model   loca_model;
typedef struct loca_context loca_context;

/** opts_json: { "n_gpu_layers": int, "use_mmap": bool, "use_mlock": bool } */
loca_model * loca_model_load(const char * gguf_path, const char * opts_json);
void         loca_model_free(loca_model * m);

/** { "n_ctx_train", "n_embd", "n_vocab", "chat_template" } */
char * loca_model_meta_json(const loca_model * m);

/** opts_json: { "n_ctx": int, "n_batch": int, "n_threads": int, "flash_attn": bool,
 *              "embeddings": bool, "pooling_type": "none"|"mean"|"cls"|"last" }
 *
 *  `embeddings` puts the context in embedding mode, where loca_embed() works and
 *  loca_decode()/loca_get_logits() do not. `pooling_type` decides how the token
 *  vectors collapse into one sequence vector and only means anything when
 *  `embeddings` is set; omitted, the model's own trained default is used, which
 *  is what any sentence-embedding GGUF wants. */
loca_context * loca_context_new(loca_model * m, const char * opts_json);
void           loca_context_free(loca_context * c);
uint32_t       loca_context_n_ctx(const loca_context * c);

/** How many tokens the context currently holds, read back from llama.cpp's own
 *  memory rather than tracked in parallel. 0 for a fresh or reset context. */
int32_t loca_context_n_past(const loca_context * c);

/** Clears the KV cache so the context can be reused for a new sequence. */
void loca_context_reset(loca_context * c);

/* ---- vocab ------------------------------------------------------------- */

/** Writes at most out_cap tokens. Returns the count, or a NEGATIVE number whose
 *  absolute value is the capacity required (llama.cpp's own convention). */
int32_t loca_tokenize(const loca_model * m, const char * text, bool add_special,
                      bool parse_special, int32_t * out, int32_t out_cap);

/** Same negated-capacity convention, in bytes. Not NUL-terminated. */
int32_t loca_token_to_piece(const loca_model * m, int32_t token, bool special,
                            char * out, int32_t out_cap);

/** True for any end-of-generation token, not just EOS. */
bool loca_token_is_eog(const loca_model * m, int32_t token);

int32_t loca_n_vocab(const loca_model * m);

/** Embedding width of this model, which is the length loca_embed() writes. */
int32_t loca_n_embd(const loca_model * m);

/* ---- vision (libmtmd) --------------------------------------------------- */

/* A loaded multimodal projector, paired with the text model it was loaded for.
 * Vision is a SECOND model file (the "mmproj") that turns pixels into
 * embeddings the language model attends to; without it a vision-capable GGUF is
 * a text model. */
typedef struct loca_mtmd loca_mtmd;

/** Loads `mmproj_path` against `m`. Null on failure (see loca_last_error()).
 *
 *  opts_json: { "n_threads": int, "use_gpu": bool }
 *
 *  The model must outlive the returned handle. */
loca_mtmd * loca_mtmd_init(const loca_model * m, const char * mmproj_path,
                           const char * opts_json);
void        loca_mtmd_free(loca_mtmd * h);

/** The marker a prompt must contain once per image, e.g. "<__media__>".
 *  BORROWED from the handle; valid until loca_mtmd_free. */
const char * loca_mtmd_marker(const loca_mtmd * h);

/** Whether this projector does IMAGES. A projector can be audio-only, and one
 *  loaded successfully still cannot answer an image prompt if it is. */
bool loca_mtmd_supports_vision(const loca_mtmd * h);

/** Tokenizes `prompt`, encodes `n_images` images, and evaluates the whole
 *  interleaved sequence into `c`. Returns the new n_past, or -1 on failure.
 *
 *  `prompt` must contain exactly `n_images` copies of loca_mtmd_marker(), and a
 *  mismatch is an error rather than a silent reordering. Images are ENCODED
 *  FILES, not raw pixels: mtmd decodes them, keeping stb_image on the C++ side.
 *
 *  This REPLACES loca_decode for a turn with images: the embeddings have to be
 *  interleaved between the text spans at exactly the marker positions. */
int32_t loca_mtmd_eval(loca_mtmd * h, loca_context * c, const char * prompt,
                       bool add_special, const unsigned char * const * images,
                       const size_t * image_lens, int32_t n_images,
                       int32_t n_batch);

/* ---- embeddings -------------------------------------------------------- */

/** Embeds ONE sequence of `n` tokens into `out`, writing exactly n_embd floats.
 *  Returns the count written, or -1 on failure (see loca_last_error()).
 *
 *  Separate from loca_decode() because it is a different call on a
 *  differently-configured context: the context must have been created with
 *  "embeddings": true, and calling this on a generation context fails rather
 *  than returning something plausible.
 *
 *  The context is cleared before the batch, so each call is independent. The
 *  vector is COPIED into `out` rather than borrowed: unlike logits there is no
 *  hot path here worth the aliasing hazard. */
int32_t loca_embed(loca_context * c, const int32_t * tokens, int32_t n,
                   float * out, int32_t out_cap);

/* ---- decode ------------------------------------------------------------ */

/** Evaluates `n` tokens, APPENDING them after whatever the context already
 *  holds. 0 on success.
 *
 *  There is deliberately no `n_past` parameter: llama.cpp derives each position
 *  from the memory state as `seq_pos_max(seq) + 1`. Read the real value with
 *  loca_context_n_past(), and use loca_context_reset() for a new sequence. */
int32_t loca_decode(loca_context * c, const int32_t * tokens, int32_t n);

/** Logits for the i-th token of the last decode; negative i counts from the
 *  end, so -1 is the last. BORROWED: invalidated by the next loca_decode. */
const float * loca_get_logits(loca_context * c, int32_t i);

/* ---- sampling ---------------------------------------------------------- */

typedef struct loca_sampler loca_sampler;

/* This wraps llama.cpp's own `common_sampler`, not a chain assembled here: a
 * hand-rolled chain over the raw llama_sampler_* API cannot express a LAZY
 * grammar with triggers, the thinking-token budget, the user-configured sampler
 * ORDER, or DRY, all of which live in common/sampling.cpp.
 *
 * Two consequences: common_sampler_init THROWS on a grammar it cannot parse,
 * where an unchecked llama_sampler_init_grammar null makes the first sample() a
 * SIGSEGV; and common_sampler_sample does NOT accept the sampled token, unlike
 * llama_sampler_sample, which accepts internally and would make a paired accept
 * a silent double-advance.
 */

/** spec_json. Every absent key keeps llama.cpp's own default, so "not set" and
 *  "set to the default" stay distinct and this shim invents no policy:
 *
 *    { "seed": uint, "n_prev": int, "min_keep": int,
 *
 *      // ORDER. This is a user setting in the shipping core, which is why it is
 *      // a list and not a set of booleans. Names accept every alias llama.cpp
 *      // accepts ("temp", "top-k", "topk", "nucleus", ...). An unrecognised
 *      // name is an ERROR here, not a warning: upstream's mapper drops it and
 *      // would turn a typo into a quietly shorter chain.
 *      "samplers": ["penalties","dry","top_n_sigma","top_k","typical_p",
 *                   "top_p","min_p","xtc","temperature","infill","adaptive_p"],
 *
 *      "temperature": f, "dynatemp_range": f, "dynatemp_exponent": f,
 *      "top_k": int, "top_p": f, "min_p": f, "typical_p": f, "top_n_sigma": f,
 *      "xtc_probability": f, "xtc_threshold": f,
 *
 *      "penalty_last_n": int, "penalty_repeat": f,
 *      "penalty_freq": f, "penalty_present": f,
 *
 *      "dry_multiplier": f, "dry_base": f, "dry_allowed_length": int,
 *      "dry_penalty_last_n": int, "dry_sequence_breakers": [str],
 *
 *      "mirostat": 0|1|2, "mirostat_tau": f, "mirostat_eta": f,
 *
 *      // GBNF. `grammar_kind` is load-bearing rather than decoration: only an
 *      // "output_format" or "tool_calls" grammar gets `generation_prompt`
 *      // prefilled into the grammar sampler. Without that prefill a
 *      // tool-capable template constrains from the first token and the model is
 *      // forced to emit a tool call on EVERY turn.
 *      "grammar": str, "grammar_kind": "user"|"output_format"|"tool_calls",
 *      "grammar_lazy": bool,
 *      "grammar_triggers": [{ "type": int, "value": str, "token": int }],
 *      "preserved_tokens": [str],
 *      "generation_prompt": str,
 *
 *      // Thinking-token budget. Tags are strings here and are tokenized
 *      // against this model's vocab, the same derivation upstream's server uses.
 *      "reasoning_budget_tokens": int,          // -1 disables
 *      "reasoning_budget_start_tag": str,
 *      "reasoning_budget_end_tags": [str],
 *      "reasoning_budget_message": str }
 *
 *  `grammar`, `grammar_lazy`, `grammar_triggers`, `preserved_tokens`,
 *  `generation_prompt` and the two thinking tags all come straight out of
 *  loca_chat_apply's out_json for the same turn. Returns null on failure,
 *  INCLUDING a grammar llama.cpp cannot parse. */
loca_sampler * loca_sampler_new(const loca_model * m, const char * spec_json);
void           loca_sampler_free(loca_sampler * s);

/** Samples one token. Does NOT accept it; call loca_sampler_accept next.
 *  Negative on failure. */
int32_t loca_sampler_sample(loca_sampler * s, loca_context * c, int32_t idx);

/** Feeds a token back so the grammar, the reasoning budget and the penalties
 *  advance. `is_generated` is false for tokens that were already in the prompt
 *  and true for tokens the model just produced. */
void loca_sampler_accept(loca_sampler * s, int32_t token, bool is_generated);

void loca_sampler_reset(loca_sampler * s);

/** The assembled chain as llama.cpp itself renders it, e.g.
 *  "logits -> penalties -> dry -> top-k -> ... -> dist ". Exposed so a test can
 *  assert the configured ORDER actually reached llama.cpp, which is otherwise
 *  only observable statistically. */
char * loca_sampler_chain_str(const loca_sampler * s);

/* ---- chat: the capability surface -------------------------------------- */

/* This is the part that cannot be had from a libllama-only binding. Jinja
 * template application, per-format tool-call extraction, GBNF grammar
 * constraint and the reasoning/<think> split all live in common/chat.cpp. */

typedef struct loca_chat_templates loca_chat_templates;
typedef struct loca_chat_params    loca_chat_params;

/** `model` may be null, in which case template_override must be a template
 *  source string; that path exists so the parse tests need no GGUF on disk.
 *  In production `model` is the loaded model and the template comes from its
 *  own metadata. */
loca_chat_templates * loca_chat_templates_init(const loca_model * model,
                                               const char * template_override,
                                               const char * bos, const char * eos);
void                  loca_chat_templates_free(loca_chat_templates * h);

/** inputs_json:
 *    { "messages": [...oai...], "tools": [...oai...] | null,
 *      "tool_choice": "auto"|"required"|"none",
 *      "add_generation_prompt": bool, "enable_thinking": bool,
 *      "parallel_tool_calls": bool,
 *      "reasoning_format": "deepseek"|"auto"|"none" }
 *
 *  Returns an opaque params handle that must be kept for the WHOLE turn: every
 *  parse of that turn's output is made against it. *out_json receives what the
 *  handle does not carry into Rust:
 *    { "prompt", "generation_prompt", "grammar", "grammar_lazy",
 *      "grammar_triggers", "format", "supports_thinking",
 *      "thinking_start_tag", "thinking_end_tags",
 *      "additional_stops", "preserved_tokens" }
 *
 *  `grammar_triggers` is not optional decoration: a lazy grammar without its
 *  triggers either constrains from the first token or never constrains at all. */
loca_chat_params * loca_chat_apply(loca_chat_templates * h, const char * inputs_json,
                                   char ** out_json);
void               loca_chat_params_free(loca_chat_params * p);

/** Parses raw model output into an OpenAI-shaped assistant message:
 *    { "role", "content", "reasoning_content", "tool_calls": [...] }
 *
 *  `params` MUST be the handle from the same turn's loca_chat_apply. Parsing
 *  against a different turn's handle is not detectable here and degrades
 *  quietly, which is why the Rust side makes it unconstructible.
 *
 *  `text` is the raw output. Do NOT prepend the generation prompt:
 *  common_chat_peg_parse prepends it itself, and prepending it twice fails the
 *  parse.
 *
 *  `is_partial` is what makes this usable mid-stream. Returns null on failure.
 *
 *  There is deliberately NO reasoning_format argument here. Whether reasoning
 *  is split into `reasoning_content` or left inline is decided at APPLY time and
 *  baked into the serialized PEG parser; `common_chat_parser_params` carries a
 *  `reasoning_format` field, but nothing in the parse path reads it at this pin
 *  (verified by experiment: varying it changes nothing, while varying the
 *  apply-time value flips the behaviour). Accepting it here would be an inert
 *  knob that a caller would reasonably believe worked. Set it on
 *  loca_chat_apply's `reasoning_format` instead. */
char * loca_chat_parse(const loca_chat_params * params, const char * text,
                       bool is_partial);

/** Whether `params` carries a serialized PEG parser, i.e. whether APPLY produced
 *  one at all. Necessary but NOT sufficient for tool-call extraction: the trap
 *  is on the parse side. See loca_chat_parser_survives_revival. */
bool loca_chat_params_has_parser(const loca_chat_params * params);

/** Whether the serialized parser survives into the parser params that
 *  loca_chat_parse actually uses.
 *
 *  This is the property the D-002 trap is about, and it is asserted here rather
 *  than inferred from a tool-call test that could pass for an unrelated reason.
 *  It runs the SAME internal revival the parse path runs, so deleting that step
 *  turns this false too; a probe with its own copy of the revival would stay
 *  green after the real one was deleted (D-100). */
bool loca_chat_parser_survives_revival(const loca_chat_params * params);

#ifdef __cplusplus
}
#endif

#endif /* LOCA_LLAMA_H */
