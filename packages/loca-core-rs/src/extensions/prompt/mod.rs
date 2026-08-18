//! The runtime permission-prompt state machine: what a tool worker's terminal
//! says, and what Core does about it.
//!
//! A worker spawns WITHOUT `--no-prompt` when a prompt context was supplied, so
//! any access outside the baked `--allow-*` set makes Deno pause and print an
//! interactive prompt. Core reads it off the PTY, decides on it, and either
//! auto-answers or forwards it to the user in chat. Without a terminal the same
//! access is a hard `NotCapable`.
//!
//! So [`parser`] is what turns "the sandbox said no" into "ask the user", and
//! [`matcher`] is what decides whether asking is even allowed. A parser bug is
//! either a hang (no answer, and the worker is killed after the give-up budget)
//! or, in the bad direction, a `y` sent to a prompt the user never saw.

/// The tripwire for the pinned `deno`: real prompts, a real terminal, real
/// answers. Desktop only, and `#[ignore]`d by default; see the module.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub mod live_probe;
pub mod matcher;
pub mod parser;

pub use matcher::{decide_prompt, PromptAction, PromptContext, PromptDecision};
pub use parser::{PromptParser, PromptParserEvent};
