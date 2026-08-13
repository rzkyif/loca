<script lang="ts">
  // The Quick Settings panel: a header (bolt icon, title, close button) with
  // intro prose, an accordion of the module sections, and a bottom exit button,
  // all in one flex column that owns its own vertical rhythm and stretches the
  // bottom button - so the panel lays out identically wherever it is rendered
  // (the desktop Bubble, the mobile Modal, the website gallery), not at the
  // mercy of the host's wrapper.
  //
  // Schema-driven and single-source: the section list comes straight from the
  // shared QUICK_SETTINGS_SECTIONS manifest, and each section's fields render
  // through the injected `field` snippet (client passes its live SettingsField)
  // or a static SettingsFieldView (website). The client owns the interactive
  // state (which section is open, the on/off toggles, the live values + field
  // change handlers) and feeds it via props/callbacks; the website feeds schema
  // defaults. A `containerRef` callback hands the accordion region to the client
  // for its horizontal-mode threshold observer.
  import type { Snippet } from "svelte";
  import type { QuickSettingsSectionDef } from "../../../domain/settings/quick-settings.ts";
  import { QUICK_SETTINGS_SECTIONS } from "../../../domain/settings/quick-settings.ts";
  import type { SettingField } from "../../../domain/settings/types.ts";
  import IconButton from "../primitives/IconButton.svelte";
  import Button from "../primitives/Button.svelte";
  import QuickSettingsSectionView from "./QuickSettingsSectionView.svelte";

  let {
    sections = QUICK_SETTINGS_SECTIONS,
    values,
    openId = null,
    horizontal = false,
    exitLabel = "Continue to Chat",
    exitTitle = "Back to Chat",
    exitIcon = "i-material-symbols-arrow-forward-rounded",
    onExit = noop,
    onToggleSection = noopId,
    onSetEnabled = noopIdBool,
    containerRef = noopEl,
    field,
  }: {
    /** The accordion sections to render (the curated manifest). */
    sections?: QuickSettingsSectionDef[];
    /** Setting id -> value (the client's live settings; the website's defaults). */
    values: Record<string, unknown>;
    /** The id of the open section, or null when all are collapsed. */
    openId?: string | null;
    horizontal?: boolean;
    /** The bottom button's label (e.g. "Review Pending Downloads"). */
    exitLabel?: string;
    /** The close button's title/tooltip. */
    exitTitle?: string;
    /** The bottom button's leading icon (e.g. a download glyph when downloads
     *  are pending). */
    exitIcon?: string;
    onExit?: () => void;
    onToggleSection?: (id: string) => void;
    onSetEnabled?: (id: string, value: boolean) => void;
    /** Receives the accordion region element so the client can observe it for
     *  its horizontal-mode threshold. */
    containerRef?: (el: HTMLElement | null) => void;
    /** Renders one field (client injects its live SettingsField). When omitted,
     *  a static SettingsFieldView is used (website). */
    field?: Snippet<[SettingField]>;
  } = $props();

  function noop(): void {}
  function noopId(_id: string): void {}
  function noopIdBool(_id: string, _value: boolean): void {}
  function noopEl(_el: HTMLElement | null): void {}

  function attachContainer(node: HTMLElement) {
    containerRef(node);
    return () => containerRef(null);
  }

  function isEnabled(section: QuickSettingsSectionDef): boolean {
    return !section.enabledField || !!values[section.enabledField];
  }
</script>

<!-- One flex column owning the panel's vertical rhythm (gap-3 between the
     header, accordion, and exit button) and the button's full-width stretch, so
     the layout is the View's, not the host wrapper's. flex-1/min-h-0 lets it
     fill a height-bounded host (the Bubble/Modal) so the accordion scrolls;
     in an unbounded host (the gallery) it sizes to content. -->
<div class="flex flex-col gap-3 flex-1 min-h-0">
  <!-- Header: the title row and its intro prose grouped so they read as one
       block with a small gap. -->
  <div class="flex flex-col gap-1 shrink-0">
    <div class="flex items-center gap-2">
      <i class="flex i-material-symbols-bolt-rounded text-2xl text-default-700"></i>
      <h1 class="text-lg font-medium text-default-800 flex-1">Quick Settings</h1>
      <IconButton
        icon="i-material-symbols-close-rounded"
        title={exitTitle}
        size="lg"
        variant="subtle"
        surface="circle"
        onclick={onExit}
      />
    </div>
    <p class="text-sm text-default-600">
      The essentials to get going. Everything here is also in Settings.
    </p>
  </div>

  <!-- Accordion -->
  <div class="flex flex-col gap-1 flex-1 min-h-0" {@attach attachContainer}>
    {#each sections as section (section.id)}
      <QuickSettingsSectionView
        {section}
        {values}
        {horizontal}
        open={openId === section.id && isEnabled(section)}
        enabled={isEnabled(section)}
        onToggleExpand={() => onToggleSection(section.id)}
        onSetEnabled={(v) => onSetEnabled(section.id, v)}
        {field}
      />
    {/each}
  </div>

  <Button
    variant="primary"
    icon={exitIcon}
    onclick={onExit}
    class="shrink-0 px-4 py-2.5 rounded-large font-medium"
  >
    {exitLabel}
  </Button>
</div>
