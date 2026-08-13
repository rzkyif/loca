use thiserror::Error;

/// Unified error type for Tauri commands and internal helpers.
///
/// Variants preserve the underlying source chain via `thiserror`, so callers
/// can use `?` on common error types (`std::io::Error`, `serde_json::Error`,
/// `reqwest::Error`, etc.) instead of stringifying them at each call site.
///
/// The wire format stays a plain string: `serde::Serialize` delegates to
/// `Display`, so the frontend sees the same error shape it did before.
#[derive(Debug, Error)]
pub enum AppError {
    #[error("{0}")]
    Validation(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("{0}")]
    External(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Serde(#[from] serde_json::Error),

    #[error(transparent)]
    Tauri(#[from] tauri::Error),

    #[error(transparent)]
    Keyring(#[from] keyring_core::Error),

    #[error(transparent)]
    Base64(#[from] base64::DecodeError),

    #[error(transparent)]
    Image(#[from] image::ImageError),
}

impl AppError {
    pub fn validation(msg: impl Into<String>) -> Self {
        AppError::Validation(msg.into())
    }

    pub fn not_found(msg: impl Into<String>) -> Self {
        AppError::NotFound(msg.into())
    }

    pub fn external(msg: impl Into<String>) -> Self {
        AppError::External(msg.into())
    }
}

impl serde::Serialize for AppError {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

pub type AppResult<T> = std::result::Result<T, AppError>;

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn helper_constructors_set_their_variants() {
        assert!(matches!(AppError::validation("x"), AppError::Validation(_)));
        assert!(matches!(AppError::not_found("x"), AppError::NotFound(_)));
        assert!(matches!(AppError::external("x"), AppError::External(_)));
    }

    #[test]
    fn display_matches_message_for_thin_variants() {
        assert_eq!(
            format!("{}", AppError::validation("bad input")),
            "bad input"
        );
        assert_eq!(
            format!("{}", AppError::not_found("session")),
            "not found: session",
        );
    }

    #[test]
    fn serialize_to_string_uses_display() {
        // The wire format is a plain JSON string; the frontend reads it
        // directly. If this ever changes the frontend's error-rendering
        // contract breaks silently.
        let json = serde_json::to_string(&AppError::validation("oops")).unwrap();
        assert_eq!(json, r#""oops""#);
    }

    #[test]
    fn io_errors_convert_via_question_mark() {
        fn boom() -> AppResult<()> {
            std::fs::read("/path/that/will/never/exist")?;
            Ok(())
        }
        let err = boom().unwrap_err();
        assert!(matches!(err, AppError::Io(_)));
    }

    #[test]
    fn serde_errors_convert_via_question_mark() {
        fn boom() -> AppResult<serde_json::Value> {
            Ok(serde_json::from_str("{")?)
        }
        let err = boom().unwrap_err();
        assert!(matches!(err, AppError::Serde(_)));
    }
}
