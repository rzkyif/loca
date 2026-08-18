//! The wire contract: Rust types generated from `@loca/shared`, which stays the
//! single source of truth for it.
//!
//! Work package **F2** (`docs/rust-port/registry.md`). Both submodules are
//! generated. Do not edit them; change the TypeScript and run
//! `deno task contract`. `deno task contract:check` runs in the gate, because a
//! stale generated file is otherwise invisible: the Rust compiles fine against
//! last week's contract.
//!
//! The split is by trust boundary, not by file of origin (D-003):
//!
//! - [`inbound`] comes from the Zod validators and covers everything the core
//!   validates on the way in. typify carries Zod's `.min` / `.max` / `.int` /
//!   `.regex` constraints into a `Deserialize` that enforces them, so
//!   **deserializing the type IS the validation**. Do not hand-write a second
//!   check after it; do not reach for a looser type to "avoid the error".
//! - [`api`] comes from the TypeScript and covers everything else: responses,
//!   the server-to-client frames the core produces, and the domain shapes both
//!   sides render. None of it carries a constraint TypeScript could have
//!   expressed, so nothing is lost by generating it from there.
//!
//! Some shapes appear in both, under the same name, with different strictness.
//! That is the partition working, not a bug: pick the one matching the
//! direction of travel. Parsing a client frame is [`inbound`]; building a frame
//! to send is [`api`].

pub mod api;
pub mod inbound;
