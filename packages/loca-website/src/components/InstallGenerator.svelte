<script lang="ts">
  // The enhanced install generator, mounted over the no-JS baseline
  // (InstallGenerator.astro) when JS is available. One instance covers the whole
  // install: the unified installer download (Client + optional Core) per OS and
  // arch, the Linux Client/Core split, and the Android APK. It auto-detects the
  // OS and architecture to highlight the right download, and reuses the same
  // lib/install helpers as the baseline so the two never drift.
  import { onMount } from "svelte";
  import {
    androidApkUrl,
    appInstallers,
    type Arch,
    type Channel,
    detectArch,
    detectOs,
    finishStep,
    formatTag,
    installerHowto,
    linuxCoreInstallers,
    linuxCoreSteps,
    type Os,
    OS_CHOICES,
  } from "../lib/install.ts";

  let os = $state<Os>("macos");
  // The genuinely detected OS (null when it can't be told, e.g. a future
  // platform). Distinct from `os`, which follows the picker: the highlight keys
  // off this so switching tabs doesn't paint another OS's list as "yours".
  let detectedOs = $state<Os | null>(null);
  // Best-effort CPU arch, resolved in onMount to highlight the installer that
  // matches the visitor's machine (like the auto-selected OS). null until known,
  // and stays null where it can't be told (macOS Safari/Firefox), so nothing is
  // highlighted rather than guessing wrong.
  let arch = $state<Arch | null>(null);
  // Stable has not shipped yet, so the channel is locked to latest; the picker
  // shows stable as a disabled option for when it does.
  const channel: Channel = "latest";

  // navigator.userAgentData is Chromium-only and not in the DOM lib typings; a
  // minimal structural type keeps the access honest without an `any` cast.
  interface UserAgentDataLike {
    getHighEntropyValues(hints: string[]): Promise<{ architecture?: string }>;
  }

  onMount(() => {
    const detected = detectOs(navigator.userAgent, navigator.platform);
    detectedOs = detected;
    // Pick the initial tab: the detected OS, or macOS when unknown.
    os = detected ?? "macos";
    // The high-entropy architecture is async and Chromium-only; other browsers
    // (and the ones that reject the request) fall back to the UA string.
    const ua = navigator.userAgent;
    const uaData = (navigator as Navigator & { userAgentData?: UserAgentDataLike }).userAgentData;
    if (uaData) {
      uaData
        .getHighEntropyValues(["architecture"])
        .then((v) => (arch = detectArch(v.architecture, ua)))
        .catch(() => (arch = detectArch(undefined, ua)));
    } else {
      arch = detectArch(undefined, ua);
    }
  });

  const isAndroid = $derived(os === "android");
  // The main download: the unified installer (macOS/Windows) or the Client
  // AppImage (Linux). Empty on Android, where the APK block handles it.
  const installers = $derived(appInstallers(os, channel));
  // The Linux-only standalone Core packages, shown under the app download.
  const coreInstallers = $derived(os === "linux" ? linuxCoreInstallers(channel) : []);
  // Only highlight the detected arch when the OS actually offers a choice of
  // arches (macOS, Windows). A single-arch OS (Linux) has nothing to
  // disambiguate, so lighting a button up there would just read as noise.
  const archChoice = $derived(new Set(installers.map((i) => i.arch)).size > 1);
  // Only recommend downloads on the visitor's own OS list: when the shown tab is
  // the detected OS, or when the OS is unknown (then we fall back to arch alone,
  // which is meaningful on every list).
  const osMatches = $derived(detectedOs === null || detectedOs === os);
  const installerSteps = $derived(installerHowto(os));
  const finish = $derived(finishStep(os));
  const coreSteps = $derived(linuxCoreSteps(channel));

  const labelCls = "text-xs font-medium uppercase tracking-wide text-default-500";
  // OS + channel buttons share one look: an inset chip that flips to the inverted
  // "primary" fill when active (the segmented controls' selected colour). The
  // pointer cursor is added per-button only on the clickable ones (the OS picker);
  // the channel picker is locked, so its buttons keep the default cursor.
  const btnBase =
    "flex items-center justify-center gap-2 px-3 py-2 rounded-medium text-sm transition-colors";
  const btnActive = `${btnBase} bg-default-inverted-300 text-default-inverted-900`;
  const btnIdle = `${btnBase} bg-surface-inset text-default-700 hover:bg-surface-inset-strong`;
</script>

<!-- not-prose: this island can mount inside the manual's `.prose`, whose
     presetTypography would otherwise restyle the list `<p>` margins. It is a
     no-op on the /install page, which has no prose ancestor. -->
