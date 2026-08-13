<script lang="ts">
  // Bottom-left of the Settings sidebar. Drives the combined client + core +
  // sidecar update flow, mapping it onto the phases UpdateButtonView renders
  // (which owns the label copy for each). `clientRestartPending` relaunches Tauri.

  import { onMount } from "svelte";
  import { platform, type UpdateHandle } from "$lib/platform";
  import { cores } from "$lib/core";
  import { confirmState } from "$stores";
  import type { BinaryKind, BinaryUpdateCheck } from "@loca/shared";
  import UpdateButtonView, {
    type UpdateButtonPhase,
  } from "@loca/shared/ui/components/settings/UpdateButtonView.svelte";
  import { useBlink } from "$composables/use-blink.svelte";
  import { getLogger } from "$lib/util/log";

  const log = getLogger("update");
  // Where an install the updater can't replace in place (a non-AppImage Linux
  // install: a distro/third-party repackage or a raw binary) sends the user to
  // grab the new build, the way VS Code opens the download page on such installs.
  const INSTALL_PAGE_URL = "https://loca.rzkyif.com/install";

  let { collapsed, disabled = false } = $props<{
    collapsed: boolean;
    disabled?: boolean;
  }>();

  let phase = $state<UpdateButtonPhase>("idle");
  let coreAvailable = $state(false);
  let coreCurrent = $state<string>("");
  let coreLatest = $state<string>("");
  let binariesAvailable = $state<BinaryUpdateCheck[]>([]);
  let clientUpdate = $state<UpdateHandle | null>(null);
  // False on a non-AppImage Linux install (the Tauri updater can only replace an
  // AppImage): those get sent to the download page instead of a self-install.
  let clientSelfInstall = $state(true);

  onMount(async () => {
    // Install-static (the packaging of this build never changes at runtime), so
    // resolve it once here rather than on every check. Defaults to true, so a
    // rare failure degrades to attempting the in-app install, not blocking it.
    try {
      clientSelfInstall = await platform().updater.canSelfInstall();
    } catch (e) {
      log.warn("canSelfInstall check failed:", e);
    }
  });

  // Pulse between two yellow shades while an update is available: a same-hue
  // attention ping. Interval matches the button's color transition so the color
  // is always mid-tween (no flat hold). The View renders the two shades; this
  // feeds it the live toggle. See useBlink.
  const updateBlink = useBlink();
  $effect(() => updateBlink.run(phase === "available"));

  async function onClick() {
    if (phase === "checking" || phase === "updating") return;
    if (phase === "clientRestartPending") {
      try {
        await platform().updater.relaunch();
      } catch (e) {
        log.warn("relaunch failed:", e);
      }
      return;
    }
    if (phase === "available") {
      await confirmInstall();
      return;
    }
    await runCheck();
  }

  async function runCheck() {
    phase = "checking";
    coreAvailable = false;
    binariesAvailable = [];
    // Release a handle left over from a prior check (or a dismissed confirm) so
    // the underlying Tauri Update object isn't leaked when we overwrite it.
    void clientUpdate?.close().catch(() => {});
    clientUpdate = null;

    const tasks: Promise<void>[] = [];

    // Core self-update.
    tasks.push(
      (async () => {
        try {
          const res = await cores().api().update.check();
          coreAvailable = res.available;
          coreCurrent = res.currentVersion;
          coreLatest = res.latestVersion;
        } catch (e) {
          log.warn("core check failed:", e);
        }
      })(),
    );

    // Sidecar binaries.
    tasks.push(
      (async () => {
        try {
          const res = await cores().api().binaries.check();
          binariesAvailable = res.filter((b) => b.available);
        } catch (e) {
          log.warn("binaries check failed:", e);
        }
      })(),
    );

    // Client (Tauri).
    tasks.push(
      (async () => {
        try {
          clientUpdate = await platform().updater.check();
        } catch (e) {
          log.warn("client check failed:", e);
        }
      })(),
    );

    await Promise.allSettled(tasks);

    const anyAvailable = coreAvailable || binariesAvailable.length > 0 || !!clientUpdate;
    phase = anyAvailable ? "available" : "idle";
  }

  async function confirmInstall() {
    // A client update this install can't apply in place (a non-AppImage Linux
    // install) is a browser hand-off to the download page, not an in-app install.
    const clientRedirect = !!clientUpdate && !clientSelfInstall;
    const lines: string[] = [];
    if (clientUpdate) {
      lines.push(
        `Loca Client v${clientUpdate.version} (Client` +
          (clientRedirect ? ", opens the download page)" : ")"),
      );
    }
    if (coreAvailable) {
      lines.push(`Loca Core v${coreLatest} (Core, currently v${coreCurrent})`);
    }
    for (const b of binariesAvailable) {
      lines.push(
        `${b.kind} v${b.latestVersion}` +
          (b.installedVersion ? ` (was v${b.installedVersion})` : ""),
      );
    }
    const clientClause = !clientUpdate
      ? `.`
      : clientRedirect
        ? `; the Client update opens the download page in your browser to get the ` +
          `latest build.`
        : `; the Client will need a restart at the end.`;
    confirmState.request({
      title: "Install updates",
      message:
        `The following components will be updated. Core and sidecars are ` +
        `downloaded here${clientClause}\n\n` +
        lines.map((l) => `• ${l}`).join("\n"),
      confirmLabel: "Install",
      onConfirm: () => void runInstall(),
    });
  }

  async function runInstall() {
    phase = "updating";

    const binaryKinds: BinaryKind[] = binariesAvailable.map((b) => b.kind);

    try {
      // Kick off sidecar binary updates first. These are short and let the
      // user see progress in the downloads modal while the bigger work runs.
      for (const kind of binaryKinds) {
        try {
          await cores().api().binaries.update(kind);
        } catch (e) {
          log.warn(`binary update ${kind} failed:`, e);
        }
      }

      // Core self-update. The HTTP request typically does NOT return cleanly.
      // The core exits during apply() to hand off to the updater binary.
      // The next /health probe (or the WS reconnect logic) will see the new
      // version. Swallow the request-aborted error.
      if (coreAvailable) {
        try {
          await cores().api().update.apply();
        } catch (e) {
          log.warn("core.apply error (expected during handoff):", e);
        }
      }

      // Client artifact. On a self-updating install (macOS, Windows, Linux
      // AppImage) tauri-plugin-updater downloads the bundle, verifies the
      // signature, and stages it for the next launch; restart is in the user's
      // hands so they don't lose unsaved input. On a non-AppImage Linux install
      // the updater can't replace the running app, so open the download page for
      // the user to fetch the new build themselves.
      if (clientUpdate) {
        if (clientSelfInstall) {
          try {
            // downloadAndInstall closes the handle implicitly on success.
            await clientUpdate.downloadAndInstall();
            phase = "clientRestartPending";
            return;
          } catch (e) {
            log.warn("client downloadAndInstall failed:", e);
            void clientUpdate.close().catch(() => {});
            clientUpdate = null;
          }
        } else {
          try {
            await platform().openExternal(INSTALL_PAGE_URL);
          } catch (e) {
            log.warn("open download page failed:", e);
          }
          // The browser hand-off won't install the staged handle; release it.
          void clientUpdate.close().catch(() => {});
          clientUpdate = null;
        }
      }

      phase = "idle";
    } catch (e) {
      log.warn("install failed:", e);
      phase = "idle";
    }
  }
</script>

<UpdateButtonView {phase} {collapsed} {disabled} blink={updateBlink.on} {onClick} />
