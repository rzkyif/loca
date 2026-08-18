//! `AppError`: returned from anywhere in the core, serialized to the wire
//! envelope `{ error: { code, message, details? } }` by the HTTP error layer.
//!
//! `ErrorCode` and its status mapping are the Rust side of
//! `@loca/shared`'s `api/errors.ts`. The set is closed on purpose: a code that
//! is not on the wire contract cannot reach a client, and the client switches on
//! these strings.

use serde::{Deserialize, Serialize};

/// Every error code the core can return. Serializes to the snake_case string
/// the wire contract uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    // Auth / pairing
    InvalidToken,
    Revoked,
    MissingToken,
    AdminTokenRequired,
    AdminPasswordRequired,
    AdminPasswordInvalid,
    AdminPasswordRateLimited,
    InvalidPairingCode,
    PairingCodeExpired,
    PairingCodeClaimed,
    PairingRateLimited,

    // Generic resource
    NotFound,
    Forbidden,
    ValidationError,
    Conflict,

    // Sessions / chat
    SessionBusy,
    SessionNotFound,
    MessageNotFound,
    StreamNotFound,
    ContextWindowExceeded,

    // LLM / scheduling
    ServerBusy,
    ServerUnavailable,
    ProviderError,
    ProviderUnauthorized,
    ProviderRateLimited,

    // Extensions
    NoLocaJson,
    InvalidLocaJson,
    DepsInstallFailed,
    TarballFetchFailed,
    ExtractFailed,
    ExtensionNotFound,
    ToolNotFound,
    PermissionsRequired,
    PermissionsRevoked,
    ExtensionHashDrift,
    ExtensionAlreadyInstalled,
    ToolNameCollision,

    // Models / binaries
    ModelNotFound,
    BinaryNotFound,
    ChecksumMismatch,
    SignatureInvalid,
    ManifestFetchFailed,

    // Update
    UpdateFailed,
    ManualRollbackRequired,
    UpdateInProgress,

    // Storage
    InsufficientStorage,

    // Internal
    InternalError,
}

