//! Local inference: the llama.cpp C ABI shim, the token loop, and the sampler
//! chain behind the `ChatProvider` seam. The engine emits `LlmDelta` directly,
//! with no SSE layer.
//!
//! `local-inference` is off by default (it compiles vendored llama.cpp), so the
//! decision logic (`message`, `diff`, `ids`, `stop`) is ungated and names no
//! `shim` type; only the C-touching glue is gated.

#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub mod backends;
pub mod diff;
pub mod ids;
pub mod message;
pub mod options;
pub mod prompt;
pub mod sampling;
pub mod stop;
pub mod text;
pub mod vision;

#[cfg(feature = "local-inference")]
pub mod embed;
#[cfg(feature = "local-inference")]
pub mod engine;
#[cfg(feature = "local-inference")]
pub mod generate;
#[cfg(feature = "local-inference")]
pub mod idle;
#[cfg(feature = "local-inference")]
pub mod provider;
#[cfg(feature = "local-inference")]
pub mod runtime;
#[cfg(feature = "local-inference")]
pub mod shim;
