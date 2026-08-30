//! Which process supervisor owns this core install.
//!
//! The self-update flow consults it so it can restart THROUGH the single owner
//! rather than racing it: launchd's `KeepAlive` relaunches core on any exit, so
//! a second relauncher would fight it over the port.
//!
//! Detected from the definition on disk (the plist, the unit, the task), never
//! from `LOCA_INSTALL_SERVICE`, which is set at install time and absent from a
//! running core, so reading it would misread a background install as a service.
//! No definition means background mode, where the client owns liveness.
//!
//! The OS and the command runner are both injected, so all four branches run on
//! any host. The definitions ([`plist_xml`], [`systemd_unit`],
//! [`scheduled_task_script`], [`uninstall_command`]) are pure functions of a
//! channel and a few paths, which is what makes them assertable here; whether
//! launchd, systemd and Task Scheduler interpret the text as intended is
//! exercised per OS at release time.

use std::path::{Path, PathBuf};

use base64::Engine as _;

use crate::config::Channel;
use crate::update::install::proc::Runner;

/// The supervisor, and what each one needs to be addressed by.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Supervisor {
    Launchd {
        label: String,
    },
    Systemd {
        unit: String,
    },
    Schtask {
        task: String,
    },
    /// No supervisor: the client owns liveness, and the updater helper is the
    /// only thing that can relaunch core.
    Background,
}

impl Supervisor {
    /// The string `start-service` puts on stdout. A wire contract: the Client
    /// keys off `supervisor == "background"` to decide whether it owns the
    /// core's lifetime, so renaming any of the four is a behaviour change.
    pub fn kind(&self) -> &'static str {
        match self {
            Supervisor::Launchd { .. } => "launchd",
            Supervisor::Systemd { .. } => "systemd",
            Supervisor::Schtask { .. } => "schtask",
            Supervisor::Background => "background",
        }
    }
}

/// The operating system, separated from the `cfg!` that names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    Macos,
    Linux,
    Windows,
    Other,
}

impl Os {
    pub fn host() -> Self {
        if cfg!(target_os = "macos") {
            Os::Macos
        } else if cfg!(target_os = "windows") {
            Os::Windows
        } else if cfg!(target_os = "linux") {
            Os::Linux
        } else {
            Os::Other
        }
    }
}

/// `com.rzkyif.loca.core<suffix>`, the launchd label and the keychain service
/// name, which are deliberately the same string.
pub fn service_label(channel: Channel) -> String {
    format!("com.rzkyif.loca.core{}", channel.suffix())
}

/// `loca-core<suffix>`, the systemd unit stem and the scheduled task name.
pub fn unit_name(channel: Channel) -> String {
    channel.bin_name("loca-core")
}

pub fn plist_path(home: &Path, channel: Channel) -> PathBuf {
    home.join("Library")
        .join("LaunchAgents")
        .join(format!("{}.plist", service_label(channel)))
}

pub fn systemd_unit_path(home: &Path, channel: Channel) -> PathBuf {
    home.join(".config")
        .join("systemd")
        .join("user")
        .join(format!("{}.service", unit_name(channel)))
}

/// Detect the supervisor.
pub async fn resolve_supervisor(
    runner: &dyn Runner,
    os: Os,
    home: &Path,
    channel: Channel,
) -> Supervisor {
    match os {
        Os::Macos if plist_path(home, channel).exists() => Supervisor::Launchd {
            label: service_label(channel),
        },
        Os::Linux
            if systemd_unit_path(home, channel).exists() && have_systemd_user(runner).await =>
        {
            Supervisor::Systemd {
                unit: format!("{}.service", unit_name(channel)),
            }
        }
        Os::Windows if scheduled_task_exists(runner, &unit_name(channel)).await => {
            Supervisor::Schtask {
                task: unit_name(channel),
            }
        }
        _ => Supervisor::Background,
    }
}

/// A unit file with no user manager to run it is a background install, the
/// same fallback the systemd installer takes.
async fn have_systemd_user(runner: &dyn Runner) -> bool {
    runner
        .run(&["systemctl", "--user", "--version"])
        .await
        .success()
}

