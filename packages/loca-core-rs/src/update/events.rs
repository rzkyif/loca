//! What an update tells the rest of the core: the WS hub broadcasts
//! `update.staged` / `update.error`, and the core-status aggregator flips to
//! "updating" on staged only. A failed update did not change the binary, so
//! `failed` deliberately pushes no status.
//!
//! The frame maps `InternalError` to `UpdateFailed` (the code an unclassified
//! failure carries on the wire); the HTTP response keeps the real code.

use std::sync::Arc;

use crate::contract::api::ServerToClientFrame;
use crate::error::{AppError, ErrorCode};
use crate::transport::status::CoreStatusService;
use crate::transport::FrameBus;

/// The two consumers, threaded rather than reached for: `AppContext` only
/// exposes `snapshot()` on its status provider.
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

/// The code the FRAME carries. Converted through the wire string: both enums
/// are generated from one list, and the round-trip test catches drift.
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
        assert_eq!(
            frame_code(ErrorCode::InternalError),
            crate::contract::api::ErrorCode::UpdateFailed
        );
    }

    #[test]
    fn every_error_code_survives_the_wire_conversion() {
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
