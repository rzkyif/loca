<script lang="ts">
  import { type Snippet, untrack } from "svelte";
  import MessageEnter from "@loca/shared/ui/components/chat/MessageEnter.svelte";
  import { claimMessageEnter } from "$lib/appearance/animations";
  import type { Alignment } from "$lib/util/types";

  let {
    alignment,
    msgId,
    delayMs = 0,
    centerDirection = "up",
    class: className = "",
    children,
  }: {
    alignment: Alignment;
    msgId?: string;
    /** Hold the entry animation for this long; see the shared MessageEnter. */
    delayMs?: number;
    /** Center-alignment entry axis; see the shared MessageEnter. */
    centerDirection?: "up" | "down";
    class?: string;
    children: Snippet;
  } = $props();

  // Claim once at mount: records the id and reports whether this mount animates
  // (suppressed during session restore, never replayed for a seen message).
  // msgId is fixed per mount, so the one-time read is intentional.
  const enabled = untrack(() => claimMessageEnter(msgId));
</script>

<MessageEnter {alignment} {delayMs} {enabled} {centerDirection} class={className}>
  {@render children()}
</MessageEnter>
