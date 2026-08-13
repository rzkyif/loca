// OS background-service registration + teardown for tomat-core.
//
// This is the single implementation of service registration: it writes
// the launchd LaunchAgent (macOS), the systemd user unit (Linux), or the
// Task Scheduler task (Windows) that starts core at login and restarts it on
// failure, all namespaced per channel so stable / latest / dev coexist. The
// TOMAT_CHANNEL is baked into the service environment so the daemon resolves the
// same ~/.tomat/<channel>/core subtree. TOMAT_INSTALL_SERVICE=0 skips the
// service and just launches core in the background (client owns liveness).
//
// uninstall-service reverses it: stop + remove the service, kill stragglers,
// delete the keychain master key, and remove the channel's core dir -
// preserving the shared ~/.tomat/models weights unless --purge-models opts
// into removing them too.

import { dirname, join } from "@std/path";
import { channel, channelKeychainSuffix, channelSuffix, coreRoot, paths } from "../paths.ts";
import { binPath } from "../paths.ts";
import { CORE_VERSION } from "../config.ts";
import { coreBinaryName } from "../binaries/versions.ts";
import { keychainDelete } from "../services/keychain.ts";
import { emitJson, progress } from "./io.ts";
import { realHome, run, runPwsh } from "./proc.ts";

function installedBinary(): string {
  return binPath(coreBinaryName("tomat-core"));
}

function serviceLabel(): string {
  return `au.tomat.core${channelSuffix()}`;
}

function systemdUnitName(): string {
  return `tomat-core${channelSuffix()}`;
}

/** The process supervisor that owns this core install. The self-update flow
 *  consults this so it can restart core THROUGH the supervisor (the single
 *  owner) rather than racing it: on macOS launchd's `KeepAlive` relaunches core
 *  on any exit, so a second relauncher (the old updater's detached spawn) fought
 *  it over the port. Mirrors installService's os + TOMAT_INSTALL_SERVICE split,
 *  including the Linux "no systemd -> background" fallback. */
export type Supervisor =
  | { kind: "launchd"; label: string }
  | { kind: "systemd"; unit: string }
  | { kind: "schtask"; task: string }
  | { kind: "background" };

/** Detect the supervisor by the definition installService actually wrote (the
 *  launchd plist / systemd unit / scheduled task), NOT by TOMAT_INSTALL_SERVICE:
 *  that flag is set at install time and is absent from a running core's
 *  environment, so a background install would otherwise be misread as a service.
 *  No matching definition -> background mode (the client owns liveness). */
export async function resolveSupervisor(): Promise<Supervisor> {
  switch (Deno.build.os) {
    case "darwin":
      return (await fileExists(plistPath()))
        ? { kind: "launchd", label: serviceLabel() }
        : { kind: "background" };
    case "linux":
      return (await fileExists(systemdUnitPath())) && (await haveSystemdUser())
        ? { kind: "systemd", unit: `${systemdUnitName()}.service` }
        : { kind: "background" };
    case "windows":
      return (await scheduledTaskExists(systemdUnitName()))
        ? { kind: "schtask", task: systemdUnitName() }
        : { kind: "background" };
    default:
      return { kind: "background" };
  }
}

async function scheduledTaskExists(task: string): Promise<boolean> {
  // schtasks.exe ships on every Windows and exits 0 iff the task exists, so it
  // needs neither the ScheduledTasks PowerShell module nor a PowerShell spawn
  // (and it's the same tool the updater uses to `/Run` the task). Misdetecting a
  // real task as absent would fall back to background mode, so prefer the most
  // universally available probe.
  return (await run(["schtasks", "/Query", "/TN", task], { ignoreError: true, capture: false }))
    .success;
}

/** Register (and start) the OS service, or launch in the background when
 *  TOMAT_INSTALL_SERVICE=0. Assumes the core binary is already on disk (placed
 *  by the seed step / native installer / self-install). */
