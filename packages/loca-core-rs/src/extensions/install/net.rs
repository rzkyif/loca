//! The one outbound path installation has, behind a seam.
//!
//! # Why a trait rather than a `reqwest::Client` field
//!
//! `AGENTS.md`'s rule is absolute: once installed, Core makes NO outbound
//! request without an explicit user action. Every request this package can make
//! is listed below, and every one of them is behind a button:
//!
//! | request                                        | authorised by                       |
//! | ---------------------------------------------- | ----------------------------------- |
//! | the npm search endpoint                        | typing in the extensions browser    |
//! | the npm metadata endpoint (latest version)     | the "Check for updates" button      |
//! | the npm metadata endpoint (a specific version) | the Install or Update button        |
//! | an npm tarball                                 | the same Install or Update button   |
//! | the signed seeded manifest + its CDN tarball   | Install, on a seeded extension      |
//!
//! **First-boot seeding is not in that table and must never join it.** The
//! offline chain reads a planted tarball and a planted signed manifest off disk
//! and installs from those bytes; if either is missing it defers to the user.
//! That property is what this seam exists to make provable rather than
//! observable: a test that runs the offline path and sees no error proves
//! nothing, because the offline path is also what runs when everything works.
//! The tests inject an [`Http`] that PANICS on any call, so the assertion is
//! structural.
//!
//! A counter would be the weaker form of the same idea and is deliberately not
//! here: a counter that never counts always reads zero, so deleting its
//! increment leaves every consent assertion green (D-050 case 3). A double that
//! panics cannot be defeated that way.

use std::time::Duration;

use async_trait::async_trait;

use crate::error::{AppError, ErrorCode, Result};

/// How long a SMALL request may take in total: npm metadata, npm search, a
/// signed manifest.
///
/// `shared/net.ts:14`'s `FETCH_TIMEOUT_MS`, which D7 and D13 already agree on.
pub const SMALL_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// How long a tarball fetch may take in total.
///
/// A whole-request cap rather than a stall window, because an extension tarball
/// is measured in megabytes rather than the hundred a core binary is, and it is
/// buffered whole before verification anyway (the integrity gate runs over the
/// same buffer that gets extracted, so there is nothing to stream to).
pub const TARBALL_TIMEOUT: Duration = Duration::from_secs(120);

/// The outbound side of extension installation.
#[async_trait]
pub trait Http: Send + Sync {
    /// A GET whose body is a document: npm metadata, or a signed manifest.
    ///
    /// Returns the TEXT rather than a parsed value, because a signature covers
    /// BYTES. Handing back a parsed struct gives the caller a second chance to
    /// differ from what was signed, which is the defect D-065 records.
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

/// Re-label a transport failure with the code its caller reports.
///
/// The specification raises `tarball_fetch_failed` for a tarball and
/// `manifest_fetch_failed` for metadata over the same `fetch`, so the code is
/// the CALLER's fact rather than the transport's. Keeping the message means a
/// support log still says which host and why.
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
///
/// The structural half of the no-non-consented-network property. Any path that
/// is supposed to be offline is driven with this in place, so the assertion is
/// "the network was not reachable" rather than "the offline path returned Ok",
/// which is what the offline path also does when everything works.
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
        // Pinned in both directions because both are plausible-looking numbers
        // and swapping them gives a tarball 30 seconds and a metadata lookup
        // two minutes, which fails a slow install and hangs a fast one.
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
        // An Err would be caught by a caller's `?` and reported as an ordinary
        // install failure, which is exactly how an accidental fetch would hide.
        let _ = NoNetwork.get_text("https://registry.npmjs.org/x").await;
    }
}
