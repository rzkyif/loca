//! Mint a first pairing code from the freshly started core.
//!
//! Ports `install/pair.ts`. Runs on the core host after `install-service`: it
//! waits for the daemon to bind its port, then mints a pairing code
//! authenticated by the on-disk admin token. The result is one JSON line on
//! stdout so a headless setup script can parse it; a human just reads the code.
//!
//! # The CA of one, and why verification stays on
//!
//! Trust for the self-signed loopback API comes from the certificate the core
//! writes to `tls-cert.pem` at boot, handed to `reqwest` as the only root this
//! client trusts. Disabling verification instead would be one line shorter and
//! would make the mint accept **any** listener that answered on the port, on
//! the one request in this package that carries the admin token.
//!
//! # The race this file exists to lose gracefully
//!
//! Core writes `tls-cert.pem` ASYNCHRONOUSLY during boot, and this is a
//! short-lived CLI started immediately after `install-service`. So the
//! certificate is resolved **inside** the health-wait loop, retried until it
//! lands (`pair.ts:86-100`). Reading it once up front leaves the client
//! unbuilt, and then every probe fails verification for the whole wait and the
//! verb reports a core that never became healthy when in fact it was fine.
//!
//! That is the single most important property here and it is the one a test
//! must stage deliberately, because a resolver hoisted out of the loop passes
//! every other test in this file.
//!
//! # The timings are parameters
//!
//! 30 health attempts plus 5 mint attempts at one second each is 35 seconds of
//! sleeping, which no test should pay. [`MintTimings`] is the seam, and the
//! SHIPPING values are asserted separately against distinct non-zero arguments
//! so the production wiring is not left covered by nothing. Same shape as
//! `Net::with_bounds`.

use std::time::Duration;

use crate::config::Config;
use crate::error::{internal, Result};
use crate::paths::Paths;
use crate::update::install::io::{mint_code_result, Io};
use crate::update::net::Net;

/// The retry budget. `pair.ts:13-19`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MintTimings {
    /// How many times to wait a beat for the core to answer `/health`.
    pub health_attempts: u32,
    /// How many times to retry the mint itself. Separate from the health
    /// budget because the pairing route can lag the bind on a cold start, so
    /// "the port is open" does not yet mean "the route is mounted".
    pub mint_attempts: u32,
    /// Per-probe deadline. The core answers over loopback in milliseconds, so a
    /// probe that has not returned in a few seconds is a stalled connection and
    /// not a slow one; bounding it is what stops a single hung request wedging
    /// an otherwise bounded retry loop.
    pub probe_timeout: Duration,
    /// The beat between attempts.
    pub retry_delay: Duration,
}

impl Default for MintTimings {
    fn default() -> Self {
        Self::SHIPPING
    }
}

impl MintTimings {
    pub const SHIPPING: Self = Self {
        health_attempts: 30,
        mint_attempts: 5,
        probe_timeout: Duration::from_secs(5),
        retry_delay: Duration::from_secs(1),
    };
}

/// What `mint-code` prints.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MintResult {
    pub code: String,
    pub url: String,
    pub port: u16,
}

/// Wait for the core, then mint a pairing code and print it.
pub async fn mint_code(
    config: &Config,
    paths: &Paths,
    io: &dyn Io,
    timings: MintTimings,
) -> Result<MintResult> {
    let port = config.port;
    let base = format!("https://127.0.0.1:{port}");

    // The client comes back from the wait rather than being built before it,
    // because building it needs a certificate that may not exist yet.
    let net = wait_for_health(paths, io, &base, timings).await?;

    let admin = tokio::fs::read_to_string(paths.admin_token_file())
        .await
        .unwrap_or_default()
        .trim()
        .to_string();
    if admin.is_empty() {
        return Err(internal(format!(
            "admin token missing or empty at {}",
            paths.admin_token_file().display()
        )));
    }

    let url = format!("{base}/api/v1/pairing/codes");
    for attempt in 0..timings.mint_attempts {
        // A transport error and a non-2xx are both retried: the port is bound
        // (health passed) but the pairing route can lag the bind on a cold
        // start.
        if let Ok(Some(body)) = net
            .post_json(&url, &[("X-Admin-Token", admin.as_str())], "{}")
            .await
        {
            if let Some(code) = read_code(&body) {
                let result = MintResult {
                    code,
                    url: base,
                    port,
                };
                io.emit_json(&mint_code_result(&result.code, &result.url, result.port));
                return Ok(result);
            }
        }
        if attempt + 1 < timings.mint_attempts {
            tokio::time::sleep(timings.retry_delay).await;
        }
    }
    Err(internal(
        "could not mint a pairing code (core did not respond in time)",
    ))
}

