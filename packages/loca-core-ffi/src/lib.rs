//! The mobile frontend to Loca Core.
//!
//! Owned by work package M1. This crate boots the core in process and marshals
//! HTTP and WebSocket traffic across the Tauri command boundary; the Tauri crate
//! depends on it only under `cfg(any(target_os = "android", target_os = "ios"))`,
//! so a desktop build never links it.
//!
//! **There is deliberately no loopback listener.** `127.0.0.1` is not
//! app-private on Android, so binding a port would expose the core to every
//! installed app and drag in `usesCleartextTraffic` / `NSAllowsLocalNetworking`.
//! Traffic goes through `transport::inproc` instead.
//!
//! # Why the logic lives here rather than in the Tauri crate
//!
//! Everything below compiles and is TESTED ON THE HOST. Inside the Tauri crate
//! the same code would only compile under `#[cfg(mobile)]` and could never be
//! run by a test binary, because `cargo test` is host-target. D-117 is what
//! happens when the mobile half of this project has no coverage, so the split is
//! the point: `commands/core.rs` on the Tauri side is delegation and one
//! [`WsSink`] implementation, and every rule with an ordering or a lifetime to
//! get wrong is in this crate.

pub mod http;
pub mod local;
pub mod ws;

/// The core itself, re-exported so the Tauri crate has ONE mobile dependency
/// rather than two that must be kept in step. The command layer needs the
/// core's error type to map it onto the client's, and nothing else from here.
pub use loca_core;

pub use http::{CoreHttpReply, CoreHttpRequest};
pub use local::{CoreInfo, LocalCore, StartOnce};
pub use ws::{Connections, WsEvent, WsSink};
