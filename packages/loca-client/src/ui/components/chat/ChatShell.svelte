<script lang="ts">
  import type { Snippet } from "svelte";
  import ChatShellView from "@loca/shared/ui/components/chat/ChatShellView.svelte";
  import CoreBar from "./CoreBar.svelte";
  import SessionBar from "./SessionBar.svelte";
  import UserInput from "./UserInput.svelte";
  import { useUiContext } from "@loca/shared/ui/context";
  import { type BubbleMerge, mergeFlatCorners, NO_MERGE } from "@loca/shared/ui/merge";

  // Feeds the live chat chrome (core status, session bar, composer) into the
  // shared ChatShellView and forwards the route-owned transcript. The shell
  // itself owns the desktop/mobile arrangement; this wrapper only supplies the
  // regions, so the layout stays single-source.
  let {
    stackDepth = 0,
    transcript,
  }: {
    stackDepth?: number;
    transcript: Snippet;
  } = $props();

  const ui = useUiContext();
  const mobile = $derived(ui.platform === "mobile");

  // The CoreBar merges with the SessionBar when both are visible. On desktop the
  // CoreBar sits below the SessionBar (it is the lower bubble, so it overlaps
  // up); on mobile the top app bar stacks them the other way, so the SessionBar
  // is the lower one. The narrower bubble squares off the seam corners; widths
  // are reported by each bar's Bubble, and a hidden SessionBar reports 0 so the
  // CoreBar floats alone again.
  let coreWidth = $state(0);
  let sessionWidth = $state(0);
  const active = $derived(coreWidth > 0 && sessionWidth > 0);
  const coreNarrow = $derived(coreWidth <= sessionWidth);
  const sessionNarrow = $derived(sessionWidth <= coreWidth);

  const coreMerge = $derived.by<BubbleMerge>(() => {
    if (!active) return NO_MERGE;
    const align = ui.getAlignment();
    return mobile
      ? { flatCorners: mergeFlatCorners(align, "bottom", coreNarrow), overlapTop: false }
      : { flatCorners: mergeFlatCorners(align, "top", coreNarrow), overlapTop: true };
  });
  const sessionMerge = $derived.by<BubbleMerge>(() => {
    if (!active) return NO_MERGE;
    const align = ui.getAlignment();
    return mobile
      ? { flatCorners: mergeFlatCorners(align, "top", sessionNarrow), overlapTop: true }
      : { flatCorners: mergeFlatCorners(align, "bottom", sessionNarrow), overlapTop: false };
  });
</script>

<ChatShellView {stackDepth} {coreBar} {sessionBar} {input} {transcript} />

{#snippet coreBar()}
  <CoreBar merge={coreMerge} onWidth={(w) => (coreWidth = w)} />
{/snippet}

{#snippet sessionBar(zIndex: number)}
  <SessionBar {zIndex} merge={sessionMerge} onWidth={(w) => (sessionWidth = w)} />
{/snippet}

{#snippet input()}
  <UserInput />
{/snippet}
