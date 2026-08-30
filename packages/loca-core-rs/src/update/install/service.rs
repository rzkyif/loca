//! OS background-service registration and teardown.
//!
//! The single implementation of service registration: it writes the launchd
//! LaunchAgent, the systemd user unit or the Task Scheduler task that starts
//! core at login and restarts it on failure, namespaced per channel so stable,
//! latest and dev coexist. `uninstall-service` reverses it. The definitions
//! themselves are [`supervisor`](super::supervisor)'s text; this file owns the
//! ORDER of the commands, what each branch does with a non-zero exit, and the
//! teardown.
//!
//! Every platform decision arrives in [`ServiceCtx`] as data, so all branches
//! run on any host. `home` is separate from `Paths`: service definitions must
//! land in the real `~/Library` or `~/.config` even when core state is
//! redirected, and passing it in avoids a `set_var` that is unsound under
//! `cargo test`'s threads.
//!
//! `uninstall-service` runs FROM the binary it is sweeping, so the straggler
//! sweep must exclude this pid. Losing that does not fail loudly: it kills the
//! uninstall mid-flight, leaving the data and the keychain master key behind.

use std::path::{Path, PathBuf};

use crate::config::{Channel, Config};
use crate::error::{internal, Result};
use crate::paths::Paths;
use crate::update::install::io::{start_service_result, Io};
use crate::update::install::proc::{RunResult, Runner};
use crate::update::install::supervisor::{
    encode_pwsh_command, plist_path, plist_xml, psq, resolve_supervisor, scheduled_task_script,
    scheduled_task_teardown_script, service_label, systemd_unit, systemd_unit_path,
    uninstall_command, unit_name, windows_display_name, windows_uninstall_key, Os, Supervisor,
};

/// The four `LOCA_INSTALL_*` variables, read once at the edge and passed as a
/// parameter, so a test never needs `set_var`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InstallEnv {
    /// `LOCA_INSTALL_SERVICE`. `0` means background mode: no service, and the
    /// client owns the core's liveness.
    pub as_service: bool,
    /// `LOCA_INSTALL_BIND_ALL`.
    pub bind_all: bool,
    /// `LOCA_INSTALL_BEHIND_PROXY`.
    pub behind_proxy: bool,
    /// `LOCA_INSTALL_REGISTER_ARP`. `0` skips the Add/Remove Programs entry,
    /// which the Windows installer sets because its `uninstall.exe` owns it.
    pub register_arp: bool,
}

impl Default for InstallEnv {
    fn default() -> Self {
        Self::parse(None, None, None, None)
    }
}

impl InstallEnv {
    /// Production: read the four variables once.
    pub fn from_env() -> Self {
        let get = |key: &str| std::env::var(key).ok();
        Self::parse(
            get("LOCA_INSTALL_SERVICE").as_deref(),
            get("LOCA_INSTALL_BIND_ALL").as_deref(),
            get("LOCA_INSTALL_BEHIND_PROXY").as_deref(),
            get("LOCA_INSTALL_REGISTER_ARP").as_deref(),
        )
    }

    /// The parse, separated from the environment that feeds it. The two
    /// polarities are not interchangeable: `_SERVICE` and `_REGISTER_ARP`
    /// default ON and are turned off by exactly `"0"`, while `_BIND_ALL` and
    /// `_BEHIND_PROXY` default OFF and are turned on by exactly `"1"`.
    pub fn parse(
        service: Option<&str>,
        bind_all: Option<&str>,
        behind_proxy: Option<&str>,
        register_arp: Option<&str>,
    ) -> Self {
        Self {
            as_service: service != Some("0"),
            bind_all: bind_all == Some("1"),
            behind_proxy: behind_proxy == Some("1"),
            register_arp: register_arp != Some("0"),
        }
    }
}

/// Removing the keychain master key, behind a seam so a test suite does not
/// delete the developer's own entry.
#[async_trait::async_trait]
pub trait MasterKey: Send + Sync {
    async fn delete(&self) -> Result<()>;
}