impl ErrorCode {
    /// Every code, in wire-contract order. Used by the parity test and by any
    /// caller that needs to enumerate the set.
    pub const ALL: &'static [ErrorCode] = &[
        ErrorCode::InvalidToken,
        ErrorCode::Revoked,
        ErrorCode::MissingToken,
        ErrorCode::AdminTokenRequired,
        ErrorCode::AdminPasswordRequired,
        ErrorCode::AdminPasswordInvalid,
        ErrorCode::AdminPasswordRateLimited,
        ErrorCode::InvalidPairingCode,
        ErrorCode::PairingCodeExpired,
        ErrorCode::PairingCodeClaimed,
        ErrorCode::PairingRateLimited,
        ErrorCode::NotFound,
        ErrorCode::Forbidden,
        ErrorCode::ValidationError,
        ErrorCode::Conflict,
        ErrorCode::SessionBusy,
        ErrorCode::SessionNotFound,
        ErrorCode::MessageNotFound,
        ErrorCode::StreamNotFound,
        ErrorCode::ContextWindowExceeded,
        ErrorCode::ServerBusy,
        ErrorCode::ServerUnavailable,
        ErrorCode::ProviderError,
        ErrorCode::ProviderUnauthorized,
        ErrorCode::ProviderRateLimited,
        ErrorCode::NoLocaJson,
        ErrorCode::InvalidLocaJson,
        ErrorCode::DepsInstallFailed,
        ErrorCode::TarballFetchFailed,
        ErrorCode::ExtractFailed,
        ErrorCode::ExtensionNotFound,
        ErrorCode::ToolNotFound,
        ErrorCode::PermissionsRequired,
        ErrorCode::PermissionsRevoked,
        ErrorCode::ExtensionHashDrift,
        ErrorCode::ExtensionAlreadyInstalled,
        ErrorCode::ToolNameCollision,
        ErrorCode::ModelNotFound,
        ErrorCode::BinaryNotFound,
        ErrorCode::ChecksumMismatch,
        ErrorCode::SignatureInvalid,
        ErrorCode::ManifestFetchFailed,
        ErrorCode::UpdateFailed,
        ErrorCode::ManualRollbackRequired,
        ErrorCode::UpdateInProgress,
        ErrorCode::InsufficientStorage,
        ErrorCode::InternalError,
    ];

    /// HTTP status this code is served as. Mirrors `ERROR_STATUS`.
    pub fn status(self) -> u16 {
        match self {
            ErrorCode::InvalidToken
            | ErrorCode::Revoked
            | ErrorCode::MissingToken
            | ErrorCode::AdminTokenRequired
            | ErrorCode::AdminPasswordRequired
            | ErrorCode::AdminPasswordInvalid
            | ErrorCode::InvalidPairingCode
            | ErrorCode::ProviderUnauthorized => 401,
            ErrorCode::Forbidden => 403,
            ErrorCode::NotFound
            | ErrorCode::SessionNotFound
            | ErrorCode::MessageNotFound
            | ErrorCode::StreamNotFound
            | ErrorCode::ExtensionNotFound
            | ErrorCode::ToolNotFound
            | ErrorCode::ModelNotFound
            | ErrorCode::BinaryNotFound => 404,
            ErrorCode::ValidationError => 400,
            ErrorCode::Conflict
            | ErrorCode::SessionBusy
            | ErrorCode::ExtensionHashDrift
            | ErrorCode::ExtensionAlreadyInstalled
            | ErrorCode::ToolNameCollision
            | ErrorCode::UpdateInProgress => 409,
            ErrorCode::PairingCodeExpired | ErrorCode::PairingCodeClaimed => 410,
            ErrorCode::PermissionsRequired | ErrorCode::PermissionsRevoked => 412,
            ErrorCode::ContextWindowExceeded => 413,
            ErrorCode::NoLocaJson
            | ErrorCode::InvalidLocaJson
            | ErrorCode::ChecksumMismatch
            | ErrorCode::SignatureInvalid => 422,
            ErrorCode::AdminPasswordRateLimited
            | ErrorCode::PairingRateLimited
            | ErrorCode::ProviderRateLimited => 429,
            ErrorCode::DepsInstallFailed
            | ErrorCode::ExtractFailed
            | ErrorCode::UpdateFailed
            | ErrorCode::ManualRollbackRequired
            | ErrorCode::InternalError => 500,
            ErrorCode::TarballFetchFailed
            | ErrorCode::ProviderError
            | ErrorCode::ManifestFetchFailed => 502,
            ErrorCode::ServerBusy | ErrorCode::ServerUnavailable => 503,
            ErrorCode::InsufficientStorage => 507,
        }
    }

    /// The wire string, allocation-free. Kept in step with the `Serialize` impl
    /// by a test, so the two can never disagree.
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::InvalidToken => "invalid_token",
            ErrorCode::Revoked => "revoked",
            ErrorCode::MissingToken => "missing_token",
            ErrorCode::AdminTokenRequired => "admin_token_required",
            ErrorCode::AdminPasswordRequired => "admin_password_required",
            ErrorCode::AdminPasswordInvalid => "admin_password_invalid",
            ErrorCode::AdminPasswordRateLimited => "admin_password_rate_limited",
            ErrorCode::InvalidPairingCode => "invalid_pairing_code",
            ErrorCode::PairingCodeExpired => "pairing_code_expired",
            ErrorCode::PairingCodeClaimed => "pairing_code_claimed",
            ErrorCode::PairingRateLimited => "pairing_rate_limited",
            ErrorCode::NotFound => "not_found",
            ErrorCode::Forbidden => "forbidden",
            ErrorCode::ValidationError => "validation_error",
            ErrorCode::Conflict => "conflict",
            ErrorCode::SessionBusy => "session_busy",
            ErrorCode::SessionNotFound => "session_not_found",
            ErrorCode::MessageNotFound => "message_not_found",
            ErrorCode::StreamNotFound => "stream_not_found",
            ErrorCode::ContextWindowExceeded => "context_window_exceeded",
            ErrorCode::ServerBusy => "server_busy",
            ErrorCode::ServerUnavailable => "server_unavailable",
            ErrorCode::ProviderError => "provider_error",
            ErrorCode::ProviderUnauthorized => "provider_unauthorized",
            ErrorCode::ProviderRateLimited => "provider_rate_limited",
            ErrorCode::NoLocaJson => "no_loca_json",
            ErrorCode::InvalidLocaJson => "invalid_loca_json",
            ErrorCode::DepsInstallFailed => "deps_install_failed",
            ErrorCode::TarballFetchFailed => "tarball_fetch_failed",
            ErrorCode::ExtractFailed => "extract_failed",
            ErrorCode::ExtensionNotFound => "extension_not_found",
            ErrorCode::ToolNotFound => "tool_not_found",
            ErrorCode::PermissionsRequired => "permissions_required",
            ErrorCode::PermissionsRevoked => "permissions_revoked",
            ErrorCode::ExtensionHashDrift => "extension_hash_drift",
            ErrorCode::ExtensionAlreadyInstalled => "extension_already_installed",
            ErrorCode::ToolNameCollision => "tool_name_collision",
            ErrorCode::ModelNotFound => "model_not_found",
            ErrorCode::BinaryNotFound => "binary_not_found",
            ErrorCode::ChecksumMismatch => "checksum_mismatch",
            ErrorCode::SignatureInvalid => "signature_invalid",
            ErrorCode::ManifestFetchFailed => "manifest_fetch_failed",
            ErrorCode::UpdateFailed => "update_failed",
            ErrorCode::ManualRollbackRequired => "manual_rollback_required",
            ErrorCode::UpdateInProgress => "update_in_progress",
            ErrorCode::InsufficientStorage => "insufficient_storage",
            ErrorCode::InternalError => "internal_error",
        }
    }
}

/// Details bag carried alongside an error. Free-form on the wire, so free-form
/// here; callers that need structure build the map and hand it over.
pub type Details = serde_json::Map<String, serde_json::Value>;

