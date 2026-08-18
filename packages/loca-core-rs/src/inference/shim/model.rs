//! Backend registry, model loading, context, vocabulary and decode.
//!
//! These are thin: they establish the invariants that make the C calls sound
//! and translate errors, and they decide nothing. Which sampler, how long to
//! generate, when to stop and what to stream are N2's.

use std::ffi::c_char;
use std::ptr::NonNull;

use serde::{Deserialize, Serialize};

use crate::error::{internal, Result};

use super::ffi;
use super::owned::{c_string, last_error, ShimString};

/// A registered compute device.
#[derive(Debug, Clone, Deserialize)]
pub struct Device {
    pub name: String,
    pub description: String,
    #[serde(rename = "type")]
    pub kind: DeviceKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeviceKind {
    Cpu,
    Gpu,
    Accel,
    #[serde(other)]
    Unknown,
}

/// Initialises ggml. Idempotent; safe to call more than once.
pub fn backend_init() {
    // SAFETY: takes no arguments, has no preconditions, and llama.cpp
    // documents it as safe to call repeatedly.
    unsafe { ffi::loca_backend_init() }
}

/// Loads one backend module by explicit path.
///
/// On desktop, backends (including the CPU baseline) are separate libraries
/// loaded at runtime, so at least one must be loaded before a model will load
/// at all. On mobile everything is linked in and this is not used.
pub fn backend_load(path: &str) -> Result<()> {
    let c_path = c_string(path, "backend path")?;
    // SAFETY: `c_path` outlives the call and the shim only reads it.
    if unsafe { ffi::loca_backend_load(c_path.as_ptr()) } {
        Ok(())
    } else {
        Err(last_error("backend load"))
    }
}

/// Loads every backend module in ONE explicit directory, returning how many
/// modules this call loaded.
///
/// The directory is always ours (an install dir, or the consent-download
/// destination under `~/.loca`), and it is the only place this looks. There is
/// deliberately no "search the usual places" variant, and the shim does the
/// enumeration itself rather than delegating to ggml (D-107):
///
/// - `ggml_backend_load_all` includes the process working directory in its
///   search and `dlopen`s anything matching `libggml-*.so` it finds, which for a
///   service startable from any directory is arbitrary code execution.
/// - `ggml_backend_load_all_from_path` confines the directory search but then
///   `dlopen`s whatever `GGML_BACKEND_PATH` names, which is the same vector
///   through the environment, and this core runs as a system service whose
///   environment is not always the operator's.
///
/// Neither is exposed across the ABI, and the shim poisons both identifiers so
/// its own C++ cannot reach them either.
pub fn backend_load_dir(dir: &str) -> Result<usize> {
    let c_dir = c_string(dir, "backend directory")?;
    // SAFETY: `c_dir` outlives the call and the shim only reads it.
    let n = unsafe { ffi::loca_backend_load_dir(c_dir.as_ptr()) };
    if n < 0 {
        return Err(last_error("backend load dir"));
    }
    Ok(n as usize)
}

/// Every registered device. Empty means inference cannot run, which on desktop
/// means no backend module was found.
pub fn devices() -> Result<Vec<Device>> {
    // SAFETY: no arguments; returns null on failure or a fresh allocation.
    let raw = unsafe { ffi::loca_backend_devices_json() };
    // SAFETY: `raw` is null or a fresh allocation from this ABI.
    let raw = unsafe { ShimString::from_raw(raw) }.ok_or_else(|| last_error("backend devices"))?;
    serde_json::from_str(&raw.to_str()?)
        .map_err(|e| internal(format!("device list unreadable: {e}")))
}

/// How to load a model file.
#[derive(Debug, Clone, Serialize)]
pub struct ModelOptions {
    /// Layers to offload to the GPU. 0 is CPU only.
    pub n_gpu_layers: i32,
    /// Keep weight pages file-backed and evictable. On iOS this is what keeps a
    /// multi-GB model under the per-app jetsam limit, so it stays on.
    pub use_mmap: bool,
    pub use_mlock: bool,
}

impl Default for ModelOptions {
    fn default() -> Self {
        Self {
            n_gpu_layers: 0,
            use_mmap: true,
            use_mlock: false,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ModelMeta {
    pub n_ctx_train: i32,
    pub n_embd: i32,
    pub n_vocab: i32,
    /// The model's own chat template, empty when it carries none.
    pub chat_template: String,
}

/// A loaded GGUF.
pub struct Model {
    ptr: NonNull<ffi::LocaModel>,
}

// SAFETY: `llama_model` is immutable once loaded; llama.cpp supports sharing
// one model across threads and contexts, which is the whole point of the
// model/context split.
unsafe impl Send for Model {}
unsafe impl Sync for Model {}

impl Model {
    pub fn load(path: &str, opts: &ModelOptions) -> Result<Self> {
        let c_path = c_string(path, "model path")?;
        let json = serde_json::to_string(opts)
            .map_err(|e| internal(format!("model options are not serialisable: {e}")))?;
        let c_json = c_string(&json, "model options")?;

        // SAFETY: both CStrings outlive the call and the shim only reads them.
        let ptr = unsafe { ffi::loca_model_load(c_path.as_ptr(), c_json.as_ptr()) };
        let ptr = NonNull::new(ptr).ok_or_else(|| last_error("model load"))?;
        Ok(Self { ptr })
    }

    pub(super) fn as_ptr(&self) -> *const ffi::LocaModel {
        self.ptr.as_ptr()
    }

    pub fn meta(&self) -> Result<ModelMeta> {
        // SAFETY: `self.ptr` is valid for the life of `self`.
        let raw = unsafe { ffi::loca_model_meta_json(self.ptr.as_ptr()) };
        // SAFETY: `raw` is null or a fresh allocation from this ABI.
        let raw = unsafe { ShimString::from_raw(raw) }.ok_or_else(|| last_error("model meta"))?;
        serde_json::from_str(&raw.to_str()?)
            .map_err(|e| internal(format!("model metadata unreadable: {e}")))
    }

    pub fn n_vocab(&self) -> i32 {
        // SAFETY: `self.ptr` is valid for the life of `self`.
        unsafe { ffi::loca_n_vocab(self.ptr.as_ptr()) }
    }

    /// Embedding width, which is the length one [`Context::embed`] produces.
    pub fn n_embd(&self) -> i32 {
        // SAFETY: `self.ptr` is valid for the life of `self`.
        unsafe { ffi::loca_n_embd(self.ptr.as_ptr()) }
    }

    pub fn is_eog(&self, token: i32) -> bool {
        // SAFETY: `self.ptr` is valid for the life of `self`.
        unsafe { ffi::loca_token_is_eog(self.ptr.as_ptr(), token) }
    }

    /// Tokenizes `text`, sizing the buffer from the shim's own negated-capacity
    /// report rather than guessing.
    pub fn tokenize(&self, text: &str, add_special: bool, parse_special: bool) -> Result<Vec<i32>> {
        let c_text = c_string(text, "text to tokenize")?;
        // A byte-per-token upper bound is wrong for multi-byte scripts, so the
        // first call is a probe with whatever we have and the negative return
        // gives the exact size.
        let mut buf: Vec<i32> = vec![0; text.len().max(8)];
        // SAFETY: `buf` is writable for `buf.len()` i32s and `c_text` outlives
        // the call.
        let n = unsafe {
            ffi::loca_tokenize(
                self.ptr.as_ptr(),
                c_text.as_ptr(),
                add_special,
                parse_special,
                buf.as_mut_ptr(),
                buf.len() as i32,
            )
        };
        let n = if n < 0 {
            let needed = n.unsigned_abs() as usize;
            buf.resize(needed, 0);
            // SAFETY: same as above, with the capacity the shim just asked for.
            let n = unsafe {
                ffi::loca_tokenize(
                    self.ptr.as_ptr(),
                    c_text.as_ptr(),
                    add_special,
                    parse_special,
                    buf.as_mut_ptr(),
                    buf.len() as i32,
                )
            };
            if n < 0 {
                return Err(internal("tokenize still reported short after resizing"));
            }
            n
        } else {
            n
        };
        buf.truncate(n as usize);
        Ok(buf)
    }

    /// The text for one token. Not valid UTF-8 on its own for a token that is
    /// half a multi-byte character, so the bytes are returned and joined by the
    /// caller.
    pub fn token_bytes(&self, token: i32, special: bool) -> Result<Vec<u8>> {
        let mut buf: Vec<c_char> = vec![0; 64];
        // SAFETY: `buf` is writable for `buf.len()` bytes.
        let n = unsafe {
            ffi::loca_token_to_piece(
                self.ptr.as_ptr(),
                token,
                special,
                buf.as_mut_ptr(),
                buf.len() as i32,
            )
        };
        let n = if n < 0 {
            buf.resize(n.unsigned_abs() as usize, 0);
            // SAFETY: same, with the reported capacity.
            let n = unsafe {
                ffi::loca_token_to_piece(
                    self.ptr.as_ptr(),
                    token,
                    special,
                    buf.as_mut_ptr(),
                    buf.len() as i32,
                )
            };
            if n < 0 {
                return Err(internal(
                    "token_to_piece still reported short after resizing",
                ));
            }
            n
        } else {
            n
        };
        Ok(buf[..n as usize].iter().map(|&c| c as u8).collect())
    }
}

impl Drop for Model {
    fn drop(&mut self) {
        // SAFETY: freed exactly once; no `Clone`, and any `Context` or
        // `ChatTemplates` built from it holds an `Arc<Model>`.
        unsafe { ffi::loca_model_free(self.ptr.as_ptr()) }
    }
}

/// How to create an inference context.
#[derive(Debug, Clone, Serialize)]
pub struct ContextOptions {
    pub n_ctx: u32,
    pub n_batch: u32,
    pub n_threads: i32,
    pub flash_attn: bool,
    /// Put the context in embedding mode: [`Context::embed`] works and
    /// [`Context::decode`] does not.
    pub embeddings: bool,
    /// How per-token states collapse into one vector per sequence. Empty means
    /// the model's own trained pooling, which is what a sentence-embedding GGUF
    /// wants; naming one here overrides it and is almost always wrong.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub pooling_type: String,
}

impl Default for ContextOptions {
    fn default() -> Self {
        Self {
            n_ctx: 4096,
            n_batch: 512,
            n_threads: 4,
            flash_attn: false,
            embeddings: false,
            pooling_type: String::new(),
        }
    }
}

/// An inference context: the KV cache and the compute state for one sequence.
pub struct Context {
    ptr: NonNull<ffi::LocaContext>,
    /// Keeps the model alive for the context's whole life, and is where
    /// [`Context::logits`] gets its length from.
    model: std::sync::Arc<Model>,
}

// SAFETY: a `llama_context` may be moved between threads but must never be used
// from two at once; llama.cpp has no internal locking. `Sync` is therefore NOT
// implemented, and every method below takes `&mut self` so the borrow checker
// enforces exclusive use. The shipping core already runs one turn at a time
// (`--parallel 1`), so serialising costs nothing.
unsafe impl Send for Context {}

impl Context {
    pub fn new(model: std::sync::Arc<Model>, opts: &ContextOptions) -> Result<Self> {
        let json = serde_json::to_string(opts)
            .map_err(|e| internal(format!("context options are not serialisable: {e}")))?;
        let c_json = c_string(&json, "context options")?;
        // SAFETY: the model outlives the context (kept in `model`), and
        // `c_json` outlives the call.
        let ptr = unsafe { ffi::loca_context_new(model.ptr.as_ptr(), c_json.as_ptr()) };
        let ptr = NonNull::new(ptr).ok_or_else(|| last_error("context create"))?;
        Ok(Self { ptr, model })
    }

    pub fn n_ctx(&self) -> u32 {
        // SAFETY: `self.ptr` is valid for the life of `self`.
        unsafe { ffi::loca_context_n_ctx(self.ptr.as_ptr()) }
    }

    /// The model this context was built from.
    ///
    /// Returns a fresh `Arc` rather than a reference, so a caller can tokenize
    /// with the model and then call [`Context::embed`] or [`Context::decode`],
    /// which need `&mut self`. A borrow would make those two steps mutually
    /// exclusive for no reason: the model is immutable and shared by design.
    pub fn model(&self) -> std::sync::Arc<Model> {
        std::sync::Arc::clone(&self.model)
    }

    /// How many tokens this context currently holds.
    ///
    /// Read back from llama.cpp's own memory rather than tracked alongside it,
    /// so it cannot drift from the truth. 0 for a fresh or reset context.
    pub fn n_past(&self) -> i32 {
        // SAFETY: `self.ptr` is valid for the life of `self`.
        unsafe { ffi::loca_context_n_past(self.ptr.as_ptr()) }
    }

    /// Clears the KV cache so the context can serve a new sequence.
    pub fn reset(&mut self) {
        // SAFETY: `&mut self` guarantees no concurrent use.
        unsafe { ffi::loca_context_reset(self.ptr.as_ptr()) }
    }

    /// Evaluates `tokens`, APPENDING them after whatever the context holds.
    ///
    /// There is deliberately no `n_past` argument. There used to be one, and the
    /// shim `(void)`-cast it away while this doc comment and the header both
    /// said it positioned the batch: llama.cpp derives each position from the
    /// memory state itself (`llama-batch.cpp:90-117`), so the argument never did
    /// anything. Use [`Context::n_past`] to read the real value and
    /// [`Context::reset`] to start a new sequence.
    pub fn decode(&mut self, tokens: &[i32]) -> Result<()> {
        if tokens.is_empty() {
            return Err(internal("decode called with no tokens"));
        }
        // SAFETY: `tokens` is valid for `tokens.len()` reads for the call, and
        // `&mut self` guarantees exclusive use of the context.
        let rc =
            unsafe { ffi::loca_decode(self.ptr.as_ptr(), tokens.as_ptr(), tokens.len() as i32) };
        if rc != 0 {
            return Err(last_error("decode"));
        }
        Ok(())
    }

    /// Logits for the `i`-th position of the last decode; `-1` is the last.
    ///
    /// Borrowed from the context's own buffer and invalidated by the next
    /// [`Context::decode`], which is why the returned slice borrows `&mut self`:
    /// reading logits after the next decode does not compile. Copying instead
    /// would mean cloning a vocab-sized `f32` buffer on every token.
    ///
    /// The length is taken from the model this context was built from. It used
    /// to be a caller-supplied `n_vocab`, which made this a safe function that
    /// produced an out-of-bounds read from an ordinary wrong number, with no
    /// `unsafe` anywhere near the call site.
    pub fn logits(&mut self, i: i32) -> Result<&[f32]> {
        let n_vocab = self.model.n_vocab();
        if n_vocab <= 0 {
            return Err(internal("model reports an empty vocabulary"));
        }
        // SAFETY: `&mut self` guarantees exclusive use. The returned pointer is
        // valid for `n_vocab` floats until the next decode, and the borrow of
        // `self` returned alongside it prevents any intervening call.
        let ptr = unsafe { ffi::loca_get_logits(self.ptr.as_ptr(), i) };
        if ptr.is_null() {
            return Err(last_error("logits"));
        }
        // SAFETY: llama.cpp guarantees `n_vocab` contiguous floats at `ptr` for
        // a successful decode, and `n_vocab` is this context's OWN model's vocab
        // size, read above rather than supplied by the caller.
        Ok(unsafe { std::slice::from_raw_parts(ptr, n_vocab as usize) })
    }

    /// One pooled embedding vector for `tokens`.
    ///
    /// Requires a context built with [`ContextOptions::embeddings`]; on a
    /// generation context this fails rather than returning something plausible.
    /// The context is cleared inside the shim before each batch, so successive
    /// calls on one context are independent and a caller may embed many texts
    /// through it in sequence.
    ///
    /// Copied out rather than borrowed, unlike [`Context::logits`]. The aliasing
    /// dance there buys avoiding a vocab-sized clone once per generated token;
    /// here it would buy avoiding a 384-float clone a few times per memory,
    /// which is not worth a borrow that the next call invalidates.
    pub fn embed(&mut self, tokens: &[i32]) -> Result<Vec<f32>> {
        if tokens.is_empty() {
            return Err(internal("embed called with no tokens"));
        }
        let n_embd = self.model.n_embd();
        if n_embd <= 0 {
            return Err(internal("model reports no embedding width"));
        }
        let mut out = vec![0.0f32; n_embd as usize];
        // SAFETY: `tokens` is valid for `tokens.len()` reads and `out` for
        // `n_embd` writes, both for the duration of the call. `&mut self`
        // guarantees exclusive use of the context. The shim writes exactly
        // `n_embd` floats on success and none on failure.
        let written = unsafe {
            ffi::loca_embed(
                self.ptr.as_ptr(),
                tokens.as_ptr(),
                tokens.len() as i32,
                out.as_mut_ptr(),
                n_embd,
            )
        };
        if written != n_embd {
            return Err(last_error("embed"));
        }
        Ok(out)
    }

    pub(super) fn as_ptr(&mut self) -> *mut ffi::LocaContext {
        self.ptr.as_ptr()
    }
}

/// A loaded multimodal projector: the second model file that lets a LOCAL model
/// see an image.
///
/// # Why this is a separate handle rather than a flag on [`Model`]
///
/// Vision in llama.cpp is two files. The language model is one GGUF; the
/// projector ("mmproj") is another, and it is what turns pixels into embeddings
/// the language model attends to. They are loaded independently, the projector
/// is often as large as the model, and a user can have one without the other.
/// So "does this model support vision" is a question about the PAIR, which is
/// exactly the distinction the catalog's `vision` capability and the
/// `llm.supportImages` setting describe and the port previously could not
/// honour: the shim linked no `libmtmd` at all.
pub struct Mtmd {
    ptr: NonNull<ffi::LocaMtmd>,
    /// Keeps the model alive for the projector's whole life. mtmd holds a
    /// borrowed `llama_model *` internally, so dropping the model first would
    /// leave it dangling.
    _model: std::sync::Arc<Model>,
    /// Copied out at construction rather than fetched per call, because the
    /// C side returns a borrowed pointer and this is read on every prompt.
    marker: String,
}

// SAFETY: same reasoning as `Context`. `mtmd_helper_eval_chunks` is documented
// NOT thread-safe, so `Sync` is not implemented and the eval below takes
// `&mut self`; moving the handle between threads is fine, which is what the
// blocking-pool hop needs.
unsafe impl Send for Mtmd {}

/// How the projector is loaded.
#[derive(Debug, Clone, Serialize)]
pub struct MtmdOptions {
    pub n_threads: i32,
    /// Offload the vision encoder to the GPU when there is one.
    pub use_gpu: bool,
}

impl Default for MtmdOptions {
    fn default() -> Self {
        Self {
            n_threads: 4,
            use_gpu: true,
        }
    }
}

impl Mtmd {
    pub fn load(
        model: std::sync::Arc<Model>,
        mmproj_path: &str,
        opts: &MtmdOptions,
    ) -> Result<Self> {
        let json = serde_json::to_string(opts)
            .map_err(|e| internal(format!("mtmd options are not serialisable: {e}")))?;
        let c_json = c_string(&json, "mtmd options")?;
        let c_path = c_string(mmproj_path, "mmproj path")?;
        // SAFETY: the model outlives this handle (kept in `_model`), and both
        // C strings outlive the call.
        let ptr =
            unsafe { ffi::loca_mtmd_init(model.ptr.as_ptr(), c_path.as_ptr(), c_json.as_ptr()) };
        let ptr = NonNull::new(ptr).ok_or_else(|| last_error("mmproj load"))?;

        // SAFETY: `ptr` is valid, and the returned pointer is borrowed from the
        // handle and is NUL-terminated. Copied immediately.
        let marker = unsafe { std::ffi::CStr::from_ptr(ffi::loca_mtmd_marker(ptr.as_ptr())) }
            .to_string_lossy()
            .into_owned();

        Ok(Self {
            ptr,
            _model: model,
            marker,
        })
    }

    /// The token a prompt must contain once per image, e.g. `<__media__>`.
    ///
    /// Read from the projector rather than hardcoded: it is model-family
    /// specific, and a wrong marker does not fail loudly, it just leaves the
    /// image unattached to any position in the prompt.
    pub fn marker(&self) -> &str {
        &self.marker
    }

    /// Whether this projector handles IMAGES.
    ///
    /// A projector can be audio-only. One that loaded successfully still cannot
    /// answer an image prompt in that case, and asking is the only way to know.
    pub fn supports_vision(&self) -> bool {
        // SAFETY: `self.ptr` is valid for the life of `self`.
        unsafe { ffi::loca_mtmd_supports_vision(self.ptr.as_ptr()) }
    }

    /// Evaluate an interleaved text-and-image prompt into `context`, returning
    /// the new `n_past`.
    ///
    /// `prompt` must contain exactly `images.len()` copies of
    /// [`marker`](Self::marker). This REPLACES [`Context::decode`] for the turn:
    /// the image embeddings have to land between the text spans at the marker
    /// positions, so a caller that decoded the text itself would have nowhere to
    /// put them.
    ///
    /// `images` are ENCODED files (png, jpeg, ...), not raw pixels. Decoding
    /// stays on the C++ side, where stb_image already lives.
    pub fn eval(
        &mut self,
        context: &mut Context,
        prompt: &str,
        add_special: bool,
        images: &[Vec<u8>],
        n_batch: i32,
    ) -> Result<i32> {
        let c_prompt = c_string(prompt, "multimodal prompt")?;
        // Two parallel arrays rather than a struct, so the ABI carries no
        // layout. Both borrow `images` and live until the call returns.
        let ptrs: Vec<*const u8> = images.iter().map(|i| i.as_ptr()).collect();
        let lens: Vec<usize> = images.iter().map(Vec::len).collect();

        // SAFETY: `ptrs` and `lens` are valid for `images.len()` reads, each
        // pointer is valid for its matching length, and all of it outlives the
        // call because `images` is borrowed for the whole function. `&mut self`
        // and `&mut context` give exclusive use of both handles, which
        // `mtmd_helper_eval_chunks` requires (it is documented not thread-safe).
        let new_n_past = unsafe {
            ffi::loca_mtmd_eval(
                self.ptr.as_ptr(),
                context.as_ptr(),
                c_prompt.as_ptr(),
                add_special,
                ptrs.as_ptr(),
                lens.as_ptr(),
                images.len() as i32,
                n_batch,
            )
        };
        if new_n_past < 0 {
            return Err(last_error("multimodal eval"));
        }
        Ok(new_n_past)
    }
}

impl Drop for Mtmd {
    fn drop(&mut self) {
        // SAFETY: freed exactly once; no `Clone`, and the model outlives it.
        unsafe { ffi::loca_mtmd_free(self.ptr.as_ptr()) }
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        // SAFETY: freed exactly once; no `Clone`, and the model outlives it.
        unsafe { ffi::loca_context_free(self.ptr.as_ptr()) }
    }
}