/// The production implementation, clearing both the keychain entry and the
/// dotfile fallback.
pub struct VaultMasterKey<'a> {
    pub paths: &'a Paths,
    pub channel: Channel,
}

#[async_trait::async_trait]
impl MasterKey for VaultMasterKey<'_> {
    async fn delete(&self) -> Result<()> {
        crate::secrets::Vault::new(self.paths, self.channel)
            .delete_master_key()
            .await
    }
}

/// Everything a service verb needs, with every platform decision as data.
pub struct ServiceCtx<'a> {
    pub config: &'a Config,
    pub paths: &'a Paths,
    pub runner: &'a dyn Runner,
    pub io: &'a dyn Io,
    pub master_key: &'a dyn MasterKey,
    pub os: Os,
    /// The user's REAL home, not the `LOCA_CORE_HOME` override.
    pub home: &'a Path,
    /// This process, excluded from every straggler sweep.
    pub self_pid: u32,
    /// This process's on-disk image, which Windows keeps locked.
    pub current_exe: PathBuf,
    pub env: InstallEnv,
}

impl ServiceCtx<'_> {
    fn channel(&self) -> Channel {
        self.config.channel
    }

    fn installed_binary(&self) -> PathBuf {
        self.paths
            .bin_path(&self.channel().core_binary_name("loca-core"))
    }

    async fn run(&self, argv: &[&str]) -> RunResult {
        self.runner.run(argv).await
    }

    /// One place, so the flags cannot drift between the callers that read the
    /// output and the ones that do not.
    async fn pwsh(&self, script: &str) -> RunResult {
        self.run(&[
            "powershell",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            script,
        ])
        .await
    }
}

// --- install ---------------------------------------------------------------

/// Register, and start, the OS service.
pub async fn install_service(ctx: &ServiceCtx<'_>) -> Result<()> {
    tokio::fs::create_dir_all(ctx.paths.logs_dir()).await?;

    if !ctx.env.as_service {
        start_background(ctx).await?;
    } else {
        match ctx.os {
            Os::Macos => install_launchd(ctx).await?,
            Os::Linux => install_systemd(ctx).await?,
            Os::Windows => install_scheduled_task(ctx).await?,
            Os::Other => {
                return Err(internal("unsupported OS for service install"));
            }
        }
    }
    // The one place the Core is listed in Add/Remove Programs. Whether a task
    // was registered decides whether the uninstall string self-elevates.
    if ctx.os == Os::Windows && ctx.env.register_arp {
        register_windows_uninstall_entry(ctx, ctx.env.as_service).await;
    }
    Ok(())
}

async fn install_launchd(ctx: &ServiceCtx<'_>) -> Result<()> {
    let plist = plist_path(ctx.home, ctx.channel());
    if let Some(parent) = plist.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(
        &plist,
        plist_xml(
            ctx.channel(),
            &ctx.installed_binary(),
            &ctx.paths.logs_dir(),
        ),
    )
    .await?;
    let plist_str = plist.to_string_lossy().into_owned();
    // Unload first so a re-install replaces a running agent. Non-zero here is
    // benign (nothing was loaded).
    let _ = ctx.run(&["launchctl", "unload", &plist_str]).await;
    let loaded = ctx.run(&["launchctl", "load", &plist_str]).await;
    if !loaded.success() {
        return Err(internal(format!(
            "launchctl load failed (exit {}); inspect {plist_str}",
            loaded.code
        )));
    }
    ctx.io.progress(&format!(
        "registered launchd agent {}",
        service_label(ctx.channel())
    ));
    Ok(())
}

