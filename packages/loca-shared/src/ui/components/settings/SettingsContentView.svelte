<script lang="ts">
  import type { Snippet } from "svelte";
  import { SETTINGS_SCHEMA, evalCondition, searchFields } from "../../../domain/settings/engine.ts";
  import { groupDestinationChips } from "../../../domain/settings/types.ts";
  import type {
    SettingField,
    SettingGroup,
    SettingSection,
  } from "../../../domain/settings/types.ts";
  import DestinationChip from "../objects/DestinationChip.svelte";
  import HelpText from "../primitives/HelpText.svelte";
  import IconButton from "../primitives/IconButton.svelte";
  import SectionHeader from "../primitives/SectionHeader.svelte";
  import SettingsFieldView from "./SettingsFieldView.svelte";
  import Tabs from "../primitives/Tabs.svelte";
  import { slideSwap } from "../../animations.ts";
  import { useUiContext } from "../../context.ts";

  // THE single settings content composition for both the client and the website
  // (single-source rule, AGENTS.md): the sticky group header (name + Client/Core
  // chips + expand/collapse actions), the optional description + tab selector, the
  // real sections (sticky sub-headers, defaultCollapsed, visibleWhen) with their
  // fields, and the search-results layout. Each field is rendered by the injected
  // `field` snippet (the client passes its live `SettingsField`; the website
  // passes none and a static `SettingsFieldView` is used with `values`), so the
  // field markup is single-source too. Section-expand and tab state are external
  // when handlers are given (client), otherwise managed internally (website).
  const ui = useUiContext();

  let {
    groupId,
    searchQuery,
    values = {},
    horizontal = true,
    field,
    expanded: expandedProp,
    onToggleSection,
    onExpandAll,
    onCollapseAll,
    activeTab: activeTabProp,
    onSelectTab,
    locked = false,
    onBack,
  }: {
    /** Render this group's header + sections. Ignored when `searchQuery` is set. */
    groupId?: string;
    /** Render search results for this query instead of a group. */
    searchQuery?: string;
    /** Setting id -> value, used by the default static field renderer. */
    values?: Record<string, unknown>;
    horizontal?: boolean;
    /** Renders one field. When omitted, a static `SettingsFieldView` is used. */
    field?: Snippet<[SettingField]>;
    /** Controlled expanded-section keys (`${groupId}-${index}`); internal if omitted. */
    expanded?: Set<string>;
    onToggleSection?: (key: string) => void;
    onExpandAll?: () => void;
    onCollapseAll?: () => void;
    /** Controlled active tab id; internal if omitted. */
    activeTab?: string;
    onSelectTab?: (id: string) => void;
    /** Reconnecting: dim + block interaction (client). */
    locked?: boolean;
    /** When set, the group header shows a leading back button (mobile nested
     *  navigation returns to the group list). Omitted on desktop. */
    onBack?: () => void;
  } = $props();

  const group = $derived<SettingGroup | undefined>(
    groupId ? SETTINGS_SCHEMA.find((g) => g.id === groupId) : undefined,
  );

  // Destination chips: a single-destination group shows one chip on its header.
  // A multi-destination (hybrid) group shows NO header chip; instead each labeled
  // section carries its own Client/Core chip (every section in a hybrid group
  // sets its own `destination`), so the split is legible per section rather than
  // as an ambiguous pair on the group header.
  const groupChips = $derived(group ? groupDestinationChips(group) : []);
  const isMultiDest = $derived(groupChips.length > 1);

  // On mobile, every label (the group header, the section headers, and the
  // fields) sits on ONE left text column, with a collapsible section's chevron
  // hanging in the gutter to the left of it. So the group header gets the same
  // pl-5 the fields already use, the section headers stay flush-left (their
  // chevron occupies the gutter, their text lands on the column), and the body
  // (description / tabs / fields) shares the column. The result: section header
  // text lines up under the group header text, not the chevron under the text.
  const stacked = $derived(ui.platform === "mobile");
  const bodyIndent = $derived(stacked ? "pl-5" : "");

  // --- tab state (controlled or internal) ---
  let activeTabInternal = $state("");
  let contentTabId = $state("");
  $effect(() => {
    const first = group?.tabs?.[0]?.id ?? "";
    activeTabInternal = activeTabProp ?? first;
    contentTabId = activeTabProp ?? first;
  });
  const activeTab = $derived(activeTabProp ?? activeTabInternal);

  let tabLayerEl = $state<HTMLDivElement>();
  let tabTransitioning = false;
  async function selectTab(id: string): Promise<void> {
    if (onSelectTab) {
      onSelectTab(id);
      return;
    }
    if (id === activeTabInternal || tabTransitioning) return;
    const tabs = group?.tabs ?? [];
    const toIdx = tabs.findIndex((t) => t.id === id);
    const fromIdx = tabs.findIndex((t) => t.id === activeTabInternal);
    activeTabInternal = id;
    tabTransitioning = true;
    await slideSwap(tabLayerEl, {
      axis: "x",
      outSign: toIdx > fromIdx ? -1 : 1,
      durationMs: ui.animationDurationMs(),
      swap: () => {
        contentTabId = id;
      },
    });
    tabTransitioning = false;
  }

  function sectionVisible(s: SettingSection): boolean {
    if (!evalCondition(s.visibleWhen, values)) return false;
    if (s.desktopOnly && ui.platform === "mobile") return false;
    if (group?.tabs && s.tab !== contentTabId) return false;
    return s.fields.length > 0;
  }

  /** A field is shown when its `visibleWhen` matches AND it is not desktop-only
   *  on a mobile shell. Mirrors `sectionVisible` at the field grain. */
  function fieldVisible(f: SettingField): boolean {
    if (f.desktopOnly && ui.platform === "mobile") return false;
    return evalCondition(f.visibleWhen, values);
  }

  // --- expand state (controlled or internal) ---
  const keyOf = (i: number): string => `${groupId}-${i}`;
  let expandedInternal = $state(new Set<string>());
  $effect(() => {
    if (onToggleSection) return; // controlled
    const next = new Set<string>();
    (group?.sections ?? []).forEach((s, i) => {
      if (s.label && !s.defaultCollapsed) next.add(keyOf(i));
    });
    expandedInternal = next;
  });
  const expanded = $derived(expandedProp ?? expandedInternal);
  function toggle(key: string): void {
    if (onToggleSection) {
      onToggleSection(key);
      return;
    }
    const next = new Set(expandedInternal);
    if (next.has(key)) next.delete(key);
    else next.add(key);
    expandedInternal = next;
  }
  const labeledKeys = $derived(
    (group?.sections ?? [])
      .map((s, i) => ({ s, i }))
      .filter((x) => !!x.s.label && sectionVisible(x.s))
      .map((x) => keyOf(x.i)),
  );
  const hasCollapsibleSections = $derived(labeledKeys.length > 0);
  function expandAll(): void {
    if (onExpandAll) return onExpandAll();
    expandedInternal = new Set(labeledKeys);
  }
  function collapseAll(): void {
    if (onCollapseAll) return onCollapseAll();
    expandedInternal = new Set();
  }

  const showGroupDesc = $derived(
    !!group?.description && (group.descriptionTier ?? "ondemand") === "always",
  );

  // An object-management group is a single section holding a single
  // object_management field, rendered full-bleed instead of as sections.
  const omField = $derived.by<SettingField | null>(() => {
    if (!group) return null;
    const om = group.sections.filter(
      (s) => sectionVisible(s) && s.fields.some((f) => f.type === "object_management"),
    );
    if (om.length !== 1 || om[0].fields.length !== 1) return null;
    return om[0].fields[0];
  });

  const search = $derived(searchQuery ? searchFields(searchQuery, values, ui.platform) : []);