export async function installService(): Promise<void> {
  const asService = (Deno.env.get("TOMAT_INSTALL_SERVICE") ?? "1") !== "0";
  await Deno.mkdir(paths().logsDir, { recursive: true });

  if (!asService) {
    await startBackground();
  } else {
    switch (Deno.build.os) {
      case "darwin":
        await installLaunchd();
        break;
      case "linux":
        await installSystemd();
        break;
      case "windows":
        await installScheduledTask();
        break;
      default:
        throw new Error(`unsupported OS for service install: ${Deno.build.os}`);
    }
  }
  // Every Windows install front-end funnels through here, so this is the one
  // place that lists the Core in Add/Remove Programs (Settings > Apps). Pass
  // whether a Scheduled Task was registered: its removal needs admin, so the
  // uninstall entry must self-elevate (see buildUninstallCommand).
  if (Deno.build.os === "windows" && shouldRegisterArpEntry()) {
    await registerWindowsUninstallEntry(asService);
  }
}

/** Whether install-service writes its own Add/Remove Programs entry.
 *  TOMAT_INSTALL_REGISTER_ARP=0 (exactly "0") skips it: the unified Windows
 *  installer writes its own ARP entry pointing at its uninstall.exe, and the
 *  skip keeps every later install-service re-run (upgrades included) from
 *  clobbering that entry back to the PowerShell teardown. Exported for tests. */
export function shouldRegisterArpEntry(
  value: string | undefined = Deno.env.get("TOMAT_INSTALL_REGISTER_ARP"),
): boolean {
  return value !== "0";
}

/** Restart the running daemon so an on-disk settings change (server.behindProxy)
 *  takes effect. Mirrors installService's service-vs-background split: a service
 *  install re-bounces the OS service; a background install stops the running
 *  core and relaunches it. Called by the enable-behind-proxy verb (cli.ts) after
 *  the local loopback pair, so TOMAT_INSTALL_SERVICE selects the same path the
 *  install used. */
export async function restartService(): Promise<void> {
  const asService = (Deno.env.get("TOMAT_INSTALL_SERVICE") ?? "1") !== "0";
  if (!asService) {
    await killStragglers();
    await startBackground();
    return;
  }
  switch (Deno.build.os) {
    case "darwin":
      // launchctl unload + load bounces the KeepAlive agent cleanly (no manual
      // kill, which would race the KeepAlive restart).
      await installLaunchd();
      break;
    case "linux":
      if (await haveSystemdUser()) {
        const rc = await run(["systemctl", "--user", "restart", `${systemdUnitName()}.service`]);
        if (!rc.success) throw new Error("systemctl --user restart failed");
        progress(`restarted systemd user unit ${systemdUnitName()}`);
      } else {
        // No systemd: the install fell back to a background launch, so restart
        // the same way.
        await killStragglers();
        await startBackground();
      }
      break;
    case "windows":
      // Kill the running instance first, then Unregister+Register+Start (via
      // installScheduledTask) a fresh one. Unregister drops the old task's
      // failure-restart policy, so the kill can't be auto-relaunched into a
      // second core.
      await killStragglers();
      await installScheduledTask();
      break;
    default:
      throw new Error(`unsupported OS for service restart: ${Deno.build.os}`);
  }
}

/** Start the installed core through whatever supervisor owns it, and print one
 *  JSON line describing the result: `{"supervisor":"<kind>","pid":<n>|null}`.
 *  The pid is non-null only in background mode, where nothing supervises the
 *  process: the Client records it so quitting the Client also stops the core it
 *  started. A supervised core outlives the Client, so it reports `pid: null`.
 *  Idempotent: every branch tolerates an already-running core. */
