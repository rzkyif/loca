<script lang="ts">
  // Presentational body of the adaptive LLM preset picker: a "check for newer
  // models" button, one card per device-tier bucket (each with computed badges
  // and an optional "better model available" notice), and an optional Custom
  // card carrying Model + Quantization dropdowns. Every value arrives
  // pre-formatted (the client owns the fit engine, the catalog, the
  // recommendations, and the byte/label formatting), so this stays pure: props
  // in, callbacks out. The FieldCard wrapper lives in the client shell; this is
  // only ModelPresetField's own bespoke body markup.
  import OptionCard from "../primitives/OptionCard.svelte";
  import Select from "../primitives/Select.svelte";
  import HelpText from "../primitives/HelpText.svelte";
  import IconButton from "../primitives/IconButton.svelte";
  import ErrorDetailView from "../chat/messages/ErrorDetailView.svelte";

  // One badge chip inside a card (icon + pre-formatted text).
  interface Badge {
    icon: string;
    text: string;
  }

  // The "a better model is available" notice attached to a bucket card.
  interface BetterNotice {
    message: string;
    applying: boolean;
    onApply: () => void;
    onDismiss: () => void;
  }

  // One device-tier bucket card. `badges` is null while the recommendation is
  // still computing (or absent); `placeholder` then carries the muted fallback
  // line. `selectable` is false when no model fits the tier.
  interface BucketCard {
    id: string;
    title: string;
    description?: string;
    selected: boolean;
    badges: Badge[] | null;
    placeholder?: string;
    selectable: boolean;
    better?: BetterNotice;
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
    checkLabel,
    checkIcon,
    checkDisabled = false,
    buckets = [],
    custom = null,
    onCheck,
    onSelectBucket,
    onSelectCustom,
    onModelSelect,
    onQuantSelect,
  }: {
    error?: string | null;
    checkLabel: string;
    checkIcon: string;
    checkDisabled?: boolean;
    buckets?: BucketCard[];
    custom?: CustomCard | null;
    onCheck?: () => void;
    onSelectBucket?: (id: string) => void;
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

  <button
    type="button"
    class="flex items-center gap-1 px-3 py-2 rounded-large bg-surface-inset text-xs text-default-600 hover:text-default-800 cursor-pointer outline-none"
    onclick={() => !checkDisabled && (onCheck ?? noop)()}
    aria-label="Check for newer models"
  >
    <i class="{checkIcon} text-sm"></i>
    <span>{checkLabel}</span>
  </button>

  {#each buckets as b (b.id)}
    {#snippet badges()}
      {#if b.badges}
        {#each b.badges as badge (badge.icon)}
          <span class="inline-flex items-center gap-1">
            <i class="{badge.icon} text-sm"></i>
            <span>{badge.text}</span>
          </span>
        {/each}
      {:else if b.placeholder}
        <span class="opacity-60">{b.placeholder}</span>
      {/if}
    {/snippet}
    <OptionCard
      selected={b.selected}
      title={b.title}
      description={b.description}
      {badges}
      ariaLabel={b.title}
      onclick={() => b.selectable && (onSelectBucket ?? noop)(b.id)}
    />
    {#if b.better}
      <!-- The "better model available" notice renders in the same inverted color
           as a selected preset card so it reads as belonging to the bucket above
           it rather than as a detached info banner. -->
      <div
        class="flex items-center gap-2 p-3 rounded-large bg-default-inverted-300 text-default-inverted-800"
      >
        <span class="flex-1 text-sm">{b.better.message}</span>
        <IconButton
          icon={b.better.applying ? "i-line-md:loading-loop" : "i-material-symbols-check-rounded"}
          title="Apply"
          size="sm"
          colorClass="text-default-inverted-600 hov:text-default-inverted-900"
          onclick={b.better.onApply}
        />
        <IconButton
          icon="i-material-symbols-close-rounded"
          title="Dismiss"
          size="sm"
          colorClass="text-default-inverted-600 hov:text-default-inverted-900"
          onclick={b.better.onDismiss}
        />
      </div>
    {/if}
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
