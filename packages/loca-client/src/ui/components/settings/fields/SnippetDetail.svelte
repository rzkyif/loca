<script lang="ts">
  import { untrack } from "svelte";
  import {
    normalizeName,
    recommendedSymbol,
    SNIPPET_PLACEMENT_OPTIONS,
    SNIPPET_SYMBOLS,
    type Snippet,
    type SnippetPlacement,
    type SnippetSymbol,
    snippetTrigger,
    validateName,
  } from "$lib/snippets/snippets";
  import { snippetsState } from "$stores";
  import { getLogger } from "$lib/util/log";
  import { createDebouncedSave } from "$lib/util/debounced-save";
  import SnippetDetailView from "@loca/shared/ui/components/settings/SnippetDetailView.svelte";

  const log = getLogger("snippets");

  // `reload` refreshes the list behind the detail so the card reflects edits.
  let { item, reload }: { item: Snippet; reload: () => void } = $props();

  // One-time snapshot of the opened snippet; later store updates must not clobber
  // an in-progress edit, so these are intentionally not derived from `item`.
  let draftName = $state(untrack(() => item.name));
  let draftSymbol = $state<SnippetSymbol>(untrack(() => item.symbol));
  let draftPlacement = $state<SnippetPlacement>(untrack(() => item.placement));
  let draftText = $state(untrack(() => item.text));
  // Once the user picks a symbol themselves (persisted as `symbolPinned`), stop
  // auto-tracking the recommendation on later placement changes. Reading the
  // stored flag (not re-inferring from the symbol) so a deliberate choice that
  // happens to equal the recommendation is still respected.
  let symbolTouched = $state(untrack(() => item.symbolPinned));

  const otherTriggers = $derived(
    snippetsState.snippets
      .filter((s) => s.id !== item.id)
      .map((s) => snippetTrigger(s).toLowerCase()),
  );
  const nameError = $derived(validateName(draftSymbol, draftName, otherTriggers));
  const recommended = $derived(recommendedSymbol(draftPlacement));
  const symbolOptions = $derived(
    SNIPPET_SYMBOLS.map((sym) => {
      const preview = `${sym}${draftName || "name"}`;
      return {
        value: sym,
        label: sym === recommended ? `${preview}  (Recommended)` : preview,
      };
    }),
  );
  const placementOptions = SNIPPET_PLACEMENT_OPTIONS.map((o) => ({
    value: o.value,
    label: o.label,
  }));

  const { scheduleSave, flushSave } = createDebouncedSave(async () => {
    if (nameError) return;
    try {
      await snippetsState.save({
        id: item.id,
        name: draftName,
        symbol: draftSymbol,
        symbolPinned: symbolTouched,
        placement: draftPlacement,
        text: draftText,
      });
      reload();
    } catch (e) {
      log.error("Failed to save snippet:", e);
    }
  });
</script>

<SnippetDetailView
  bind:draftName
  {draftSymbol}
  {draftPlacement}
  bind:draftText
  {nameError}
  {symbolOptions}
  {placementOptions}
  onNameInput={(v) => {
    draftName = normalizeName(v);
    scheduleSave();
  }}
  onNameBlur={() => flushSave()}
  onSymbolChange={(v) => {
    draftSymbol = v as SnippetSymbol;
    symbolTouched = true;
    scheduleSave();
  }}
  onPlacementChange={(v) => {
    draftPlacement = v as SnippetPlacement;
    if (!symbolTouched) draftSymbol = recommendedSymbol(draftPlacement);
    scheduleSave();
  }}
  onTextInput={(v) => {
    draftText = v;
    scheduleSave();
  }}
  onTextBlur={() => flushSave()}
/>