export async function startService(): Promise<void> {
  const sup = await resolveSupervisor();
  let pid: number | null = null;
  switch (sup.kind) {
    case "launchd": {
      // `load` is non-zero when the agent is already loaded, which is benign.
      // RunAtLoad starts it, and `start` covers the already-loaded case; both
      // are no-ops against a running job. Stays on the legacy launchctl verbs
      // that installLaunchd / removeLaunchd already use, so no uid lookup.
      await run(["launchctl", "load", plistPath()], { ignoreError: true });
      const rc = await run(["launchctl", "start", sup.label]);
      if (!rc.success) throw new Error(`launchctl start ${sup.label} failed`);
      break;
    }
    case "systemd": {
      const rc = await run(["systemctl", "--user", "start", sup.unit]);
      if (!rc.success) throw new Error(`systemctl --user start ${sup.unit} failed`);
      break;
    }
    case "schtask": {
      const rc = await run(["schtasks", "/Run", "/TN", sup.task], { ignoreError: true });
      if (!rc.success) throw new Error(`schtasks /Run ${sup.task} failed`);
      break;
    }
    case "background":
      pid = await startBackground();
      break;
  }
  emitJson({ supervisor: sup.kind, pid });
}

/** Stop the running core through whatever supervisor owns it. Idempotent: a
 *  core that is already down is not an error. A plain kill would be undone by
 *  launchd's KeepAlive / systemd's Restart, so each branch goes through the
 *  supervisor; only background mode kills the process directly. */
export async function stopService(): Promise<void> {
  const sup = await resolveSupervisor();
  switch (sup.kind) {
    case "launchd":
      // `unload` stops the job AND drops its KeepAlive, so the agent stays down
      // until startService loads it again. Non-zero when already unloaded.
      await run(["launchctl", "unload", plistPath()], { ignoreError: true });
      progress(`stopped launchd agent ${sup.label}`);
      break;
    case "systemd":
      await run(["systemctl", "--user", "stop", sup.unit], { ignoreError: true });
      progress(`stopped systemd user unit ${sup.unit}`);
      break;
    case "schtask":
      // /End stops the task's running instance; the RestartCount policy only
      // fires on failure, so an explicit end stays ended. Sweep any survivor.
      await run(["schtasks", "/End", "/TN", sup.task], { ignoreError: true });
      await killStragglers();
      progress(`stopped scheduled task ${sup.task}`);
      break;
    case "background":
      await killStragglers();
      break;
  }
}

/** Stop + remove the service, kill stragglers, clear the keychain master key,
 *  and remove the channel's core dir (unless keepData). purgeModels also
 *  removes the shared ~/.tomat/models weights, independent of keepData: the
 *  uninstaller offers "keep my data" and "remove downloaded models" as separate
 *  choices, and the models dir is shared across channels either way. */
export async function uninstallService(opts: {
  keepData: boolean;
  purgeModels?: boolean;
}): Promise<void> {
  switch (Deno.build.os) {
    case "darwin":
      await removeLaunchd();
      break;
    case "linux":
      await removeSystemd();
      break;
    case "windows":
      await removeScheduledTask();
      await unregisterWindowsUninstallEntry();
      break;
    default:
      throw new Error(`unsupported OS for service uninstall: ${Deno.build.os}`);
  }
  await killStragglers();
  if (!opts.keepData) {
    await clearKeychainMasterKey();
    await removeCoreDir();
  } else {
    progress("keeping core data and keychain master key (--keep-data)");
  }
  if (opts.purgeModels) await removeModelsDir();
}

// --- macOS launchd --------------------------------------------------------

function plistPath(): string {
  return join(realHome(), "Library", "LaunchAgents", `${serviceLabel()}.plist`);
}

async function installLaunchd(): Promise<void> {
  const bin = installedBinary();
  const logs = paths().logsDir;
  const plist = plistPath();
  await Deno.mkdir(dirname(plist), { recursive: true });
  const xml = `<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>           <string>${serviceLabel()}</string>
  <key>ProgramArguments</key><array><string>${bin}</string></array>
  <key>EnvironmentVariables</key><dict><key>TOMAT_CHANNEL</key><string>${channel()}</string></dict>
  <key>RunAtLoad</key>       <true/>
  <key>KeepAlive</key>       <true/>
  <key>StandardOutPath</key> <string>${join(logs, "core.stdout.log")}</string>
  <key>StandardErrorPath</key><string>${join(logs, "core.stderr.log")}</string>
</dict>
</plist>
`;
  await Deno.writeTextFile(plist, xml);
  await run(["launchctl", "unload", plist], { ignoreError: true });
  const rc = await run(["launchctl", "load", plist]);
  if (!rc.success) {
    throw new Error(`launchctl load failed (exit ${rc.code}); inspect ${plist}`);
  }
  progress(`registered launchd agent ${serviceLabel()}`);
}

