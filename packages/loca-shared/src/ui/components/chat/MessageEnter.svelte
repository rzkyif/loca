<script lang="ts">
  import { onMount, type Snippet } from "svelte";
  import { BASE_MS, runMessageEnter } from "../../animations.ts";
  import { useUiContext } from "../../context.ts";
  import type { Alignment } from "../../types.ts";

  // Animated mount wrapper for a chat bubble: on mount it grows max-height while
  // sliding the child in from the alignment edge (see `runMessageEnter`). Shared
  // so the client transcript and the website showcase animate identically; the
  // duration comes from the UI context (settings-aware in the client, BASE_MS on
  // the website, 0 under reduced motion). The host decides WHETHER a given mount
  // animates via `enabled`: the client gates it (no replay for a seen message,
  // suppressed during the session-restore burst); the website gates it off while
  // it measures content height.
  const ui = useUiContext();

  let {
    /** Edge to slide in from. Defaults to the context alignment so a session
     *  bar or bubble matches the app without the host threading it through. */
    alignment,
    /** Hold the bubble offscreen this long before the motion starts (stagger). */
    delayMs = 0,
    /** When false the child renders in place with no entry motion. */
    enabled = true,
    /** Center-alignment entry axis: "up" enters from below (default), "down"
     *  enters from above. No effect when the alignment is left/right. */
    centerDirection = "up",
    class: className = "",
    children,
  }: {
    alignment?: Alignment;
    delayMs?: number;
    enabled?: boolean;
    centerDirection?: "up" | "down";
    class?: string;
    children: Snippet;
  } = $props();

  let el: HTMLElement | undefined = $state();

  onMount(() => {
    if (!enabled || !el) return;
    runMessageEnter(
      el,
      alignment ?? ui.getAlignment(),
      ui.animationDurationMs(BASE_MS),
      delayMs,
      centerDirection,
    );
  });
</script>

<!-- No permanent will-change here: a persistent will-change-transform makes
     every row its own stacking context, which lets one bubble's drop shadow
     paint over neighbours. runMessageEnter sets the hint inline for the entry
     animation's duration only. -->
<div bind:this={el} class={className}>
  {@render children()}
</div>