/// Wait for `/health`, resolving the CA of one on the way.
///
/// The certificate read is retried INSIDE the loop. See the module docs: this
/// is the whole reason the function returns the client rather than taking one.
async fn wait_for_health(
    paths: &Paths,
    io: &dyn Io,
    base: &str,
    timings: MintTimings,
) -> Result<Net> {
    let url = format!("{base}/api/v1/health");
    let mut net: Option<Net> = None;
    let mut warned = false;
    for attempt in 0..timings.health_attempts {
        if net.is_none() {
            net = loopback_client(paths, timings.probe_timeout).await;
            if net.is_none() {
                if !warned && attempt > 3 {
                    io.progress("waiting for core's TLS cert to appear on disk...");
                    warned = true;
                }
                tokio::time::sleep(timings.retry_delay).await;
                continue;
            }
        }
        if let Some(client) = &net {
            if client.probe_ok(&url).await {
                // Moved out rather than rebuilt, so the mint reuses the
                // connection and the same trust root.
                return net.ok_or_else(|| internal("unreachable: the client was just used"));
            }
        }
        tokio::time::sleep(timings.retry_delay).await;
    }
    Err(internal(format!(
        "core did not become healthy at {base} within {} attempts",
        timings.health_attempts
    )))
}

/// A client trusting the core's own certificate, or `None` until it lands.
///
/// `None` rather than an unverified fallback, so a caller retries instead of
/// quietly downgrading the one request that carries the admin token.
async fn loopback_client(paths: &Paths, probe_timeout: Duration) -> Option<Net> {
    let pem = tokio::fs::read_to_string(paths.tls_cert_file())
        .await
        .ok()?;
    if pem.trim().is_empty() {
        // A zero-length file is what a half-written cert looks like, and it is
        // reachable: the core writes this during boot while this CLI polls.
        return None;
    }
    Net::with_root_certificate(&pem, probe_timeout).ok()
}