async function removeLaunchd(): Promise<void> {
  const plist = plistPath();
  if (!(await fileExists(plist))) {
    progress("launchd agent not installed");
    return;
  }
  // `launchctl unload` is non-zero for benign reasons (already exited); the
  // load-bearing step is removing the plist so it isn't auto-loaded next login.
  await run(["launchctl", "unload", plist], { ignoreError: true });
  await Deno.remove(plist).catch(() => {});
  progress(`removed launchd agent ${serviceLabel()}`);
}

// --- Linux systemd (user) -------------------------------------------------

function systemdUnitPath(): string {
  return join(realHome(), ".config", "systemd", "user", `${systemdUnitName()}.service`);
}

async function haveSystemdUser(): Promise<boolean> {
  return (await run(["systemctl", "--user", "--version"], { ignoreError: true })).success;
}

async function installSystemd(): Promise<void> {
  if (!(await haveSystemdUser())) {
    progress("systemd --user unavailable; starting core in the background instead");
    await startBackground();
    return;
  }
  const bin = installedBinary();
  const logs = paths().logsDir;
  const unit = systemdUnitPath();
  await Deno.mkdir(dirname(unit), { recursive: true });
  const contents = `[Unit]
Description=${systemdUnitName()}
Wants=network-online.target
After=network-online.target

[Service]
Environment=TOMAT_CHANNEL=${channel()}
ExecStart=${bin}
Restart=on-failure
RestartSec=5
StandardOutput=append:${join(logs, "core.stdout.log")}
StandardError=append:${join(logs, "core.stderr.log")}

[Install]
WantedBy=default.target
`;
  await Deno.writeTextFile(unit, contents);
  const reload = await run(["systemctl", "--user", "daemon-reload"]);
  if (!reload.success) {
    throw new Error("systemctl --user daemon-reload failed; re-run with TOMAT_INSTALL_SERVICE=0");
  }
  const enable = await run([
    "systemctl",
    "--user",
    "enable",
    "--now",
    `${systemdUnitName()}.service`,
  ]);
  if (!enable.success) {
    throw new Error("systemctl --user enable failed; re-run with TOMAT_INSTALL_SERVICE=0");
  }
  progress(`enabled systemd user unit ${systemdUnitName()}`);
}

async function removeSystemd(): Promise<void> {
  const unit = systemdUnitPath();
  if (!(await fileExists(unit))) {
    progress("systemd user unit not installed");
    return;
  }
  await run(["systemctl", "--user", "disable", "--now", `${systemdUnitName()}.service`], {
    ignoreError: true,
  });
  await Deno.remove(unit).catch(() => {});
  await run(["systemctl", "--user", "daemon-reload"], { ignoreError: true });
  progress(`removed systemd user unit ${systemdUnitName()}`);
}

// --- Windows Task Scheduler -----------------------------------------------