async fn install_systemd(ctx: &ServiceCtx<'_>) -> Result<()> {
    if !have_systemd_user(ctx).await {
        // A unit directory with no user manager is a background install, and
        // saying so beats failing: the core still starts.
        ctx.io
            .progress("systemd --user unavailable; starting core in the background instead");
        start_background(ctx).await?;
        return Ok(());
    }
    let unit = systemd_unit_path(ctx.home, ctx.channel());
    if let Some(parent) = unit.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(
        &unit,
        systemd_unit(
            ctx.channel(),
            &ctx.installed_binary(),
            &ctx.paths.logs_dir(),
        ),
    )
    .await?;
    if !ctx
        .run(&["systemctl", "--user", "daemon-reload"])
        .await
        .success()
    {
        return Err(internal(
            "systemctl --user daemon-reload failed; re-run with LOCA_INSTALL_SERVICE=0",
        ));
    }
    let service = format!("{}.service", unit_name(ctx.channel()));
    if !ctx
        .run(&["systemctl", "--user", "enable", "--now", &service])
        .await
        .success()
    {
        return Err(internal(
            "systemctl --user enable failed; re-run with LOCA_INSTALL_SERVICE=0",
        ));
    }
    ctx.io.progress(&format!(
        "enabled systemd user unit {}",
        unit_name(ctx.channel())
    ));
    Ok(())
}

async fn install_scheduled_task(ctx: &ServiceCtx<'_>) -> Result<()> {
    let script = scheduled_task_script(ctx.channel(), &ctx.installed_binary());
    if !ctx.pwsh(&script).await.success() {
        return Err(internal(
            "Register-ScheduledTask failed; Task Scheduler may be disabled. \
             Re-run with LOCA_INSTALL_SERVICE=0",
        ));
    }
    ctx.io.progress(&format!(
        "registered scheduled task {}",
        unit_name(ctx.channel())
    ));
    Ok(())
}

/// List this install in Add/Remove Programs. Non-fatal on failure: a missing
/// Apps entry must not fail the install itself.
async fn register_windows_uninstall_entry(ctx: &ServiceCtx<'_>, as_service: bool) {
    let root = ctx.paths.root();
    let channel_dir = root.parent().unwrap_or(root);
    let command = uninstall_command(
        as_service,
        &ctx.installed_binary(),
        root,
        channel_dir,
        ctx.channel(),
    );
    let key = windows_uninstall_key(ctx.channel());
    let name = windows_display_name(ctx.channel());
    let script = format!(
        "$key = '{key}'\n\
         New-Item -Path $key -Force | Out-Null\n\
         Set-ItemProperty -Path $key -Name DisplayName -Value '{name}'\n\
         Set-ItemProperty -Path $key -Name DisplayVersion -Value '{version}'\n\
         Set-ItemProperty -Path $key -Name Publisher -Value 'Loca'\n\
         Set-ItemProperty -Path $key -Name InstallLocation -Value '{root}'\n\
         Set-ItemProperty -Path $key -Name DisplayIcon -Value '{bin}'\n\
         Set-ItemProperty -Path $key -Name UninstallString -Value '{command}'\n\
         Set-ItemProperty -Path $key -Name QuietUninstallString -Value '{command}'\n\
         Set-ItemProperty -Path $key -Name NoModify -Value 1 -Type DWord\n\
         Set-ItemProperty -Path $key -Name NoRepair -Value 1 -Type DWord\n",
        key = psq(&key),
        name = psq(&name),
        version = psq(ctx.config.version),
        root = psq(&root.display().to_string()),
        bin = psq(&ctx.installed_binary().display().to_string()),
        command = psq(&command),
    );
    let rc = ctx.pwsh(&script).await;
    if !rc.success() {
        ctx.io.progress(&format!(
            "could not register the Add/Remove Programs entry (exit {})",
            rc.code
        ));
        return;
    }
    ctx.io
        .progress(&format!("registered \"{name}\" in Add/Remove Programs"));
}

// --- start / stop / restart ------------------------------------------------

