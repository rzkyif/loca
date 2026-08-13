//! loca-core-updater: tiny standalone helper compiled to its own binary,
//! separate from loca-core. It finishes a self-update from a process outside
//! core, on the platforms that need one: Windows (a running .exe can't be
//! replaced in place) and the no-service background mode (nothing else relaunches
//! core). On Unix with a supervisor (launchd / systemd) core swaps the binary in
//! place and the supervisor restarts it, so the updater is not involved. When
//! invoked it:
//!   1. Waits 2s for the parent core process to exit.
//!   2. If `--staged` is given, atomically renames it over `--current`,
//!      preserving the previous binary as `<name>.old` (Unix: a hard link kept
//!      before the atomic rename; Windows: the old .exe renamed aside first).
//!      core's boot-time rollback restores that `.old` if the new binary
//!      crash-loops, and deletes it once the update is committed. When `--staged`
//!      is absent, core already swapped the binary; the updater only restarts.
//!   3. Restarts core: `--supervisor schtask` starts the `--service-label` Task
//!      Scheduler task so the supervisor owns the new instance; otherwise it
//!      spawns core directly with `--restart-args` (forwarded as the new
//!      process's argv), detached (stdin/stdout/stderr null), then exits.
//!
//! Usage:
//!   loca-core-updater --current <path> [--staged <path>] [--restart-args <json>]
//!                      [--supervisor schtask --service-label <task>]
//!
//! Exit codes:
//!   0  success (swap, if any, committed + core restarted)
//!   2  bad arguments (missing --current)
//!   3  swap failed at the Windows "move current aside" stage
//!   4  swap failed at the install-rename stage
//!   5  restart failed (spawn of the new core, or starting the task, failed;
//!      any swap we performed here is reverted)
//!   6  could not create the `<current>.old` rollback anchor (Unix); nothing
//!      was changed on disk, safe to re-run the update
//!
//! Logging: appends `{ISO8601_UTC} {LEVEL} {msg}\n` to a per-channel log file
//! and mirrors WARN/ERROR to stderr. Path resolution order:
//!   1. env LOCA_CORE_HOME -> {LOCA_CORE_HOME}/logs/updater.log
//!   2. derived from --current (binary at <core>/bin/<name>) ->
//!      <core>/logs/updater.log  (two dirs up from the binary, then /logs)
//!   3. env HOME or USERPROFILE -> {home}/.loca/core/logs/updater.log
//!   4. none -> skip file logging (stderr-only for warn/error)

use std::ffi::OsString;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::Duration;

use time::format_description::well_known::iso8601::{
    Config, EncodedConfig, Iso8601, TimePrecision,
};
use time::OffsetDateTime;

// `new Date().toISOString()` == `YYYY-MM-DDTHH:MM:SS.sssZ` (UTC, 3 fractional
// digits, literal Z). time's Iso8601 formatter at 3 decimal places matches it.
// EncodedConfig is built in a const fn chain, so no `macros` feature is needed.
const TS_CONFIG: EncodedConfig = Config::DEFAULT
    .set_time_precision(TimePrecision::Second {
        decimal_digits: std::num::NonZeroU8::new(3),
    })
    .encode();

/// Render the current UTC instant as `YYYY-MM-DDTHH:MM:SS.sssZ`. Falls back to
/// an empty string if formatting somehow fails (it cannot for a valid
/// OffsetDateTime, but we stay panic-free for the `panic = "abort"` profile).
fn now_iso8601() -> String {
    OffsetDateTime::now_utc()
        .format(&Iso8601::<TS_CONFIG>)
        .unwrap_or_default()
}