async function installScheduledTask(): Promise<void> {
  const bin = installedBinary();
  const task = systemdUnitName(); // same tomat-core<suffix> naming
  const ch = channel();
  // Scheduled tasks have no environment field, so for non-stable channels wrap
  // the launch in cmd.exe to set TOMAT_CHANNEL before exec (the same trick the
  // unified installer's install-service invocation uses).
  const action =
    ch === "stable"
      ? `$a = New-ScheduledTaskAction -Execute '${bin}'`
      : `$a = New-ScheduledTaskAction -Execute 'cmd.exe' -Argument '/c set TOMAT_CHANNEL=${ch}&& "${bin}"'`;
  const script = `
${action}
$t = New-ScheduledTaskTrigger -AtLogOn
# RestartCount/RestartInterval double as the self-update backstop: core exits
# non-zero after staging an update, and this restart-on-failure relaunches the
# already-swapped binary if the updater's fast-path start never lands.
# MultipleInstances=IgnoreNew (the default, pinned explicitly) dedups that
# backstop against the updater's own Start-ScheduledTask so only one core runs.
$s = New-ScheduledTaskSettingsSet -StartWhenAvailable -DontStopOnIdleEnd -RestartCount 5 -RestartInterval (New-TimeSpan -Minutes 1) -MultipleInstances IgnoreNew -AllowStartIfOnBatteries
$p = New-ScheduledTaskPrincipal -UserId $env:USERNAME -LogonType Interactive -RunLevel Limited
Unregister-ScheduledTask -TaskName '${task}' -Confirm:$false -ErrorAction SilentlyContinue
Register-ScheduledTask -TaskName '${task}' -Action $a -Trigger $t -Settings $s -Principal $p -Description 'tomat-core' | Out-Null
Start-ScheduledTask -TaskName '${task}'
`;
  // capture: false (null stdio) as defense-in-depth: Start-ScheduledTask hands
  // the launch to the Task Scheduler service, which does not inherit this
  // PowerShell's handles the way Start-Process (startBackground) would, but we
  // never read the output here (only rc.success), so keeping nothing piped
  // removes any chance a read-to-EOF blocks on an inherited handle.
  const rc = await runPwsh(script, { capture: false });
  if (!rc.success) {
    throw new Error(
      "Register-ScheduledTask failed; Task Scheduler may be disabled. Re-run with TOMAT_INSTALL_SERVICE=0",
    );
  }
  progress(`registered scheduled task ${task}`);
}

async function removeScheduledTask(): Promise<void> {
  const task = systemdUnitName();
  const teardown =
    `Stop-ScheduledTask -TaskName '${task}' -ErrorAction SilentlyContinue; ` +
    `Unregister-ScheduledTask -TaskName '${task}' -Confirm:$false -ErrorAction SilentlyContinue`;
  await runPwsh(teardown, { ignoreError: true });
  // Unregistering a root-folder task can need admin (see buildUninstallCommand)
  // while callers - the unified installer's uninstall.exe included - run this
  // unelevated. When the plain attempt left the task behind, retry once
  // self-elevated (an expected UAC prompt) so no dead task pointing at a
  // deleted binary lingers and fires at every logon.
  if (await scheduledTaskExists(task)) {
    await runPwsh(
      `Start-Process powershell.exe -Verb RunAs -Wait -ArgumentList ` +
        `'-NoProfile','-WindowStyle','Hidden','-ExecutionPolicy','Bypass',` +
        `'-EncodedCommand','${encodePwshCommand(teardown)}'`,
      { ignoreError: true },
    );
  }
  progress(`removed scheduled task ${task}`);
}

// --- Windows Add/Remove Programs entry --------------------------------------

function windowsUninstallKey(): string {
  return `HKCU:\\Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\tomat-core${channelSuffix()}`;
}

/** The display name a person sees in Settings > Apps: parenthesized channel,
 *  matching the Client's "Tomat (latest)" convention. */
function windowsDisplayName(): string {
  const ch = channel();
  return ch === "stable" ? "Tomat Core" : `Tomat Core (${ch})`;
}

/** Base64(UTF-16LE) encode a command for PowerShell `-EncodedCommand`. Handing
 *  the teardown across the Start-Process -Verb RunAs relaunch as one opaque
 *  token sidesteps all nested-quoting. Exported for tests. */
export function encodePwshCommand(cmd: string): string {
  const bytes = new Uint8Array(cmd.length * 2);
  for (let i = 0; i < cmd.length; i++) {
    const code = cmd.charCodeAt(i);
    bytes[i * 2] = code & 0xff;
    bytes[i * 2 + 1] = (code >> 8) & 0xff;
  }
  let binary = "";
  for (const b of bytes) binary += String.fromCharCode(b);
  return btoa(binary);
}