</script>

{#snippet renderField(f: SettingField)}
  {#if field}
    {@render field(f)}
  {:else}
    <SettingsFieldView
      field={f}
      value={values[f.id] ?? ("defaultValue" in f ? f.defaultValue : "")}
      {horizontal}
    />
  {/if}
{/snippet}

{#if searchQuery}
  <div class="flex flex-col gap-4">
    {#each search as rg (rg.sectionKey)}
      <div class="flex flex-col gap-2">
        <div class="text-base text-default-500 font-medium uppercase tracking-wide">
          {rg.groupName}{rg.sectionLabel ? ` › ${rg.sectionLabel}` : ""}
        </div>
        {#each rg.fields as f (f.id)}
          {@render renderField(f)}
        {/each}
      </div>
    {:else}
      <div class="bg-surface-inset rounded-large px-4 py-2 text-default-600 text-base">
        No matching settings found.
      </div>
    {/each}
  </div>
{:else if group}
  <!-- An object-management group fills the viewport (h-full min-h-0) so its
       sticky header + tabs stay put and only its inner list scrolls; every other
       group grows with its content and the panel viewport scrolls it. -->
  <section class="flex flex-col {omField ? 'h-full min-h-0' : ''}">
    <!-- Group header: sticky at the very top (z above section headers at top-7).
         The label sits on the SAME pl-5 column as the body on mobile so the two
         line up. The back affordance (iOS, where there is no system back) is
         absolutely positioned in the gutter to the LEFT of that column - it hangs
         into the shell's p-3 padding rather than pushing the label right, so the
         group content stays aligned under the header label with or without it. -->
    <div class="sticky top-0 z-20">
      {#if onBack}
        <IconButton
          icon="i-material-symbols-arrow-back-rounded"
          title="Back to settings"
          size="sm"
          variant="subtle"
          class="absolute left-0 top-1/2 -translate-y-1/2 -ml-2 w-7 shrink-0 z-10"
          onclick={onBack}
        />
      {/if}
      <SectionHeader label={group.name} level="group" class={stacked ? "pl-5" : ""}>
        {#snippet badge()}
          {#if !isMultiDest}
            <span class="inline-flex items-center gap-1 shrink-0">
              {#each groupChips as dest (dest)}
                <DestinationChip {dest} />
              {/each}
            </span>
          {/if}
        {/snippet}
        {#snippet actions()}
          {#if hasCollapsibleSections}
            <IconButton
              icon="i-material-symbols-unfold-more-rounded"
              title="Expand all sections"
              size="sm"
              variant="subtle"
              onclick={expandAll}
            />
            <IconButton
              icon="i-material-symbols-unfold-less-rounded"
              title="Collapse all sections"
              size="sm"
              variant="subtle"
              onclick={collapseAll}
            />
          {/if}
        {/snippet}
      </SectionHeader>
    </div>

    {#if showGroupDesc && group.description}
      <div class="shrink-0 pt-1 {bodyIndent}"><HelpText text={group.description} /></div>
    {/if}

    {#if group.tabs}
      <div class="shrink-0 pt-2 pb-3 {bodyIndent}">
        <Tabs
          tabs={group.tabs.map((t) => ({ id: t.id, label: t.label }))}
          active={activeTab}
          onSelect={selectTab}
          slideMs={ui.animationDurationMs() * 2}
        />
      </div>
    {/if}

    <div
      bind:this={tabLayerEl}
      class="flex flex-1 min-h-0 flex-col transition-opacity {locked ? 'opacity-50' : ''}"
      inert={locked}
    >
      {#if omField}
        <!-- The object-management shell shares the same left column as every other
             label on mobile, so its header/rows line up under the group header. -->
        <div class="flex-1 min-h-0 pt-1 {bodyIndent}">
          {@render renderField(omField)}
        </div>
      {:else}
        <!-- gap-3 separates sections; each section is a tight (gap-1) unit. -->
        <div class="flex flex-col gap-3">
          {#each group.sections as section, i (i)}
            {#if sectionVisible(section)}
              {@const collapsible = !!section.label}
              {@const isExpanded = !collapsible || expanded.has(keyOf(i))}
              <div class="flex flex-col gap-1 {collapsible && !isExpanded ? '-mb-2' : ''}">
                {#if section.label}
                  <div class="sticky top-7 z-10 pt-1.5 bg-surface">
                    <SectionHeader
                      label={section.label}
                      level="section"
                      collapsible
                      expanded={isExpanded}
                      onToggle={() => toggle(keyOf(i))}
                    >
                      {#snippet badge()}
                        {#if isMultiDest && section.destination}
                          <DestinationChip dest={section.destination} />
                        {/if}
                      {/snippet}
                    </SectionHeader>
                  </div>
                {/if}
                {#if isExpanded}
                  <div class="flex flex-col gap-1" class:pl-5={collapsible || stacked}>
                    {#each section.fields as f (f.id)}
                      {#if fieldVisible(f)}
                        {@render renderField(f)}
                      {/if}
                    {/each}
                  </div>
                {/if}
              </div>
            {/if}
          {/each}
        </div>
      {/if}
    </div>
  </section>
{/if}
