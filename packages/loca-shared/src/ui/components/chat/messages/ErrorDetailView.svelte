<script lang="ts">
  import IconText from "../../primitives/IconText.svelte";

  // The one standardized error display: an optional friendly message row (a red
  // IconText) above an optional neutral mono card holding the raw technical
  // detail. On a neutral surface the card stays neutral (`bg-surface-inset`,
  // neutral mono text) and the red accent shows as an inset outline, so the
  // message row and the outline are the only accent signals. Inside an
  // already-red-accented bubble (the chat error bubble, an errored tool call)
  // the host sets `onAccentSurface`, dropping the outline so the card reads as
  // that bubble's inset well (retinted red by the bubble) rather than a second
  // red block. Used by every error site (chat error bubble, tool calls,
  // relevant memories/tools, the add-a-core wizard, the core bar, and the
  // settings load-failure states) so they cannot drift. Pure: data in, no
  // callbacks.
  let {
    message = undefined,
    detail = undefined,
    icon = "i-material-symbols-error-rounded",
    onAccentSurface = false,
  }: {
    /** Friendly, plain-language failure message. Omit for a detail-only card
     *  (e.g. a tool-call bubble whose header already says it failed). */
    message?: string;
    /** Raw technical detail, shown in a scrollable mono card. */
    detail?: string;
    /** Leading icon for the message row. */
    icon?: string;
    /** True when the card sits inside an already-red-accented surface (a chat
     *  error bubble), so the red accent outline is dropped to avoid
     *  double-accenting. */
    onAccentSurface?: boolean;
  } = $props();
</script>

<div class="flex flex-col gap-2 text-left">
  {#if message}
    <IconText {icon} color="text-accent-red-700">{message}</IconText>
  {/if}
  {#if detail}
    <pre
      class="loca-scroll-inset m-0 font-mono text-xs text-default-800 bg-surface-inset {onAccentSurface
        ? ''
        : 'outline-2 -outline-offset-2 outline-accent-red-700'} rounded-medium px-2.5 py-2 max-h-48 overflow-auto whitespace-pre-wrap break-words">{detail}</pre>
  {/if}
</div>
