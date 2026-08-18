//! Supervision of the remaining child processes, and the registry the aggregate
//! core status pulls its subsystem list from.
//!
//! Work package **D14** (`docs/rust-port/registry.md`). This module is that
//! package's owned path; nothing outside it writes here.
//!
//! # Why this is small
//!
//! The Deno core supervised three server-style children: two `llama-server`
//! instances and `loca-core-speech`. All three are absorbed into this process
//! (N1/N2 and N3), so **no long-lived, health-checked, auto-restarted child
//! survives**. With them go the readiness probes, the start supersession, the
//! restart backoff and the crash flap guard: every one of those exists to keep a
//! local HTTP server alive, and there is no longer a local HTTP server.
//!
//! What is left is two jobs that happen to share a name:
//!
//! - **Child-process mechanics** ([`process`]), for the pinned `deno` tool
//!   workers D12 spawns. An explicit environment, pipes drained so they cannot
//!   deadlock, a graceful kill with a hard fallback, and orphan cleanup when the
//!   core dies without running its shutdown path.
//! - **The subsystem registry** ([`Manager`]), which implements
//!   [`crate::transport::status::SidecarSource`]. An in-process engine has no
//!   pid, but it still has a state the core bar renders, so the source became a
//!   registry that N2, N3 and D12 publish into rather than a table of live
//!   processes.

pub mod deno;
mod jobctl;
pub mod metrics;
pub mod process;
mod registry;
pub mod routes;

#[cfg(test)]
mod tests;

pub use process::{Child, Exit, Output, Spawn, Stdin};
pub use registry::{ChildHandle, Manager, SubsystemControl};
