//! Supervision of the remaining child processes, and the registry the aggregate
//! core status pulls its subsystem list from.
//!
//! Inference and speech run in-process, so no long-lived, health-checked,
//! auto-restarted child exists and there are no readiness probes or restart
//! backoff. Two jobs are left:
//!
//! - **Child-process mechanics** ([`process`]), for the pinned `deno` tool
//!   workers: an explicit environment, pipes drained so they cannot deadlock, a
//!   graceful kill with a hard fallback, and orphan cleanup.
//! - **The subsystem registry** ([`Manager`]), implementing
//!   [`crate::transport::status::SidecarSource`]. An in-process engine has no
//!   pid but still has a state the core bar renders, so this is a registry each
//!   subsystem publishes into rather than a table of live processes.

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