#[derive(Debug, PartialEq, Eq)]
pub struct Args {
    /// The staged new binary to install over `current`. Absent on Unix service +
    /// background restarts, where core swapped the binary in place before exiting
    /// (a running Unix binary can be renamed over; a Windows .exe cannot), so the
    /// updater only restarts.
    pub staged: Option<String>,
    pub current: String,
    pub restart_args: String,
    /// How to restart core: `Some("schtask")` starts the named Task Scheduler
    /// task (Windows, so the supervisor owns the new instance); otherwise the
    /// updater spawns core directly (background mode, no supervisor).
    pub supervisor: Option<String>,
    /// The supervisor's service identifier (the Task Scheduler task name for
    /// `schtask`).
    pub service_label: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ArgsError {
    MissingRequired,
}

/// Pure arg parser, mirroring the TS `spawnUpdater`. Accepts the flags in any
/// order; `--restart-args` defaults to "[]". Only `--current` is required (an
/// empty value counts as missing); `--staged` is optional (absent = swap already
/// done, just restart). `argv` excludes argv[0], matching Deno.args.
pub fn parse_updater_args(argv: &[String]) -> Result<Args, ArgsError> {
    let mut staged: Option<String> = None;
    let mut current: Option<String> = None;
    let mut restart_args: Option<String> = None;
    let mut supervisor: Option<String> = None;
    let mut service_label: Option<String> = None;

    let mut i = 0;
    while i < argv.len() {
        let take = |dst: &mut Option<String>, i: &mut usize| {
            if let Some(v) = argv.get(*i + 1) {
                *dst = Some(v.clone());
                *i += 2;
            } else {
                *i += 1;
            }
        };
        match argv[i].as_str() {
            "--staged" => take(&mut staged, &mut i),
            "--current" => take(&mut current, &mut i),
            "--restart-args" => take(&mut restart_args, &mut i),
            "--supervisor" => take(&mut supervisor, &mut i),
            "--service-label" => take(&mut service_label, &mut i),
            _ => i += 1,
        }
    }

    match current {
        Some(c) if !c.is_empty() => Ok(Args {
            // Treat an empty `--staged` as absent, mirroring the current guard.
            staged: staged.filter(|s| !s.is_empty()),
            current: c,
            restart_args: restart_args.unwrap_or_else(|| "[]".to_string()),
            supervisor,
            service_label,
        }),
        _ => Err(ArgsError::MissingRequired),
    }
}

/// Outcome of `perform_swap`, mirroring the TS `SwapResult` union. The error
/// stages map to process exit codes (aside -> 3, rename -> 4).
#[derive(Debug)]
pub enum SwapResult {
    Ok,
    /// Windows: failed to move the running binary aside (`current` ->
    /// `current.old`). Nothing was changed on disk.
    AsideFailed(std::io::Error),
    /// Failed to install the staged binary (`staged` -> `current`). On Windows
    /// the aside was already reverted before returning.
    RenameFailed(std::io::Error),
    /// Unix: neither the hard-link nor the copy could create the `<current>.old`
    /// rollback anchor, so installing the staged binary would leave no
    /// recoverable fallback if the new binary fails to spawn. Nothing was
    /// changed on disk (we bail before the install rename).
    AnchorFailed,
}

/// Append `.old` to a path at the byte level (NOT via extension replacement),
/// so a Windows `loca-core.exe` correctly becomes `loca-core.exe.old`,
/// matching what core's boot-time rollback (`rollback.ts`) looks for.
fn old_path(current: &Path) -> PathBuf {
    let mut s: OsString = current.as_os_str().to_os_string();
    s.push(".old");
    PathBuf::from(s)
}

/// Move `staged` over `current`. `is_windows` is passed explicitly (not derived
/// from the host) so tests exercise both branches on any platform (same design
/// as the TS `performSwap(staged, current, isWindows)`).
///
/// Both platforms preserve the previous binary as `<current>.old` so core's
/// boot-time rollback (`rollback.ts`) can restore it if the new binary
/// crash-loops.
///
/// Unix: hard-link `current` -> `<current>.old` (instant, same-FS, keeps the old
/// inode alive), then a single atomic `rename(staged, current)` swaps the new
/// binary in with no window where `current` is missing; then chmod 0o755.
///
/// Windows: the running .exe can't be deleted but can be renamed, so move it
/// aside to `.old` first; if the follow-on install rename fails, revert the
/// aside so the supervisor still finds the previous working binary on relaunch.
pub fn perform_swap(staged: &Path, current: &Path, is_windows: bool) -> SwapResult {
    if is_windows {
        let old = old_path(current);
        // Remove a stale `.old` from a previous swap; ignore any error.
        let _ = std::fs::remove_file(&old);
        if let Err(e) = std::fs::rename(current, &old) {
            return SwapResult::AsideFailed(e);
        }
        if let Err(e) = std::fs::rename(staged, current) {
            // Install rename failed after we'd moved current aside. Revert so
            // the supervisor finds the previous binary at `current`. The caller
            // logs and exits; manual recovery is from `.old` if this also fails.
            let _ = std::fs::rename(&old, current);
            return SwapResult::RenameFailed(e);
        }
        return SwapResult::Ok;
    }

    // Unix: preserve the old binary as `<current>.old` before the atomic rename.
    // A hard link keeps the old inode alive without a copy and without ever
    // leaving `current` missing. Fall back to a copy on filesystems that lack
    // hard links. The anchor is MANDATORY: if neither succeeds we bail BEFORE
    // touching `current`, because installing the staged binary without a
    // rollback anchor would leave no way to recover if the new binary fails to
    // spawn (revert_swap renames `<current>.old` back over `current`).
    let old = old_path(current);
    let _ = std::fs::remove_file(&old);
    if std::fs::hard_link(current, &old).is_err() {
        // Copy fallback (filesystems without hard links). Unlike a hard link, a
        // copy can fail partway (e.g. disk full) and leave a TRUNCATED file that
        // still `exists()`, which would make revert_swap later restore a corrupt
        // binary. Require the copy to complete AND byte-match the source; drop a
        // partial anchor and bail before touching `current`.
        let copied = std::fs::copy(current, &old).unwrap_or(0);
        let src_len = std::fs::metadata(current)
            .map(|m| m.len())
            .unwrap_or(u64::MAX);
        if copied != src_len {
            let _ = std::fs::remove_file(&old);
            return SwapResult::AnchorFailed;
        }
    }
    if !old.exists() {
        return SwapResult::AnchorFailed;
    }
    if let Err(e) = std::fs::rename(staged, current) {
        return SwapResult::RenameFailed(e);
    }
    set_executable(current);
    SwapResult::Ok
}

/// chmod 0o755 on `current`, best-effort. The unix permission API is gated; on
/// non-unix hosts (where the unix branch only runs under test) this is a no-op,
/// matching the TS which only ever chmods on the real Unix path.
#[cfg(unix)]
fn set_executable(current: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(current, std::fs::Permissions::from_mode(0o755));
}

#[cfg(not(unix))]
fn set_executable(_current: &Path) {}

/// Environment values the log resolver depends on, injected so path resolution
/// is unit-testable without mutating the real process environment.
#[derive(Default, Clone)]
pub struct LogEnv {
    pub loca_core_home: Option<String>,
    pub home: Option<String>,
    pub user_profile: Option<String>,
}

impl LogEnv {
    fn from_process() -> Self {
        // Treat empty strings as absent to mirror the TS `length > 0` checks.
        fn non_empty(key: &str) -> Option<String> {
            std::env::var(key).ok().filter(|v| !v.is_empty())
        }
        LogEnv {
            loca_core_home: non_empty("LOCA_CORE_HOME"),
            home: non_empty("HOME"),
            user_profile: non_empty("USERPROFILE"),
        }
    }
}

/// Resolve where to append log lines, in priority order:
///   1. LOCA_CORE_HOME/logs/updater.log
///   2. <current>/../../logs/updater.log  (binary lives at <core>/bin/<name>)
///   3. {HOME|USERPROFILE}/.loca/core/logs/updater.log
///   4. None -> stderr-only
///
/// `current` is the `--current` path (the binary being swapped). Pure: takes
/// the env snapshot so tests pin each branch deterministically.
pub fn resolve_log_path(current: &Path, env: &LogEnv) -> Option<PathBuf> {
    if let Some(home) = env.loca_core_home.as_deref().filter(|v| !v.is_empty()) {
        return Some(Path::new(home).join("logs").join("updater.log"));
    }
    // Derived-from-current: <core>/bin/<name> -> two dirs up is <core>.
    if let Some(bin_dir) = current.parent() {
        if let Some(core_dir) = bin_dir.parent() {
            return Some(core_dir.join("logs").join("updater.log"));
        }
    }
    let home = env
        .home
        .as_deref()
        .filter(|v| !v.is_empty())
        .or_else(|| env.user_profile.as_deref().filter(|v| !v.is_empty()))?;
    Some(
        Path::new(home)
            .join(".loca")
            .join("core")
            .join("logs")
            .join("updater.log"),
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Warn,
    Error,
}

impl Level {
    fn as_str(self) -> &'static str {
        match self {
            Level::Info => "INFO",
            Level::Warn => "WARN",
            Level::Error => "ERROR",
        }
    }
}

/// Holds the resolved log path (computed once from `--current` + env) and lazily
/// opens the file on each write, mirroring the TS append logger. Append failures
/// are silently dropped (disk full, file gone); stderr is the fallback for
/// warn/error.
struct Logger {
    path: Option<PathBuf>,
}

impl Logger {
    fn new(current: &Path) -> Self {
        Logger {
            path: resolve_log_path(current, &LogEnv::from_process()),
        }
    }

