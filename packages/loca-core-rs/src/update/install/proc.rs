//! Running a command, behind a seam.
//!
//! Every OS integration in this package is a subprocess: `launchctl`,
//! `systemctl`, `schtasks`, PowerShell. None of them can be driven honestly on
//! a host that is not that OS, and a test that asserts on real output is a test
//! that only runs on one third of the fleet.
//!
//! So the argv is the thing under test. A [`Runner`] records what was issued
//! and answers with a scripted result, which makes the DECISIONS assertable
//! everywhere: which command, in what order, with which flags, and what each
//! branch does with a non-zero exit.
//!
//! **What that does not cover, stated rather than implied.** Whether the OS
//! tools actually behave as the argv assumes. Nothing on one host can check
//! that `launchctl load` is idempotent or that `schtasks /Query` exits non-zero
//! for an absent task; those are exercised per OS at release time, and they are
//! the same class of gap D11 records for its Windows variants.

use crate::error::Result;

/// What a command did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunResult {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl RunResult {
    pub fn success(&self) -> bool {
        self.code == 0
    }

    pub fn ok(stdout: &str) -> Self {
        Self {
            code: 0,
            stdout: stdout.to_string(),
            stderr: String::new(),
        }
    }

    pub fn failed(code: i32) -> Self {
        Self {
            code,
            stdout: String::new(),
            stderr: String::new(),
        }
    }
}

/// The one way this package starts a process.
#[async_trait::async_trait]
pub trait Runner: Send + Sync {
    async fn run(&self, argv: &[&str]) -> RunResult;
}

/// The production implementation.
///
/// A spawn failure is a `RunResult` rather than an error, matching
/// `install/proc.ts:38-42`: every caller here is probing or tearing down, and
/// "the tool is not installed" and "the tool said no" are the same answer to
/// all of them.
pub struct SystemRunner;

#[async_trait::async_trait]
impl Runner for SystemRunner {
    async fn run(&self, argv: &[&str]) -> RunResult {
        let Some((program, args)) = argv.split_first() else {
            return RunResult::failed(127);
        };
        match tokio::process::Command::new(program)
            .args(args)
            .stdin(std::process::Stdio::null())
            .output()
            .await
        {
            Ok(out) => RunResult {
                code: out.status.code().unwrap_or(1),
                stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            },
            Err(err) => RunResult {
                code: 127,
                stdout: String::new(),
                stderr: err.to_string(),
            },
        }
    }
}

/// The user's real home, independent of the `LOCA_CORE_HOME` override.
///
/// Service definitions must land in the actual `~/Library` or `~/.config` even
/// when core state is redirected to a tempdir, which is why this is separate
/// from `Paths` (`install/proc.ts:77-84`). Passed as an argument everywhere it
/// is used, so a test can point it at a tempdir without `set_var`, which is
/// unsound under `cargo test`'s threads (D-010).
pub fn real_home() -> Result<std::path::PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(std::path::PathBuf::from)
        .ok_or_else(|| {
            crate::error::internal(
                "could not determine the home directory (no HOME or USERPROFILE)",
            )
        })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
pub mod testing {
    use super::*;
    use std::sync::Mutex;

    /// Records every argv and answers from a script.
    ///
    /// The script is matched on a substring of the joined argv, so a test says
    /// what it is scripting rather than restating the whole command line, which
    /// would make the assertion a copy of the code.
    #[derive(Default)]
    pub struct RecordingRunner {
        calls: Mutex<Vec<String>>,
        script: Mutex<Vec<(String, RunResult)>>,
    }

    impl RecordingRunner {
        pub fn new() -> Self {
            Self::default()
        }

        /// Answer `result` for any argv containing `needle`.
        pub fn on(self, needle: &str, result: RunResult) -> Self {
            self.script
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push((needle.to_string(), result));
            self
        }

        pub fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap_or_else(|e| e.into_inner()).clone()
        }
    }

    #[async_trait::async_trait]
    impl Runner for RecordingRunner {
        async fn run(&self, argv: &[&str]) -> RunResult {
            let joined = argv.join(" ");
            self.calls
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(joined.clone());
            for (needle, result) in self.script.lock().unwrap_or_else(|e| e.into_inner()).iter() {
                if joined.contains(needle.as_str()) {
                    return result.clone();
                }
            }
            // Unscripted commands FAIL rather than succeed. A runner that
            // helpfully answers 0 to anything makes every probe report
            // "installed" and every teardown report success, which is D-050
            // case 1 with the mock filling in what the test left out.
            RunResult::failed(1)
        }
    }
}
