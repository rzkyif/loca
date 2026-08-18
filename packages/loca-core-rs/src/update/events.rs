//! What an update tells the rest of the core, and the two consumers of it.
//!
//! The oracle fans `emitUpdate` out to exactly two subscribers: the WS hub
//! broadcasts `update.staged` / `update.error` to every client
//! (`ws/hub.ts:467-478`), and the core-status aggregator flips to "updating" on
//! staged only (`core-status.ts:103-108`). Both are kept.
//!
//! # `update.error` deliberately pushes no status
//!
//! Only `staged` sets `updating`. That reads like an omission and is not: a
//! failed update did not change the binary, so the core is not updating, and
//! leaving the bar stuck on "updating" after a failure would be worse than
//! saying nothing. Stated here because the asymmetry is one line and the next
//! reader will otherwise "fix" it.
//!
//! # The error code a frame carries is not always the error's own
//!
//! `applyUpdate` (`self-updater.ts:94-99`) emits `err.code` for an `AppError`
//! and the literal `update_failed` for anything else. In Rust everything is an
//! `AppError`, and the code a plain JavaScript `Error` would have become is
//! `InternalError` (a transport failure, an io error). So `InternalError` is
//! mapped back to `UpdateFailed` on the way into the frame and every other code
//! passes through, which reproduces the oracle exactly rather than approximately.
//!
//! The HTTP response is untouched by that mapping and still carries the real
//! code, which is also what the oracle does: it re-throws the original error
//! after emitting. So a transport failure is `update_failed` on the frame and
//! `internal_error` on the response, and that asymmetry is the specification's.

use std::sync::Arc;

use crate::contract::api::ServerToClientFrame;
use crate::error::{AppError, ErrorCode};
use crate::transport::status::CoreStatusService;
use crate::transport::FrameBus;

/// The two consumers, threaded rather than reached for.
///
/// `AppContext` carries `Arc<dyn StatusProvider>`, which exposes only
/// `snapshot()`, so the concrete service has to be passed in. Same shape as the
/// sidecar manager and the LLM scheduler.
#[derive(Clone)]
pub struct UpdateEvents {
    frames: Arc<FrameBus>,
    status: Option<Arc<CoreStatusService>>,
}

impl UpdateEvents {
    pub fn new(frames: Arc<FrameBus>, status: Option<Arc<CoreStatusService>>) -> Self {
        Self { frames, status }
    }

    /// An update has been staged and the core is about to restart.
    pub fn staged(&self, version: &str) {
        self.frames
            .broadcast_all(&ServerToClientFrame::UpdateStaged {
                version: version.to_string(),
            });
        if let Some(status) = &self.status {
            status.note_update_staged();
        }
        tracing::info!(target: "loca::update", "update {version} staged");
    }

    /// An update failed. No status push; see the module docs.
    pub fn failed(&self, err: &AppError) {
        self.frames
            .broadcast_all(&ServerToClientFrame::UpdateError {
                code: frame_code(err.code),
                message: err.message.clone(),
            });
    }
}

/// The code the FRAME carries, which is not always the error's own.
///
/// Converted through the wire string rather than a second 47-arm match. Both
/// enums are generated from one list (`api/errors.ts`), so a variant that fails
/// to parse means the two have drifted, which is a build-level problem rather
/// than something to handle per call: the fallback keeps the frame legible and
/// `every_error_code_survives_the_wire_conversion` is what turns the drift into
/// a red test instead of a silently degraded frame.
///
/// D4 has the same conversion as a private macro in `chat/turn.rs`. Two copies
/// of a mapping is the D-049 shape, and hoisting it is filed rather than done
/// here, because `chat/` is not this package's to edit.
fn frame_code(code: ErrorCode) -> crate::contract::api::ErrorCode {
    let mapped = if code == ErrorCode::InternalError {
        ErrorCode::UpdateFailed
    } else {
        code
    };
    mapped
        .as_str()
        .parse()
        .unwrap_or(crate::contract::api::ErrorCode::UpdateFailed)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn an_unclassified_failure_reaches_the_client_as_update_failed() {
        // The oracle emits the literal "update_failed" for anything that is not
        // an AppError, and in Rust that class arrives as InternalError. Without
        // the mapping a network drop mid-download would surface to the client
        // as a generic internal error, which says nothing about what failed.
        assert_eq!(
            frame_code(ErrorCode::InternalError),
            crate::contract::api::ErrorCode::UpdateFailed
        );
    }

    #[test]
    fn every_error_code_survives_the_wire_conversion() {
        // The conversion goes through a string, so a variant the contract does
        // not carry would silently become `update_failed` on every frame. This
        // is what makes that drift a red test rather than a quiet degradation,
        // and it is the reason the string form is acceptable at all.
        for code in ErrorCode::ALL {
            let wire = frame_code(*code);
            let expected = if *code == ErrorCode::InternalError {
                "update_failed"
            } else {
                code.as_str()
            };
            assert_eq!(wire.to_string(), expected, "{code:?} did not round-trip");
        }
    }

    #[test]
    fn a_classified_failure_keeps_its_own_code() {
        // The other half, and the one a single test cannot see: mapping
        // everything to update_failed would also pass a test that only checks
        // the internal-error case.
        for code in [
            ErrorCode::ManifestFetchFailed,
            ErrorCode::SignatureInvalid,
            ErrorCode::ChecksumMismatch,
            ErrorCode::UpdateFailed,
        ] {
            assert_eq!(
                frame_code(code).to_string(),
                code.as_str(),
                "{code:?} was rewritten"
            );
        }
    }
}