    fn log(&self, level: Level, msg: &str) {
        let line = format!("{} {} {}\n", now_iso8601(), level.as_str(), msg);
        if let Some(path) = &self.path {
            // Create the logs dir recursively, then append. Drop any error.
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
                let _ = f.write_all(line.as_bytes());
            }
        }
        if level == Level::Error || level == Level::Warn {
            // Mirror to stderr without the trailing newline (eprintln adds one),
            // matching the TS `console.error(line.trimEnd())`.
            eprintln!("{}", line.trim_end());
        }
    }
}

/// Parse `--restart-args` as a JSON array of strings. Any parse error (or a
/// non-array / non-string-element shape) falls back to an empty Vec, mirroring
/// the TS `try { JSON.parse } catch { [] }`. serde_json honours JSON string
/// escapes (\", \n, \uXXXX, ...).
fn parse_restart_args(raw: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(raw).unwrap_or_default()
}

/// Spawn `current` with `args`, fully detached (all stdio null). std's
/// `Command::spawn` does not wait for or reap the child, and does not kill it
/// when this process exits, so dropping the returned `Child` detaches it on both
/// unix and Windows: equivalent to Deno's `proc.spawn()` with null stdio.
fn spawn_new_core(current: &Path, args: &[String]) -> std::io::Result<()> {
    Command::new(current)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_child| ())
}

