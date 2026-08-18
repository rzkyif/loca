<script lang="ts">
  import { onDestroy, onMount, tick } from "svelte";
  import { fade } from "svelte/transition";
  import CoreBar from "$components/chat/CoreBar.svelte";
  import MessageTranscript from "$components/chat/MessageTranscript.svelte";
  import ActionSheetHost from "$components/chat/ActionSheetHost.svelte";
  import Settings from "$components/settings/Settings.svelte";
  import NewCore from "$components/new-core/NewCore.svelte";
  import QuickSettings from "$components/quick-settings/QuickSettings.svelte";
  import SessionList from "$components/session-list/SessionList.svelte";
  import { bubbleGap, useUiContext } from "@loca/shared/ui/context";
  import { type BubbleMerge, mergeFlatCorners, NO_MERGE } from "@loca/shared/ui/merge";
  import {
    downloadsState,
    memoriesState,
    scheduledPromptsState,
    serversState,
    sessionsState,
    settingsState,
    snippetsState,
    streamingState,
    extensionsState,
    mcpState,
    updateState,
    viewState,
  } from "$stores";
  import { connectionState } from "$stores/connection.svelte";
  import { backState } from "$stores/back.svelte";
  import type { AppMode } from "$stores/view.svelte";
  import {
    cores,
    dropTokenlessCores,
    dropUnauthorizedCore,
    ensureLocalCoreUpIfNeeded,
    isLoopbackUrl,
    pruneUninstalledLocalCores,
  } from "$lib/core";
  import { platform } from "$lib/platform";
  import { useTheme } from "$composables/use-theme.svelte";
  import { WindowChoreography } from "$composables/use-window-choreography.svelte";
  import { getLogger } from "$lib/util/log";
  import { shortcutHandler, windowTransition } from "$stores/shortcut.svelte";
  import { enableMessageAnimations, getDuration } from "$lib/appearance/animations";

  // Sidecar lifecycle is server-side; the client only attaches WS-driven
  // state subscribers so status frames reach the UI.
  function setupSidecarListeners(): void {
    /* no-op: handled by serversState.attach() in onMount */
  }
  async function startConfiguredServices(): Promise<void> {
    /* no-op: handled server-side */
  }
  import {
    startClickThrough,
    stopClickThrough,
    pauseClickThrough,
    resumeClickThrough,
  } from "$lib/window/window";
  import { startBlurKeepalive, stopBlurKeepalive } from "$lib/window/window";

  const log = getLogger("boot");
  const ui = useUiContext();

  // Settings view (desktop panelColumn): the CoreBar pins flush against the
  // panel's bottom. The panel is always wider than the small CoreBar, so the
  // CoreBar is the narrower "plug" (both top corners squared, pulled up to
  // overlap) and the panel squares off its aligned bottom corner. Other panel
  // modes (session list) leave the CoreBar floating.
  const settingsCoreMerge = $derived.by<BubbleMerge>(() =>
    viewState.mode === "settings"
      ? {
          flatCorners: mergeFlatCorners(settingsState.getAlignment(), "top", true),
          overlapTop: true,
        }
      : NO_MERGE,
  );
  const settingsPanelFlat = $derived(
    mergeFlatCorners(settingsState.getAlignment(), "bottom", false),
  );
  // On mobile the app is a single opaque fullscreen activity: the transparent
  // bubble window, click-through, blur keepalive, cursor polling, and the
  // offscreen slide-in choreography are all desktop-only and gated off here.
  const onMobile = ui.platform === "mobile";

  // The keepalive runs exactly while halo rings exist (same condition as
  // Bubble.svelte's ringCount); without rings there's no backdrop to keep
  // fresh. Desktop only: there is no transparent backdrop to resample on mobile.
  const blurActive = $derived(
    settingsState.currentSettings["appearance.bubbleBlurEnabled"] !== false &&
      ((settingsState.currentSettings["appearance.bubbleBlurRings"] as number) ?? 3) > 0,
  );
  $effect(() => {
    if (onMobile) return;
    if (blurActive) startBlurKeepalive();
    else stopBlurKeepalive();
  });

  // Appearance/layout settings applied to documentElement live in the theme
  // composable; the consumer owns the boot apply + the per-key $effects below.
  // SSR is off (see +layout.ts) so it's safe to touch `window`/`document`.
  const theme = useTheme();

  let loaded = $state(false);
  let sessionLoading = $state(true);
  let contentEl: HTMLElement | undefined = $state();

  // The window slide engine (boot reveal, shortcut show/hide, panel + mobile
  // carousel navigation). This component binds its element refs via `bind:this`,
  // calls its methods from the boot onMount and the visibility/hide/monitor
  // subscriptions, and keeps the two $effects below that trigger runSlide (on a
  // pending navigation) and positionWindow (on alignment/monitor/width change).
  const choreo = new WindowChoreography(onMobile);

  const animationsEnabled = $derived(
    !!settingsState.currentSettings["appearance.animationsEnabled"],
  );

  const linkHandler = (e: MouseEvent) => {
    const anchor = (e.target as HTMLElement).closest("a");
    if (anchor && anchor.href && anchor.href.startsWith("http")) {
      const url = new URL(anchor.href);
      if (!url.hostname.includes("localhost") && !url.hostname.includes("tauri.localhost")) {
        e.preventDefault();
        void platform().openExternal(anchor.href);
      }
    }
  };

  // Greetings. The greetings.* settings are client-local, so the client gates on
  // them and asks the core to open a session only when one should run. A single
  // POST helper is shared by the boot (launch) trigger below and the on-show
  // trigger in the visibility listener.
  let lastGreetingAtMs = 0; // Date.now() of the last greeting POST we sent

  function greetingCooldownMs(): number {
    const s = settingsState.currentSettings;
    return (Number(s["greetings.showCooldown"] ?? 10) || 10) * 1000;
  }

  async function runGreeting(): Promise<{ ran: boolean; sessionId?: string }> {
    const s = settingsState.currentSettings;
    return await cores()
      .api()
      .greetings.run({
        sessionTitle: (s["greetings.sessionTitle"] as string | undefined) ?? "",
        instruction: (s["greetings.instruction"] as string | undefined) ?? "",
        cooldownMs: greetingCooldownMs(),
      });
  }

  // Fired on every window reveal (desktop only). Greets when "Every Show" is
  // selected and the cooldown has elapsed, matching an app-open greeting. The
  // core dedups on the same cooldown, so the client guard only avoids a
  // redundant POST (e.g. the launch greeting's own show_when_done reveal).
  async function maybeGreetOnShow(): Promise<void> {
    const s = settingsState.currentSettings;
    if (s["greetings.enabled"] !== true) return;
    if (((s["greetings.runOn"] as string | undefined) ?? "autostart") !== "every_show") return;
    if (cores().currentClient()?.connectionState !== "connected") return;
    const now = Date.now();
    if (now - lastGreetingAtMs < greetingCooldownMs()) return;
    lastGreetingAtMs = now;
    try {
      await runGreeting();
    } catch (e) {
      log.warn("greeting on show failed:", e);
    }
  }

  let unlistenVisibility: (() => void) | null = null;
  let unlistenMonitor: (() => void) | null = null;
  let unlistenHideRequested: (() => void) | null = null;
  let unlistenBack: (() => void) | null = null;
  let cleanupSystemTheme: (() => void) | null = null;

  // Autostart (login) launches stay hidden until something reveals the window:
  // a greeting completing, the core reporting no greeting ran, or this
  // last-resort watchdog firing when the core never connects at all. Module
  // scope so onDestroy can clear it.
  let connectWatchdog: ReturnType<typeof setTimeout> | null = null;
  const AUTOSTART_REVEAL_FALLBACK_MS = 10_000;

  // Redirect out of the two core-backed transient modes while the core is
  // unavailable: quick settings falls back to settings (which shows its own
  // disabled state) and the session list (which reads/loads sessions from the
  // core) falls back to chat, the safe resting mode.
  $effect(() => {
    if (!connectionState.coreUnavailable) return;
    if (viewState.mode === "quickSettings") viewState.navigate("settings");
    else if (viewState.mode === "sessionList") viewState.navigate("chat");
  });

  // The core rejected our bearer token (e.g. its DB was reset, so it no longer
  // knows this client). The stored token is permanently dead, so silently drop
  // the core and fall back to onboarding instead of stranding the user. Mirrors
  // the manual unpair flow in CoresField. dropUnauthorizedCore owns which cores
  // that even applies to, and guards re-entry: nothing is current once removed,
  // so the effect can't act twice.
  $effect(() => {
    if (!connectionState.unauthorized) return;
    void (async () => {
      try {
        if (!(await dropUnauthorizedCore())) return;
        const remaining = await cores().list();
        if (remaining.length === 0) viewState.setLocked(true);
        else if (!cores().currentEntry()) await cores().select(remaining[0].id);
      } catch (e) {
        log.error("auto-unpair after auth rejection failed:", e);
      }
    })();
  });

  // Boot counterpart of the auth-rejection effect above. restoreSelected can
  // leave no core active when a paired entry's token is gone from the keychain
  // (e.g. after a dev reset): select() throws "no token" before any connection,
  // so connectionState never reaches "unauthorized" and the effect never fires.
  // Drop those tokenless cores (dropTokenlessCores, which owns the rules about
  // which cores that question even applies to) and fall back to onboarding
  // instead of hanging in "Connecting to core...".
  async function dropTokenlessCoresAndMaybeOnboard(): Promise<void> {
    if (cores().currentEntry()) return; // a core selected fine; nothing to do
    await dropTokenlessCores();
    const remaining = await cores().list();
    if (remaining.length === 0) viewState.setLocked(true);
    else if (!cores().currentEntry()) {
      try {
        await cores().select(remaining[0].id);
      } catch (e) {
        log.error("select after tokenless cleanup failed:", e);
      }
    }
  }

  onMount(async () => {
    // Local critical path: do ONLY the local work needed to position + theme
    // the window, then show it. Everything that touches the core / network /
    // keychain is deferred to after the window is visible (the deferred phase
    // below), so a slow or unreachable core can never keep the window hidden.
    // Whether this was a login/autostart launch decides whether we show the
    // window now or stay hidden until a greeting (or the watchdog) reveals it.
    // It is a local Tauri call (reads launch args); default to a manual launch.
    let autostarted = false;
    try {
      autostarted = await platform()
        .autostart.wasAutostarted()
        .catch(() => false);
      await settingsState.loadClientSettings();
      // Decide the initial mode from LOCAL data only: whether a core is paired
      // is the client-settings `cores` list, with no select()/keychain/network.
      // A paired local core whose core was uninstalled is dropped in the deferred
      // phase (pruneUninstalledLocalCores); don't count it here, or the window
      // would flash into chat before landing on onboarding. isLocalCoreInstalled
      // is a fast on-disk check (no keychain/network), so it is allowed here; on
      // an error assume installed rather than force onboarding.
      const list = await cores().list();
      let paired = list.length > 0;
      if (paired && list.every((c) => isLoopbackUrl(c.baseUrl))) {
        paired = await platform()
          .pairing.isLocalCoreInstalled()
          .catch(() => true);
      }
      if (paired) {
        viewState.setImmediate("chat");
      } else {
        viewState.setImmediate("newCore");
        viewState.setLocked(true);
      }
      // Apply every appearance/layout DOM setting on the boot critical path so
      // the window paints correctly themed; the per-key $effects re-apply later.
      theme.applyAll(settingsState.currentSettings);
      // Only show the "Loading latest session…" placeholder when we're actually
      // about to load one: a core must be paired, and "always start new" mode
      // has nothing to load so the placeholder would mislead.
      sessionLoading = paired && !settingsState.currentSettings["general.session.alwaysStartNew"];
      await choreo.positionWindow();
    } catch (e) {
      // A local read should never keep the window hidden. Log and show anyway.
      log.error("local critical path failed:", e);
    } finally {
      loaded = true;
      await tick();
      if (autostarted) {
        // Autostart (login) launch: park the content offscreen (no transition,
        // window still hidden) but do NOT show it, so the app starts quietly in
        // the background and a later reveal slides in cleanly. The reveal comes
        // from a greeting finishing (session.created show_when_done), the
        // deferred phase seeing the core report no greeting ran, or the
        // connectWatchdog firing when the core never connects, so an autostarted
        // app can never stay invisible.
        choreo.applyWindowState("offscreen", false);
        connectWatchdog = setTimeout(() => {
          void platform().windowing.show();
        }, AUTOSTART_REVEAL_FALLBACK_MS);
      } else if (getDuration() > 0) {
        // Park the content offscreen (no transition), reveal the window while
        // it is still clear of the viewport, then slide it in after a settle
        // beat. `windowTransition` spans the whole reveal so an early shortcut
        // press can't fight the slide. show() stays awaited so its
        // `window-visibility: true` event fires before the listener below
        // registers; otherwise that listener would slide the content in early.
        windowTransition.begin();
        choreo.applyWindowState("offscreen", false);
        await platform().windowing.show();
        void choreo.revealAfterSettle();
      } else {
        // Animations off: show immediately with no slide.
        await platform().windowing.show();
        choreo.applyWindowState("visible", false);
      }
    }

    // Post-paint work. Fire-and-forget; the window is already visible.
    document.addEventListener("click", linkHandler);

    cleanupSystemTheme = theme.listenSystemTheme(() => {
      if (settingsState.currentSettings["appearance.theme"] === "auto") {
        theme.applyTheme("auto");
      }
    });

    if (contentEl && !onMobile) {
      void startClickThrough(contentEl);
    }

    platform()
      .windowing.subscribeVisibility((visible) => {
        if (visible) {
          choreo.applyWindowState("visible", true);
          resumeClickThrough();
          // Mirror the slide-in animation duration so spammed shortcut
          // presses can't reverse the in-progress show into a hide and
          // flicker.
          windowTransition.begin();
          setTimeout(() => windowTransition.end(), getDuration());
          // Desktop only: greet on this reveal when "Every Show" is set. Mobile's
          // visibility maps to app foreground/background, which should not greet.
          if (!onMobile) void maybeGreetOnShow();
        } else {
          pauseClickThrough();
        }
      })
      .then((unlisten) => {
        unlistenVisibility = unlisten;
      });

    platform()
      .windowing.subscribeHideRequested(() => {
        void choreo.animateHideThenHide();
      })
      .then((unlisten) => {
        unlistenHideRequested = unlisten;
      });

    platform()
      .windowing.subscribeMonitorChanged(() => {
        if (loaded) choreo.positionWindow();
      })
      .then((unlisten) => {
        unlistenMonitor = unlisten;
      });

    // Mobile back: feed every press to the back-handler registry
    // (state/back.svelte.ts), which resolves the priority chain (overlay ->
    // wizard -> non-chat mode -> chat-root double-back-to-exit). The source is
    // the Android hardware/gesture back or an iOS left-edge swipe; inert on
    // desktop (the stream never fires).
    platform()
      .backButton.subscribe(() => backState.back())
      .then((unlisten) => {
        unlistenBack = unlisten;
      });

    // Global-shortcut listener lives here (not in UserInput) so it stays
    // attached when the user is in the Settings view; otherwise the
    // shortcut silently stops working whenever UserInput is unmounted.
    void shortcutHandler.attach();

    // Probe disk for every HF file the current configuration references,
    // then decide whether to start sidecars / TTS or wait. Nothing
    // downloads until the user explicitly confirms via the Settings
    // ConfirmModal: if anything is missing we stash the pending list
    // and leave sidecar startup to the modal's onConfirm handler. If
    // everything is already on disk we kick sidecars off immediately
    // (their `ensure()` calls hit the file-exists fast path with no
    // network I/O).
    setupSidecarListeners();
    // Deferred core phase: the window is already visible. Connect to the core,
    // merge its (non-visual) settings, and wire the WS-driven stores. Each
    // essential step logs on failure so a developer sees it in the dev console;
    // the window is up regardless.
    void (async () => {
      log.info("deferred boot: connecting to core");
      // Drop paired local cores whose core was uninstalled (the user removed both
      // parts then reinstalled just the Client, or removed the Core on its own)
      // before selecting, so restoreSelected never picks a core that is gone and
      // hangs on "Connecting to core...". Keys off the on-disk core binary, so a
      // running update never trips it. Falls through to onboarding or a remaining
      // remote core via the restore + tokenless-cleanup below. Guarded like the
      // steps below so a failure here can't abort the rest of the deferred boot.
      try {
        await pruneUninstalledLocalCores();
      } catch (e) {
        log.error("local-core prune failed:", e);
      }
      try {
        await cores().restoreSelected();
      } catch (e) {
        log.error("core restore failed:", e);
      }
      // If nothing got selected because a paired core's token is gone, unpair it
      // and return to onboarding rather than hanging in "Connecting to core...".
      await dropTokenlessCoresAndMaybeOnboard();
      // Reload the active core's sessions on every later core switch. Registered
      // after restoreSelected so boot's own select() doesn't double-trigger it.
      // The registry notifies on rename / unpair too, so detach the in-flight
      // turn ONLY on an actual current-core change: a deliberate swap closes the
      // old socket without emitting "disconnected", so the streaming disconnect
      // handler never fires. Detaching here (before the reload) keeps the old
      // core's streamId adoptable on return instead of letting load()'s
      // interruptStreaming abandon it and cross-interrupt the new core.
      let lastSelectedCoreId = cores().currentEntry()?.id ?? null;
      cores().subscribe(() => {
        const id = cores().currentEntry()?.id ?? null;
        if (id !== lastSelectedCoreId) {
          lastSelectedCoreId = id;
          streamingState.detachForResume();
        }
        void sessionsState.loadLatest();
      });
      // On-demand install: spawn the loopback core if it isn't up yet.
      await ensureLocalCoreUpIfNeeded();
      // Wire every WS-driven store to the core socket. These register listeners
      // synchronously and persist across core switches (cores().subscribeWs
      // re-binds on select()), so one attach is enough even before a core is
      // selected. This also kicks off the WS connect, whose failures now surface
      // in the reconnect banner. settingsState.attach() owns the core settings
      // baseline + live sync from here on: it loads on selection changes and
      // connected edges, and side effects (TTS arming etc.) fire through
      // settings-effects on every merged transition, so boot needs no explicit
      // core-settings fetch or per-module kick.
      settingsState.attach();
      extensionsState.attach();
      mcpState.attach();
      memoriesState.attach();
      scheduledPromptsState.attach();
      streamingState.attach();
      serversState.attach();
      sessionsState.attach();
      downloadsState.attach();
      updateState.attach();
      // Run a greeting once a core is reachable (once per app run). The
      // greetings.* settings are client-local, so this gates on them here and
      // asks the core to open a session only when one should run.
      let greetingReported = false;
      let unsubGreeting: (() => void) | null = null;
      unsubGreeting = cores().subscribeConnectionState((state) => {
        if (state !== "connected" || greetingReported) return;
        greetingReported = true;
        // One-shot: later connected edges must not re-trigger, so drop the
        // subscription instead of leaking it for the app's lifetime. (Null
        // only if this fires synchronously during subscribe; the flag above
        // already guards re-entry then.)
        unsubGreeting?.();
        // Connected: the no-connection watchdog is moot now.
        if (connectWatchdog !== null) {
          clearTimeout(connectWatchdog);
          connectWatchdog = null;
        }
        void (async () => {
          const s = settingsState.currentSettings;
          // Mobile has no login autostart, but every mobile launch should greet
          // (the runOn choice is hidden there), so it counts as an automatic
          // start.
          const launchedAutomatically = autostarted || onMobile;
          const enabled = s["greetings.enabled"] === true;
          const runOn = (s["greetings.runOn"] as string | undefined) ?? "autostart";
          // Gate locally: off, or "automatic start only" on a manual launch,
          // means no greeting. Reveal the window ourselves in that case.
          const shouldRun = enabled && !(runOn === "autostart" && !launchedAutomatically);
          if (!shouldRun) {
            if (autostarted) await platform().windowing.show();
            return;
          }
          // Stamp the debounce now so the greeting's own show_when_done reveal
          // (which fires the visibility listener) doesn't re-trigger a greeting.
          lastGreetingAtMs = Date.now();
          // The route answers immediately (the greeting itself starts in the
          // background once the model is loaded), so a slow response means
          // something is wrong; don't let an autostart window stay hidden
          // behind a hung request.
          const res = await Promise.race([
            runGreeting(),
            new Promise<never>((_resolve, reject) =>
              setTimeout(() => reject(new Error("greeting trigger timed out")), 10_000),
            ),
          ]);
          // Safety net for an autostart launch that stayed hidden: if the core
          // ran no greeting (e.g. the per-client dedup suppressed it), reveal
          // the window now. When a greeting DID run, its session reveals the
          // window on completion via the show_when_done focus path.
          if (autostarted && !res.ran) await platform().windowing.show();
        })().catch((e) => {
          log.warn("greeting trigger failed:", e);
          // On failure during an autostart launch, reveal rather than strand.
          if (autostarted) void platform().windowing.show();
        });
      });
      // Pull the required-files snapshot now that a core may be selected, and
      // re-pull on later core switches (downloadsState also keeps it live via
      // the requirements.snapshot WS frame).
      void downloadsState.refetchRequirements();
      cores().subscribe(() => void downloadsState.refetchRequirements());
      void snippetsState.load();
      // Initial session load, with the "Loading latest session…" placeholder.
      try {
        if (cores().currentEntry()) {
          if (settingsState.currentSettings["general.session.alwaysStartNew"]) {
            await sessionsState.loadList();
          } else {
            await sessionsState.loadLatest();
          }
        }
      } catch (e) {
        log.error("session load failed:", e);
      } finally {
        sessionLoading = false;
        // Enable per-message entry animations only AFTER the bulk restore
        // completes; otherwise 50 messages all animate in at once.
        await tick();
        enableMessageAnimations();
      }
    })();
  });

  onDestroy(() => {
    document.removeEventListener("click", linkHandler);
    stopClickThrough();
    stopBlurKeepalive();
    shortcutHandler.detach();
    unlistenVisibility?.();
    unlistenMonitor?.();
    unlistenHideRequested?.();
    unlistenBack?.();
    cleanupSystemTheme?.();
    if (connectWatchdog !== null) clearTimeout(connectWatchdog);
  });

  // Drive the panel slide whenever a navigation is requested. viewState holds
  // `pendingMode` (requested) and `mode` (committed/rendered); this effect runs
  // the 3-phase slide (out → commit (offscreen) → in) toward pendingMode.
  $effect(() => {
    const target = viewState.pendingMode;
    if (target === viewState.mode) return; // settled, nothing to slide
    if (choreo.sliding) return; // a slide is already mid-flight
    void choreo.runSlide();
  });

  $effect(() => {
    const _align = settingsState.getAlignment();
    const _mon = settingsState.getMonitor();
    const _width = settingsState.currentSettings["layout.width"];

    if (loaded) {
      choreo.positionWindow();
    }
  });

  // Appearance settings applied to the document: one idempotent applier per
  // setting (set a CSS var / class / font), run together whenever any of them
  // changes. They are all order-independent idempotent DOM writes, so a single
  // coalesced effect re-applying the whole set on any change is equivalent to
  // the former one-effect-per-key fan-out, with far less noise. (Window
  // positioning above stays its own effect: repositioning the OS window is NOT
  // idempotent.) bubbleBlurEnabled / bubbleBlurRings drive the halo layer count
  // directly in Bubble.svelte (reactive read of settingsState), so no apply here.
  const appearanceAppliers: ReadonlyArray<() => void> = [
    () => {
      const mode = settingsState.currentSettings["appearance.theme"];
      if (mode) theme.applyTheme(mode);
    },
    () => {
      const size = settingsState.currentSettings["appearance.textSize"];
      if (size) theme.applyTextSize(size as number);
    },
    () =>
      theme.setThemeColor(
        "--user-bubble-bg-light",
        "--user-bubble-bg-dark",
        settingsState.currentSettings["appearance.userBubbleColor"],
      ),
    () =>
      theme.setThemeColor(
        "--agent-bubble-bg-light",
        "--agent-bubble-bg-dark",
        settingsState.currentSettings["appearance.agentBubbleColor"],
      ),
    () =>
      theme.setThemeColor(
        "--agent2-bubble-bg-light",
        "--agent2-bubble-bg-dark",
        settingsState.currentSettings["appearance.secondaryAgentBubbleColor"],
      ),
    () =>
      theme.applyBubbleColor(
        "--default-base",
        settingsState.currentSettings["appearance.defaultColor"],
      ),
    () =>
      theme.applyBubbleColor(
        "--accent-red-base",
        settingsState.currentSettings["appearance.accentRed"],
      ),
    () =>
      theme.applyBubbleColor(
        "--accent-blue-base",
        settingsState.currentSettings["appearance.accentBlue"],
      ),
    () =>
      theme.applyBubbleColor(
        "--accent-purple-base",
        settingsState.currentSettings["appearance.accentPurple"],
      ),
    () =>
      theme.applyBubbleColor(
        "--accent-green-base",
        settingsState.currentSettings["appearance.accentGreen"],
      ),
    () =>
      theme.applyBubbleColor(
        "--accent-yellow-base",
        settingsState.currentSettings["appearance.accentYellow"],
      ),
    () =>
      theme.applyCssVarPx(
        "--rounded-small",
        settingsState.currentSettings["appearance.roundedSmall"] as number,
      ),
    () =>
      theme.applyCssVarPx(
        "--rounded-medium",
        settingsState.currentSettings["appearance.roundedMedium"] as number,
      ),
    () =>
      theme.applyCssVarPx(
        "--rounded-large",
        settingsState.currentSettings["appearance.roundedLarge"] as number,
      ),
    () =>
      theme.applyFont("--font-default", settingsState.currentSettings["appearance.defaultFont"]),
    () => theme.applyFont("--font-mono", settingsState.currentSettings["appearance.monoFont"]),
    () =>
      theme.setThemeColor(
        "--bubble-shadow-color-light",
        "--bubble-shadow-color-dark",
        settingsState.currentSettings["appearance.bubbleShadowColor"],
      ),
    () =>
      theme.applyCssVarPx(
        "--bubble-shadow-distance",
        settingsState.currentSettings["appearance.bubbleShadowDistance"] as number,
      ),
  ];
  $effect(() => {
    if (!loaded) return;
    for (const apply of appearanceAppliers) apply();
  });
