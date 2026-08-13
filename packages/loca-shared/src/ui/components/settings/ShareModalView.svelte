<script lang="ts">
  // Settings share popup: Export curates a JSON of non-default values for the
  // selected groups/sections/fields; Import pastes that JSON and applies the
  // chosen keys. Both drive the same ShareTreeView. Overwrites are signalled
  // inline (warning rows) and in the Import button text rather than a modal.
  // The modal keeps a constant height: the tree explorer is the scrolling
  // region, and the tab body slides horizontally on tab change (mirroring the
  // settings group-tab animation).
  //
  // Pure View: the client computes the trees, the export JSON, parse results,
  // and import labels/counts, and owns the tab-slide animation and clipboard.
  // Everything arrives as plain props and callbacks.
  import Modal from "../primitives/Modal.svelte";
  import Button from "../primitives/Button.svelte";
  import IconButton from "../primitives/IconButton.svelte";
  import Alert from "../primitives/Alert.svelte";
  import Textarea from "../primitives/Textarea.svelte";
  import SettingsTabs from "../primitives/Tabs.svelte";
  import ErrorDetailView from "../chat/messages/ErrorDetailView.svelte";
  import ShareTreeView, { type ShareTreeGroup } from "./ShareTreeView.svelte";

  type TabId = "import" | "export";

  let {
    // shell + tabs
    open,
    tabs,
    activeTab,
    contentTab,
    tabSlideMs,
    animationsEnabled,
    // export
    exportTree,
    exportSelected = $bindable<Set<string>>(new Set()),
    exportJson,
    exportHasOutput,
    copied,
    // import
    importText = $bindable(""),
    parsedError,
    unknownKeysCount,
    importError,
    importTree,
    importSelected = $bindable<Set<string>>(new Set()),
    importHasTree,
    importCanApply,
    importing,
    importLabel,
    // callbacks
    onClose = () => {},
    onSelectTab = () => {},
    onCopyExport = () => {},
    onPasteImport = () => {},
    onImport = () => {},
    tabLayerEl = $bindable<HTMLDivElement | undefined>(undefined),
  }: {
    open: boolean;
    tabs: { id: string; label: string }[];
    activeTab: TabId;
    contentTab: TabId;
    tabSlideMs: number;
    animationsEnabled: boolean;
    exportTree: ShareTreeGroup[];
    exportSelected?: Set<string>;
    exportJson: string;
    exportHasOutput: boolean;
    copied: boolean;
    importText?: string;
    parsedError?: string;
    unknownKeysCount: number;
    importError?: string | null;
    importTree: ShareTreeGroup[];
    importSelected?: Set<string>;
    importHasTree: boolean;
    importCanApply: boolean;
    importing: boolean;
    importLabel: string;
    onClose?: () => void;
    onSelectTab?: (id: string) => void;
    onCopyExport?: () => void;
    onPasteImport?: () => void;
    onImport?: () => void;
    tabLayerEl?: HTMLDivElement;
  } = $props();
</script>

{#snippet floatButton(icon: string, title: string, onclick: () => void)}
  <IconButton
    {icon}
    {title}
    size="lg"
    surface="none"
    rounded="rounded-large"
    class="absolute top-2 right-2 z-1 bg-surface-inset hover:bg-surface shadow-sm"
    {onclick}
  />
{/snippet}

{#snippet exportPanel()}
  <p class="text-default-600 text-sm shrink-0">
    Choose which settings to export. Only values you have changed from their defaults are included.
  </p>
  <div class="flex-1 min-h-0 overflow-y-auto loca-scroll pr-1">
    <ShareTreeView nodes={exportTree} bind:selected={exportSelected} />
  </div>
  {#if exportHasOutput}
    <div class="relative flex-1 min-h-0">
      <Textarea
        value={exportJson}
        disabled
        mono
        autoResize="none"
        ariaLabel="Exported settings JSON"
        class="h-full w-full overflow-y-auto resize-none"
      />
      {@render floatButton(
        copied
          ? "i-material-symbols-check-rounded"
          : "i-material-symbols-content-copy-outline-rounded",
        "Copy to clipboard",
        onCopyExport,
      )}
    </div>
  {/if}
{/snippet}

{#snippet importPanel()}
  <p class="text-default-600 text-sm shrink-0">
    Paste exported settings, then choose which to apply. Highlighted rows will overwrite values you
    have customized.
  </p>
  <div class="relative flex-1 min-h-0">
    <!-- A native textarea (not the Textarea component) so its single bind:value
         keeps the caret in place during manual edits; routing the value through
         a component's prop round-trips it and bounces the caret to the end. -->
    <textarea
      bind:value={importText}
      spellcheck="false"
      aria-label="Settings JSON to import"
      class="h-full w-full resize-none overflow-y-auto loca-scroll rounded-medium px-2 py-1.5 text-sm font-mono text-default-800 outline-none {parsedError
        ? 'bg-surface-inset loca-error-ring'
        : 'bg-surface-inset'}"
    ></textarea>
    {@render floatButton(
      "i-material-symbols-content-paste-rounded",
      "Paste from clipboard",
      onPasteImport,
    )}
  </div>

  {#if parsedError}
    <ErrorDetailView message={parsedError} />
  {:else if unknownKeysCount > 0}
    <Alert variant="warning" size="sm" class="shrink-0">
      Ignoring {unknownKeysCount} unrecognized or unsupported key{unknownKeysCount === 1
        ? ""
        : "s"}.
    </Alert>
  {/if}
  {#if importError}
    <ErrorDetailView message={importError} />
  {/if}

  {#if importHasTree}
    <div class="flex-1 min-h-0 overflow-y-auto loca-scroll pr-1">
      <ShareTreeView nodes={importTree} bind:selected={importSelected} />
    </div>
  {/if}

  {#if importHasTree}
    <Button
      variant="primary"
      loading={importing}
      disabled={!importCanApply}
      icon="i-material-symbols-check-rounded"
      onclick={onImport}
      class="w-full px-4 py-2.5 rounded-large text-center whitespace-normal shrink-0"
    >
      {importCanApply ? importLabel : "Nothing to Apply"}
    </Button>
  {/if}
{/snippet}

<Modal
  {open}
  onclose={onClose}
  maxWidth="lg"
  ariaLabel="Import or export settings"
  class="h-[70vh] max-h-[40rem]"
>
  <div class="flex items-center justify-between shrink-0">
    <div class="text-default-800 font-medium">Import / Export Settings</div>
    <IconButton
      icon="i-material-symbols-close-rounded"
      title="Close"
      ariaLabel="Close"
      surface="circle"
      variant="subtle"
      size="md"
      class="hover:bg-default-400"
      onclick={onClose}
    />
  </div>

  <div class="shrink-0">
    <SettingsTabs {tabs} active={activeTab} onSelect={onSelectTab} slideMs={tabSlideMs} />
  </div>

  <!-- overflow-x clip contains the offscreen layer during the slide so the
       modal never grows a horizontal scrollbar; the y axis stays visible for
       the tree's own scroll. -->
  <div class="relative flex-1 min-h-0 flex flex-col" style:overflow-x="clip">
    <div
      bind:this={tabLayerEl}
      class="flex-1 min-h-0 flex flex-col gap-3"
      class:will-change-transform={animationsEnabled}
    >
      {#if contentTab === "export"}
        {@render exportPanel()}
      {:else}
        {@render importPanel()}
      {/if}
    </div>
  </div>
</Modal>