/// Revert the swap after a spawn failure by restoring the previous binary from
/// the `<current>.old` anchor that perform_swap preserved on both platforms.
/// `rename(old, current)` atomically replaces the unspawnable new binary, so
/// `current` is never missing. (The old Unix path renamed `current` back to
/// `staged`, which left `current` missing entirely.)
fn revert_swap(current: &Path, is_windows: bool) -> std::io::Result<()> {
    let old = old_path(current);
    if is_windows {
        // Windows can't overwrite the (failed-to-spawn) running-ish binary via
        // rename, so remove it first, then move `.old` back.
        let _ = std::fs::remove_file(current); // .catch(() => {}) in TS
    }
    std::fs::rename(&old, current)
}

fn usage() {
    eprintln!("usage: loca-core-updater --staged <path> --current <path> [--restart-args <json>]");
}

fn main() -> ExitCode {
    // argv[0] is the program name; skip it to match Deno.args semantics.
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let args = match parse_updater_args(&argv) {
        Ok(a) => a,
        Err(ArgsError::MissingRequired) => {
            usage();
            return ExitCode::from(2);
        }
    };
    run(&args, cfg!(windows), Duration::from_secs(2))
}

/// Core orchestration, factored out of `main` so the OS flag and settle duration
/// are injectable. Returns the process exit code.
fn run(args: &Args, is_windows: bool, settle: Duration) -> ExitCode {
    // Anchor logs to the channel of the binary we're swapping before logging.
    let log = Logger::new(Path::new(&args.current));
    log.log(
        Level::Info,
        &format!(
            "started; staged={} current={}",
            args.staged.as_deref().unwrap_or("<none>"),
            args.current
        ),
    );

    // Let core exit cleanly.
    std::thread::sleep(settle);

    let current = Path::new(&args.current);

    // `--staged` is present only when the updater owns the swap (Windows, where
    // the running .exe can't be replaced in place). On Unix, core swapped the
    // binary before exiting, so there is nothing to swap here.
    let swapped = if let Some(staged) = args.staged.as_deref() {
        match perform_swap(Path::new(staged), current, is_windows) {
            SwapResult::Ok => {}
            SwapResult::AsideFailed(e) => {
                log.log(
                    Level::Error,
                    &format!("failed to move current binary aside: {}", e),
                );
                return ExitCode::from(3);
            }
            SwapResult::RenameFailed(e) => {
                log.log(
                    Level::Error,
                    &format!("failed to install staged binary: {}", e),
                );
                return ExitCode::from(4);
            }
            SwapResult::AnchorFailed => {
                log.log(
                    Level::Error,
                    "could not create rollback anchor (<current>.old); refusing to install \
                     without a recoverable fallback. Nothing changed; re-run the update.",
                );
                return ExitCode::from(6);
            }
        }
        log.log(Level::Info, "swap committed");
        true
    } else {
        false
    };

    // Restart core. `schtask` starts the Task Scheduler task so the supervisor
    // owns the new instance; otherwise (background mode) spawn core directly.
    if args.supervisor.as_deref() == Some("schtask") {
        let task = args.service_label.as_deref().unwrap_or_default();
        return match start_scheduled_task(task) {
            Ok(()) => {
                log.log(
                    Level::Info,
                    &format!("started scheduled task {}; exiting", task),
                );
                ExitCode::SUCCESS
            }
            Err(e) => {
                // The new binary is installed; leave it in place (Task Scheduler
                // starts it at next logon) rather than rolling back a good update.
                log.log(
                    Level::Error,
                    &format!("failed to start scheduled task {}: {}", task, e),
                );
                ExitCode::from(5)
            }
        };
    }

    let restart_args = parse_restart_args(&args.restart_args);
    match spawn_new_core(current, &restart_args) {
        Ok(()) => {
            log.log(Level::Info, "spawned new core; exiting");
            ExitCode::SUCCESS
        }
        Err(e) => {
            log.log(Level::Error, &format!("failed to spawn new core: {}", e));
            // Only a swap we performed here can be reverted; if core swapped in
            // place before exit, its own boot-time rollback owns recovery.
            if swapped {
                match revert_swap(current, is_windows) {
                    Ok(()) => log.log(Level::Warn, "swap reverted after spawn failure"),
                    Err(re) => log.log(
                        Level::Error,
                        &format!("revert failed; manual intervention required: {}", re),
                    ),
                }
            }
            ExitCode::from(5)
        }
    }
}

