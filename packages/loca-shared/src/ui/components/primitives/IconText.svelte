<script lang="ts">
  import type { Snippet } from "svelte";

  // An icon paired with one short line of text: a status line, a field error, an
  // error header, the permission/schedule prompt headers. A single `color` sets
  // the container text color and the icon (a monochrome UnoCSS icon) paints from
  // `currentColor`, so the icon and text colors are always identical by
  // construction. The icon is sized one step above the text and pinned to the
  // first line, so when the text wraps the icon stays put. Shared so the client
  // and website render every icon+text row identically. Keep the text short and
  // user-friendly; wrap any inline code in a mono chip rather than passing raw
  // technical text.
  //
  // ICON RULE: always pass the FILLED icon variant here, never the outline one
  // (`i-material-symbols-error-rounded`, NOT `...-error-outline-rounded`). A
  // filled glyph reads as a deliberate badge at this small size; the outline
  // variant looks weightless next to the text. This is the one place that breaks
  // the app's usual outline-icon default.
  //
  // COPY RULE: no trailing period on the text, even when it reads as a sentence
  // ("[tool] wants to read a file", not "...a file."). It is a compact label, not
  // prose.
  let {
    icon,
    color = "text-default-700",
    children,
  }: {
    /** Leading icon class (`i-<collection>-<name>`). */
    icon: string;
    /** Color class for the whole row; the icon inherits it via `currentColor`. */
    color?: string;
    /** The short text; may contain an inline mono code chip. */
    children: Snippet;
  } = $props();
</script>

<div class="flex items-start gap-1.5 text-xs {color}">
  <span class="flex items-center shrink-0 h-4"><i class="flex {icon} text-sm"></i></span>
  <span class="min-w-0 break-words leading-4">{@render children()}</span>
</div>
