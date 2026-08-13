<script lang="ts">
  // One Quick Settings accordion section: a header row (an expand button plus an
  // optional module on/off toggle) and, while open, a vertically scrollable body
  // of the section's curated schema fields. The toggle is a sibling of the
  // expand button, not a child: nesting the Toggle's input inside a <button>
  // would be invalid HTML.
  //
  // Schema-driven and single-source: the section's fields come straight from the
  // shared QUICK_SETTINGS_SECTIONS manifest, resolved against SETTINGS_SCHEMA,
  // and each renders through the injected `field` snippet (the client passes its
  // live SettingsField) or, with no snippet, a static SettingsFieldView (the
  // website) - exactly the pattern SettingsContentView uses, so the client and
  // the website render identical fields.
  import type { Snippet } from "svelte";
  import type { QuickSettingsSectionDef } from "../../../domain/settings/quick-settings.ts";
  import type { SettingField } from "../../../domain/settings/types.ts";
  import { evalCondition, findField } from "../../../domain/settings/engine.ts";
  import Expand from "../primitives/Expand.svelte";
  import Toggle from "../primitives/Toggle.svelte";
  import SettingsFieldView from "../settings/SettingsFieldView.svelte";

  let {
    section,
    values,
    open = false,
    enabled = true,
    horizontal = false,
    onToggleExpand = noop,
    onSetEnabled = noopBool,
    field,
  }: {
    section: QuickSettingsSectionDef;
    /** Setting id -> value, used to evaluate visibility and (for the static
     *  fallback renderer) to paint each field. */
    values: Record<string, unknown>;
    /** Body rendered. The caller derives this as `selected && enabled`, so a
     *  disabled module can never be open. */
    open?: boolean;
    enabled?: boolean;
    horizontal?: boolean;
    onToggleExpand?: () => void;
    onSetEnabled?: (value: boolean) => void;
    /** Renders one field (client injects its live SettingsField). When omitted,
     *  a static SettingsFieldView is used (website). */
    field?: Snippet<[SettingField]>;
  } = $props();

  function noop(): void {}
  function noopBool(_value: boolean): void {}

  // The manifest is static, so an unresolved id only happens on schema drift
  // (guarded by quick-settings.test.ts); drop it rather than crash.
  const resolved = $derived(
    section.fields.flatMap((ref) => {
      const f = findField(ref.id);
      return f ? [{ ref, field: f }] : [];
    }),
  );

  const hasToggle = $derived(!!section.enabledField);
</script>

<!-- The open section claims the remaining panel height and scrolls inside it;
     closed sections are fixed header rows. -->
<div class="flex flex-col {open ? 'flex-1 min-h-0' : 'shrink-0'}">
  <div class="flex items-center gap-2 shrink-0">
    <button
      type="button"
      class="flex flex-1 items-center gap-2 py-2 text-default-800 cursor-pointer transition-colors hover:text-default-900 disabled:opacity-50 disabled:cursor-default"
      disabled={!enabled}
      aria-expanded={open}
      onclick={onToggleExpand}
    >
      <i
        class="flex text-xl transition-transform duration-200 {open
          ? 'i-material-symbols-keyboard-arrow-down-rounded'
          : 'i-material-symbols-chevron-right-rounded'}"
      ></i>
      <span class="flex-1 text-left text-base font-medium">{section.title}</span>
    </button>
    {#if hasToggle}
      <div class="w-24 shrink-0">
        <Toggle
          compact
          labels={{ on: "ON", off: "OFF" }}
          checked={enabled}
          ariaLabel={`Enable ${section.title}`}
          onchange={onSetEnabled}
        />
      </div>
    {/if}
  </div>
  <!-- pl-7 lines the field column up under the header title: chevron
       (text-xl, 1.25rem) + the header's gap-2 (0.5rem). The Expand wrapper
       doubles as the scroll container: runExpand reads its scrollHeight,
       which a scroll container reports as the full content height even while
       max-height clamps it, so the open/close sweep tracks the real size. -->
  <Expand {open} class="loca-scroll overflow-y-auto min-h-0 flex flex-col gap-1 pl-7 pr-2 pb-1">
    {#each resolved as { ref, field: f } (ref.id)}
      {#if evalCondition(ref.visibleWhen, values) && evalCondition(f.visibleWhen, values)}
        {#if field}
          {@render field(f)}
        {:else}
          <SettingsFieldView
            field={f}
            value={values[f.id] ?? ("defaultValue" in f ? f.defaultValue : "")}
            {horizontal}
          />
        {/if}
      {/if}
    {/each}
  </Expand>
</div>