<div class="not-prose flex flex-col gap-6">
  <!-- Operating system. -->
  <div class="flex flex-col gap-2">
    <span class={labelCls}>Operating System</span>
    <div class="grid gap-2 grid-cols-2 sm:grid-cols-4">
      {#each OS_CHOICES as o (o.id)}
        <button
          type="button"
          class={`${os === o.id ? btnActive : btnIdle} hover:cursor-pointer`}
          aria-pressed={os === o.id}
          onclick={() => (os = o.id)}
        >
          <i class={`${o.icon} text-lg`}></i>
          {o.label}
        </button>
      {/each}
    </div>
  </div>

  <!-- Channel, locked to latest for now (stable not yet released). -->
  <div class="flex flex-col gap-2">
    <span class={labelCls}>Channel</span>
    <div class="grid grid-cols-2 gap-2">
      <button
        type="button"
        class={`${btnBase} bg-surface-inset text-default-500 opacity-60 cursor-not-allowed`}
        disabled
        aria-disabled="true"
      >
        <i class="i-material-symbols-shield-outline-rounded text-lg"></i>
        Stable
      </button>
      <button type="button" class={btnActive} aria-pressed="true">
        <i class="i-material-symbols-bolt-rounded text-lg"></i>
        Latest
      </button>
    </div>
  </div>

  {#if isAndroid}
    <!-- Android installs from a downloaded APK. -->
    <div class="flex flex-col gap-2">
      <span class={labelCls}>Download</span>
      <div class="flex flex-col gap-3 rounded-large bg-surface-inset px-4 py-3">
        <p class="text-sm text-default-700 m-0">
          The Android app is a signed APK you install directly.
        </p>
        <a
          href={androidApkUrl(channel)}
          class="inline-flex items-center justify-center gap-2 rounded-large bg-default-inverted-300 px-4 py-2.5 text-sm text-default-inverted-900 hover:cursor-pointer"
        >
          <i class="i-material-symbols-download-rounded text-base"></i>
          Download the Android APK
        </a>
        <ol class="m-0 flex flex-col gap-1 text-sm text-default-600 list-decimal pl-5">
          <li>Open the downloaded file on your phone.</li>
          <li>If asked, allow installing apps from this source.</li>
          <li>Confirm the install, then open Loca and connect it to a Core.</li>
        </ol>
      </div>
    </div>
  {:else}
    <!-- The main download: one installer per arch. -->
    <div class="flex flex-col gap-2">
      <span class={labelCls}>
        {os === "linux" ? "Download the app (AppImage)" : "Download the installer"}
      </span>
      <div class="grid grid-cols-2 gap-2">
        {#each installers as inst (inst.url)}
          <!-- Recommend a download only on the visitor's own OS list (or when
               the OS is unknown). Within it, a lone button (the single-arch
               Linux AppImage) is always the right arch; otherwise highlight the
               button matching the detected arch. -->
          {@const recommended =
            osMatches && (installers.length === 1 || (archChoice && arch === inst.arch))}
          <a
            href={inst.url}
            class={`${recommended ? btnActive : btnIdle} hover:cursor-pointer`}
            title={recommended ? "Recommended for your device" : undefined}
          >
            <i class="i-material-symbols-download-rounded text-lg"></i>
            {inst.archLabel}
          </a>
        {/each}
      </div>
    </div>

    {#if os === "linux"}
      <!-- The Linux-only standalone Core packages. -->
      <div class="flex flex-col gap-2">
        <span class={labelCls}>Run the Core on this machine too</span>
        <div class="grid grid-cols-2 gap-2">
          {#each coreInstallers as inst (inst.url)}
            <a href={inst.url} class={`${btnIdle} hover:cursor-pointer`}>
              <i class="i-material-symbols-download-rounded text-lg"></i>
              Loca Core · {formatTag(inst.format)}
            </a>
          {/each}
        </div>
        <ol class="m-0 flex flex-col gap-1 text-sm text-default-600 list-decimal pl-5">
          {#each coreSteps as step (step)}
            <li>{step}</li>
          {/each}
        </ol>
      </div>
    {/if}

    <!-- How to Install. -->
    <div class="flex flex-col gap-2">
      <span class={labelCls}>How to Install</span>
      <ol class="m-0 flex flex-col gap-2 text-sm text-default-700 list-decimal pl-5">
        {#each installerSteps as step, i (i)}
          {#if typeof step === "string"}
            <li>{step}</li>
          {:else}
            <li>
              {step.text}
              {#if step.sub.length}
                <ul class="mt-1 flex flex-col gap-1 list-disc pl-5">
                  {#each step.sub as s (s)}
                    <li>{s}</li>
                  {/each}
                </ul>
              {/if}
            </li>
          {/if}
        {/each}
        <li>{finish}</li>
        <li>
          Setting up a server or a machine without a screen? See <a
            href="/manual/getting-started/installing"
            class="underline hov:text-default-700">headless installs</a
          > in the manual.
        </li>
        <li>
          If you ever need to uninstall, follow <a
            href="/manual/maintenance/uninstalling"
            class="underline hov:text-default-700">the uninstallation guide</a
          >.
        </li>
      </ol>
    </div>
  {/if}
</div>
