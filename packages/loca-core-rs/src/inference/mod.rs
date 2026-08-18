//! Local inference: the llama.cpp C ABI shim, the token loop, and the sampler
//! chain, behind D3's `ChatProvider` seam.
//!
//! Work packages **N1** (`shim/`) and **N2** (everything else), per
//! `docs/rust-port/registry.md`. Nothing outside this module writes here.
//!
//! # There is no SSE layer, and that is deliberate
//!
//! Server-sent events are the EXTERNAL path's business. D3 built
//! [`crate::llm::ChatProvider`] as the seam Stage 3 swaps, and the in-process
//! engine implements it by emitting [`crate::llm::LlmDelta`] directly: no
//! encoder, no decoder, no bytes. The obligation is not a shape but a property,
//! and it is testable without llama.cpp: **the delta sequence emitted here must
//! be indistinguishable from what `llm::sse::SseDecoder` plus
//! `ChatChunk::to_delta` would have produced from `llama-server`'s stream.**
//! `diff::tests` asserts exactly that by running one scripted generation through
//! both paths.
//!
//! # The split, and why it is where it is
//!
//! `local-inference` is OFF by default: it compiles llama.cpp from
//! `vendor/llama.cpp`, which CI does not check out. A feature-gated safety net
//! does not run, so this module is split on the line "does it touch C" rather
//! than on subject matter:
//!
//! - **Always compiled, always in the default gate:** [`message`], [`diff`],
//!   [`ids`], [`stop`]. These carry the decisions that are invisible when wrong
//!   (which deltas are emitted, in what order, with what ids) and they need no
//!   GGUF, no feature flag and no llama.cpp.
//! - **Gated on `local-inference`:** [`shim`] and, as they land, the engine and
//!   the token loop. Thin glue: own a handle, call in a loop, push to a channel.
//!
//! Nothing in the ungated half names a type from [`shim`]. That is not
//! tidiness: it is what lets the shim's surface change (it is being redesigned
//! around `common_sampler` under D-136) without touching a line of the decision
//! logic. One conversion per type, in the gated half, absorbs it.

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
