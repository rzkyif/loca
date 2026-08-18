//! Loca Core: the local-first AI service.
//!
//! One library, two frontends. On desktop the `loca-core` binary in this crate
//! serves it over HTTPS/WSS. On mobile the `loca-core-ffi` crate links it into
//! the Tauri app and the client talks to it in-process, so the phone embeds the
//! real core rather than a reimplementation of it.
//!
//! # Module tree
//!
//! Every module maps to exactly one work package in
//! `docs/rust-port/registry.md`, and a package writes only inside its own
//! module. This file, [`error`], [`contract`], `transport::router`, and
//! `schema.sql` are shared surfaces: orchestrator-owned, so a package that needs
//! a change there files a request in `docs/rust-port/shared/interfaces.md`
//! rather than editing them.
//!
//! Each domain module owns its own route slice at `<module>/routes.rs`; the
//! router that mounts them lives in [`transport`].

// Foundations.
pub mod boot;
pub mod config;
pub mod contain;
pub mod contract;
pub mod db;
pub mod error;
pub mod external;
pub mod files;
pub mod log;
pub mod paths;
pub mod secrets;
/// The `ToolHost` join: the one type that names `extensions`, `mcp` and `llm`
/// together, so that none of the three has to name another.
pub mod tool_host;
pub mod transport;

pub mod auth;

// Domain.
pub mod chat;
pub mod downloads;
pub mod extensions;
pub mod llm;
pub mod memories;
pub mod models;
pub mod requirements;
pub mod scheduled;
pub mod sessions;
pub mod settings;
pub mod sidecars;
pub mod storage;
pub mod update;

pub mod mcp;

// Native.
pub mod inference;
pub mod speech;

pub use config::{Channel, Config, CORE_VERSION};
pub use error::{AppError, ErrorCode, Result};