/// `schtasks.exe` rather than the PowerShell module: it ships on every Windows
/// and exits 0 only when the task exists. Misdetecting a real task as absent
/// falls back to background mode, so the most available probe is the right one.
async fn scheduled_task_exists(runner: &dyn Runner, task: &str) -> bool {
    runner
        .run(&["schtasks", "/Query", "/TN", task])
        .await
        .success()
}

// --- the definitions, as text ---------------------------------------------

/// The two log files every definition redirects into.
fn log_files(logs_dir: &Path) -> (PathBuf, PathBuf) {
    (
        logs_dir.join("core.stdout.log"),
        logs_dir.join("core.stderr.log"),
    )
}

/// The launchd LaunchAgent. `KeepAlive` makes launchd the single owner of this
/// core's lifetime, which is why `apply.rs`'s launchd branch only exits:
/// exiting IS the restart, and without the key nothing relaunches.
pub fn plist_xml(channel: Channel, bin: &Path, logs_dir: &Path) -> String {
    let (out, err) = log_files(logs_dir);
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>           <string>{label}</string>
  <key>ProgramArguments</key><array><string>{bin}</string></array>
  <key>EnvironmentVariables</key><dict><key>LOCA_CHANNEL</key><string>{ch}</string></dict>
  <key>RunAtLoad</key>       <true/>
  <key>KeepAlive</key>       <true/>
  <key>StandardOutPath</key> <string>{out}</string>
  <key>StandardErrorPath</key><string>{err}</string>
</dict>
</plist>
"#,
        label = service_label(channel),
        bin = bin.display(),
        ch = channel.as_str(),
        out = out.display(),
        err = err.display(),
    )
}

/// The systemd user unit. `Restart=on-failure` rather than `always` pairs with
/// the update flow: a core that staged an update exits non-zero and is
/// relaunched, and one asked to stop exits zero and stays stopped.
pub fn systemd_unit(channel: Channel, bin: &Path, logs_dir: &Path) -> String {
    let (out, err) = log_files(logs_dir);
    format!(
        "[Unit]
Description={unit}
Wants=network-online.target
After=network-online.target

[Service]
Environment=LOCA_CHANNEL={ch}
ExecStart={bin}
Restart=on-failure
RestartSec=5
StandardOutput=append:{out}
StandardError=append:{err}

[Install]
WantedBy=default.target
",
        unit = unit_name(channel),
        ch = channel.as_str(),
        bin = bin.display(),
        out = out.display(),
        err = err.display(),
    )
}

/// The PowerShell that registers the Windows scheduled task. Two load-bearing
/// details: a task has no environment field, so every non-stable channel
/// launches through `cmd.exe /c set LOCA_CHANNEL=...` or it would resolve the
/// STABLE subtree; and `RestartCount`/`RestartInterval` are the self-update
/// backstop, deduped against the updater's own `Start-ScheduledTask` by
/// `MultipleInstances IgnoreNew`.
pub fn scheduled_task_script(channel: Channel, bin: &Path) -> String {
    let task = unit_name(channel);
    let bin = bin.display();
    let ch = channel.as_str();
    let action = if channel == Channel::Stable {
        format!("$a = New-ScheduledTaskAction -Execute '{bin}'")
    } else {
        format!(
            "$a = New-ScheduledTaskAction -Execute 'cmd.exe' -Argument '/c set LOCA_CHANNEL={ch}&& \"{bin}\"'"
        )
    };
    format!(
        "
{action}
$t = New-ScheduledTaskTrigger -AtLogOn
# RestartCount/RestartInterval double as the self-update backstop: core exits
# non-zero after staging an update, and this restart-on-failure relaunches the
# already-swapped binary if the updater's fast-path start never lands.
# MultipleInstances=IgnoreNew (the default, pinned explicitly) dedups that
# backstop against the updater's own Start-ScheduledTask so only one core runs.
$s = New-ScheduledTaskSettingsSet -StartWhenAvailable -DontStopOnIdleEnd -RestartCount 5 -RestartInterval (New-TimeSpan -Minutes 1) -MultipleInstances IgnoreNew -AllowStartIfOnBatteries
$p = New-ScheduledTaskPrincipal -UserId $env:USERNAME -LogonType Interactive -RunLevel Limited
Unregister-ScheduledTask -TaskName '{task}' -Confirm:$false -ErrorAction SilentlyContinue
Register-ScheduledTask -TaskName '{task}' -Action $a -Trigger $t -Settings $s -Principal $p -Description 'loca-core' | Out-Null
Start-ScheduledTask -TaskName '{task}'
"
    )
}

