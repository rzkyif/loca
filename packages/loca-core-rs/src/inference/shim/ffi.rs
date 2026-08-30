//! Raw declarations of the C ABI in `loca_llama.h`.
//!
//! One declaration per header line, hand-written rather than generated:
//! `bindgen` would put `libclang` on every build machine and every cross
//! toolchain, and this surface is small and stable enough that the header is
//! easier to diff against than a generator's output.
//!
//! Nothing here is called outside this module. The safe wrappers in the sibling
//! files are the only entry points, because every one of these needs an
//! invariant established before it is sound.

use std::ffi::{c_char, c_float, c_uint};

// Opaque handles. Each is only ever held behind a `NonNull` in a wrapper that
// frees it on drop; they are deliberately uninhabited-ish (no constructor) so
// nothing can fabricate one.
#[repr(C)]
pub struct LocaModel {
    _private: [u8; 0],
}
#[repr(C)]
pub struct LocaContext {
    _private: [u8; 0],
}
#[repr(C)]
pub struct LocaSampler {
    _private: [u8; 0],
}
#[repr(C)]
pub struct LocaChatTemplates {
    _private: [u8; 0],
}
#[repr(C)]
pub struct LocaChatParams {
    _private: [u8; 0],
}
#[repr(C)]
pub struct LocaMtmd {
    _private: [u8; 0],
}

extern "C" {
    pub fn loca_last_error() -> *const c_char;
    pub fn loca_string_free(s: *mut c_char);

    pub fn loca_backend_init();
    pub fn loca_backend_load(path: *const c_char) -> bool;
    pub fn loca_backend_load_dir(dir: *const c_char) -> i32;
    pub fn loca_backend_devices_json() -> *mut c_char;

    pub fn loca_model_load(gguf_path: *const c_char, opts_json: *const c_char) -> *mut LocaModel;
    pub fn loca_model_free(m: *mut LocaModel);
    pub fn loca_model_meta_json(m: *const LocaModel) -> *mut c_char;

    pub fn loca_context_new(m: *mut LocaModel, opts_json: *const c_char) -> *mut LocaContext;
    pub fn loca_context_free(c: *mut LocaContext);
    pub fn loca_context_n_ctx(c: *const LocaContext) -> c_uint;
    pub fn loca_context_n_past(c: *const LocaContext) -> i32;
    pub fn loca_context_reset(c: *mut LocaContext);

    pub fn loca_tokenize(
        m: *const LocaModel,
        text: *const c_char,
        add_special: bool,
        parse_special: bool,
        out: *mut i32,
        out_cap: i32,
    ) -> i32;
    pub fn loca_token_to_piece(
        m: *const LocaModel,
        token: i32,
        special: bool,
        out: *mut c_char,
        out_cap: i32,
    ) -> i32;
    pub fn loca_token_is_eog(m: *const LocaModel, token: i32) -> bool;
    pub fn loca_n_vocab(m: *const LocaModel) -> i32;
    pub fn loca_n_embd(m: *const LocaModel) -> i32;

    pub fn loca_mtmd_init(
        m: *const LocaModel,
        mmproj_path: *const c_char,
        opts_json: *const c_char,
    ) -> *mut LocaMtmd;
    pub fn loca_mtmd_free(h: *mut LocaMtmd);
    pub fn loca_mtmd_marker(h: *const LocaMtmd) -> *const c_char;
    pub fn loca_mtmd_supports_vision(h: *const LocaMtmd) -> bool;
    pub fn loca_mtmd_eval(
        h: *mut LocaMtmd,
        c: *mut LocaContext,
        prompt: *const c_char,
        add_special: bool,
        images: *const *const u8,
        image_lens: *const usize,
        n_images: i32,
        n_batch: i32,
    ) -> i32;

    pub fn loca_embed(
        c: *mut LocaContext,
        tokens: *const i32,
        n: i32,
        out: *mut c_float,
        out_cap: i32,
    ) -> i32;

    pub fn loca_decode(c: *mut LocaContext, tokens: *const i32, n: i32) -> i32;
    pub fn loca_get_logits(c: *mut LocaContext, i: i32) -> *const c_float;

    pub fn loca_sampler_new(m: *const LocaModel, spec_json: *const c_char) -> *mut LocaSampler;
    pub fn loca_sampler_free(s: *mut LocaSampler);
    pub fn loca_sampler_sample(s: *mut LocaSampler, c: *mut LocaContext, idx: i32) -> i32;
    pub fn loca_sampler_accept(s: *mut LocaSampler, token: i32, is_generated: bool);
    pub fn loca_sampler_reset(s: *mut LocaSampler);
    pub fn loca_sampler_chain_str(s: *const LocaSampler) -> *mut c_char;

    pub fn loca_chat_templates_init(
        model: *const LocaModel,
        template_override: *const c_char,
        bos: *const c_char,
        eos: *const c_char,
    ) -> *mut LocaChatTemplates;
    pub fn loca_chat_templates_free(h: *mut LocaChatTemplates);

    pub fn loca_chat_apply(
        h: *mut LocaChatTemplates,
        inputs_json: *const c_char,
        out_json: *mut *mut c_char,
    ) -> *mut LocaChatParams;
    pub fn loca_chat_params_free(p: *mut LocaChatParams);
    pub fn loca_chat_params_has_parser(p: *const LocaChatParams) -> bool;
    pub fn loca_chat_parser_survives_revival(p: *const LocaChatParams) -> bool;

    pub fn loca_chat_parse(
        params: *const LocaChatParams,
        text: *const c_char,
        is_partial: bool,
    ) -> *mut c_char;
}

// Both of ggml's convenience loaders are intentionally absent from this file
// and from the header. `ggml_backend_load_all` searches the process working
// directory and `dlopen`s anything matching `libggml-*.so` there;
// `ggml_backend_load_all_from_path(dir)` closes that half but then `dlopen`s
// whatever `GGML_BACKEND_PATH` names. Both are arbitrary code execution for a
// service startable from any directory, by an environment that is not always
// the operator's. `loca_backend_load_dir` therefore enumerates the directory
// and loads each module by path, and the shim poisons BOTH identifiers so
// neither can be reintroduced there either.