/** Build the Add/Remove Programs UninstallString. The teardown re-runs this
 *  binary's own uninstall-service (which removes the Scheduled Task, keychain
 *  key, and this ARP entry), then sweeps the core dir once the binary has exited
 *  and unlocked its own exe, and the channel dir if it is left empty.
 *
 *  A service install's task lives in the root Task Scheduler folder and needs
 *  admin to unregister, but Settings > Apps runs this string UNELEVATED - so for
 *  a service the command self-elevates (Start-Process -Verb RunAs, an expected
 *  UAC prompt for a background service); otherwise the task removal silently
 *  fails and a dead task pointing at a deleted binary lingers. A background
 *  install has no task, so it runs the teardown directly with no prompt.
 *  Exported for tests. */
export function buildUninstallCommand(opts: {
  asService: boolean;
  bin: string;
  root: string;
  channelDir: string;
  ch: string;
}): string {
  const { asService, bin, root, channelDir, ch } = opts;
  // The env prefix is required: an ARP launch carries no TOMAT_CHANNEL, and the
  // binary resolves its channel from the environment.
  const teardown =
    `$env:TOMAT_CHANNEL='${psq(ch)}'; ` +
    `& '${psq(bin)}' uninstall-service; ` +
    `Remove-Item -LiteralPath '${psq(root)}' -Recurse -Force -ErrorAction SilentlyContinue; ` +
    `$cd='${psq(channelDir)}'; ` +
    `if ((Test-Path -LiteralPath $cd) -and -not (Get-ChildItem -Force -LiteralPath $cd)) ` +
    `{ Remove-Item -LiteralPath $cd -Force -ErrorAction SilentlyContinue }`;
  const prefix = "powershell.exe -NoProfile -WindowStyle Hidden -ExecutionPolicy Bypass";
  if (!asService) return `${prefix} -Command "${teardown}"`;
  return (
    `${prefix} -Command "Start-Process powershell.exe -Verb RunAs -Wait -ArgumentList ` +
    `'-NoProfile','-WindowStyle','Hidden','-ExecutionPolicy','Bypass',` +
    `'-EncodedCommand','${encodePwshCommand(teardown)}'"`
  );
}

/** List this install in Add/Remove Programs (per-user HKCU key, matching the
 *  per-user install). Idempotent: re-installs simply refresh the values. */
async function registerWindowsUninstallEntry(asService: boolean): Promise<void> {
  const bin = installedBinary();
  const root = coreRoot();
  const uninstallCmd = buildUninstallCommand({
    asService,
    bin,
    root,
    channelDir: dirname(root),
    ch: channel(),
  });
  const key = windowsUninstallKey();
  const rc = await runPwsh(
    `$key = '${psq(key)}'\n` +
      `New-Item -Path $key -Force | Out-Null\n` +
      `Set-ItemProperty -Path $key -Name DisplayName -Value '${psq(windowsDisplayName())}'\n` +
      `Set-ItemProperty -Path $key -Name DisplayVersion -Value '${psq(CORE_VERSION)}'\n` +
      `Set-ItemProperty -Path $key -Name Publisher -Value 'Tomat'\n` +
      `Set-ItemProperty -Path $key -Name InstallLocation -Value '${psq(root)}'\n` +
      `Set-ItemProperty -Path $key -Name DisplayIcon -Value '${psq(bin)}'\n` +
      `Set-ItemProperty -Path $key -Name UninstallString -Value '${psq(uninstallCmd)}'\n` +
      `Set-ItemProperty -Path $key -Name QuietUninstallString -Value '${psq(uninstallCmd)}'\n` +
      `Set-ItemProperty -Path $key -Name NoModify -Value 1 -Type DWord\n` +
      `Set-ItemProperty -Path $key -Name NoRepair -Value 1 -Type DWord\n`,
  );
  if (!rc.success) {
    // Non-fatal: a missing Apps entry must not fail the install itself.
    progress(`could not register the Add/Remove Programs entry (exit ${rc.code})`);
    return;
  }
  progress(`registered "${windowsDisplayName()}" in Add/Remove Programs`);
}

