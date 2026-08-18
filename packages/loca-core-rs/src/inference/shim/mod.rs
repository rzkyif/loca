//! The C ABI over llama.cpp, and the safe Rust surface above it.
//!
//! Desktop today downloads upstream's `llama-server` and spawns it. That is
//! impossible on mobile: Android `targetSdk 36` enforces W^X so an app cannot
//! exec a binary it wrote to app-private storage, and iOS forbids executing
//! downloaded code outright. So the engine is compiled in, and this module is
//! the seam.
//!
//! # Why `common`, and not a crate off the shelf
//!
//! Jinja chat-template application, per-format tool-call extraction, GBNF
//! grammar constraint and the `--reasoning-format deepseek` `<think>` split all
//! live in llama.cpp's `common/chat.cpp`, **not** in `libllama`. Every published
//! Rust binding wraps `libllama` only, so adopting one would silently drop
//! exactly the three capabilities every model in the catalog advertises
//! (`{tools, vision, reasoning}`). We link `libllama` + `ggml` + `llama-common`.
//!
//! # Where the boundary is
//!
//! Rust binds only [`loca_llama.h`](../shim/loca_llama.h). No llama.cpp struct,
//! enum or by-value parameter crosses: handles are opaque and compound values
//! are JSON. That is deliberate. The shim is compiled against llama.cpp's real
//! headers, so a version bump that changes a struct is a C++ compile error
//! naming the field. A hand-declared or `bindgen`-generated struct on the Rust
//! side would instead be silently misaligned at runtime, which is the failure
//! mode this port can least afford.
//!
//! # Ownership contract
//!
//! Restated here because it is the thing an FFI reviewer needs, and enforced by
//! types rather than by discipline:
//!
//! - Every `char *` the shim returns was `malloc`ed by the shim.
//!   [`owned::ShimString`] is the only holder and frees it on drop. Nothing here
//!   uses `CString::from_raw`, which would free a `malloc` block with Rust's
//!   allocator.
//! - Every handle has exactly one owning Rust struct with a `Drop`. Ownership is
//!   hierarchical and expressed with `Arc`: a [`model::Context`],
//!   [`sampler::SamplerChain`] or [`chat::ChatTemplates`] holds its
//!   [`model::Model`] alive, so use-after-free is not reachable.
//! - [`owned::last_error`] copies out of a thread-local the next call
//!   overwrites; the pointer is never stored.
//! - [`model::Context::logits`] returns the one borrowed value, tied to
//!   `&mut Context` so reading it after the next decode does not compile. Its
//!   length comes from the context's own model, never from the caller: it used
//!   to take an `n_vocab` argument, which made a safe function produce an
//!   out-of-bounds read from an ordinary wrong number.
//! - `Send`/`Sync` are written explicitly, never inferred. [`model::Model`],
//!   [`chat::ChatTemplates`] and [`chat::ChatTurn`] are `Send + Sync`;
//!   [`model::Context`] and [`sampler::SamplerChain`] are `Send` only, because
//!   llama.cpp has no internal locking for either. The `Sync` justifications
//!   name what was actually inspected in the pinned tree rather than appealing
//!   to C++ `const`, which permits `mutable` members and shared caches and so
//!   guarantees nothing about threads.
//!
//! # What is deliberately absent
//!
//! BOTH of ggml's convenience backend loaders, for the same reason in two
//! halves (D-107):
//!
//! - `ggml_backend_load_all`. Its default search path includes the process
//!   working directory and it `dlopen`s anything matching `libggml-*.so` it
//!   finds there, which for a service startable from any directory is arbitrary
//!   code execution.
//! - `ggml_backend_load_all_from_path`. It confines the DIRECTORY SEARCH to the
//!   path given, closing that half, but it then reads the `GGML_BACKEND_PATH`
//!   environment variable and `dlopen`s whatever it names. For a service whose
//!   environment is not always the operator's, that is the same vector through
//!   a different door. This was confirmed by observing the `dlopen` attempt, not
//!   inferred, and it means the earlier claim that the explicit-path form was
//!   safe was wrong.
//!
//! Neither is in the header, neither is in [`ffi`], and the shim's C++ poisons
//! both identifiers so neither can be reintroduced there. [`model::backend_load`]
//! takes one explicit path; [`model::backend_load_dir`] enumerates one explicit
//! directory in C++ and loads each module by path. No code path in this module
//! opens a library whose path did not come from the caller.
//!
//! # Scope
//!
//! N1 owns everything that touches C; N2 owns everything that decides. So the
//! token loop, the composition of the sampler chain, and the SSE layer are not
//! here: this module offers the pieces and takes no position on how they are
//! used.

// The mobile build must stay statically linked. Android and iOS are CPU-only
// for v1: they load no backend module and download no executable code, which is
// what W^X and App Store 2.5.2 require, and it is what keeps the linked
// artifact at the ~8.1 MB that Stage 4's per-ABI APK budget is built on.
// Switching mobile to the desktop shared-library path for consistency would
// break all three at once and would do it quietly, so it breaks the build here
// instead.
#[cfg(all(
    any(target_os = "android", target_os = "ios"),
    loca_llama_link = "shared"
))]
compile_error!(
    "mobile targets must link llama.cpp statically. The shared/backend-DL mode is \
     desktop-only: it ships loadable backend modules, which Android W^X and App Store \
     2.5.2 forbid, and it would grow the per-ABI payload well past the ~8.1 MB the \
     APK-split budget assumes. See build.rs LinkMode and D-106."
);

pub mod chat;
pub mod ffi;
pub mod model;
pub mod owned;
pub mod sampler;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests;

pub use chat::{
    AppliedChat, AssistantMessage, ChatInputs, ChatTemplates, ChatTurn, GrammarTrigger,
    ReasoningFormat, ToolCall, ToolCallFunction,
};
pub use model::{
    backend_init, backend_load, backend_load_dir, devices, Context, ContextOptions, Device,
    DeviceKind, Model, ModelMeta, ModelOptions, Mtmd, MtmdOptions,
};
pub use sampler::{
    GrammarKind, ReasoningBudget, SamplerChain, SamplerKind, SamplerSpec, SamplerTrigger,
};
