<script lang="ts">
  // Presentational body of the Speech-to-Text catalog picker: one card per
  // preset (each with computed badges) and an optional Custom card carrying
  // Model + Quantization dropdowns. Every value arrives pre-formatted (the
  // client owns the catalog, the selection state, and the size/language
  // formatting), so this stays pure: props in, callbacks out. The FieldCard
  // wrapper lives in the client shell; this is only SttPresetField's own
  // bespoke body markup.
  import OptionCard from "../primitives/OptionCard.svelte";
  import Select from "../primitives/Select.svelte";
  import HelpText from "../primitives/HelpText.svelte";
  import ErrorDetailView from "../chat/messages/ErrorDetailView.svelte";

  // One badge chip inside a card (icon + pre-formatted text).
  interface Badge {
    icon: string;
    text: string;
  }

  // One preset card. `badges` is null while the catalog is still loading (or
  // the preset is absent); `placeholder` then carries the muted fallback line.
  // `selectable` is false until the preset's catalog entry has loaded.
  interface PresetCard {
    id: string;
    title: string;
    description?: string;
    selected: boolean;
    badges: Badge[] | null;
    placeholder?: string;
    selectable: boolean;
  }

  // A <select> dropdown (Model or Quantization), pre-resolved by the client.
  interface Dropdown {
    value: string;
    options: { value: string; label: string; disabled?: boolean }[];
  }

  // The Custom card with its Model + (optional) Quantization dropdowns.
  interface CustomCard {
    title: string;
    description?: string;
    selected: boolean;
    model: Dropdown;
    quant: Dropdown | null;
  }

  let {
    error = null,
    presets = [],
    custom = null,
    onSelectPreset,
    onSelectCustom,
    onModelSelect,
    onQuantSelect,
  }: {
    error?: string | null;
    presets?: PresetCard[];
    custom?: CustomCard | null;
    onSelectPreset?: (id: string) => void;
    onSelectCustom?: () => void;
    onModelSelect?: (value: string) => void;
    onQuantSelect?: (value: string) => void;
  } = $props();

  const noop = (): void => {};
</script>

<div class="flex flex-col gap-2">
  {#if error}
    <ErrorDetailView message={error} />
  {/if}

  {#each presets as p (p.id)}
    {#snippet badges()}
      {#if p.badges}
        {#each p.badges as badge (badge.icon)}
          <span class="inline-flex items-center gap-1">
            <i class="{badge.icon} text-sm"></i>
            <span>{badge.text}</span>
          </span>
        {/each}
      {:else if p.placeholder}
        <span class="opacity-60">{p.placeholder}</span>
      {/if}
    {/snippet}
    <OptionCard
      selected={p.selected}
      title={p.title}
      description={p.description}
      {badges}
      ariaLabel={p.title}
      onclick={() => p.selectable && (onSelectPreset ?? noop)(p.id)}
    />
  {/each}

  {#if custom}
    {@const labelClass = custom.selected ? "text-default-inverted-600" : "text-default-600"}
    <div
      class="flex flex-col gap-2 p-3 rounded-large {custom.selected
        ? 'bg-default-inverted-300 text-default-inverted-800'
        : 'bg-surface-inset text-default-800'}"
    >
      <button
        type="button"
        class="text-left flex flex-col gap-1.5 cursor-pointer outline-none"
        onclick={() => (onSelectCustom ?? noop)()}
      >
        <span class="text-base font-semibold leading-tight">
          {custom.title}
        </span>
        {#if custom.description}
          <HelpText
            text={custom.description}
            variant="compact"
            class={custom.selected ? "text-default-inverted-500" : "text-default-500"}
          />
        {/if}
      </button>
      <div class="flex flex-col gap-1">
        <span class="text-xs font-medium {labelClass}">Model</span>
        <Select
          value={custom.model.value}
          options={custom.model.options}
          onchange={(v) => (onModelSelect ?? noop)(v)}
          ariaLabel="Choose a model"
        />
      </div>
      {#if custom.quant}
        <div class="flex flex-col gap-1">
          <span class="text-xs font-medium {labelClass}">Quantization</span>
          <Select
            value={custom.quant.value}
            options={custom.quant.options}
            onchange={(v) => (onQuantSelect ?? noop)(v)}
            ariaLabel="Choose a quantization"
          />
        </div>
      {/if}
    </div>
  {/if}
</div>