/// The PowerShell that removes the scheduled task. Issued once plainly, then
/// wrapped in a self-elevation when the task survived, since unregistering a
/// root-folder task can need admin.
pub fn scheduled_task_teardown_script(channel: Channel) -> String {
    let task = unit_name(channel);
    format!(
        "Stop-ScheduledTask -TaskName '{task}' -ErrorAction SilentlyContinue; \
Unregister-ScheduledTask -TaskName '{task}' -Confirm:$false -ErrorAction SilentlyContinue"
    )
}

/// The per-user Add/Remove Programs key.
pub fn windows_uninstall_key(channel: Channel) -> String {
    format!(
        "HKCU:\\Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\{}",
        channel.bin_name("loca-core")
    )
}

/// The name a person sees in Settings > Apps, parenthesised per channel to
/// match the Client's "Loca (latest)" convention.
pub fn windows_display_name(channel: Channel) -> String {
    match channel {
        Channel::Stable => "Loca Core".to_string(),
        other => format!("Loca Core ({})", other.as_str()),
    }
}

/// The Add/Remove Programs `UninstallString`. Three things it must carry, each
/// of which fails silently if lost: the `LOCA_CHANNEL` prefix, since an ARP
/// launch carries no environment; self-elevation **only** for a service
/// install, whose root-folder task needs admin to unregister while Settings >
/// Apps runs unelevated (a background install has no task, so it never
/// prompts); and the empty-channel-dir sweep.
pub fn uninstall_command(
    as_service: bool,
    bin: &Path,
    root: &Path,
    channel_dir: &Path,
    channel: Channel,
) -> String {
    let teardown = format!(
        "$env:LOCA_CHANNEL='{ch}'; \
& '{bin}' uninstall-service; \
Remove-Item -LiteralPath '{root}' -Recurse -Force -ErrorAction SilentlyContinue; \
$cd='{cd}'; \
if ((Test-Path -LiteralPath $cd) -and -not (Get-ChildItem -Force -LiteralPath $cd)) \
{{ Remove-Item -LiteralPath $cd -Force -ErrorAction SilentlyContinue }}",
        ch = psq(channel.as_str()),
        bin = psq(&bin.display().to_string()),
        root = psq(&root.display().to_string()),
        cd = psq(&channel_dir.display().to_string()),
    );
    let prefix = "powershell.exe -NoProfile -WindowStyle Hidden -ExecutionPolicy Bypass";
    if !as_service {
        return format!("{prefix} -Command \"{teardown}\"");
    }
    format!(
        "{prefix} -Command \"Start-Process powershell.exe -Verb RunAs -Wait -ArgumentList \
'-NoProfile','-WindowStyle','Hidden','-ExecutionPolicy','Bypass',\
'-EncodedCommand','{}'\"",
        encode_pwsh_command(&teardown)
    )
}