</script>

{#if loaded}
  <!-- Static surface backdrop behind the sliding panel layer: during the mobile
       carousel the layer is partly off-screen, so this keeps the uncovered edge
       reading as app surface instead of flashing the page background. -->
  {#if onMobile}
    <div class="fixed inset-0 bg-surface -z-10" aria-hidden="true"></div>
  {/if}
  <div
    bind:this={choreo.panelLayer}
    class="w-screen h-screen overflow-hidden"
    class:will-change-transform={animationsEnabled}
  >
    {#if !onMobile}
      <main
        bind:this={choreo.container}
        class="no-scrollbar flex flex-col-reverse justify-start p-10 text-default-800 w-fit max-w-screen min-h-screen max-h-screen overflow-x-clip overflow-y-auto"
        class:mx-auto={settingsState.getAlignment() === "center"}
        class:ml-auto={settingsState.getAlignment() === "right"}
        class:mr-auto={settingsState.getAlignment() === "left"}
        class:will-change-transform={animationsEnabled}
        style="opacity: 0"
      >
        <div bind:this={contentEl} class="flex flex-col-reverse gap-2 my-auto">
          {#if viewState.mode === "chat"}
            <MessageTranscript {sessionLoading} />
          {:else}
            <div
              class="w-fit flex flex-col pointer-events-none"
              style:gap={bubbleGap(ui)}
              class:ml-auto={settingsState.getAlignment() === "right"}
              class:mr-auto={settingsState.getAlignment() === "left"}
              class:mx-auto={settingsState.getAlignment() === "center"}
            >
              {@render panelColumn()}
            </div>
          {/if}
        </div>
      </main>
    {/if}

    {#if onMobile}
      <!-- Mobile: a single fullscreen activity. Chat becomes a top app bar
         (session + core), a flex-1 scrolling transcript (still flex-col-reverse
         so the newest row stays anchored at the bottom), and the input pinned
         above the on-screen keyboard. Other modes render their panel full
         screen. The desktop bubble/window machinery is gated off (see onMobile
         in the script). -->
      <!-- The frame fills the whole edge-to-edge window (h-screen, stable: the
         keyboard is handled by padding, not by resizing the viewport) and is the
         single safe-area boundary for every in-flow screen. pt clears EXACTLY the
         status-bar/notch inset (no extra), so each screen's own padding is the gap
         above its first element: the top distance reads as status-bar height +
         that screen's padding, matching the left/right padding instead of adding a
         second, device-independent strip on top. pb clears whichever is taller,
         the gesture/home bar or the soft keyboard, so the chat composer (and any
         focused field in settings or the pairing wizard) rides above the keyboard
         instead of being covered. The inset values are injected natively as CSS
         variables (see MainActivity) because Android WebView reports env(safe-
         area-*) as 0 and visualViewport never sees the keyboard. Individual
         screens must NOT re-apply these (that would double-pad); fixed overlays
         (Modal sheet, autocomplete) sit outside this frame and lift themselves by
         --keyboard-inset. -->
      <!-- The committed frame. It is the single mount of the current screen, and
         the element the carousel pages IN (runSlide translates mobileFrameEl). -->
      <div
        bind:this={choreo.mobileFrameEl}
        class="flex flex-col h-screen w-screen pt-[var(--safe-area-inset-top,0px)] pb-[max(var(--safe-area-inset-bottom,0px),var(--keyboard-inset,0px))] text-default-800 bg-surface"
      >
        {@render mobileScreen(viewState.mode)}
      </div>
      <!-- Exit overlay: a transient render of the OUTGOING screen, paged OUT the
         opposite edge as the frame pages in, so the change reads as one
         cross-slide. Mounted only mid-slide; `fixed` over the frame, clipped to
         the screen by the panel layer's overflow. -->
      {#if choreo.slideOutMode}
        <div
          bind:this={choreo.mobileExitEl}
          class="fixed inset-0 z-40 flex flex-col h-screen w-screen pt-[calc(var(--safe-area-inset-top,0px)+0.5rem)] pb-[max(var(--safe-area-inset-bottom,0px),var(--keyboard-inset,0px))] text-default-800 bg-surface"
        >
          {@render mobileScreen(choreo.slideOutMode)}
        </div>
      {/if}
      <!-- Hosts the in-app action sheet that backs platform().menu.showContextMenu
         on touch (long-press a message / session). Rendered once, above all
         mobile modes. -->
      <ActionSheetHost />
      <!-- Quick Settings presents as a draggable bottom sheet over the chat; it
         self-gates on viewState.mode so it slides in and out on its own. -->
      <QuickSettings />
      {#if backState.exitHint}
        <!-- A back press at the chat root arms a brief window; this hint says a
           second press leaves the app. Sits above the gesture/home bar. -->
        <div
          class="fixed inset-x-0 bottom-[max(var(--safe-area-inset-bottom,0px),1rem)] flex justify-center pointer-events-none z-50"
          transition:fade={{ duration: getDuration() }}
        >
          <div
            class="bg-surface-inset-strong text-default-800 text-sm rounded-large px-4 py-2 shadow"
          >
            Press back again to exit
          </div>
        </div>
      {/if}
    {/if}
  </div>
{/if}

{#snippet mobileScreen(mode: AppMode)}
  {#if mode === "chat" || mode === "quickSettings"}
    <!-- Chat stays mounted under the Quick Settings bottom sheet, so the sheet
         rises over a live chat instead of swapping to a full panel. -->
    <MessageTranscript {sessionLoading} />
  {:else if mode === "settings"}
    <!-- Full-screen stacked settings: it fills the frame and owns its own
         internal scroll, so it gets no outer padding/scroll wrapper. -->
    <Settings />
  {:else if mode === "newCore"}
    <!-- The wizard owns its own scroll + pinned footer; the frame just gives it
         height (padding lives inside the view per the safe-area contract above). -->
    <div class="flex-1 min-h-0 flex flex-col">
      <NewCore />
    </div>
  {:else}
    <!-- Session list: a scrollable column of session bubbles. -->
    <div class="flex-1 min-h-0 overflow-y-auto no-scrollbar flex flex-col p-3">
      <SessionList />
    </div>
  {/if}
{/snippet}

{#snippet panelColumn()}
  {#if viewState.mode === "newCore"}
    <NewCore />
  {:else if viewState.mode === "quickSettings"}
    <QuickSettings />
  {:else if viewState.mode === "sessionList"}
    <SessionList />
  {:else}
    <Settings flatCorners={settingsPanelFlat} />
  {/if}
  <!-- CoreBar pins below the session-list / settings panel (which core
               you're on, its status, quick switch). Hidden in newCore (no core
               yet) and quickSettings (transient overlay). In settings it merges
               flush onto the panel's bottom; in the session list it floats. -->
  {#if viewState.mode === "sessionList" || viewState.mode === "settings"}
    <div class="relative pointer-events-none">
      <CoreBar merge={settingsCoreMerge} />
    </div>
  {/if}
{/snippet}

<style lang="scss">
  :global(.no-scrollbar::-webkit-scrollbar) {
    display: none;
  }
  :global(.no-scrollbar) {
    -ms-overflow-style: none;
    scrollbar-width: none;
  }
  :global(::selection) {
    background-color: rgba(0, 0, 0, 0.5);
    color: white;
  }
</style>
