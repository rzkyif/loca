//! The one outbound path installation has, behind a seam.
//!
//! Every request here is behind a user action (npm search/metadata/tarball,
//! the signed seeded manifest + CDN tarball). First-boot seeding is NOT one of
//! them: tests drive offline paths with an [`Http`] that PANICS on any call, so
//! the no-network property is structural rather than observed.

use std::time::Duration;

use async_trait::async_trait;

use crate::error::{AppError, ErrorCode, Result};

/// Whole-request cap for small requests: npm metadata, search, a signed manifest.
pub const SMALL_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Whole-request cap for a tarball fetch (small enough to buffer whole before
/// verification, so no stall window is needed).
pub const TARBALL_TIMEOUT: Duration = Duration::from_secs(120);

/// The outbound side of extension installation.
#[async_trait]
pub trait Http: Send + Sync {
    /// A GET whose body is a document. Returns text, not a parsed value: a
    /// signature covers bytes.
    async fn get_text(&self, url: &str) -> Result<String>;

    /// A GET whose body is a tarball.
    async fn get_bytes(&self, url: &str) -> Result<Vec<u8>>;
}

/// The production client.
pub struct HttpClient {
    client: reqwest::Client,
    small_request_timeout: Duration,
    tarball_timeout: Duration,
}

impl HttpClient {
    pub fn new() -> Result<Self> {
        let client = reqwest::Client::builder().build().map_err(|err| {
            AppError::new(
                ErrorCode::InternalError,
                format!("could not build the extension HTTP client: {err}"),
            )
        })?;
        Ok(Self {
            client,
            small_request_timeout: SMALL_REQUEST_TIMEOUT,
            tarball_timeout: TARBALL_TIMEOUT,
        })
    }
}

#[async_trait]
impl Http for HttpClient {
    async fn get_text(&self, url: &str) -> Result<String> {
        let fetch = async {
            let res = self
                .client
                .get(url)
                .send()
                .await
                .map_err(unreachable(url))?;
            check_status(url, &res)?;
            res.text().await.map_err(|err| {
                AppError::new(
                    ErrorCode::ManifestFetchFailed,
                    format!("could not read {url}: {err}"),
                )
            })
        };
        match tokio::time::timeout(self.small_request_timeout, fetch).await {
            Err(_elapsed) => Err(timed_out(url, self.small_request_timeout)),
            Ok(text) => text,
        }
    }

    async fn get_bytes(&self, url: &str) -> Result<Vec<u8>> {
        let fetch = async {
            let res = self
                .client
                .get(url)
                .send()
                .await
                .map_err(unreachable(url))?;
            check_status(url, &res)?;
            res.bytes().await.map(|b| b.to_vec()).map_err(|err| {
                AppError::new(
                    ErrorCode::TarballFetchFailed,
                    format!("could not read {url}: {err}"),
                )
            })
        };
        match tokio::time::timeout(self.tarball_timeout, fetch).await {
            Err(_elapsed) => Err(timed_out(url, self.tarball_timeout)),
            Ok(bytes) => bytes,
        }
    }
}

/// Re-label a transport failure with the code its caller reports, keeping the message.
pub fn as_code(err: AppError, code: ErrorCode) -> AppError {
    AppError::new(code, err.message)
}

fn unreachable(url: &str) -> impl FnOnce(reqwest::Error) -> AppError + use<> {
    let url = url.to_string();
    move |err| {
        AppError::new(
            ErrorCode::ServerUnavailable,
            format!("could not reach {url}: {err}"),
        )
    }
}

fn check_status(url: &str, res: &reqwest::Response) -> Result<()> {
    let status = res.status();
    if status.is_success() {
        return Ok(());
    }
    Err(AppError::new(
        ErrorCode::ManifestFetchFailed,
        format!(
            "HTTP {} {} for {url}",
            status.as_u16(),
            status.canonical_reason().unwrap_or_default()
        ),
    ))
}

fn timed_out(url: &str, after: Duration) -> AppError {
    AppError::new(
        ErrorCode::ServerUnavailable,
        format!("{url} did not respond within {}s", after.as_secs()),
    )
}

/// An [`Http`] that fails the test if anything reaches it.
#[cfg(test)]
pub struct NoNetwork;

#[cfg(test)]
#[async_trait]
#[allow(clippy::panic)]
impl Http for NoNetwork {
    async fn get_text(&self, url: &str) -> Result<String> {
        panic!("this path must not reach the network, and it fetched {url}");
    }
    async fn get_bytes(&self, url: &str) -> Result<Vec<u8>> {
        panic!("this path must not reach the network, and it fetched {url}");
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn the_two_bounds_are_the_shipping_values_and_are_not_transposed() {
        assert_eq!(SMALL_REQUEST_TIMEOUT, Duration::from_secs(30));
        assert_eq!(TARBALL_TIMEOUT, Duration::from_secs(120));
        assert!(TARBALL_TIMEOUT > SMALL_REQUEST_TIMEOUT);
    }

    #[test]
    fn recoding_a_failure_keeps_the_message_and_changes_only_the_code() {
        let err = AppError::new(
            ErrorCode::ServerUnavailable,
            "could not reach https://x: no",
        );
        let recoded = as_code(err, ErrorCode::TarballFetchFailed);
        assert_eq!(recoded.code, ErrorCode::TarballFetchFailed);
        assert!(
            recoded.message.contains("could not reach https://x"),
            "a re-coded error that drops the message leaves a support log with \
             nothing but the code: {}",
            recoded.message
        );
    }

    #[tokio::test]
    #[should_panic(expected = "must not reach the network")]
    async fn the_offline_double_panics_rather_than_returning_an_error() {
        let _ = NoNetwork.get_text("https://registry.npmjs.org/x").await;
    }
}