async function unregisterWindowsUninstallEntry(): Promise<void> {
  await runPwsh(
    `Remove-Item -Path '${psq(windowsUninstallKey())}' -Recurse -Force -ErrorAction SilentlyContinue`,
    { ignoreError: true },
  );
  progress("removed the Add/Remove Programs entry");
}

// --- TOMAT_INSTALL_SERVICE=0 background launch ----------------------------

/** Launch core detached, with nothing supervising it, and return its PID so the
 *  caller can hand it to whoever owns the process's lifetime (the Client stops
 *  exactly this PID when it quits). Returns null when the PID could not be
 *  determined; the core is still running in that case. */
async function startBackground(): Promise<number | null> {
  const bin = installedBinary();
  const logs = paths().logsDir;
  const out = join(logs, "core.stdout.log");
  const err = join(logs, "core.stderr.log");
  if (Deno.build.os === "windows") {
    // capture: false is load-bearing: Start-Process (.NET) passes
    // bInheritHandles, so the detached core inherits every inheritable handle
    // of this PowerShell, including piped stdio. With pipes, our read-to-EOF
    // would block for as long as the core lives - the "stuck on installing"
    // hang the client used to hit. Null stdio leaves nothing to hold open,
    // which is why the PID travels through a temp FILE rather than stdout.
    const pidFile = await Deno.makeTempFile({ prefix: "tomat-core-pid-" });
    try {
      const rc = await runPwsh(
        `$env:TOMAT_CHANNEL='${psq(channel())}'; ` +
          `$p = Start-Process -FilePath '${psq(bin)}' -WindowStyle Hidden -PassThru ` +
          `-RedirectStandardOutput '${psq(out)}' -RedirectStandardError '${psq(err)}'; ` +
          `$p.Id | Set-Content -LiteralPath '${psq(pidFile)}'`,
        { capture: false },
      );
      if (!rc.success) {
        throw new Error(`could not start the core in the background (exit ${rc.code})`);
      }
      progress("started core in the background (no service registered)");
      return parsePid(await Deno.readTextFile(pidFile).catch(() => ""));
    } finally {
      await Deno.remove(pidFile).catch(() => {});
    }
  }
  // Detach via sh so the `&`-backgrounded core outlives this short-lived CLI,
  // exactly like the scripts' nohup branch. Capturing sh's stdout is safe here
  // (unlike the Windows branch): the core's own stdio is redirected into the
  // log files, so the pipe only ever carries the `echo $!` line and reaches EOF
  // when sh exits.
  const shell = `nohup ${shQuote(bin)} >> ${shQuote(out)} 2>> ${shQuote(err)} & ` + `echo $!`;
  const rc = await run(["sh", "-c", shell], {
    env: { ...Deno.env.toObject(), TOMAT_CHANNEL: channel() },
  });
  if (!rc.success) {
    throw new Error(`could not start the core in the background (exit ${rc.code})`);
  }
  progress("started core in the background (no service registered)");
  return parsePid(rc.stdout);
}

/** The PID `startBackground` echoes, or null when the output holds no positive
 *  integer (a shell that printed nothing, an unreadable Windows pid file). */
export function parsePid(raw: string): number | null {
  const n = Number(raw.trim());
  return Number.isInteger(n) && n > 0 ? n : null;
}

// --- shared teardown steps ------------------------------------------------

async function killStragglers(): Promise<void> {
  const binName = coreBinaryName("tomat-core");
  // uninstall-service runs FROM the tomat-core<suffix> binary it is sweeping, so
  // every match below includes THIS process. Excluding our own PID keeps
  // killStragglers from terminating the uninstall mid-flight, before it clears
  // the keychain master key and removes the core dir.
  const self = Deno.pid;
  if (Deno.build.os === "windows") {
    await runPwsh(
      `Get-Process -Name '${binName.replace(/\.exe$/, "")}' -ErrorAction SilentlyContinue | ` +
        `Where-Object { $_.Id -ne ${self} } | ` +
        `Stop-Process -Force -ErrorAction SilentlyContinue`,
      { ignoreError: true },
    );
    return;
  }
  const target = join(paths().binDir, binName);
  const found = await run(["pgrep", "-f", target], { ignoreError: true });
  if (!found.success) return;
  const pids = found.stdout
    .split(/\s+/)
    .map((s) => s.trim())
    .filter(Boolean)
    .filter((pid) => pid !== String(self));
  for (const pid of pids) await run(["kill", pid], { ignoreError: true });
  // Brief grace, then SIGKILL any survivors.
  await new Promise((r) => setTimeout(r, 1000));
  for (const pid of pids) await run(["kill", "-9", pid], { ignoreError: true });
  if (pids.length) progress(`stopped ${pids.length} straggler core process(es)`);
}

