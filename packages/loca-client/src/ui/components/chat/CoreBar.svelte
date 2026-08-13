<script lang="ts">
  import { onMount } from "svelte";
  import CoreBarView from "@loca/shared/ui/components/chat/CoreBarView.svelte";
  import type { DisplayCoreStatus } from "@loca/shared/ui/components/chat/CoreBarView.svelte";
  import MessageEnter from "./MessageEnter.svelte";
  import { useUiContext } from "@loca/shared/ui/context";
  import { coreStatusState, settingsState, viewState } from "../../state";
  import { connectionState } from "$stores/connection.svelte";
  import { localCoreState } from "$stores/local-core.svelte";
  import { cores } from "$lib/core";
  import { hasAlpha } from "$lib/appearance/color";
  import { isExpanded, expansionState } from "$stores/expansion.svelte";
  import { type BubbleMerge, NO_MERGE } from "@loca/shared/ui/merge";

  // Quick core switcher + connected-core status. Merges the backend CoreStatus
  // (coreStatusState, via core.status frames) with the client transport state
  // (connectionState): transport wins whenever we're not connected, since a
  // transport state can never ride the wire. Full core management (pair / unpair
  // / rename) stays in Settings; the gear jumps there.

  // Merge with the bubble above (the SessionBar in chat, the Settings panel in
  // the settings view) is decided by the caller; floats alone by default.
  let {
    merge = NO_MERGE,
    onWidth = undefined,
  }: {
    merge?: BubbleMerge;
    onWidth?: (width: number) => void;
  } = $props();

  const themeOverride = $derived(
    settingsState.currentSettings["appearance.sessionBarDefaultColor"] as string,
  );
  const themeOverrideHex = $derived(hasAlpha(themeOverride) ? themeOverride : null);

  let coreList = $state<{ id: string; name: string }[]>([]);
  let currentCoreId = $state("");
  async function refreshCores(): Promise<void> {
    try {
      coreList = (await cores().list()).map((c) => ({ id: c.id, name: c.name }));
      currentCoreId = cores().currentEntry()?.id ?? "";
    } catch {
      /* registry not readable yet */
    }
  }
  onMount(() => {
    void refreshCores();
    const unsub = cores().subscribe(() => void refreshCores());
    return () => unsub();
  });

  // Merge transport + backend status into the single value the View paints. The
  // settled failures come first: each names a different fix, and the ordering
  // puts the most specific one (we know the core is not even running) on top.
  const status = $derived.by((): DisplayCoreStatus => {
    if (connectionState.unauthorized) return "unauthorized";
    if (connectionState.stopped) return "stopped";
    if (connectionState.gaveUp) return "disconnected";
    if (connectionState.state !== "connected") {
      return connectionState.reconnecting ? "reconnecting" : "connecting";
    }
    return coreStatusState.snapshot.status;
  });
  const connected = $derived(
    connectionState.state === "connected" && !connectionState.unauthorized,
  );
  const detail = $derived(
    connected ? coreStatusState.snapshot.detail : (connectionState.reason ?? undefined),
  );
  const progress = $derived(connected ? coreStatusState.snapshot.progress : undefined);
  // The per-subsystem / queue detail only exists while connected (it rides the
  // backend snapshot); a transport state has no card.
  const subsystems = $derived(connected ? coreStatusState.snapshot.subsystems : []);
  const queues = $derived(connected ? coreStatusState.snapshot.queues : undefined);

  // Clicking a settled-failure pill is the way out of it: start this device's
  // core when it is stopped, otherwise resume the parked reconnect loop. The
  // tray's Start Core item runs the same localCoreState.start().
  function handleStatusAction(): void {
    if (connectionState.stopped) {
      void localCoreState.start();
      return;
    }
    connectionState.retryFromUi();
    cores().currentClient()?.retryNow();
  }

  // Expansion lives in the shared registry (one id, so it survives the
  // panel-slide remounts and stays in sync across the chat / settings views).
  const CORE_BAR_EXPANSION_ID = "core-bar";
  const expanded = $derived(isExpanded(CORE_BAR_EXPANSION_ID, false));

  // Mobile slides the whole chat screen in via the panel carousel, so the bar
  // rides that motion; a separate per-bar entry slide here would double up and
  // (because the bar mounts mid-carousel) read as a pop. Desktop keeps it.
  const onMobile = useUiContext().platform === "mobile";
</script>

{#snippet bar()}
  <CoreBarView
    {status}
    {detail}
    {progress}
    {subsystems}
    {queues}
    {expanded}
    onToggleExpand={() => expansionState.set(CORE_BAR_EXPANSION_ID, !expanded)}
    onStatusAction={handleStatusAction}
    cores={coreList}
    {currentCoreId}
    onSwitch={(id) => {
      if (id !== currentCoreId) void cores().select(id);
    }}
    onSettings={() => viewState.navigate("settings")}
    baseColorOverride={themeOverrideHex}
    {merge}
    {onWidth}
  />
{/snippet}

<!-- Entry animation mirrors the SessionBar: centered, the bar slides down from
     above on mount (no msgId, so it animates on every entry; the session-restore
     gate keeps a restored bar from animating on load). Off-center it just
     appears, with no entry motion. -->
{#if !onMobile && settingsState.getAlignment() === "center"}
  <MessageEnter alignment="center" centerDirection="down">
    {@render bar()}
  </MessageEnter>
{:else}
  {@render bar()}
{/if}
