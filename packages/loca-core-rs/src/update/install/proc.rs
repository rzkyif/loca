//! Running a command, behind a seam.
//!
//! Every OS integration here is a subprocess (`launchctl`, `systemctl`,
//! `schtasks`, PowerShell), and none can be driven on a host that is not that
//! OS. So the argv is the thing under test: a [`Runner`] records what was
//! issued and answers from a script, making the decisions assertable
//! everywhere. Whether the OS tools behave as the argv assumes is a platform
//! fact, exercised per OS at release time.

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

/// The production implementation. A spawn failure is a `RunResult` rather than
/// an error: every caller is probing or tearing down, where "not installed" and
/// "said no" are the same answer.
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

/// The user's real home, independent of the `LOCA_CORE_HOME` override: service
/// definitions must land in the actual `~/Library` or `~/.config` even when
/// core state is redirected. Passed as an argument, never read from the
/// environment, so a test can point it at a tempdir without `set_var`.
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

    /// Records every argv and answers from a script, matched on a substring of
    /// the joined argv so a test need not restate the whole command line.
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
            // Unscripted commands FAIL: a runner answering 0 to anything makes
            // every probe report "installed" and every teardown succeed.
            RunResult::failed(1)
        }
    }
}