/// Start the named Task Scheduler task via `schtasks /Run`, so a Windows update
/// hands the relaunch back to the supervisor that owns core at logon. Runs the
/// real command on Windows; on other platforms (only reached under test) it is a
/// no-op success, since `schtask` is passed only on Windows.
fn start_scheduled_task(task: &str) -> std::io::Result<()> {
    if !cfg!(windows) {
        return Ok(());
    }
    let status = Command::new("schtasks")
        .args(["/Run", "/TN", task])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "schtasks /Run exited with {}",
            status
        )))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    // Unique tempdir under std::env::temp_dir(); a process-wide counter avoids
    // collisions between tests sharing the same nanosecond. Cleaned up on drop.
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    struct TempDir {
        path: PathBuf,
    }
    impl TempDir {
        fn new() -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "loca-updater-test-{}-{}-{}",
                std::process::id(),
                nanos,
                n
            ));
            std::fs::create_dir_all(&path).unwrap();
            TempDir { path }
        }
        fn join(&self, name: &str) -> PathBuf {
            self.path.join(name)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn write(path: &Path, content: &str) {
        std::fs::write(path, content).unwrap();
    }
    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap()
    }
    fn argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    // --- arg parsing (ports main.test.ts) ----------------------------------

    #[test]
    fn accepts_staged_and_current_defaults_restart_args() {
        let r = parse_updater_args(&argv(&[
            "--staged",
            "/tmp/new",
            "--current",
            "/usr/local/bin/loca-core",
        ]))
        .unwrap();
        assert_eq!(r.staged.as_deref(), Some("/tmp/new"));
        assert_eq!(r.current, "/usr/local/bin/loca-core");
        assert_eq!(r.restart_args, "[]");
        assert_eq!(r.supervisor, None);
    }

    #[test]
    fn forwards_restart_args_verbatim() {
        let r = parse_updater_args(&argv(&[
            "--staged",
            "/tmp/new",
            "--current",
            "/usr/local/bin/loca-core",
            "--restart-args",
            r#"["--port","8000"]"#,
        ]))
        .unwrap();
        assert_eq!(r.restart_args, r#"["--port","8000"]"#);
    }

    #[test]
    fn staged_is_optional_current_alone_is_a_restart_only_run() {
        // Unix service + background restarts pass no `--staged` (core already
        // swapped the binary), so `--current` alone must parse.
        let r = parse_updater_args(&argv(&["--current", "/x"])).unwrap();
        assert_eq!(r.staged, None);
        assert_eq!(r.current, "/x");
    }

    #[test]
    fn parses_schtask_supervisor_and_label() {
        let r = parse_updater_args(&argv(&[
            "--current",
            "/x",
            "--staged",
            "/tmp/new",
            "--supervisor",
            "schtask",
            "--service-label",
            "loca-core-latest",
        ]))
        .unwrap();
        assert_eq!(r.supervisor.as_deref(), Some("schtask"));
        assert_eq!(r.service_label.as_deref(), Some("loca-core-latest"));
    }

    #[test]
    fn missing_current_is_error() {
        assert_eq!(
            parse_updater_args(&argv(&["--staged", "/x"])),
            Err(ArgsError::MissingRequired)
        );
    }

    #[test]
    fn empty_current_is_treated_as_missing() {
        assert_eq!(
            parse_updater_args(&argv(&["--staged", "/x", "--current", ""])),
            Err(ArgsError::MissingRequired)
        );
    }

    #[test]
    fn empty_staged_is_treated_as_absent() {
        let r = parse_updater_args(&argv(&["--staged", "", "--current", "/x"])).unwrap();
        assert_eq!(r.staged, None);
    }

    #[test]
    fn empty_argv_is_error() {
        assert_eq!(parse_updater_args(&[]), Err(ArgsError::MissingRequired));
    }

    // --- swap (ports swap.test.ts) -----------------------------------------

    #[test]
    fn unix_moves_staged_over_current_and_preserves_old_anchor() {
        let d = TempDir::new();
        let staged = d.join("staged");
        let current = d.join("current");
        write(&staged, "v2");
        write(&current, "v1");

        assert!(matches!(
            perform_swap(&staged, &current, false),
            SwapResult::Ok
        ));
        assert_eq!(read(&current), "v2");
        assert!(!staged.exists());
        // The previous binary must survive as `<current>.old` so boot rollback
        // can restore it on Unix (it didn't before; rollback was a no-op there).
        assert_eq!(read(&old_path(&current)), "v1");
    }

    #[test]
    fn unix_revert_restores_old_binary_over_current() {
        let d = TempDir::new();
        let staged = d.join("staged");
        let current = d.join("current");
        write(&staged, "v2");
        write(&current, "v1");

        // Install v2, preserving v1 as `.old`.
        assert!(matches!(
            perform_swap(&staged, &current, false),
            SwapResult::Ok
        ));
        // Spawn-failure revert must put the working v1 back at `current` (the old
        // path renamed current -> staged, leaving current missing).
        revert_swap(&current, false).unwrap();
        assert_eq!(read(&current), "v1");
    }

    #[test]
    fn windows_renames_current_to_old_before_install() {
        let d = TempDir::new();
        let staged = d.join("staged");
        let current = d.join("current");
        write(&staged, "v2");
        write(&current, "v1");

        assert!(matches!(
            perform_swap(&staged, &current, true),
            SwapResult::Ok
        ));
        assert_eq!(read(&current), "v2");
        assert_eq!(read(&old_path(&current)), "v1");
    }

    #[test]
    fn windows_stale_old_is_replaced_not_preserved() {
        let d = TempDir::new();
        let staged = d.join("staged");
        let current = d.join("current");
        write(&staged, "v3");
        write(&current, "v2");
        write(&old_path(&current), "v1"); // leftover from an earlier upgrade

        assert!(matches!(
            perform_swap(&staged, &current, true),
            SwapResult::Ok
        ));
        assert_eq!(read(&old_path(&current)), "v2");
    }

    #[test]
    fn rename_failure_surfaces_stage_rename() {
        let d = TempDir::new();
        let staged = d.join("does-not-exist");
        let current = d.join("current");
        write(&current, "v1");

        assert!(matches!(
            perform_swap(&staged, &current, false),
            SwapResult::RenameFailed(_)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn unix_chmod_makes_current_executable() {
        use std::os::unix::fs::PermissionsExt;
        let d = TempDir::new();
        let staged = d.join("staged");
        let current = d.join("current");
        write(&staged, "v2");
        write(&current, "v1");

        perform_swap(&staged, &current, false);
        let mode = std::fs::metadata(&current).unwrap().permissions().mode();
        // set_permissions forces 0o755, so the user-exec bit is deterministic.
        assert_ne!(mode & 0o100, 0);
    }

    #[test]
    fn windows_rename_failure_after_aside_reverts_old_to_current() {
        let d = TempDir::new();
        let staged = d.join("does-not-exist");
        let current = d.join("current");
        write(&current, "v1");

        assert!(matches!(
            perform_swap(&staged, &current, true),
            SwapResult::RenameFailed(_)
        ));
        // current restored from .old; the .old anchor is consumed by the revert.
        assert_eq!(read(&current), "v1");
        assert!(!old_path(&current).exists());
    }

    // --- restart-args parsing ----------------------------------------------

    #[test]
    fn restart_args_parses_string_array() {
        assert_eq!(
            parse_restart_args(r#"["--port","8000"]"#),
            vec!["--port".to_string(), "8000".to_string()]
        );
    }

    #[test]
    fn restart_args_handles_escapes() {
        assert_eq!(
            parse_restart_args(r#"["a\"b","c\nd"]"#),
            vec!["a\"b".to_string(), "c\nd".to_string()]
        );
    }

    #[test]
    fn restart_args_falls_back_to_empty_on_parse_error() {
        assert_eq!(parse_restart_args("not json"), Vec::<String>::new());
        assert_eq!(parse_restart_args("[1,2]"), Vec::<String>::new()); // not strings
        assert_eq!(parse_restart_args("{}"), Vec::<String>::new()); // not an array
        assert_eq!(parse_restart_args("[]"), Vec::<String>::new());
    }

    // --- log path resolution -----------------------------------------------

    #[test]
    fn log_path_prefers_loca_core_home() {
        let env = LogEnv {
            loca_core_home: Some("/srv/tcore".to_string()),
            home: Some("/home/u".to_string()),
            user_profile: None,
        };
        assert_eq!(
            resolve_log_path(Path::new("/x/bin/loca-core"), &env),
            Some(PathBuf::from("/srv/tcore/logs/updater.log"))
        );
    }

    #[test]
    fn log_path_derives_from_current_when_no_explicit_home() {
        let env = LogEnv {
            loca_core_home: None,
            home: Some("/home/u".to_string()),
            user_profile: None,
        };
        // <core>/bin/<name> -> <core>/logs/updater.log
        assert_eq!(
            resolve_log_path(
                Path::new("/home/u/.loca/latest/core/bin/loca-core-latest"),
                &env
            ),
            Some(PathBuf::from("/home/u/.loca/latest/core/logs/updater.log"))
        );
    }

    #[test]
    fn log_path_falls_back_to_home_dot_loca() {
        // current is a bare filename, so derivation fails and we fall through.
        let env = LogEnv {
            loca_core_home: None,
            home: Some("/home/u".to_string()),
            user_profile: None,
        };
        assert_eq!(
            resolve_log_path(Path::new("loca-core"), &env),
            Some(PathBuf::from("/home/u/.loca/core/logs/updater.log"))
        );
    }

    #[test]
    fn log_path_uses_user_profile_when_home_absent() {
        let env = LogEnv {
            loca_core_home: None,
            home: None,
            user_profile: Some("C:\\Users\\u".to_string()),
        };
        let got = resolve_log_path(Path::new("loca-core"), &env).unwrap();
        assert!(got.ends_with("updater.log"));
        assert!(got.to_string_lossy().contains(".loca"));
    }

    #[test]
    fn log_path_none_when_nothing_resolves() {
        let env = LogEnv {
            loca_core_home: None,
            home: None,
            user_profile: None,
        };
        // Bare filename: no parent.parent(), and no home env -> None.
        assert_eq!(resolve_log_path(Path::new("loca-core"), &env), None);
    }

    // --- timestamp format --------------------------------------------------

    #[test]
    fn timestamp_matches_iso8601_millis_z() {
        let ts = now_iso8601();
        // YYYY-MM-DDTHH:MM:SS.sssZ -> exactly 24 chars, ends with Z, '.' at 19.
        assert_eq!(ts.len(), 24, "got {ts}");
        assert!(ts.ends_with('Z'), "got {ts}");
        assert_eq!(ts.as_bytes()[19], b'.', "got {ts}");
        assert_eq!(ts.as_bytes()[10], b'T', "got {ts}");
    }
}
