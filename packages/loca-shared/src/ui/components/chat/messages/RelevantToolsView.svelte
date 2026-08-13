<script lang="ts">
  import type {
    ToolFilterEntryPersisted,
    ToolFilterPhase1Persisted,
  } from "../../../../domain/session.ts";
  import ExpandableMessageView from "./ExpandableMessageView.svelte";
  import ErrorDetailView from "./ErrorDetailView.svelte";

  // The "Found N relevant tools" bubble: the shared collapsed-message shell with
  // a body summarizing each tool-filter phase that ran. The client wrapper maps
  // the message's flat phase fields onto these props; this stays presentational.
  let {
    id,
    phase1,
    phase2,
    alwaysAvailable,
    nameMatched,
    mcp,
    status = "complete",
    errorMessage,
    defaultExpanded = false,
    neighborLeft = false,
    neighborRight = false,
  }: {
    id?: string;
    phase1?: ToolFilterPhase1Persisted[];
    phase2?: ToolFilterEntryPersisted[];
    alwaysAvailable?: ToolFilterEntryPersisted[];
    nameMatched?: ToolFilterEntryPersisted[];
    mcp?: ToolFilterEntryPersisted[];
    status?: string;
    errorMessage?: string;
    defaultExpanded?: boolean;
    neighborLeft?: boolean;
    neighborRight?: boolean;
  } = $props();

  type ScoredPhase = { kind: "embedding"; entries: ToolFilterPhase1Persisted[] };
  type PlainPhase = {
    kind: "llm" | "always" | "named" | "mcp";
    entries: ToolFilterEntryPersisted[];
  };
  type Phase = ScoredPhase | PlainPhase;

  // Active sections in execution order; skipped methods drop out so the
  // remaining "Phase N" labels renumber 1..N seamlessly. The extra sections
  // (named, always, mcp) only appear when they carry tools; "mcp" lists the MCP
  // tools offered this turn (always-available ones plus any matched by relevance).
  const phases = $derived.by<Phase[]>(() => {
    const out: Phase[] = [];
    if (phase1 !== undefined) out.push({ kind: "embedding", entries: phase1 });
    if (phase2 !== undefined) out.push({ kind: "llm", entries: phase2 });
    if (nameMatched !== undefined && nameMatched.length > 0) {
      out.push({ kind: "named", entries: nameMatched });
    }
    if (alwaysAvailable !== undefined && alwaysAvailable.length > 0) {
      out.push({ kind: "always", entries: alwaysAvailable });
    }
    if (mcp !== undefined && mcp.length > 0) {
      out.push({ kind: "mcp", entries: mcp });
    }
    return out;
  });

  const bypassCount = $derived(
    (alwaysAvailable?.length ?? 0) + (nameMatched?.length ?? 0) + (mcp?.length ?? 0),
  );
  const baseCount = $derived(phase2 !== undefined ? phase2.length : (phase1?.length ?? 0));
  const count = $derived(baseCount + bypassCount);

  const titleText = $derived.by(() => {
    if (status === "error") return "Failed to find relevant tools";
    // A directly-named tool is a relevant match even when nothing scored, so it
    // (unlike an always-available tool) lifts the "No relevant tools" title.
    if (baseCount === 0 && (nameMatched?.length ?? 0) === 0) return "No relevant tools";
    return `Found ${count} relevant tool${count === 1 ? "" : "s"}`;
  });

  function phaseLabel(kind: Phase["kind"]): string {
    if (kind === "embedding") return "Embedding";
    if (kind === "llm") return "LLM Filter";
    if (kind === "named") return "Named Tools";
    if (kind === "mcp") return "MCP Tools";
    return "Always Available Tools";
  }
  function phaseEmptyText(kind: Phase["kind"]): string {
    if (kind === "embedding") return "No candidates matched.";
    if (kind === "llm") return "No tools survived the filter.";
    return "No tools.";
  }
  const formatScore = (s: number): string => s.toFixed(2);
</script>

<ExpandableMessageView
  {id}
  title={titleText}
  applyColorOverride
  {defaultExpanded}
  {neighborLeft}
  {neighborRight}
>
  {#snippet body()}
    <div class="flex flex-col gap-2 text-xs text-left">
      {#if errorMessage}
        <div class="flex flex-col gap-1">
          <ErrorDetailView message="Couldn't filter tools" detail={errorMessage} />
          <div class="text-accent-red-700">
            Phase-1 candidates are kept as a fallback so the main model still sees tools.
          </div>
        </div>
      {/if}

      {#each phases as phase, i (phase.kind)}
        <div class="flex flex-col gap-1">
          <div class="text-default-600 font-bold">
            Phase {i + 1}: {phaseLabel(phase.kind)} ({phase.entries.length})
          </div>
          {#if phase.entries.length === 0}
            <div class="text-default-500 italic px-3 py-2">{phaseEmptyText(phase.kind)}</div>
          {:else if phase.kind === "embedding"}
            <div
              class="bg-surface-inset rounded-large px-4 py-2 font-mono text-default-800 whitespace-pre-wrap break-words"
            >
              {#each phase.entries as tool, ti (tool.toolId ?? ti)}{#if ti > 0},
                {/if}{tool.name}<span class="text-default-500 tabular-nums"
                  >&nbsp;({formatScore(tool.score)})</span
                >{/each}
            </div>
          {:else}
            <div
              class="bg-surface-inset rounded-large px-4 py-2 font-mono text-default-800 whitespace-pre-wrap break-words"
            >
              {#each phase.entries as tool, ti (tool.name + ti)}{#if ti > 0},
                {/if}{tool.name}{/each}
            </div>
          {/if}
        </div>
      {/each}
    </div>
  {/snippet}
</ExpandableMessageView>