/// Start the installed core through whatever supervisor owns it, and print one
/// JSON line. Idempotent in every branch: a running core is not an error.
pub async fn start_service(ctx: &ServiceCtx<'_>) -> Result<()> {
    let sup = resolve_supervisor(ctx.runner, ctx.os, ctx.home, ctx.channel()).await;
    let mut pid = None;
    match &sup {
        Supervisor::Launchd { label } => {
            let plist = plist_path(ctx.home, ctx.channel())
                .to_string_lossy()
                .into_owned();
            // `load` is non-zero when the agent is already loaded, which is
            // benign. Only `start` failing is an error.
            let _ = ctx.run(&["launchctl", "load", &plist]).await;
            if !ctx.run(&["launchctl", "start", label]).await.success() {
                return Err(internal(format!("launchctl start {label} failed")));
            }
        }
        Supervisor::Systemd { unit } => {
            if !ctx
                .run(&["systemctl", "--user", "start", unit])
                .await
                .success()
            {
                return Err(internal(format!("systemctl --user start {unit} failed")));
            }
        }
        Supervisor::Schtask { task } => {
            if !ctx.run(&["schtasks", "/Run", "/TN", task]).await.success() {
                return Err(internal(format!("schtasks /Run {task} failed")));
            }
        }
        Supervisor::Background => {
            pid = start_background(ctx).await?;
        }
    }
    // Non-null ONLY in background mode: a supervised core outlives the
    // Client, which would otherwise stop the service on every quit.
    ctx.io.emit_json(&start_service_result(sup.kind(), pid));
    Ok(())
}

/// Stop the running core through its supervisor.
///
/// A plain kill would be undone by launchd's `KeepAlive` or systemd's
/// `Restart`, so every supervised branch goes through the supervisor and only
/// background mode kills the process.
pub async fn stop_service(ctx: &ServiceCtx<'_>) -> Result<()> {
    let sup = resolve_supervisor(ctx.runner, ctx.os, ctx.home, ctx.channel()).await;
    match &sup {
        Supervisor::Launchd { label } => {
            let plist = plist_path(ctx.home, ctx.channel())
                .to_string_lossy()
                .into_owned();
            // `unload` also drops KeepAlive, so it stays down until
            // `start_service` loads it again.
            let _ = ctx.run(&["launchctl", "unload", &plist]).await;
            ctx.io.progress(&format!("stopped launchd agent {label}"));
        }
        Supervisor::Systemd { unit } => {
            let _ = ctx.run(&["systemctl", "--user", "stop", unit]).await;
            ctx.io
                .progress(&format!("stopped systemd user unit {unit}"));
        }
        Supervisor::Schtask { task } => {
            // `/End` stops the running instance; the RestartCount policy only
            // fires on failure, so an explicit end stays ended. Sweep survivors.
            let _ = ctx.run(&["schtasks", "/End", "/TN", task]).await;
            kill_stragglers(ctx).await;
            ctx.io.progress(&format!("stopped scheduled task {task}"));
        }
        Supervisor::Background => kill_stragglers(ctx).await,
    }
    Ok(())
}

/// Restart so an on-disk settings change takes effect.
pub async fn restart_service(ctx: &ServiceCtx<'_>) -> Result<()> {
    if !ctx.env.as_service {
        kill_stragglers(ctx).await;
        start_background(ctx).await?;
        return Ok(());
    }
    match ctx.os {
        // unload + load bounces the agent cleanly; a kill would race KeepAlive.
        Os::Macos => install_launchd(ctx).await,
        Os::Linux => {
            if have_systemd_user(ctx).await {
                let service = format!("{}.service", unit_name(ctx.channel()));
                if !ctx
                    .run(&["systemctl", "--user", "restart", &service])
                    .await
                    .success()
                {
                    return Err(internal("systemctl --user restart failed"));
                }
                ctx.io.progress(&format!(
                    "restarted systemd user unit {}",
                    unit_name(ctx.channel())
                ));
                Ok(())
            } else {
                kill_stragglers(ctx).await;
                start_background(ctx).await?;
                Ok(())
            }
        }
        Os::Windows => {
            // Kill first, THEN re-register: unregistering drops the old task's
            // failure-restart policy, so the kill is not auto-relaunched.
            kill_stragglers(ctx).await;
            install_scheduled_task(ctx).await
        }
        Os::Other => Err(internal("unsupported OS for service restart")),
    }
}

// --- uninstall -------------------------------------------------------------