/// Base64(UTF-16LE) for PowerShell's `-EncodedCommand`. Handing a script across
/// the `Start-Process -Verb RunAs` relaunch as one opaque token sidesteps
/// nested quoting entirely.
pub fn encode_pwsh_command(cmd: &str) -> String {
    let mut bytes = Vec::with_capacity(cmd.len() * 2);
    for unit in cmd.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Escape a value for interpolation INSIDE a single-quoted PowerShell literal;
/// doubling is PowerShell's single-quote escape. Callers write `'{psq(v)}'`.
pub fn psq(s: &str) -> String {
    s.replace('\'', "''")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::update::install::proc::testing::RecordingRunner;
    use crate::update::install::proc::RunResult;

    fn home() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[tokio::test]
    async fn a_launchd_plist_on_disk_is_a_launchd_install() {
        let h = home();
        let plist = plist_path(h.path(), Channel::Stable);
        std::fs::create_dir_all(plist.parent().unwrap()).unwrap();
        std::fs::write(&plist, "<plist/>").unwrap();
        let runner = RecordingRunner::new();

        let sup = resolve_supervisor(&runner, Os::Macos, h.path(), Channel::Stable).await;

        assert_eq!(
            sup,
            Supervisor::Launchd {
                label: "com.rzkyif.loca.core".into()
            }
        );
        assert!(
            runner.calls().is_empty(),
            "detecting launchd runs no command; a probe here would be a spawn on every apply"
        );
    }

    #[tokio::test]
    async fn no_plist_is_background_rather_than_launchd() {
        let h = home();
        let sup = resolve_supervisor(
            &RecordingRunner::new(),
            Os::Macos,
            h.path(),
            Channel::Stable,
        )
        .await;
        assert_eq!(sup, Supervisor::Background);
    }

    #[tokio::test]
    async fn a_systemd_unit_needs_a_user_manager_to_count() {
        // Both halves: a unit file with no `systemd --user` is the background
        // fallback, and asserting only the file would classify it wrong.
        let h = home();
        let unit = systemd_unit_path(h.path(), Channel::Stable);
        std::fs::create_dir_all(unit.parent().unwrap()).unwrap();
        std::fs::write(&unit, "[Unit]").unwrap();

        let with_manager =
            RecordingRunner::new().on("systemctl --user --version", RunResult::ok(""));
        assert_eq!(
            resolve_supervisor(&with_manager, Os::Linux, h.path(), Channel::Stable).await,
            Supervisor::Systemd {
                unit: "loca-core.service".into()
            }
        );

        let without = RecordingRunner::new().on("systemctl", RunResult::failed(127));
        assert_eq!(
            resolve_supervisor(&without, Os::Linux, h.path(), Channel::Stable).await,
            Supervisor::Background
        );
    }

    #[tokio::test]
    async fn a_registered_scheduled_task_is_a_schtask_install() {
        let h = home();
        let present = RecordingRunner::new().on("schtasks /Query", RunResult::ok(""));
        assert_eq!(
            resolve_supervisor(&present, Os::Windows, h.path(), Channel::Latest).await,
            Supervisor::Schtask {
                task: "loca-core-latest".into()
            }
        );
        assert!(present.calls()[0].contains("loca-core-latest"));

        let absent = RecordingRunner::new().on("schtasks /Query", RunResult::failed(1));
        assert_eq!(
            resolve_supervisor(&absent, Os::Windows, h.path(), Channel::Stable).await,
            Supervisor::Background
        );
    }

    #[tokio::test]
    async fn every_os_branch_answers_on_this_host() {
        // The point of injecting the OS: `cfg!(target_os = ...)` has one
        // answer here, leaving three branches no test could enter.
        let h = home();
        let runner = RecordingRunner::new();
        for os in [Os::Macos, Os::Linux, Os::Windows, Os::Other] {
            assert_eq!(
                resolve_supervisor(&runner, os, h.path(), Channel::Stable).await,
                Supervisor::Background,
                "{os:?} with nothing installed"
            );
        }
    }

    #[test]
    fn the_supervisor_kinds_are_the_strings_the_client_parses() {
        // The Client compares this against the literal "background" to decide
        // whether it owns the core's lifetime, so all four are frozen.
        assert_eq!(Supervisor::Launchd { label: "x".into() }.kind(), "launchd");
        assert_eq!(Supervisor::Systemd { unit: "x".into() }.kind(), "systemd");
        assert_eq!(Supervisor::Schtask { task: "x".into() }.kind(), "schtask");
        assert_eq!(Supervisor::Background.kind(), "background");
    }

    // --- the definitions ---------------------------------------------------

    #[test]
    fn the_launchd_agent_keeps_the_core_alive_and_starts_it_at_login() {
        // `RunAtLoad` is the boot start; `KeepAlive` makes launchd the single
        // owner, which is why apply.rs's launchd branch does nothing but exit.
        let xml = plist_xml(
            Channel::Stable,
            Path::new("/u/.loca/stable/core/bin/loca-core"),
            Path::new("/u/.loca/stable/core/logs"),
        );
        assert!(xml.contains("<key>RunAtLoad</key>       <true/>"));
        assert!(xml.contains("<key>KeepAlive</key>       <true/>"));
        assert!(xml.contains("<string>com.rzkyif.loca.core</string>"));
        assert!(xml.contains("<array><string>/u/.loca/stable/core/bin/loca-core</string></array>"));
        // The KEY is part of each assertion: without it, swapping the two log
        // filenames leaves both strings present and the test green.
        assert!(xml.contains(
            "<key>StandardOutPath</key> <string>/u/.loca/stable/core/logs/core.stdout.log</string>"
        ));
        assert!(xml.contains(
            "<key>StandardErrorPath</key><string>/u/.loca/stable/core/logs/core.stderr.log</string>"
        ));
        assert!(xml.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n"));
        assert!(xml.ends_with("</plist>\n"));
    }

    #[test]
    fn every_channel_bakes_its_own_channel_into_its_definitions() {
        // The daemon resolves ~/.loca/<channel>/core from LOCA_CHANNEL, so a
        // definition that omits it starts a core against the WRONG subtree.
        for ch in [Channel::Stable, Channel::Dev, Channel::Latest] {
            let bin = Path::new("/b/loca-core");
            let logs = Path::new("/l");
            assert!(
                plist_xml(ch, bin, logs).contains(&format!(
                    "<dict><key>LOCA_CHANNEL</key><string>{}</string></dict>",
                    ch.as_str()
                )),
                "plist, {ch:?}"
            );
            assert!(
                systemd_unit(ch, bin, logs)
                    .contains(&format!("Environment=LOCA_CHANNEL={}", ch.as_str())),
                "unit, {ch:?}"
            );
            assert!(
                plist_xml(ch, bin, logs).contains(&format!(
                    "<key>Label</key>           <string>{}</string>",
                    service_label(ch)
                )),
                "label, {ch:?}"
            );
            assert!(
                systemd_unit(ch, bin, logs).contains(&format!("Description={}", unit_name(ch))),
                "description, {ch:?}"
            );
        }
    }

    #[test]
    fn the_systemd_unit_restarts_on_failure_rather_than_always() {
        // A core that staged an update exits non-zero and is relaunched; one
        // asked to stop exits zero and STAYS stopped. `always` would make
        // `systemctl --user stop` a race against the restart.
        let unit = systemd_unit(Channel::Dev, Path::new("/b/loca-core-dev"), Path::new("/l"));
        assert!(unit.contains("\nRestart=on-failure\n"));
        assert!(!unit.contains("Restart=always"));
        assert!(unit.contains("\nRestartSec=5\n"));
        assert!(unit.contains("\nExecStart=/b/loca-core-dev\n"));
        assert!(unit.contains("StandardOutput=append:/l/core.stdout.log"));
        assert!(unit.contains("StandardError=append:/l/core.stderr.log"));
        assert!(unit.contains("\nWantedBy=default.target\n"));
        assert!(unit.contains("After=network-online.target"));
    }

    #[test]
    fn a_non_stable_scheduled_task_carries_its_channel() {
        // A scheduled task has no environment field, so every non-stable
        // channel launches through cmd.exe to set LOCA_CHANNEL before exec.
        // Stable needs no wrapper and does not get one.
        let bin = Path::new(r"C:\Users\a\.loca\dev\core\bin\loca-core-dev.exe");
        let dev = scheduled_task_script(Channel::Dev, bin);
        assert!(dev.contains(
            r#"New-ScheduledTaskAction -Execute 'cmd.exe' -Argument '/c set LOCA_CHANNEL=dev&& "C:\Users\a\.loca\dev\core\bin\loca-core-dev.exe"'"#
        ));

        let stable = scheduled_task_script(Channel::Stable, Path::new(r"C:\b\loca-core.exe"));
        assert!(stable.contains(r"New-ScheduledTaskAction -Execute 'C:\b\loca-core.exe'"));
        assert!(
            !stable.contains("cmd.exe"),
            "stable needs no wrapper, and adding one would set an empty channel"
        );
    }

    #[test]
    fn the_task_dedupes_the_update_backstop_against_the_updater() {
        // RestartCount/RestartInterval ARE the self-update backstop on
        // Windows; IgnoreNew stops that backstop and the updater's own
        // Start-ScheduledTask from producing two cores.
        let script = scheduled_task_script(Channel::Latest, Path::new(r"C:\b\c.exe"));
        assert!(script.contains("-RestartCount 5"));
        assert!(script.contains("-RestartInterval (New-TimeSpan -Minutes 1)"));
        assert!(script.contains("-MultipleInstances IgnoreNew"));
        assert!(script.contains("-StartWhenAvailable"));
        assert!(script.contains("-AllowStartIfOnBatteries"));
        assert!(script.contains("New-ScheduledTaskTrigger -AtLogOn"));
        assert!(script.contains("-LogonType Interactive -RunLevel Limited"));
    }

    #[test]
    fn the_task_is_unregistered_before_it_is_registered_and_started() {
        // Order, not presence: registering over a live task fails, and
        // starting first starts the OLD definition. Anchored to a line start,
        // because the script's own comment mentions `Start-ScheduledTask` and
        // "Register-" is a substring of "Unregister-".
        let script = scheduled_task_script(Channel::Stable, Path::new(r"C:\b\c.exe"));
        let unregister = script.find("\nUnregister-ScheduledTask").unwrap();
        let register = script.find("\nRegister-ScheduledTask -TaskName").unwrap();
        let start = script.find("\nStart-ScheduledTask -TaskName").unwrap();
        assert!(unregister < register, "unregister first");
        assert!(register < start, "register before starting");
        assert!(script.contains("-TaskName 'loca-core'"));
    }

    #[test]
    fn the_teardown_stops_the_task_before_it_unregisters_it() {
        let teardown = scheduled_task_teardown_script(Channel::Latest);
        let stop = teardown.find("Stop-ScheduledTask").unwrap();
        let unregister = teardown.find("Unregister-ScheduledTask").unwrap();
        assert!(stop < unregister);
        assert!(teardown.contains("-TaskName 'loca-core-latest'"));
        assert!(
            teardown.contains("-Confirm:$false"),
            "an unattended uninstall cannot answer a prompt"
        );
    }

    #[test]
    fn only_a_service_uninstall_self_elevates() {
        // A service's root-folder task needs admin to unregister, but Settings
        // > Apps runs this string UNELEVATED. A background install has no task,
        // so prompting there would be a UAC dialog for nothing.
        let bin = Path::new(r"C:\b\loca-core.exe");
        let root = Path::new(r"C:\u\.loca\stable\core");
        let cd = Path::new(r"C:\u\.loca\stable");

        let service = uninstall_command(true, bin, root, cd, Channel::Stable);
        assert!(service.contains("Start-Process powershell.exe -Verb RunAs -Wait"));
        assert!(service.contains("-EncodedCommand"));

        let background = uninstall_command(false, bin, root, cd, Channel::Stable);
        assert!(!background.contains("RunAs"), "no prompt with no task");
        assert!(!background.contains("-EncodedCommand"));
        assert!(background.contains("uninstall-service"));
    }

    #[test]
    fn the_uninstall_string_carries_its_channel_and_sweeps_an_empty_channel_dir() {
        // The background form is the one where the teardown is readable; the
        // service form carries the same text base64'd, decoded by the next test.
        let cmd = uninstall_command(
            false,
            Path::new("/b/loca-core-dev"),
            Path::new("/u/.loca/dev/core"),
            Path::new("/u/.loca/dev"),
            Channel::Dev,
        );
        // An ARP launch carries no environment, so without this the binary
        // uninstalls the STABLE channel.
        assert!(cmd.contains("$env:LOCA_CHANNEL='dev';"));
        assert!(cmd.contains("& '/b/loca-core-dev' uninstall-service;"));
        assert!(cmd.contains("Remove-Item -LiteralPath '/u/.loca/dev/core' -Recurse -Force"));
        assert!(cmd.contains("$cd='/u/.loca/dev';"));
        assert!(
            cmd.contains("-not (Get-ChildItem -Force -LiteralPath $cd)"),
            "the channel dir is removed only when EMPTY: the client may still live there"
        );
        assert!(cmd.starts_with(
            "powershell.exe -NoProfile -WindowStyle Hidden -ExecutionPolicy Bypass -Command"
        ));
    }

    #[test]
    fn the_elevated_form_carries_the_same_teardown_it_would_have_run_directly() {
        // Decoded rather than assumed: the elevated path is the default and
        // its payload is unreadable, so a truncating encoder would look fine.
        let bin = Path::new("/b/loca-core");
        let root = Path::new("/u/.loca/stable/core");
        let cd = Path::new("/u/.loca/stable");
        let service = uninstall_command(true, bin, root, cd, Channel::Stable);
        let background = uninstall_command(false, bin, root, cd, Channel::Stable);

        let start = service.find("'-EncodedCommand','").unwrap() + "'-EncodedCommand','".len();
        let encoded = &service[start..service[start..].find('\'').unwrap() + start];
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap();
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        let decoded = String::from_utf16(&units).unwrap();

        assert!(decoded.contains("uninstall-service"));
        assert!(decoded.contains("$env:LOCA_CHANNEL='stable';"));
        assert!(
            background.contains(&decoded),
            "the two forms must run the same teardown, elevated or not"
        );
    }

    #[test]
    fn a_value_with_a_quote_cannot_break_out_of_a_powershell_literal() {
        // An apostrophe in a home directory would close the literal and make
        // the rest of the path PowerShell run by a SELF-ELEVATED shell.
        assert_eq!(psq("/Users/o'brien/x"), "/Users/o''brien/x");
        assert_eq!(psq("a'b'c"), "a''b''c");
        assert_eq!(psq("no quotes"), "no quotes");

        let cmd = uninstall_command(
            false,
            Path::new("/Users/o'brien/bin/loca-core"),
            Path::new("/Users/o'brien/.loca/stable/core"),
            Path::new("/Users/o'brien/.loca/stable"),
            Channel::Stable,
        );
        assert!(cmd.contains("& '/Users/o''brien/bin/loca-core' uninstall-service;"));
    }

    #[test]
    fn the_encoded_command_is_base64_of_utf16le() {
        // Exact vectors: a round trip through this file's own decoder passes
        // for any self-consistent encoding, and PowerShell accepts one.
        assert_eq!(encode_pwsh_command("hi"), "aABpAA==");
        assert_eq!(
            encode_pwsh_command("Stop-ScheduledTask"),
            "UwB0AG8AcAAtAFMAYwBoAGUAZAB1AGwAZQBkAFQAYQBzAGsA"
        );
        // Non-ASCII: UTF-8 would encode this as two bytes, not one code unit.
        assert_eq!(encode_pwsh_command("\u{e9}"), "6QA=");
        // Outside the BMP, so a surrogate PAIR rather than a scalar value.
        assert_eq!(encode_pwsh_command("\u{1f600}"), "PdgA3g==");
        assert_eq!(encode_pwsh_command(""), "");
    }

    #[test]
    fn the_add_remove_entry_is_named_and_keyed_per_channel() {
        // Two channels install side by side, so one entry per channel or
        // installing dev overwrites stable's uninstaller.
        assert_eq!(
            windows_uninstall_key(Channel::Stable),
            r"HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\loca-core"
        );
        assert_eq!(
            windows_uninstall_key(Channel::Dev),
            r"HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\loca-core-dev"
        );
        assert_eq!(windows_display_name(Channel::Stable), "Loca Core");
        assert_eq!(windows_display_name(Channel::Dev), "Loca Core (dev)");
        assert_eq!(windows_display_name(Channel::Latest), "Loca Core (latest)");
    }

    #[test]
    fn the_names_are_channel_namespaced() {
        // Two channels coexist, so a dev core must not detect stable's agent.
        // The label doubles as the keychain service name, a byte-exact parity
        // surface, so it is asserted literally.
        assert_eq!(service_label(Channel::Stable), "com.rzkyif.loca.core");
        assert_eq!(service_label(Channel::Dev), "com.rzkyif.loca.core-dev");
        assert_eq!(unit_name(Channel::Latest), "loca-core-latest");
        assert!(plist_path(Path::new("/h"), Channel::Dev)
            .to_string_lossy()
            .ends_with("Library/LaunchAgents/com.rzkyif.loca.core-dev.plist"));
        assert!(systemd_unit_path(Path::new("/h"), Channel::Dev)
            .to_string_lossy()
            .ends_with(".config/systemd/user/loca-core-dev.service"));
    }
}