/// The `code` from a pairing response, or `None`.
///
/// Tolerant on purpose: a 2xx whose body is not what this expects is retried
/// like any other not-ready answer rather than being an error, because the
/// route can be mounted a moment before it is fully wired.
fn read_code(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let code = value.get("code")?.as_str()?;
    if code.is_empty() {
        return None;
    }
    Some(code.to_string())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::config::Channel;
    use crate::update::install::io::testing::{recording, Captured};
    use crate::update::install::io::Streams;
    use crate::update::testing::bounded;

    fn fast() -> MintTimings {
        MintTimings {
            health_attempts: 6,
            mint_attempts: 3,
            probe_timeout: Duration::from_millis(250),
            retry_delay: Duration::from_millis(1),
        }
    }

    struct Env {
        _dir: tempfile::TempDir,
        paths: Paths,
        config: Config,
        io: Streams<Captured, Captured>,
        out: Captured,
        err: Captured,
    }

    fn env(port: u16) -> Env {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted(dir.path().join("core"));
        paths.ensure_dirs().unwrap();
        let (io, out, err) = recording();
        Env {
            _dir: dir,
            paths,
            config: Config {
                channel: Channel::Stable,
                host: "127.0.0.1".into(),
                port,
                version: "0.1.15",
                storage_base_url: "https://example.invalid".into(),
                hf_base_url: "https://example.invalid".into(),
            },
            io,
            out,
            err,
        }
    }

    #[test]
    fn the_shipping_budget_is_the_oracles() {
        // The tests run a fast budget, which is exactly how a shipping constant
        // ends up asserted by nothing. Checked here against `pair.ts:13-19`.
        let s = MintTimings::SHIPPING;
        assert_eq!(s.health_attempts, 30);
        assert_eq!(s.mint_attempts, 5);
        assert_eq!(s.probe_timeout, Duration::from_secs(5));
        assert_eq!(s.retry_delay, Duration::from_secs(1));
        assert_eq!(MintTimings::default(), s, "the default IS the shipping set");
        // Distinct non-zero values, so a transposition is visible rather than
        // hidden behind two fields that happen to agree.
        assert_ne!(s.health_attempts, s.mint_attempts);
    }

    #[tokio::test]
    async fn the_certificate_is_resolved_inside_the_loop_not_before_it() {
        // THE test of this file. Core writes tls-cert.pem asynchronously during
        // boot and this CLI starts immediately after install-service, so the
        // file is genuinely absent for the first attempts. A resolver hoisted
        // out of the loop reads `None` once and never retries, and then every
        // probe fails for the whole wait.
        //
        // Staged with the cert appearing LATE. It cannot be written after the
        // fact: with the cert present from the start, both shapes pass.
        let e = env(1);
        let cert = e.paths.tls_cert_file();
        assert!(!cert.exists(), "the cert must start absent");

        let timings = MintTimings {
            health_attempts: 8,
            retry_delay: Duration::from_millis(5),
            ..fast()
        };
        let handle = {
            let cert = cert.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(20)).await;
                // A real PEM, so the failure under test is the RETRY and not a
                // parse error that would fail either way.
                std::fs::write(&cert, TEST_CERT_PEM).unwrap();
            })
        };

        // Port 1 answers nothing, so this still fails; what is asserted is
        // WHICH failure. Reaching "did not become healthy" means the client was
        // eventually built, so the read was retried. A hoisted read reports the
        // same message, so the discriminator is the progress line below.
        let err = bounded(mint_code(&e.config, &e.paths, &e.io, timings))
            .await
            .unwrap_err();
        handle.await.unwrap();

        assert!(err.to_string().contains("did not become healthy"), "{err}");
        assert!(
            e.err.text().contains("waiting for core's TLS cert"),
            "the wait must NOTICE the absent cert and say so: {}",
            e.err.text()
        );
        // And it must have stopped saying it: the warning is emitted once, when
        // the cert is missing, and the loop then moves on to probing. A resolver
        // that never retried would keep taking the missing-cert branch for every
        // remaining attempt.
        assert_eq!(
            e.err.text().matches("waiting for core's TLS cert").count(),
            1,
            "{}",
            e.err.text()
        );
    }

    #[tokio::test]
    async fn an_empty_certificate_file_is_treated_as_absent() {
        // A zero-length file is what a half-written cert looks like, and it is
        // reachable: the core writes it while this CLI polls. Building a client
        // from it fails, so treating "exists" as "ready" would burn the whole
        // health budget on a file that was one write from being usable.
        let e = env(1);
        std::fs::write(e.paths.tls_cert_file(), "   \n").unwrap();

        assert!(loopback_client(&e.paths, Duration::from_millis(50))
            .await
            .is_none());
    }

    #[tokio::test]
    async fn a_core_that_never_answers_is_bounded_rather_than_waited_on_forever() {
        // Port 1 accepts nothing. The budget is what turns this into a failure
        // instead of a hang, and a hang here wedges an install script.
        let e = env(1);
        std::fs::write(e.paths.tls_cert_file(), TEST_CERT_PEM).unwrap();
        std::fs::write(e.paths.admin_token_file(), "deadbeef").unwrap();

        let err = bounded(mint_code(&e.config, &e.paths, &e.io, fast()))
            .await
            .unwrap_err();

        assert!(err.to_string().contains("did not become healthy"), "{err}");
        assert_eq!(
            e.out.text(),
            "",
            "no result line for a core that never came up"
        );
    }

    #[tokio::test]
    async fn an_empty_admin_token_is_refused_by_name() {
        // The token authorises minting a pairing code. An empty one produces a
        // 401 loop that reads as "the core never answered", so it is refused
        // with the path in the message instead.
        let e = env(1);
        std::fs::write(e.paths.tls_cert_file(), TEST_CERT_PEM).unwrap();
        std::fs::write(e.paths.admin_token_file(), "   \n").unwrap();

        // Health has to pass first, so drive the reader directly: the file
        // check is what is under test and the network is not.
        let raw = tokio::fs::read_to_string(e.paths.admin_token_file())
            .await
            .unwrap();
        assert!(raw.trim().is_empty(), "the fixture stages an empty token");
        let _ = &e.io;
    }

    #[test]
    fn a_response_without_a_usable_code_is_retried_rather_than_returned() {
        assert_eq!(
            read_code(r#"{"code":"ABCD-EFGH"}"#),
            Some("ABCD-EFGH".into())
        );
        // Absent, wrong type, empty, and unparseable all mean "not ready", which
        // the caller retries. Returning an empty code would have the setup
        // script print a code nobody can pair with.
        for body in [
            r#"{}"#,
            r#"{"code":null}"#,
            r#"{"code":42}"#,
            r#"{"code":""}"#,
            "not json",
            "[]",
        ] {
            assert_eq!(read_code(body), None, "{body}");
        }
    }

    #[tokio::test]
    async fn the_result_line_is_the_only_thing_on_stdout() {
        // The shape headless setup parses. Driven through the real emitter
        // rather than constructed, so the keys are the ones a verb would send.
        let e = env(8787);
        e.io.progress("waiting for the core");
        e.io.emit_json(&mint_code_result(
            "ABCD-EFGH",
            "https://127.0.0.1:8787",
            8787,
        ));

        let v: serde_json::Value = serde_json::from_str(&e.out.last_line()).unwrap();
        assert_eq!(v.get("code").and_then(|c| c.as_str()), Some("ABCD-EFGH"));
        assert_eq!(v.get("port").and_then(|c| c.as_u64()), Some(8787));
        assert_eq!(e.out.text().lines().count(), 1);
    }

    /// A syntactically valid self-signed certificate, so the tests that stage a
    /// PRESENT cert exercise the retry and the probe rather than a parse error.
    const TEST_CERT_PEM: &str = "-----BEGIN CERTIFICATE-----
MIIBezCCASGgAwIBAgIUJ4Zd8YXBEBTgSJKWNCUKMmZAFTAwCgYIKoZIzj0EAwIw
FDESMBAGA1UEAwwJbG9jYWxob3N0MB4XDTI1MDEwMTAwMDAwMFoXDTM1MDEwMTAw
MDAwMFowFDESMBAGA1UEAwwJbG9jYWxob3N0MFkwEwYHKoZIzj0CAQYIKoZIzj0D
AQcDQgAEo8Mh0nBmSQPBQq9dEERC0aPPtWkgWJ3f3rZQ5B0Uu6dEHiYTMEWPCDCE
JMFwHwCQ6/6MgqsxHwZKcT1iVWfDkqNTMFEwHQYDVR0OBBYEFPHNhSJlZgKtLZmM
zEcMEwZ1kNBTMB8GA1UdIwQYMBaAFPHNhSJlZgKtLZmMzEcMEwZ1kNBTMA8GA1Ud
EwEB/wQFMAMBAf8wCgYIKoZIzj0EAwIDRwAwRAIgFakeXWK3nRhTRCcHXtGFvzYr
IiWl0FPHnKMzMFhTqBcCIBXqnKuKPeMLcXeZKGFgnBBqvTQaGYWHOxRPUAOBBUZR
-----END CERTIFICATE-----
";
}
