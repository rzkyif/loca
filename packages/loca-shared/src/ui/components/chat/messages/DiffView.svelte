<script lang="ts">
  // Line diff between two texts, rendered as added/removed/context rows.
  // Used by the display bubble's diff content and the memory tool-result
  // bubbles; intentionally presentation-only (no accept/reject controls).
  import { diffLines } from "diff";

  let { before, after }: { before: string; after: string } = $props();

  type Row = { kind: "added" | "removed" | "context"; text: string };

  const rows = $derived.by<Row[]>(() => {
    const out: Row[] = [];
    for (const part of diffLines(before, after)) {
      const kind = part.added ? "added" : part.removed ? "removed" : "context";
      // diffLines yields multi-line chunks; split so each line gets its own
      // marker row (trailing newline would otherwise add an empty row).
      const lines = part.value.replace(/\n$/, "").split("\n");
      for (const text of lines) out.push({ kind, text });
    }
    return out;
  });
</script>

<!-- A thick border the same color as the fill (matching the SessionBar context
     gauge) frames the rows so the rounded card clips their corners instead of
     leaving square row rectangles floating inside padding. -->
<div
  class="font-mono text-xs text-left bg-surface-inset rounded-large border-0.25em border-default-200 overflow-x-auto"
>
  {#each rows as row}
    <div
      class="whitespace-pre px-2 {row.kind === 'added'
        ? 'bg-accent-green-200 text-accent-green-700'
        : row.kind === 'removed'
          ? 'bg-accent-red-200 text-accent-red-700'
          : 'text-default-700'}"
    >
      {row.kind === "added" ? "+ " : row.kind === "removed" ? "- " : "  "}{row.text}
    </div>
  {/each}
</div>
