<script lang="ts">
  import { onDestroy, type Snippet } from "svelte";
  import Expandable from "../../primitives/Expandable.svelte";
  import { useUiContext } from "../../../context.ts";

  // The reasoning disclosure inside an agent reasoning bubble: a live
  // "Thinking for Xs..." / "Thought for X" header over an expandable pill that
  // holds the reasoning body (markdown in the client, plain text on the site).
  // Shared so both render identically; only the body is supplied by the caller.
  const ui = useUiContext();
  // Mobile chat bubbles always sit on the agent side (left).
  const align = $derived(ui.platform === "mobile" ? "left" : ui.getAlignment());

  let {
    isStreaming,
    pillBgClass = "bg-surface-inset",
    reasoningDurationMs,
    expanded = $bindable(false),
    body,
  }: {
    /** True while reasoning chunks are still arriving (the elapsed timer runs);
     *  cleared once content starts or the stream ends (timer freezes). */
    isStreaming: boolean;
    pillBgClass?: string;
    /** Persisted duration for historic messages that can't be live-tracked. */
    reasoningDurationMs?: number;
    /** Bindable so the parent can mirror it into a shared expansion map. */
    expanded?: boolean;
    /** The reasoning content (e.g. rendered markdown). */
    body: Snippet;
  } = $props();

  const overrideHex = $derived(ui.systemMessageDefaultColor);
  let receivingReasoning = $derived(isStreaming);

  let startTime = $state<number | null>(null);
  let nowTime = $state(Date.now());
  let finalMs = $state<number | null>(null);
  let tickTimer: ReturnType<typeof setTimeout> | null = null;

  function scheduleNextTick() {
    const elapsed = startTime !== null ? Date.now() - startTime : 0;
    const intervalMs = elapsed < 1000 ? 100 : 1000;
    tickTimer = setTimeout(() => {
      nowTime = Date.now();
      scheduleNextTick();
    }, intervalMs);
  }

  $effect(() => {
    if (receivingReasoning) {
      if (startTime === null) startTime = Date.now();
      finalMs = null;
      nowTime = Date.now();
      if (!tickTimer) scheduleNextTick();
    } else {
      if (tickTimer) {
        clearTimeout(tickTimer);
        tickTimer = null;
      }
      if (startTime !== null && finalMs === null) {
        finalMs = Date.now() - startTime;
      }
    }
  });

  onDestroy(() => {
    if (tickTimer) clearTimeout(tickTimer);
  });

  function formatElapsed(ms: number): string {
    const safeMs = Math.max(0, ms);
    if (safeMs < 1000) return `0.${Math.floor(safeMs / 100)}s`;
    const totalSec = Math.floor(safeMs / 1000);
    const hours = Math.floor(totalSec / 3600);
    const minutes = Math.floor((totalSec % 3600) / 60);
    const seconds = totalSec % 60;
    // Show every unit from the largest non-zero one down to seconds.
    const parts: string[] = [];
    if (hours > 0) parts.push(`${hours}h`);
    if (hours > 0 || minutes > 0) parts.push(`${minutes}m`);
    parts.push(`${seconds}s`);
    return parts.join(" ");
  }

  let headerText = $derived.by(() => {
    if (receivingReasoning && startTime !== null) {
      return `Thinking for ${formatElapsed(nowTime - startTime)}...`;
    }
    const persistedMs = finalMs ?? reasoningDurationMs;
    if (persistedMs !== undefined) return `Thought for ${formatElapsed(persistedMs)}`;
    return "Reasoning";
  });
</script>

<div style:display="contents" style:--default-base={overrideHex}>
  <Expandable bind:expanded alignment={align}>
    {#snippet title()}
      <span>{headerText}</span>
    {/snippet}
    {#snippet children()}
      <!-- `text-left` keeps the reasoning body alignment-independent; the
           Expandable would otherwise right-align it in a right-aligned bubble. -->
      <div class="{pillBgClass} px-4 py-2 rounded-large text-default-700 text-xs text-left">
        {@render body()}
      </div>
    {/snippet}
  </Expandable>
</div>