/// The error every fallible core path returns.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct AppError {
    pub code: ErrorCode,
    pub message: String,
    pub details: Option<Details>,
}

/// The core's result alias. Prefer this over spelling `AppError` at every site.
pub type Result<T, E = AppError> = std::result::Result<T, E>;

impl AppError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: None,
        }
    }

    pub fn with_details(mut self, details: Details) -> Self {
        self.details = Some(details);
        self
    }

    pub fn status(&self) -> u16 {
        self.code.status()
    }

    /// The wire envelope. The HTTP layer serializes this as the response body
    /// for every non-2xx.
    pub fn to_body(&self) -> ApiErrorBody {
        ApiErrorBody {
            error: ApiError {
                code: self.code,
                message: self.message.clone(),
                details: self.details.clone(),
            },
        }
    }
}

/// One-liner constructors for the codes thrown from everywhere. Anything more
/// specific is spelled out with `AppError::new`.
pub fn not_found(message: impl Into<String>) -> AppError {
    AppError::new(ErrorCode::NotFound, message)
}

pub fn validation(message: impl Into<String>) -> AppError {
    AppError::new(ErrorCode::ValidationError, message)
}

pub fn conflict(message: impl Into<String>) -> AppError {
    AppError::new(ErrorCode::Conflict, message)
}

pub fn forbidden(message: impl Into<String>) -> AppError {
    AppError::new(ErrorCode::Forbidden, message)
}

pub fn internal(message: impl Into<String>) -> AppError {
    AppError::new(ErrorCode::InternalError, message)
}

/// The wire envelope: `{ error: { code, message, details? } }`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiErrorBody {
    pub error: ApiError,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiError {
    pub code: ErrorCode,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Details>,
}

/// A full disk is a 507, not a 500: the client shows a distinct "free some
/// space" path for it. `StorageFull` is the portable signal; the raw errno
/// fallback covers platforms where the OS error is not mapped to it.
fn is_no_space(err: &std::io::Error) -> bool {
    if err.kind() == std::io::ErrorKind::StorageFull {
        return true;
    }
    match err.raw_os_error() {
        // ENOSPC on unix, ERROR_DISK_FULL / ERROR_HANDLE_DISK_FULL on Windows.
        Some(28) if cfg!(unix) => true,
        Some(39) | Some(112) if cfg!(windows) => true,
        _ => false,
    }
}

impl From<std::io::Error> for AppError {
    fn from(err: std::io::Error) -> Self {
        let code = if is_no_space(&err) {
            ErrorCode::InsufficientStorage
        } else if err.kind() == std::io::ErrorKind::NotFound {
            ErrorCode::NotFound
        } else {
            ErrorCode::InternalError
        };
        AppError::new(code, err.to_string())
    }
}

impl From<serde_json::Error> for AppError {
    fn from(err: serde_json::Error) -> Self {
        AppError::new(ErrorCode::ValidationError, err.to_string())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn every_code_is_in_all_and_has_a_status() {
        // 47 codes, matching ERROR_CODES in packages/loca-shared/src/api/errors.ts.
        assert_eq!(ErrorCode::ALL.len(), 47);
        let mut seen = std::collections::HashSet::new();
        for code in ErrorCode::ALL {
            assert!(seen.insert(*code), "{code:?} listed twice in ALL");
            let status = code.status();
            assert!((200..=599).contains(&status), "{code:?} -> {status}");
        }
    }

    #[test]
    fn as_str_matches_the_serialized_form() {
        for code in ErrorCode::ALL {
            let json = serde_json::to_string(code).expect("unit variant serializes");
            assert_eq!(json, format!("\"{}\"", code.as_str()));
        }
    }

    #[test]
    fn envelope_shape_matches_the_wire_contract() {
        let mut details = Details::new();
        details.insert("field".into(), serde_json::json!("password"));
        let err = validation("password too short").with_details(details);
        assert_eq!(err.status(), 400);
        let body = serde_json::to_value(err.to_body()).expect("body serializes");
        assert_eq!(
            body,
            serde_json::json!({
                "error": {
                    "code": "validation_error",
                    "message": "password too short",
                    "details": { "field": "password" },
                }
            })
        );
    }

    #[test]
    fn details_are_omitted_when_absent() {
        let body =
            serde_json::to_value(not_found("no such session").to_body()).expect("body serializes");
        assert_eq!(
            body,
            serde_json::json!({ "error": { "code": "not_found", "message": "no such session" } })
        );
    }

    #[test]
    fn disk_full_maps_to_insufficient_storage() {
        let err = std::io::Error::from(std::io::ErrorKind::StorageFull);
        assert_eq!(AppError::from(err).code, ErrorCode::InsufficientStorage);
        let err = std::io::Error::from(std::io::ErrorKind::NotFound);
        assert_eq!(AppError::from(err).code, ErrorCode::NotFound);
        let err = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        assert_eq!(AppError::from(err).code, ErrorCode::InternalError);
    }
}
