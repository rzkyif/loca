<script lang="ts">
  import type { Snippet } from "svelte";
  import Bubble from "../../primitives/Bubble.svelte";
  import ReasoningTraceView from "./ReasoningTraceView.svelte";
  import { useUiContext } from "../../../context.ts";

  // Presentational agent message. `kind: "reasoning"` renders a small
  // `bg-surface` bubble wrapping the reasoning disclosure; `kind: "content"`
  // renders the answer bubble (`bubble-agent`/`-secondary`). The markdown body
  // is supplied by the caller (client: MessageMarkdown; website: plain prose),
  // so this stays free of the heavy markdown pipeline. Alignment via context.
  const ui = useUiContext();
  // On mobile, agent bubbles always sit on the left (a conventional chat app),
  // regardless of the desktop window-alignment setting; desktop follows it.
  const align = $derived(ui.platform === "mobile" ? "left" : ui.getAlignment());

  let {
    kind,
    bgClass = "bubble-agent",
    isStreaming = false,
    reasoningDurationMs,
    reasoningExpanded = $bindable(false),
    active = false,
    pulse = false,
    neighborLeft = false,
    neighborRight = false,
    oncontextmenu,
    onlongpress,
    body,
  }: {
    kind: "reasoning" | "content";
    /** Content bubble background (`bubble-agent` or `bubble-agent-secondary`). */
    bgClass?: string;
    isStreaming?: boolean;
    reasoningDurationMs?: number;
    reasoningExpanded?: boolean;
    /** Content bubble active border + pulse (the client drives these from TTS). */
    active?: boolean;
    pulse?: boolean;
    neighborLeft?: boolean;
    neighborRight?: boolean;
    oncontextmenu?: (e: MouseEvent) => void;
    /** Touch long-press (mobile stand-in for the right-click context menu). */
    onlongpress?: () => void;
    /** The message body: reasoning content or the answer markdown. */
    body: Snippet;
  } = $props();
</script>

{#if kind === "reasoning"}
  <Bubble
    selectedAlignment={align}
    bgClass="bg-surface"
    extraClass="markdown overflow-clip"
    size="small"
    {neighborLeft}
    {neighborRight}
    {oncontextmenu}
    {onlongpress}
  >
    <ReasoningTraceView
      {isStreaming}
      {reasoningDurationMs}
      bind:expanded={reasoningExpanded}
      pillBgClass="bg-surface-inset"
      {body}
    />
  </Bubble>
{:else}
  <Bubble
    selectedAlignment={align}
    {bgClass}
    extraClass="markdown overflow-clip flex flex-col gap-3"
    {active}
    {pulse}
    {oncontextmenu}
    {onlongpress}
  >
    {@render body()}
  </Bubble>
{/if}