async function clearKeychainMasterKey(): Promise<void> {
  const service = `au.tomat.core${channelKeychainSuffix()}`;
  // keychainDelete is idempotent and returns false when the helper is absent;
  // on macOS fall back to the native security tool so a partially-removed
  // install still clears the entry.
  const ok = await keychainDelete(service, "master-key");
  if (ok) {
    progress("cleared keychain master key");
    return;
  }
  if (Deno.build.os === "darwin") {
    await run(["security", "delete-generic-password", "-s", service, "-a", "master-key"], {
      ignoreError: true,
    });
    progress("cleared keychain master key (security fallback)");
  }
}

async function removeCoreDir(): Promise<void> {
  const root = coreRoot();
  if (!(await fileExists(root))) {
    progress("core directory already gone");
    return;
  }
  try {
    await Deno.remove(root, { recursive: true });
  } catch (err) {
    // On Windows this subcommand IS the running tomat-core.exe under
    // <root>/bin, so it can't delete its own on-disk image (sharing violation).
    // Remove everything we can now and leave the locked binary for the caller
    // that outlives this process (the NSIS uninstaller's RMDir, or the thin
    // core-uninstall.ps1's post-exit retry) to sweep. On unix a running binary's
    // file deletes fine, so this branch is Windows-only in practice.
    if (Deno.build.os !== "windows") throw err;
    const self = Deno.execPath();
    await removeExcept(root, self);
    progress(`removed core data; the running binary is swept after exit`);
    return;
  }
  // Drop the now-empty ~/.tomat/<channel> dir if the client is gone too; the
  // shared models dir lives under ~/.tomat, not the channel dir, so it survives.
  await Deno.remove(dirname(root)).catch(() => {});
  progress(`removed ${root}`);
}

/** Remove the shared ~/.tomat/models downloads (--purge-models). Independent of
 *  keepData: models are shared across channels and offered as their own
 *  uninstall choice. Exported for tests. */
export async function removeModelsDir(): Promise<void> {
  const models = paths().modelsDir;
  if (!(await fileExists(models))) {
    progress("models directory already gone");
    return;
  }
  await Deno.remove(models, { recursive: true });
  progress(`removed ${models} (--purge-models)`);
}

// Recursively delete everything under `dir` except the single file at `keep`
// (the currently-running executable, which Windows keeps locked). Best-effort
// per entry so one locked file never aborts the rest of the teardown.
async function removeExcept(dir: string, keep: string): Promise<void> {
  for await (const entry of Deno.readDir(dir)) {
    const path = join(dir, entry.name);
    if (path === keep) continue;
    if (entry.isDirectory) {
      await removeExcept(path, keep);
      await Deno.remove(path).catch(() => {}); // now-empty (or still holds `keep`)
    } else {
      await Deno.remove(path).catch(() => {});
    }
  }
}

// --- small utilities ------------------------------------------------------

async function fileExists(path: string): Promise<boolean> {
  try {
    await Deno.stat(path);
    return true;
  } catch {
    return false;
  }
}

// Single-quote a path for a POSIX shell (only the nohup branch needs this).
function shQuote(s: string): string {
  return `'${s.replaceAll("'", "'\\''")}'`;
}

// Escape a value for interpolation INSIDE a single-quoted PowerShell literal
// (doubling is PowerShell's single-quote escape). Callers write '${psq(v)}'.
function psq(s: string): string {
  return s.replaceAll("'", "''");
}