/// Stop and remove the service, sweep stragglers, clear the keychain master key
/// and remove the channel's core dir.
///
/// `purge_models` is INDEPENDENT of `keep_data`: the uninstaller offers the two
/// as separate choices, and the models directory is shared across channels.
pub async fn uninstall_service(
    ctx: &ServiceCtx<'_>,
    keep_data: bool,
    purge_models: bool,
) -> Result<()> {
    match ctx.os {
        Os::Macos => remove_launchd(ctx).await,
        Os::Linux => remove_systemd(ctx).await,
        Os::Windows => {
            remove_scheduled_task(ctx).await;
            unregister_windows_uninstall_entry(ctx).await;
        }
        Os::Other => return Err(internal("unsupported OS for service uninstall")),
    }
    kill_stragglers(ctx).await;
    if keep_data {
        ctx.io
            .progress("keeping core data and keychain master key (--keep-data)");
    } else {
        clear_keychain_master_key(ctx).await;
        remove_core_dir(ctx).await;
    }
    if purge_models {
        remove_models_dir(ctx).await;
    }
    Ok(())
}

async fn remove_launchd(ctx: &ServiceCtx<'_>) {
    let plist = plist_path(ctx.home, ctx.channel());
    if tokio::fs::metadata(&plist).await.is_err() {
        ctx.io.progress("launchd agent not installed");
        return;
    }
    // `unload` is non-zero for benign reasons (already exited); removing the
    // plist is what stops it being auto-loaded at the next login.
    let _ = ctx
        .run(&["launchctl", "unload", &plist.to_string_lossy()])
        .await;
    let _ = tokio::fs::remove_file(&plist).await;
    ctx.io.progress(&format!(
        "removed launchd agent {}",
        service_label(ctx.channel())
    ));
}

async fn remove_systemd(ctx: &ServiceCtx<'_>) {
    let unit = systemd_unit_path(ctx.home, ctx.channel());
    if tokio::fs::metadata(&unit).await.is_err() {
        ctx.io.progress("systemd user unit not installed");
        return;
    }
    let service = format!("{}.service", unit_name(ctx.channel()));
    let _ = ctx
        .run(&["systemctl", "--user", "disable", "--now", &service])
        .await;
    let _ = tokio::fs::remove_file(&unit).await;
    let _ = ctx.run(&["systemctl", "--user", "daemon-reload"]).await;
    ctx.io.progress(&format!(
        "removed systemd user unit {}",
        unit_name(ctx.channel())
    ));
}

async fn remove_scheduled_task(ctx: &ServiceCtx<'_>) {
    let teardown = scheduled_task_teardown_script(ctx.channel());
    let _ = ctx.pwsh(&teardown).await;
    // Unregistering a root-folder task can need admin while every caller runs
    // unelevated, so retry once self-elevated rather than leaving a dead task.
    let task = unit_name(ctx.channel());
    if ctx
        .run(&["schtasks", "/Query", "/TN", &task])
        .await
        .success()
    {
        let elevated = format!(
            "Start-Process powershell.exe -Verb RunAs -Wait -ArgumentList \
             '-NoProfile','-WindowStyle','Hidden','-ExecutionPolicy','Bypass',\
             '-EncodedCommand','{}'",
            encode_pwsh_command(&teardown)
        );
        let _ = ctx.pwsh(&elevated).await;
    }
    ctx.io.progress(&format!("removed scheduled task {task}"));
}

async fn unregister_windows_uninstall_entry(ctx: &ServiceCtx<'_>) {
    let script = format!(
        "Remove-Item -Path '{}' -Recurse -Force -ErrorAction SilentlyContinue",
        psq(&windows_uninstall_key(ctx.channel()))
    );
    let _ = ctx.pwsh(&script).await;
    ctx.io.progress("removed the Add/Remove Programs entry");
}

