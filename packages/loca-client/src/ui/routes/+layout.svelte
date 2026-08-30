<script lang="ts">
  import "virtual:uno.css";
  import "@unocss/reset/tailwind-v4.css";
  import "@loca/shared/ui/styles/base.css";
  import "../app.css";
  import { onMount } from "svelte";
  import { browser } from "$app/environment";
  import {
    installPlatform,
    isAndroidPlatform,
    isIosPlatform,
    isMobilePlatform,
    isWindowsPlatform,
  } from "$lib/platform/select";
  import { connectionState } from "$stores/connection.svelte";
  import { capabilitiesState, coreStatusState } from "$stores/core-status.svelte";
  import { localCoreState } from "$stores/local-core.svelte";
  import { makeUiContext, setUiContext } from "@loca/shared/ui/context";
  import { getDefaultSettings } from "@loca/shared";
  import { settingsState } from "$stores";
  import { expansionState, isExpanded } from "$stores/expansion.svelte";
  import { backState } from "$stores/back.svelte";
  import { getDuration } from "$lib/appearance/animations";

  // Same schema defaults the website's DEFAULT_UI_CONTEXT uses, so a missing
  // live setting falls back to exactly what a fresh app would render.
  const settingDefaults = getDefaultSettings();

  // Back the shared UI context (read by the extracted @loca/shared/ui
  // components: Bubble, Tabs, ...) with the client's live stores. The factory
  // owns the settings-derived members (alignment, blur, the system-message
  // tint), so the client cannot drift from the website's DEFAULT_UI_CONTEXT.
  // `getSetting` reads the live store, keeping the reactive link so a settings
  // change repaints bubbles/knobs exactly as before extraction.
  // Form factor drives the shared Views' layout/touch branches. Resolved from
  // the running OS in the browser only; prerender stays desktop-shaped (no Tauri
  // runtime), and the browser script init re-runs and sets the real values.
  const onMobile = browser && isMobilePlatform();
  const onWindows = browser && isWindowsPlatform();

  // Raise platform-specific defaults (mobile text size; Windows `super`-free
  // shortcut defaults) before +page's onMount loads the stored settings over the
  // defaults. Runs in the layout script, ahead of any child onMount, like
  // installPlatform below.
  settingsState.setPlatformDefaults(onMobile, onWindows);

  setUiContext(
    makeUiContext({
      getSetting: (key) => settingsState.currentSettings[key] ?? settingDefaults[key],
      animationDurationMs: (ms) => getDuration(ms),
      expansionGet: (id, fallback) => isExpanded(id, fallback),
      expansionSet: (id, value) => expansionState.set(id, value),
      expansionInit: (id, value) => {
        if (!expansionState.has(id)) expansionState.set(id, value);
      },
      platform: onMobile ? "mobile" : "desktop",
      pointer: onMobile ? "coarse" : "fine",
      // Android owns the system back gesture, so its shells drop the in-app
      // back / close buttons; iOS and desktop keep them.
      hasSystemBack: onMobile && isAndroidPlatform(),
      registerBack: (handler) => backState.push(handler),
    }),
  );

  // Install the platform singleton during the layout's script init, NOT in
  // onMount: in SvelteKit the child +page.svelte's onMount fires before the
  // parent layout's, and +page.svelte's onMount calls platform() (via
  // loadClientSettings -> applyToggleWindowShortcut). The parent script runs
  // before any child onMount, so this guarantees setPlatform() lands first.
  // Guarded by `browser` to stay out of SSR/prerender (the impl imports
  // @tauri-apps/*).
  if (browser) installPlatform();

  onMount(() => {
    // Connection-state subscription is persistent (cores() re-binds it on
    // every select()), so it is safe to attach before any core is paired.
    // The core restore + initial-mode decision happens in +page.svelte so it
    // completes before the page's first render.
    connectionState.attach();
    // Backend core status (core.status frames) feeds the CoreBar alongside the
    // transport state above; same persistent-subscription rationale.
    coreStatusState.attach();
    // What the selected core can DO, read from /health on connect. Beside
    // coreStatusState rather than with the stores in +page.svelte because both
    // are per-core state that must reset on a core switch, not on a page mount.
    capabilitiesState.attach();
    // Liveness of the core installed on this device, feeding the tray's Start /
    // Stop Core item and the CoreBar's Stopped pill. Probes on boot and on every
    // transport transition; never polls.
    localCoreState.attach();
    // Android injects the soft-keyboard / safe-area insets natively as CSS
    // variables (see the Android MainActivity). iOS has no such native layer, so
    // the shell reads the safe area from CSS env() (the .platform-ios class in
    // app.css) and derives --keyboard-inset from the visual viewport here.
    if (isIosPlatform()) return installIosInsets();
  });

  // Set --keyboard-inset from the visual viewport on iOS: the layout viewport
  // stays full-size (interactive-widget=overlays-content, see app.html) while the
  // visual viewport shrinks by the keyboard's height, so the difference is the
  // inset the composer lifts by. The safe-area insets come from CSS env() and
  // need no JS. Returns a teardown for onMount.
  function installIosInsets(): () => void {
    const root = document.documentElement;
    root.classList.add("platform-ios");
    const vv = globalThis.visualViewport;
    if (!vv) return () => root.classList.remove("platform-ios");
    const update = (): void => {
      const inset = Math.max(0, globalThis.innerHeight - vv.height - vv.offsetTop);
      root.style.setProperty("--keyboard-inset", `${inset}px`);
    };
    vv.addEventListener("resize", update);
    vv.addEventListener("scroll", update);
    update();
    return () => {
      vv.removeEventListener("resize", update);
      vv.removeEventListener("scroll", update);
      root.style.removeProperty("--keyboard-inset");
      root.classList.remove("platform-ios");
    };
  }

  // With `dragDropEnabled: false` on the desktop window the webview handles file
  // drops itself, so an unhandled drop would navigate the page to the dropped
  // file. The composer catches drops in chat view; this app-wide guard blocks
  // navigation everywhere else (Settings, onboarding, ...). Scoped to file drags
  // so dragging text within an input is unaffected; inert on mobile (no file
  // drag) and during SSR (window handlers don't run).
  function blockFileDropNavigation(e: DragEvent): void {
    if (Array.from(e.dataTransfer?.types ?? []).includes("Files")) e.preventDefault();
  }
</script>

<svelte:window ondragover={blockFileDropNavigation} ondrop={blockFileDropNavigation} />

<slot />
