<script lang="ts" module>
  type OptionValue = string | number;
  export type QuickSelectOption = {
    value: OptionValue;
    label: string;
    display?: string;
    disabled?: boolean;
  };
  /** One quick-access dropdown: the icon, title, and aria-label are fixed per
   *  slot by the View; the caller supplies the live value/options/handler. */
  export type QuickSelect = {
    value: OptionValue;
    options: QuickSelectOption[];
    onchange: (v: string) => void;
    ariaLabel?: string;
  };
</script>

<script lang="ts">
  import FlushSelect from "../../primitives/FlushSelect.svelte";

  // Presentational shell for the chat input's quick model controls: a left
  // column (Model, optional Quantization) and a right column (Thinking Effort,
  // Creativity). The client feeds live llm.* selections; the website showcase
  // feeds default-state selections. Layout, icons, titles, and tones live here
  // so both render identically.

  let {
    model,
    quant,
    thinking,
    creativity,
  }: {
    /** Left column. Omit to hide (e.g. external provider). Shows either the
     *  smart-preset picker or the custom-model picker; both use the same icon
     *  and "Model" title. */
    model?: QuickSelect;
    /** Optional quantization picker beside the model (custom model only). */
    quant?: QuickSelect;
    thinking: QuickSelect;
    creativity: QuickSelect;
  } = $props();
</script>

<div class="flex items-center justify-between gap-6 w-full min-w-0">
  <div class="flex items-center gap-2 min-w-0">
    {#if model}
      <FlushSelect
        icon="i-material-symbols-auto-awesome-outline-rounded"
        value={model.value}
        options={model.options}
        onchange={model.onchange}
        ariaLabel={model.ariaLabel ?? "Model"}
        title="Model"
      />
    {/if}
    {#if quant}
      <FlushSelect
        icon="i-material-symbols-bolt-outline-rounded"
        value={quant.value}
        options={quant.options}
        onchange={quant.onchange}
        ariaLabel={quant.ariaLabel ?? "Quantization"}
        title="Quantization"
      />
    {/if}
  </div>

  <div class="flex items-center gap-2 shrink-0">
    <FlushSelect
      icon="i-material-symbols-psychology-outline-rounded"
      value={thinking.value}
      options={thinking.options}
      onchange={thinking.onchange}
      ariaLabel={thinking.ariaLabel ?? "Thinking effort"}
      title="Thinking Effort"
    />
    <FlushSelect
      icon="i-material-symbols-palette-outline"
      value={creativity.value}
      options={creativity.options}
      onchange={creativity.onchange}
      ariaLabel={creativity.ariaLabel ?? "Creativity"}
      title="Creativity"
    />
  </div>
</div>
