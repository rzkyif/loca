//! Self-update, boot-time rollback, and the install/service subcommands: swap
//! the running binary, decide on the next boot whether to keep it, and provide
//! the CLI verbs every native installer wraps.
//!
//! Three invariants: a signature is verified over the raw manifest bytes, never
//! a re-serialised struct; a validly-signed OLDER manifest is refused by
//! `crate::config::compare_semver` (replay is not a downgrade path); and nothing
//! here touches the network at boot or on a timer. Every swap preserves the
//! previous binary as `<bin>.old` first, and refuses the update if it cannot,
//! since without that anchor rollback has nothing to restore.

pub mod apply;
pub mod artifact;
pub mod events;
pub mod helpers;
pub mod install;
pub mod manifest;
pub mod marker;
pub mod net;
pub mod rollback;
pub mod routes;
pub mod swap;

#[cfg(test)]
pub mod testing;
