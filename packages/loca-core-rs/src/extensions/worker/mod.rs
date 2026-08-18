//! One tool worker's lifetime: the protocol it speaks, the process behind it,
//! and the pool that keeps a few warm.
//!
//! Ports `worker-handle.ts`, `worker-pool.ts`, `worker-protocol.ts`,
//! `worker-call.ts`, `worker-call-timeout.ts`, `worker-entry-resolution.ts` and
//! `worker-display-bounds.ts`.

pub mod args;
pub mod call;
pub mod display;
pub mod entry;
pub mod handle;
pub mod pool;
pub mod protocol;
pub mod spawn_plan;
/// Desktop only, for the same reason [`crate::extensions::pty`] is: this is
/// the module that actually starts a downloaded interpreter, which is the
/// thing Android's W^X policy and App Store rule 2.5.2 forbid.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub mod spawner;
pub mod timeout;

/// How long a freshly spawned worker gets to report `booted`.
///
/// Every other budget in the spawn path is sized against this one: the control
/// socket's accept gives up first, so a worker that never connects is reported
/// as the socket failure it is rather than as a generic boot timeout naming the
/// wrong subsystem.
pub const BOOT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
