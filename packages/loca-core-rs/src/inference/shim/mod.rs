//! The C ABI over llama.cpp and the safe Rust surface above it. Mobile cannot
//! exec a downloaded `llama-server`, so the engine is compiled in. Rust binds
//! only `loca_llama.h`: handles are opaque and compound values are JSON, so a
//! llama.cpp struct change is a C++ compile error, not a silent misalignment.
//!
//! Handles nest via `Arc` (a `Context` keeps its `Model` alive) and every
//! `char *` is freed only by [`owned::ShimString`]. `ggml_backend_load_all*` is
//! deliberately unbound: it `dlopen`s from the working directory or
//! `GGML_BACKEND_PATH`, which for a service is arbitrary code execution.

// Mobile is CPU-only and statically linked: no backend modules, no downloaded
// code (W^X, App Store 2.5.2), and the per-ABI APK budget assumes it.
#[cfg(all(
    any(target_os = "android", target_os = "ios"),
    loca_llama_link = "shared"
))]
compile_error!(
    "mobile targets must link llama.cpp statically. The shared/backend-DL mode is \
     desktop-only: it ships loadable backend modules, which Android W^X and App Store \
     2.5.2 forbid, and it would grow the per-ABI payload well past the ~8.1 MB the \
     APK-split budget assumes."
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
