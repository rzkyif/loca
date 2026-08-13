// Pairing-flow commands for the LOCAL core (installed by the native
// installer; the client never installs one itself).
//
//  - `read_admin_token`: read ~/.loca/<channel>/core/.admin-token off disk
//    so the client can mint pairing codes on the LOCAL core. Returns None if
//    the file doesn't exist (e.g. paired with a remote core).
//
//  - `local_core_installed`: whether the installer planted a core binary on
//    disk. Drives the first-boot silent pair and the on-demand spawn.
//
//  - `local_core_running`: whether the local core's port is answering. Drives
//    the CoreBar's "Stopped" state and the tray's Start/Stop item.
//
//  - `start_local_core` / `stop_local_core`: idempotently start or stop the
//    locally-installed core. Both delegate to the core binary's own
//    `start-service` / `stop-service` verbs, which resolve the supervisor
//    (launchd / systemd / Scheduled Task / none) and act through it. A plain
//    kill would be undone by launchd's KeepAlive; the verbs are the one place
//    that logic lives.

use crate::error::{AppError, AppResult};
use crate::state::AppState;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::Ordering;
use tauri::State;

/// True when this is the dev-channel build. The dev core runs from source via
/// `deno task dev`, so there's no installed binary to spawn.
fn is_dev() -> bool {
    crate::channel::channel() == "dev"
}

/// True when the developer opted into exercising the no-local-core welcome
/// flow in dev (`deno task dev:reset:install` sets LOCA_DEV_FRESH_INSTALL).
/// Always false outside dev.
fn dev_fresh_install_requested() -> bool {
    is_dev()
        && std::env::var("LOCA_DEV_FRESH_INSTALL")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
}

#[tauri::command]
pub fn read_admin_token() -> AppResult<Option<String>> {
    read_admin_token_at(&admin_token_path()?)
}

fn read_admin_token_at(path: &std::path::Path) -> AppResult<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                Ok(None)
            } else {
                Ok(Some(trimmed.to_string()))
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(AppError::Io(err)),
    }
}

fn admin_token_path() -> AppResult<PathBuf> {
    let home = std::env::home_dir()
        .ok_or_else(|| AppError::external("could not determine home directory"))?;
    Ok(crate::channel::channel_root(&home)
        .join("core")
        .join(".admin-token"))
}

fn boot_error_path() -> AppResult<PathBuf> {
    let home = std::env::home_dir()
        .ok_or_else(|| AppError::external("could not determine home directory"))?;
    Ok(crate::channel::channel_root(&home)
        .join("core")
        .join("last-error.txt"))
}

/// Read the local core's last fatal boot-failure reason (one line), if any. The
/// core writes it on a fatal startup path and clears it once it next binds, so
/// this is non-empty only when the local core failed to come up (port in use,
/// missing helper, ...). Lets the pair flow explain an otherwise-opaque
/// connection failure. Returns None when the file is absent or blank. Reuses the
/// admin-token reader, which is the same "trimmed non-empty string or None".
#[tauri::command]
pub fn read_local_core_boot_error() -> AppResult<Option<String>> {
    read_admin_token_at(&boot_error_path()?)
}

fn local_core_binary() -> AppResult<PathBuf> {
    let home = std::env::home_dir()
        .ok_or_else(|| AppError::external("could not determine home directory"))?;
    // Channel-suffixed: latest's core lives at .../bin/loca-core-latest(.exe).
    Ok(crate::channel::channel_root(&home)
        .join("core")
        .join("bin")
        .join(crate::channel::core_binary_name()))
}

/// Returns `true` if the local core binary exists on disk.
#[tauri::command]
pub fn local_core_installed() -> AppResult<bool> {
    // Dev runs the core from source (no installed binary), but a dev core IS
    // running via `deno task dev`. Report it as installed so the first-boot
    // silent pair connects to it, unless the developer opted into the
    // fresh-install flow, where `false` makes the welcome remote-connect page
    // show instead.
    if is_dev() {
        return Ok(!dev_fresh_install_requested());
    }
    Ok(local_core_binary().map(|p| p.exists()).unwrap_or(false))
}

/// Loopback base URL of THIS channel's local core, with the channel-aware port
/// (stable 7800, latest 7810, …). The UI uses it for the local-core pair flow
/// so a latest client targets the latest core.
#[tauri::command]
pub fn local_core_base_url() -> String {
    format!("https://127.0.0.1:{}", crate::channel::core_port())
}

/// This channel's default local sidecar ports (llama / speech). The UI uses
/// them as fallbacks when the paired core hasn't overridden llm.port/stt.port,
/// so a latest client talks to the latest sidecars (7711/7712), not stable's.
#[tauri::command]
pub fn local_sidecar_ports() -> std::collections::HashMap<String, u16> {
    let mut m = std::collections::HashMap::new();
    m.insert("llm".to_string(), crate::channel::llm_port());
    m.insert("stt".to_string(), crate::channel::stt_port());
    m
}

