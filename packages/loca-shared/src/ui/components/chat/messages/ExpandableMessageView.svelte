<script lang="ts">
  import { type Snippet, untrack } from "svelte";
  import Bubble from "../../primitives/Bubble.svelte";
  import Expandable from "../../primitives/Expandable.svelte";
  import { useUiContext } from "../../../context.ts";

  // The collapsed-bubble shell shared by the small core-authored messages
  // (system prompt, automated prompt, tool `display` content): a small Bubble
  // wrapping an Expandable with a title, whose body is either the default
  // styled text (`text`) or a caller-supplied `body` snippet. Expansion lives in
  // the UI context's per-id registry (the single source of truth), so the
  // client's click-to-expand (from the message stack) and the bubble stay in
  // sync; with no `id`/provider it toggles a local fallback from
  // `defaultExpanded`. Alignment and the optional system-message color override
  // also come from the context.
  const ui = useUiContext();
  // Mobile chat bubbles always sit on the agent side (left).
  const align = $derived(ui.platform === "mobile" ? "left" : ui.getAlignment());

  let {
    id,
    title: titleText,
    text = "",
    defaultExpanded = false,
    neighborLeft = false,
    neighborRight = false,
    applyColorOverride = false,
    body,
  }: {
    id?: string;
    title: string;
    /** Default text body, used when no `body` snippet is supplied. */
    text?: string;
    defaultExpanded?: boolean;
    neighborLeft?: boolean;
    neighborRight?: boolean;
    /** Tint the bubble with the appearance system-message color override. */
    applyColorOverride?: boolean;
    body?: Snippet;
  } = $props();

  // Local fallback for a no-`id` render (the website gallery / a preview); when
  // an `id` is present the registry is the only source of truth.
  let localExpanded = $state(untrack(() => defaultExpanded));
  // Seed the per-id default so the message-stack layout (which reads the
  // registry raw) promotes a default-expanded bubble to its own row. Idempotent,
  // so a remount from stack regrouping never clobbers the user's toggle.
  $effect(() => {
    if (id !== undefined) ui.expansionInit(id, defaultExpanded);
  });

  const overrideHex = $derived(applyColorOverride ? (ui.systemMessageDefaultColor ?? null) : null);
</script>

<div style:display="contents" style:--default-base={overrideHex}>
  <Bubble selectedAlignment={align} size="small" {neighborLeft} {neighborRight}>
    <Expandable
      bind:expanded={
        () => (id !== undefined ? ui.expansionGet(id, defaultExpanded) : localExpanded),
        (v) => {
          if (id !== undefined) ui.expansionSet(id, v);
          else localExpanded = v;
        }
      }
      alignment={align}
    >
      {#snippet title()}
        <span>{titleText}</span>
      {/snippet}
      {#snippet children()}
        {#if body}
          <!-- `text-left` keeps the body alignment-independent. -->
          <div class="text-left">{@render body()}</div>
        {:else}
          <div
            class="whitespace-pre-wrap bg-surface-inset text-default-700 text-xs text-left px-4 py-2 rounded-large"
          >
            {text}
          </div>
        {/if}
      {/snippet}
    </Expandable>
  </Bubble>
</div>