async fn clear_keychain_master_key(ctx: &ServiceCtx<'_>) {
    if ctx.master_key.delete().await.is_ok() {
        ctx.io.progress("cleared keychain master key");
        return;
    }
    if ctx.os == Os::Macos {
        // A partially removed install can leave no helper able to clear the
        // entry, so fall back to the native tool.
        let service = format!("com.rzkyif.loca.core{}", ctx.channel().suffix());
        let _ = ctx
            .run(&[
                "security",
                "delete-generic-password",
                "-s",
                &service,
                "-a",
                "master-key",
            ])
            .await;
        ctx.io
            .progress("cleared keychain master key (security fallback)");
    }
}

async fn remove_core_dir(ctx: &ServiceCtx<'_>) {
    let root = ctx.paths.root().to_path_buf();
    if tokio::fs::metadata(&root).await.is_err() {
        ctx.io.progress("core directory already gone");
        return;
    }
    match tokio::fs::remove_dir_all(&root).await {
        Ok(()) => {
            // Drop the now-empty ~/.loca/<channel> if the client is gone too.
            // The shared models dir is under ~/.loca, so it survives.
            if let Some(parent) = root.parent() {
                let _ = tokio::fs::remove_dir(parent).await;
            }
            ctx.io.progress(&format!("removed {}", root.display()));
        }
        Err(err) => {
            if ctx.os != Os::Windows {
                ctx.io
                    .progress(&format!("could not remove {}: {err}", root.display()));
                return;
            }
            // On Windows this subcommand IS the running exe, so it cannot
            // delete its own image: leave it for the caller that outlives it.
            remove_except(&root, &ctx.current_exe).await;
            ctx.io
                .progress("removed core data; the running binary is swept after exit");
        }
    }
}

async fn remove_models_dir(ctx: &ServiceCtx<'_>) {
    let models = ctx.paths.models_dir().to_path_buf();
    if tokio::fs::metadata(&models).await.is_err() {
        ctx.io.progress("models directory already gone");
        return;
    }
    let _ = tokio::fs::remove_dir_all(&models).await;
    ctx.io
        .progress(&format!("removed {} (--purge-models)", models.display()));
}

/// Delete everything under `dir` except the single file at `keep`. Best-effort
/// per entry, so one locked file never aborts the rest of the teardown.
async fn remove_except(dir: &Path, keep: &Path) {
    let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
        return;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if path == keep {
            continue;
        }
        if entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false) {
            Box::pin(remove_except(&path, keep)).await;
            let _ = tokio::fs::remove_dir(&path).await;
        } else {
            let _ = tokio::fs::remove_file(&path).await;
        }
    }
}

// --- background mode -------------------------------------------------------

/// Launch core detached with nothing supervising it, returning its pid, which
/// the Client uses to stop exactly this process when it quits. `None` means the
/// pid could not be determined; the core is running either way.
async fn start_background(ctx: &ServiceCtx<'_>) -> Result<Option<u32>> {
    let bin = ctx.installed_binary().display().to_string();
    let logs = ctx.paths.logs_dir();
    let out = logs.join("core.stdout.log").display().to_string();
    let err = logs.join("core.stderr.log").display().to_string();
    let channel = ctx.channel().as_str();

    if ctx.os == Os::Windows {
        // The pid travels through a temp FILE, not stdout: `Start-Process`
        // passes `bInheritHandles`, so the detached core inherits this
        // PowerShell's piped stdio and a read-to-EOF blocks for its lifetime.
        let pid_file = std::env::temp_dir().join(format!("loca-core-pid-{}", uuid::Uuid::new_v4()));
        let pid_str = pid_file.display().to_string();
        let script = format!(
            "$env:LOCA_CHANNEL='{ch}'; \
             $p = Start-Process -FilePath '{bin}' -WindowStyle Hidden -PassThru \
             -RedirectStandardOutput '{out}' -RedirectStandardError '{err}'; \
             $p.Id | Set-Content -LiteralPath '{pid_str}'",
            ch = psq(channel),
            bin = psq(&bin),
            out = psq(&out),
            err = psq(&err),
            pid_str = psq(&pid_str),
        );
        let rc = ctx.pwsh(&script).await;
        if !rc.success() {
            return Err(internal(format!(
                "could not start the core in the background (exit {})",
                rc.code
            )));
        }
        ctx.io
            .progress("started core in the background (no service registered)");
        let raw = tokio::fs::read_to_string(&pid_file)
            .await
            .unwrap_or_default();
        let _ = tokio::fs::remove_file(&pid_file).await;
        return Ok(parse_pid(&raw));
    }

    // Detach via sh so the backgrounded core outlives this short-lived CLI.
    // Capturing sh's stdout is safe here: the core's own stdio is redirected
    // into the log files, so the pipe reaches EOF when sh exits.
    let shell = format!(
        "LOCA_CHANNEL={ch} nohup {bin} >> {out} 2>> {err} & echo $!",
        ch = sh_quote(channel),
        bin = sh_quote(&bin),
        out = sh_quote(&out),
        err = sh_quote(&err),
    );
    let rc = ctx.run(&["sh", "-c", &shell]).await;
    if !rc.success() {
        return Err(internal(format!(
            "could not start the core in the background (exit {})",
            rc.code
        )));
    }
    ctx.io
        .progress("started core in the background (no service registered)");
    Ok(parse_pid(&rc.stdout))
}

