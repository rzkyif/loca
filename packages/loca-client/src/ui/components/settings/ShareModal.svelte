<script lang="ts">
  // Client shell for the settings share popup. Owns all logic: builds the
  // import/export trees from the schema, computes the export JSON, parses and
  // classifies pasted import JSON, drives the tab-slide animation, and talks to
  // the clipboard and the settings store. The presentational markup lives in
  // ShareModalView.
  import { tick } from "svelte";
  import { getDefaultSettings } from "@loca/shared";
  import { settingsState } from "../../state";
  import { CSS_EASING, getDuration } from "$lib/appearance/animations";
  import { getLogger } from "$lib/util/log";
  import ShareModalView from "@loca/shared/ui/components/settings/ShareModalView.svelte";
  import {
    buildTree,
    classifyImport,
    computeExportJson,
    EXPORTABLE_FIELD_IDS,
    parseImport,
  } from "$lib/settings/share";

  let { open, onClose }: { open: boolean; onClose: () => void } = $props();

  const log = getLogger("settings-share");
  const defaults = getDefaultSettings();

  type TabId = "import" | "export";
  const tabs = [
    { id: "import", label: "Import" },
    { id: "export", label: "Export" },
  ];

  // `activeTab` drives the knob (moves immediately); `contentTab` is the body
  // actually rendered, swapped at the slide's midpoint while it's offscreen.
  let activeTab = $state<TabId>("export");
  let contentTab = $state<TabId>("export");
  let tabLayerEl = $state<HTMLDivElement>();
  let tabTransitioning = false;
  const tabSlideMs = $derived(getDuration() * 2);
  const animationsEnabled = $derived(
    !!settingsState.currentSettings["appearance.animationsEnabled"],
  );

  async function selectTab(id: string): Promise<void> {
    const target = id as TabId;
    if (target === activeTab || tabTransitioning) return;
    const toIdx = tabs.findIndex((t) => t.id === target);
    const fromIdx = tabs.findIndex((t) => t.id === activeTab);
    const dur = getDuration();
    activeTab = target;
    const swap = () => (contentTab = target);
    if (!tabLayerEl || dur <= 0) {
      swap();
      return;
    }
    tabTransitioning = true;
    // Later tab -> current leaves left, new enters from the right; earlier tab
    // reverses it.
    const outSign = toIdx > fromIdx ? -1 : 1;
    const trans = `transform ${dur}ms ${CSS_EASING}`;
    tabLayerEl.style.transition = trans;
    tabLayerEl.style.transform = `translateX(${100 * outSign}%)`;
    await new Promise((r) => setTimeout(r, dur));

    swap();
    await tick();
    tabLayerEl.style.transition = "none";
    tabLayerEl.style.transform = `translateX(${100 * -outSign}%)`;
    void tabLayerEl.offsetHeight;

    tabLayerEl.style.transition = trans;
    tabLayerEl.style.transform = "";
    await new Promise((r) => setTimeout(r, dur));
    tabLayerEl.style.transition = "";
    tabTransitioning = false;
  }

  // --- export ---------------------------------------------------------------
  const exportTree = buildTree(EXPORTABLE_FIELD_IDS);
  let exportSelected = $state<Set<string>>(new Set());
  const exportJson = $derived(
    computeExportJson(exportSelected, settingsState.currentSettings, defaults),
  );
  let copied = $state(false);
  let copyTimer: ReturnType<typeof setTimeout> | null = null;

  async function copyExport() {
    try {
      await navigator.clipboard.writeText(exportJson);
      copied = true;
      if (copyTimer) clearTimeout(copyTimer);
      copyTimer = setTimeout(() => (copied = false), 1500);
    } catch (e) {
      log.warn("copy failed:", e);
    }
  }

  // --- import ---------------------------------------------------------------
  let importText = $state("");
  let importing = $state(false);
  let importError = $state<string | null>(null);
  let importSelected = $state<Set<string>>(new Set());

  const parsed = $derived(parseImport(importText));
  function importKind(id: string) {
    return classifyImport(parsed.values[id], settingsState.currentSettings[id], defaults[id]);
  }

  const importTree = $derived(
    buildTree(
      new Set(Object.keys(parsed.values)),
      (id) => importKind(id) === "overwrite",
      (id) => importKind(id) === "noop",
    ),
  );

  // Default to selecting every applicable key in the pasted JSON (skipping the
  // no-ops, which aren't selectable); reset on every change.
  $effect(() => {
    const next = new Set<string>();
    for (const id of Object.keys(parsed.values)) {
      if (importKind(id) !== "noop") next.add(id);
    }
    importSelected = next;
  });

  const counts = $derived.by(() => {
    let apply = 0;
    let overwrite = 0;
    for (const id of importSelected) {
      const kind = importKind(id);
      if (kind === "apply") apply++;
      else if (kind === "overwrite") overwrite++;
    }
    return { apply, overwrite };
  });

  function plural(n: number): string {
    return `${n} Setting${n === 1 ? "" : "s"}`;
  }

  const importLabel = $derived.by(() => {
    const { apply, overwrite } = counts;
    if (apply > 0 && overwrite > 0)
      return `Apply ${plural(apply)} and Overwrite ${plural(overwrite)}`;
    if (overwrite > 0) return `Overwrite ${plural(overwrite)}`;
    return `Apply ${plural(apply)}`;
  });

  // Output only exists once a selection yields non-default values; the tree only
  // exists once the pasted JSON has importable keys. When the paired textarea
  // and tree are both present they each take half the height (flex-1); when one
  // is absent the other fills.
  const exportHasOutput = $derived(exportJson !== "{}");
  const importHasTree = $derived(Object.keys(parsed.values).length > 0);
  const importCanApply = $derived(counts.apply + counts.overwrite > 0);

  async function pasteImport() {
    try {
      importText = await navigator.clipboard.readText();
    } catch (e) {
      log.warn("paste failed:", e);
    }
  }

  async function doImport() {
    const updates: Record<string, unknown> = {};
    for (const id of importSelected) {
      if (importKind(id) !== "noop") updates[id] = parsed.values[id];
    }
    if (Object.keys(updates).length === 0) return;
    importing = true;
    importError = null;
    try {
      await settingsState.updateSettings(updates);
      onClose();
    } catch (e) {
      log.warn("import failed:", e);
      importError = "Failed to apply settings";
    } finally {
      importing = false;
    }
  }
</script>

<ShareModalView
  {open}
  {tabs}
  {activeTab}
  {contentTab}
  {tabSlideMs}
  {animationsEnabled}
  {exportTree}
  bind:exportSelected
  {exportJson}
  {exportHasOutput}
  {copied}
  bind:importText
  parsedError={parsed.error}
  unknownKeysCount={parsed.unknownKeys.length}
  {importError}
  {importTree}
  bind:importSelected
  {importHasTree}
  {importCanApply}
  {importing}
  {importLabel}
  {onClose}
  onSelectTab={selectTab}
  onCopyExport={copyExport}
  onPasteImport={pasteImport}
  onImport={doImport}
  bind:tabLayerEl
/>
