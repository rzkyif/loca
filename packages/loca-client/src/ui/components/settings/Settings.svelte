<script lang="ts">
  import { onMount } from "svelte";
  import { platform } from "$lib/platform";
  import { isTauri } from "$lib/util/env";
  import {
    destinationNeedsCore,
    groupDestinations,
    isGroupVisible,
    SETTINGS_SCHEMA,
  } from "@loca/shared";
  import { useUiContext } from "@loca/shared/ui/context";
  import type { BubbleCorner } from "@loca/shared/ui/merge";
  import Select from "@loca/shared/ui/components/primitives/Select.svelte";
  import type { SettingField } from "@loca/shared";
  import type { Monitor } from "$lib/util/types";
  import { hasAlpha } from "$lib/appearance/color";
  import { getLogger } from "$lib/util/log";
  import { defaultExpandedSections } from "@loca/shared";
  import { downloadsState, settingsState, viewState } from "../../state";
  import { connectionState } from "$stores/connection.svelte";
  import { confirmState } from "$stores/confirm.svelte";
  import { useSettingsForm } from "$composables/use-settings-form.svelte";
  import { useSettingsSearch } from "$composables/use-settings-search.svelte";
  import { useSettingsScroll } from "$composables/use-settings-scroll.svelte";
  import { useResponsiveLayout } from "$composables/use-responsive-layout.svelte";
  import { cores } from "$lib/core";
  import type { PairedCoreEntry } from "$lib/core";

  // The settings panel is a thin client shell over the shared SettingsShellView
  // (single-source rule, AGENTS.md): the shell owns the bubble, header, sidebar
  // layout, the group-swap slide, and the scroll fades; this file feeds the live
  // groups + status footer, the field-rendering content, search/section state,
  // the multi-core picker, and the modal overlays.
  import SettingsShellView from "@loca/shared/ui/components/settings/SettingsShellView.svelte";
  import SettingsContentView from "@loca/shared/ui/components/settings/SettingsContentView.svelte";
  import SettingsField from "./SettingsField.svelte";
  import DownloadsButton from "./DownloadsButton.svelte";
  import UpdateButton from "./UpdateButton.svelte";
  import ColorPickerModal from "./ColorPickerModal.svelte";
  import ConfirmModal from "./ConfirmModal.svelte";
  import PasswordPromptModal from "./PasswordPromptModal.svelte";
  import DownloadsModal from "./DownloadsModal.svelte";
  import DeletionsModal from "./DeletionsModal.svelte";
  import ShareModal from "./ShareModal.svelte";
  import BuiltinToolkitModal from "./BuiltinToolkitModal.svelte";

  const log = getLogger("settings");

  // The settings view pins the CoreBar flush against the panel's bottom edge; the
  // route passes the corners to square off so the panel merges with it. Empty
  // (float) elsewhere and on mobile (no docked CoreBar there).
  let { flatCorners = [] }: { flatCorners?: BubbleCorner[] } = $props();

  let shell = $state<SettingsShellView>();
  let monitors: Monitor[] = $state([]);
  let fonts: string[] = $state([]);
  let shareOpen = $state(false);
  let expandedSections = $state<Set<string>>(new Set());

  let pairedCores: PairedCoreEntry[] = $state([]);
  let selectedCoreId: string = $state("");

  async function loadPairedCores() {
    try {
      pairedCores = await cores().list();
      selectedCoreId = cores().currentEntry()?.id ?? "";
    } catch {
      /* not paired yet */
    }
  }

  async function onSelectedCoreChanged(): Promise<void> {
    if (!selectedCoreId) return;
    try {
      await cores().select(selectedCoreId);
    } catch (e) {
      log.warn("select core failed:", e);
    }
  }

  const search = useSettingsSearch();
  const scroll = useSettingsScroll();
  const layout = useResponsiveLayout();
  // Shared field-change engine (validation / optimistic apply / preset flip).
  // Its hook reacts to section `expandWhen` deps by mutating this panel's
  // expanded-sections set.
  const form = useSettingsForm((sectionKey, expand) => {
    const next = new Set(expandedSections);
    if (expand) next.add(sectionKey);
    else next.delete(sectionKey);
    expandedSections = next;
  });

  // The single group whose sections are shown on the right; bound to the shell.
  let selectedGroupId = $state(SETTINGS_SCHEMA[0].id);

  const ui = useUiContext();
  const onMobile = ui.platform === "mobile";
  const visibleGroups = $derived(SETTINGS_SCHEMA.filter((g) => isGroupVisible(g, ui.platform)));
  const shellGroups = $derived(
    visibleGroups.map((g) => ({
      id: g.id,
      name: g.name,
      icon: g.icon,
      iconInactive: g.iconInactive,
    })),
  );
  const selectedGroup = $derived(visibleGroups.find((g) => g.id === selectedGroupId));

  const sidebarCollapsed = $derived(
    !!settingsState.currentSettings["appearance.settings.sidebarCollapsed"],
  );

  // The slide is owned by the shell; delegate the search composable's slide to it.
  $effect(() => {
    const s = shell;
    search.onSlide = s ? (active: boolean) => s.setSearch(active) : undefined;
  });

  // Track the user's horizontal-mode threshold setting + observe the content
  // container width (the wrapper lives inside the groupContent snippet below).
  $effect(() => {
    layout.threshold =
      (settingsState.currentSettings["appearance.settings.horizontalThreshold"] as number) ?? 680;
  });
  $effect(() => layout.observe());

  // While the core is unavailable, a group whose single destination needs it (the
  // shared `core` store or a `client-on-core` per-client overlay) can't be
  // edited; `locked` dims + inerts them. A multi-destination group stays usable.
  const coreGroupLocked = $derived(
    connectionState.coreUnavailable &&
      !!selectedGroup &&
      groupDestinations(selectedGroup).length === 1 &&
      destinationNeedsCore(groupDestinations(selectedGroup)[0]),
  );

  function isGroupDisabled(id: string): boolean {
    const group = SETTINGS_SCHEMA.find((g) => g.id === id);
    if (!group) return false;
    return (
      connectionState.coreUnavailable &&
      groupDestinations(group).length === 1 &&
      destinationNeedsCore(groupDestinations(group)[0])
    );
  }

  const withScrollAnchor = (fn: () => void) => {
    scroll.scrollEl = shell?.getScrollEl();
    scroll.withAnchor(fn);
  };

  function toggleSidebar(): void {
    withScrollAnchor(() => {
      settingsState.updateSetting("appearance.settings.sidebarCollapsed", !sidebarCollapsed);
    });
  }

  function toggleSection(key: string) {
    const next = new Set(expandedSections);
    if (next.has(key)) next.delete(key);
    else next.add(key);
    expandedSections = next;
  }

  // Section keys (`${groupId}-${sectionIndex}`) for a group's labeled (thus
  // collapsible) sections. Unlabeled sections render inline and have no key.
  function groupLabeledSectionKeys(groupId: string): string[] {
    const group = SETTINGS_SCHEMA.find((g) => g.id === groupId);
    if (!group) return [];
    const keys: string[] = [];
    group.sections.forEach((section, si) => {
      if (section.label) keys.push(`${groupId}-${si}`);
    });
    return keys;
  }

  function expandAllInGroup(groupId: string) {
    withScrollAnchor(() => {
      const next = new Set(expandedSections);
      for (const key of groupLabeledSectionKeys(groupId)) next.add(key);
      expandedSections = next;
    });
  }

  function collapseAllInGroup(groupId: string) {
    withScrollAnchor(() => {
      const next = new Set(expandedSections);
      for (const key of groupLabeledSectionKeys(groupId)) next.delete(key);
      expandedSections = next;
    });
  }

  // Restore a group's sections to their schema defaults (expanded unless the
  // section is `defaultCollapsed`). Fired by the shell when the active group is
  // re-clicked. Caller wraps in withScrollAnchor.
  function resetGroupToDefault(groupId: string) {
    const group = SETTINGS_SCHEMA.find((g) => g.id === groupId);
    if (!group) return;
    const next = new Set(expandedSections);
    group.sections.forEach((section, si) => {
      if (!section.label) return;
      const key = `${groupId}-${si}`;
      if (section.defaultCollapsed) next.delete(key);
      else next.add(key);
    });
    expandedSections = next;
  }

  onMount(async () => {
    // Open on a specific group when an external flow requested one (e.g. the
    // add-core wizard returning to the Cores manager), then clear the request.
    if (viewState.pendingSettingsGroup) {
      const g = viewState.pendingSettingsGroup;
      viewState.pendingSettingsGroup = null;
      selectedGroupId = g;
      // On mobile, jump straight into that group's detail screen so the CoreBar's
      // Cores shortcut lands on the fields, not the group list.
      if (onMobile) void shell?.selectGroup(g);
    }
    expandedSections = defaultExpandedSections();
    void loadPairedCores();
    try {
      const all = await platform().monitors.available();
      const mapped = all.map((mon, i) => ({
        id: mon.id || i.toString(),
        name: mon.name || `Monitor ${i + 1}`,
        isPrimary: mon.isPrimary,
      }));
      mapped.sort((a, b) => (a.isPrimary === b.isPrimary ? 0 : a.isPrimary ? -1 : 1));
      monitors = mapped;
    } catch (e) {
      log.error("Failed to load monitors:", e);
    }
    if (isTauri()) {
      try {
        fonts = await platform().fonts.list();
      } catch (e) {
        log.error("Failed to load fonts:", e);
      }
    }
    form.validateAllFields();
    // Desktop autofocuses search for keyboard-first use; mobile does not, so the
    // soft keyboard does not pop open the moment Settings appears. `preventScroll`
    // matters because this runs on mount: entering Settings from another panel
    // focuses the field WHILE the panel layer is still translated off-screen
    // mid-slide, and without it the WebView scrolls the layer to reveal the
    // off-screen field, shifting the whole UI and then snapping back (the "slides
    // to the wrong spot then teleports" glitch).
    if (!onMobile) search.inputEl?.focus({ preventScroll: true });
  });

  // While the core is unreachable, settings can't be read/written: clear the
  // search and dismiss the pending-downloads modal (acting on it needs the core).
  $effect(() => {
    if (!connectionState.coreUnavailable) return;
    if (search.query) search.clear();
    if (confirmState.pending?.title === "Pending Downloads") confirmState.cancel();
  });

  // Single pending-downloads popup, driven by the core's authoritative `missing`
  // requirements snapshot (see the original notes preserved below).
  let dismissedSignature = $state<string | null>(null);
  let shownSignature = $state<string | null>(null);
  $effect(() => {
    const sig = downloadsState.missingSignature;
    if (connectionState.coreUnavailable) return;
    // Prompt when files await approval OR when an approved download failed (so a
    // silent resolve/install failure becomes an actionable, retryable modal
    // instead of a limbo the user can't act on).
    const shouldPrompt = downloadsState.needsApproval || downloadsState.failed.length > 0;
    if (!shouldPrompt) {
      dismissedSignature = null;
      shownSignature = null;
      return;
    }
    if (sig === dismissedSignature || sig === shownSignature) return;
    shownSignature = sig;
    void downloadsState.requestRequiredModal({
      onConfirm: () => (dismissedSignature = null),
      onCancel: () => (dismissedSignature = sig),
    });
  });

  const themeOverride = $derived(
    settingsState.currentSettings["appearance.settingsDefaultColor"] as string,
  );
  const themeOverrideHex = $derived(hasAlpha(themeOverride) ? themeOverride : null);