/// The pid `start_background` reports, or `None` for anything that is not a
/// POSITIVE integer. `0` matters specifically: a valid `u32` naming no process,
/// which is worse to hand the Client than nothing.
pub fn parse_pid(raw: &str) -> Option<u32> {
    raw.trim()
        .parse::<i64>()
        .ok()
        .filter(|n| *n > 0)?
        .try_into()
        .ok()
}

// --- shared teardown -------------------------------------------------------

/// Kill every core process except this one. `uninstall-service` runs FROM the
/// binary it is sweeping, so without the exclusion the sweep terminates the
/// uninstall mid-flight, silently leaving the data and the key behind.
async fn kill_stragglers(ctx: &ServiceCtx<'_>) {
    let bin_name = ctx.channel().core_binary_name("loca-core");
    if ctx.os == Os::Windows {
        let process_name = bin_name.trim_end_matches(".exe");
        let script = format!(
            "Get-Process -Name '{process_name}' -ErrorAction SilentlyContinue | \
             Where-Object {{ $_.Id -ne {self_pid} }} | \
             Stop-Process -Force -ErrorAction SilentlyContinue",
            self_pid = ctx.self_pid,
        );
        let _ = ctx.pwsh(&script).await;
        return;
    }
    let target = ctx.paths.bin_dir().join(&bin_name).display().to_string();
    let found = ctx.run(&["pgrep", "-f", &target]).await;
    if !found.success() {
        return;
    }
    let self_pid = ctx.self_pid.to_string();
    let pids: Vec<String> = found
        .stdout
        .split_whitespace()
        .map(str::trim)
        .filter(|pid| !pid.is_empty() && *pid != self_pid)
        .map(str::to_string)
        .collect();
    for pid in &pids {
        let _ = ctx.run(&["kill", pid]).await;
    }
    if !pids.is_empty() {
        // A brief grace, then SIGKILL any survivor. One fixed sleep, no poll.
        tokio::time::sleep(std::time::Duration::from_millis(GRACE_MS)).await;
        for pid in &pids {
            let _ = ctx.run(&["kill", "-9", pid]).await;
        }
        ctx.io.progress(&format!(
            "stopped {} straggler core process(es)",
            pids.len()
        ));
    }
}

/// The wait between `kill` and `kill -9`.
#[cfg(not(test))]
const GRACE_MS: u64 = 1000;
/// A test drives the same code path without paying the grace period; the
/// shipping value is asserted separately by `the_shipping_grace_is_one_second`.
#[cfg(test)]
const GRACE_MS: u64 = 1;

async fn have_systemd_user(ctx: &ServiceCtx<'_>) -> bool {
    ctx.run(&["systemctl", "--user", "--version"])
        .await
        .success()
}

/// Single-quote a value for a POSIX shell.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests;
