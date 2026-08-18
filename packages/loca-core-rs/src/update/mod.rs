//! Self-update, boot-time rollback, and the install/service subcommands.
//!
//! Work package **D13** (`docs/rust-port/registry.md`). This module is that
//! package's owned path; nothing outside it writes here.
//!
//! # What this is
//!
//! The code that replaces the core's own binary while it is running, decides on
//! the next boot whether to keep the replacement, and provides the CLI verbs
//! every native installer wraps. Ported from
//! `packages/loca-core/src/update/*` and `packages/loca-core/src/install/*`.
//!
//! # Layout
//!
//! | file            | what it owns                                                          |
//! | --------------- | --------------------------------------------------------------------- |
//! | `net.rs`        | the HTTP client, the two bounds, the call counter                     |
//! | `manifest.rs`   | `core.json`: shape, raw-bytes signature verification, the trust root  |
//! | `artifact.rs`   | stream, gunzip, streaming sha256, atomic placement                    |
//! | `swap.rs`       | the verified anchor, the in-place swap, the crash-window table        |
//! | `apply.rs`      | check and apply, the artifact staging, the supervisor handoff         |
//! | `events.rs`     | `update.staged` / `update.error`, and the status push                 |
//! | `helpers.rs`    | the boot-time helper check (D-025), and the aside repair              |
//! | `routes.rs`     | `GET /update/check`, `POST /update/apply`, and the exit seam          |
//! | `install/`      | the command seam and supervisor detection                             |
//! | `marker.rs`     | the update marker, the one state that survives the swap               |
//! | `rollback.rs`   | the boot decision, the anchor check, the four-phase restore           |
//! | `testing.rs`    | the mock upstream and the transcribed published manifest (test only)  |
//!
//! One more file is test-only and answers a question rather than porting
//! anything: `windows_dir_swap_spike.rs` measures, on a `windows-latest` CI
//! runner, whether a directory can be renamed while a DLL loaded from inside it
//! is mapped into the process. That is the single unknown deciding whether the
//! D-106 directory swap can stay directory-granular on Windows. See D-153 and
//! `docs/rust-port/packages/D13-WIN/status.md`.
//!
//! # The three properties to preserve
//!
//! **1. A signature covers bytes.** Verification never runs over a
//! re-serialised struct, so a field a future release adds does not silently
//! vanish out of the message and read as a forgery on every deployed core. See
//! `manifest.rs` and D-065.
//!
//! **2. A validly-signed OLDER manifest is still validly signed.** The version
//! ordering is the only thing between a replayed release and a rollback to a
//! fixed vulnerability, and it is `crate::config::compare_semver` (D-053), not
//! a second comparator. A naive string compare gets `1.10.0` versus `1.9.0`
//! backwards, which is the whole reason that function exists.
//!
//! **3. Nothing here reaches the network at boot or on a timer.** The two
//! update paths are behind a button and the two install paths run in the
//! install phase, which is the one exception `AGENTS.md` names. `net.rs` has
//! the full table and the tests that hold it.
//!
//! # The rollback anchor is mandatory, and existing is not the same as usable
//!
//! Every swap preserves the previous binary as `<bin>.old` before it replaces
//! anything, and refuses the whole update if it cannot. Without it the
//! boot-time rollback has nothing to restore, so a broken release becomes a
//! machine that needs a manual reinstall.
//!
//! `rollback.rs` additionally refuses an anchor that is present but cannot run,
//! because installing one is strictly worse than leaving the binary that at
//! least might: it turns "did not reach the healthy checkpoint" into "cannot
//! exec at all", and the marker is cleared either way so there is no second
//! attempt. See its module docs for the case that is reachable today and for
//! the one this cannot catch, which is `swap.rs`'s to close.

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

/// The D-106 Windows spike. Test-only and Windows-only, so it costs every other
/// host nothing: a `cfg`-false out-of-line module is never even loaded.
#[cfg(all(test, windows))]
mod windows_dir_swap_spike;