/// Whether the local core's port is answering. Cheap enough for the UI to call
/// on every connection-state change; see `probe_local_core`.
#[tauri::command]
pub async fn local_core_running() -> bool {
    probe_local_core().await
}

/// Start the locally-installed core when it isn't already running. Idempotent:
/// if the port already answers, this is a no-op and returns `false`. Returns
/// `true` if a new core was started.
///
/// Delegates to the core binary's `start-service` verb, which resolves the
/// supervisor and starts through it (spawning directly would fight launchd's
/// KeepAlive). The verb prints `{"supervisor":..,"pid":..}`; a `pid` arrives
/// only in background mode, where nothing supervises the process and the client
/// therefore owns its liveness.
#[tauri::command]
pub async fn start_local_core(state: State<'_, AppState>) -> AppResult<bool> {
    // Dev: the core is started from source by `deno task dev`, not a binary we
    // can spawn here. Confirm it's reachable; never try to launch it.
    if is_dev() {
        return if probe_local_core().await {
            Ok(false)
        } else {
            Err(AppError::external(
                "dev core not reachable on 127.0.0.1:7820. Is `deno task dev` running?",
            ))
        };
    }

    // Cheap liveness probe: if 127.0.0.1:7800 is already answering, leave it.
    if probe_local_core().await {
        return Ok(false);
    }

    let stdout = run_core_verb("start-service").await?;
    // Record a background-mode core so app-exit stops exactly it. A supervised
    // core reports no pid: the service outlives the client and must not be
    // stopped when it quits.
    state
        .0
        .spawned_core_pid
        .store(background_pid(&stdout).unwrap_or(0), Ordering::SeqCst);

    // Best-effort wait for the port to come up so the next pairing /
    // settings call doesn't race with the start.
    for _ in 0..20 {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        if probe_local_core().await {
            return Ok(true);
        }
    }
    // It might still be coming up, so return success rather than block the UI.
    Ok(true)
}

/// Stop the locally-installed core, through its supervisor when it has one.
/// Idempotent: stopping an already-stopped core is not an error. Clears the
/// spawned-pid record so the app-exit handler doesn't signal a dead PID.
#[tauri::command]
pub async fn stop_local_core(state: State<'_, AppState>) -> AppResult<()> {
    if is_dev() {
        return Err(AppError::external(
            "the dev core is owned by `deno task dev`; stop it there",
        ));
    }
    run_core_verb("stop-service").await?;
    state.0.spawned_core_pid.store(0, Ordering::SeqCst);
    Ok(())
}