</script>

<div style:display="contents" style:--default-base={themeOverrideHex}>
  <!-- One field row: the shared SettingsContentView composes the group/section
       layout and renders each field through this snippet, so the client's live
       field rendering and the website's match (single-source rule). -->
  {#snippet fieldRow(f: SettingField)}
    <SettingsField
      field={f}
      {monitors}
      {fonts}
      error={form.validationErrors[f.id] ?? null}
      horizontal={layout.horizontal}
      onChange={form.handleChange}
      onReset={form.resetToDefault}
      onPresetSelect={form.handlePresetSelect}
    />
  {/snippet}

  {#snippet groupContent(gid: string)}
    <div bind:this={layout.containerEl} class="relative h-full">
      <SettingsContentView
        groupId={gid}
        values={settingsState.currentSettings}
        horizontal={layout.horizontal}
        field={fieldRow}
        expanded={expandedSections}
        onToggleSection={toggleSection}
        onExpandAll={() => expandAllInGroup(gid)}
        onCollapseAll={() => collapseAllInGroup(gid)}
        locked={coreGroupLocked}
        onBack={onMobile && !ui.hasSystemBack ? () => shell?.mobileBack() : undefined}
      />
    </div>
  {/snippet}

  {#snippet searchContent()}
    <SettingsContentView
      searchQuery={search.query}
      values={settingsState.currentSettings}
      field={fieldRow}
    />
  {/snippet}

  {#snippet belowHeader()}
    {#if pairedCores.length > 1}
      <!-- Multi-core "editing settings for: <core>" picker. Switches the
           core-side settings store so groups marked destination:"core" read/write
           against the selected core's /api/v1/settings. -->
      <div class="flex items-center gap-2 px-1 py-1 text-sm">
        <span class="text-default-600">Editing settings for:</span>
        <div class="w-48">
          <Select
            value={selectedCoreId}
            options={pairedCores.map((c) => ({ value: c.id, label: c.name }))}
            ariaLabel="Editing settings for"
            onchange={(v) => {
              selectedCoreId = v;
              onSelectedCoreChanged();
            }}
          />
        </div>
      </div>
    {/if}
  {/snippet}

  {#snippet sidebarFooter(collapsed: boolean)}
    <!-- Sidecar health is no longer shown here as per-service chips: every
         failure folds into the core's status, surfaced (and expandable for the
         specific errors) in the CoreBar pinned at the bottom of this view. -->
    <DownloadsButton {collapsed} disabled={connectionState.coreUnavailable} />
    <UpdateButton {collapsed} disabled={connectionState.coreUnavailable} />
  {/snippet}

  <!-- Positioned, bubble-sized wrapper: the settings modals use Modal's default
       `positioning="absolute"`, so they need a `relative` ancestor matching the
       bubble for their backdrop blur (inset-0) to stay clipped to the bubble
       instead of spilling across the viewport. -->
  <div class="relative {onMobile ? 'w-full h-full' : 'w-fit'}">
    <SettingsShellView
      bind:this={shell}
      {flatCorners}
      groups={shellGroups}
      bind:selectedGroupId
      {sidebarCollapsed}
      bind:searchValue={search.query}
      bind:searchMode={search.mode}
      bind:searchEl={search.inputEl}
      searchPlaceholder={connectionState.unavailableCopy ?? "Search settings..."}
      searchDisabled={connectionState.coreUnavailable}
      onSearchInput={() => search.onInput()}
      onSearchFocus={() => {
        if (search.query.trim() && !search.mode) void search.setMode(true);
      }}
      onSearchClear={() => search.clear()}
      onQuickSettings={() => viewState.navigate("quickSettings")}
      onShare={() => (shareOpen = true)}
      onClose={() => viewState.navigate("chat")}
      {isGroupDisabled}
      onReselectGroup={(id) => withScrollAnchor(() => resetGroupToDefault(id))}
      onToggleSidebar={toggleSidebar}
      {belowHeader}
      {groupContent}
      {searchContent}
      {sidebarFooter}
    />

    <ConfirmModal />
    <PasswordPromptModal />
    <DownloadsModal />
    <DeletionsModal />
    <ShareModal open={shareOpen} onClose={() => (shareOpen = false)} />
    <ColorPickerModal />
    <BuiltinToolkitModal surface="settings" />
  </div>
</div>