/// Run one of the installed core binary's install-CLI verbs to completion and
/// return its stdout. The verb's human-readable progress goes to stderr, so
/// stdout carries only the machine-readable JSON line (if any).
async fn run_core_verb(verb: &'static str) -> AppResult<String> {
    let bin = local_core_binary()?;
    if !bin.exists() {
        return Err(AppError::external(format!(
            "local core not installed at {}",
            bin.display(),
        )));
    }
    let out = tokio::task::spawn_blocking(move || {
        let mut cmd = Command::new(&bin);
        cmd.arg(verb)
            // The compiled-in channel isn't an OS env var, so the core wouldn't
            // otherwise know which channel's install to act on.
            .env("LOCA_CHANNEL", crate::channel::channel());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        cmd.output()
    })
    .await
    .map_err(|e| AppError::external(format!("{verb} task failed: {e}")))??;
    if !out.status.success() {
        return Err(AppError::external(format!(
            "core {verb} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim(),
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The PID from `start-service`'s JSON line, but ONLY when it reports the
/// background supervisor. Any supervised kind (launchd / systemd / schtask)
/// owns the core's lifetime, so the client must not record - and later stop -
/// its PID. Unparseable output yields `None`: the core is running either way,
/// we just don't claim ownership of it.
fn background_pid(stdout: &str) -> Option<u32> {
    let line = stdout.lines().rev().find(|l| !l.trim().is_empty())?;
    let v: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    if v.get("supervisor")?.as_str()? != "background" {
        return None;
    }
    u32::try_from(v.get("pid")?.as_u64()?).ok()
}

/// Minimal liveness probe of the local core: a plain TCP connect to the core
/// port. The core now serves TLS, so we can't speak HTTP/1.0 in the clear here;
/// a successful connect means the listener is up. The real health check happens
/// over the pinned TLS net layer once paired. Raw TcpStream avoids pulling a
/// TLS client into this one cheap probe.
async fn probe_local_core() -> bool {
    use std::net::TcpStream;
    use std::time::Duration;
    let task = tokio::task::spawn_blocking(|| -> bool {
        let Ok(addr) = format!("127.0.0.1:{}", crate::channel::core_port()).parse() else {
            return false;
        };
        TcpStream::connect_timeout(&addr, Duration::from_millis(150)).is_ok()
    });
    task.await.unwrap_or(false)
}

/// Stop a core THIS session spawned (service-less mode). Called from the app's
/// exit handler so quitting the client also stops a core it started. Best-effort
/// and non-blocking: `pid == 0` (we never spawned one, e.g. a background service
/// owns the core) is a no-op. Unix sends SIGTERM so the core runs its graceful
/// shutdown (sidecar/MCP teardown). Windows hard-kills the tree; the in-core Job
/// Object still reaps sidecars when the core dies, so nothing is orphaned.
pub fn stop_spawned_core(pid: u32) {
    if pid == 0 {
        return;
    }
    #[cfg(unix)]
    {
        // Guard against PID reuse: a mid-session core crash could free this PID
        // for an unrelated process, and we must not signal that. Only proceed if
        // the PID still looks like our core. "loca-core" survives Linux's
        // 15-char `comm` truncation and appears in macOS's full-path `comm`.
        let looks_like_core = Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "comm="])
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains("loca-core"))
            .unwrap_or(false);
        if !looks_like_core {
            return;
        }
        unsafe extern "C" {
            fn kill(pid: i32, sig: i32) -> i32;
        }
        const SIGTERM: i32 = 15;
        unsafe {
            kill(pid as i32, SIGTERM);
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        // The IMAGENAME filter is the PID-reuse guard: taskkill matches nothing
        // (a no-op) unless the PID is still a loca-core process, so a recycled
        // PID belonging to something else is never killed.
        let image = crate::channel::core_binary_name();
        let _ = Command::new("taskkill")
            .args([
                "/F",
                "/T",
                "/FI",
                &format!("PID eq {pid}"),
                "/FI",
                &format!("IMAGENAME eq {image}"),
            ])
            .creation_flags(CREATE_NO_WINDOW)
            .status();
    }
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchPrefill {
    core_url: Option<String>,
    pairing_code: Option<String>,
}

/// Read optional `--core-url` / `--pairing-code` launch arguments (both
/// `--flag value` and `--flag=value` accepted) for the onboarding remote-core
/// prefill. `None` when neither is present. Doubles as a shareable setup
/// command, and is how `deno task dev` hands the dev core URL + minted code to
/// the client.
#[tauri::command]
pub fn read_launch_prefill() -> Option<LaunchPrefill> {
    let args: Vec<String> = std::env::args().collect();
    let core_url = arg_value(&args, "--core-url");
    let pairing_code = arg_value(&args, "--pairing-code");
    if core_url.is_none() && pairing_code.is_none() {
        return None;
    }
    Some(LaunchPrefill {
        core_url,
        pairing_code,
    })
}

/// Find `--name=value` or `--name value` in `args`; `None` if absent.
fn arg_value(args: &[String], name: &str) -> Option<String> {
    let eq_prefix = format!("{name}=");
    for (i, a) in args.iter().enumerate() {
        if let Some(v) = a.strip_prefix(&eq_prefix) {
            return Some(v.to_string());
        }
        if a == name {
            return args.get(i + 1).cloned();
        }
    }
    None
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn read_admin_token_missing_file_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("no-such-file");
        assert_eq!(read_admin_token_at(&path).unwrap(), None);
    }

    #[test]
    fn read_admin_token_empty_file_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty");
        std::fs::write(&path, "   \n").unwrap();
        assert_eq!(read_admin_token_at(&path).unwrap(), None);
    }

    #[test]
    fn read_admin_token_trims_whitespace_and_newlines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tok");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "  abc123  ").unwrap();
        assert_eq!(
            read_admin_token_at(&path).unwrap(),
            Some("abc123".to_string())
        );
    }

    #[test]
    fn arg_value_reads_equals_form() {
        let args = vec!["app".into(), "--core-url=https://x:7820".into()];
        assert_eq!(
            arg_value(&args, "--core-url").as_deref(),
            Some("https://x:7820")
        );
    }

    #[test]
    fn arg_value_reads_space_form() {
        let args = vec!["app".into(), "--pairing-code".into(), "123456".into()];
        assert_eq!(
            arg_value(&args, "--pairing-code").as_deref(),
            Some("123456")
        );
    }

    #[test]
    fn arg_value_absent_is_none() {
        let args = vec!["app".into(), "--other".into()];
        assert_eq!(arg_value(&args, "--core-url"), None);
    }

    #[test]
    fn background_pid_reads_the_json_line() {
        assert_eq!(
            background_pid("{\"supervisor\":\"background\",\"pid\":4242}\n"),
            Some(4242)
        );
    }

    #[test]
    fn background_pid_ignores_a_supervised_core() {
        // launchd / systemd / schtask own the core's lifetime; recording their
        // PID would make quitting the client stop a service that must persist.
        for sup in ["launchd", "systemd", "schtask"] {
            let line = format!("{{\"supervisor\":\"{sup}\",\"pid\":4242}}");
            assert_eq!(background_pid(&line), None);
        }
    }

    #[test]
    fn background_pid_tolerates_unparseable_output() {
        for raw in [
            "",
            "\n",
            "not json",
            "{\"supervisor\":\"background\",\"pid\":null}",
        ] {
            assert_eq!(background_pid(raw), None);
        }
    }
}
