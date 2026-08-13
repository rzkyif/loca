<script lang="ts">
  import type { Snippet } from "svelte";
  import type { SettingField, SettingOption } from "../../../domain/settings/types.ts";
  import { displayToStored, isValidColor, storedToDisplay } from "../../color.ts";
  import FormField from "../primitives/FormField.svelte";
  import Input from "../primitives/Input.svelte";
  import Select from "../primitives/Select.svelte";
  import Toggle from "../primitives/Toggle.svelte";
  import Slider from "../primitives/Slider.svelte";
  import OptionCard from "../primitives/OptionCard.svelte";

  // THE single field-row renderer for both the client and the website (see the
  // single-source UI rule in AGENTS.md). It owns the markup for every
  // presentational field type (boolean, select, text/number, color, slider); the
  // client wraps it feeding live value/onChange/error/editable and the
  // interactive bits (color picker open, resolved select options), the website
  // wraps it feeding schema defaults. Types that need heavy client-only behavior
  // (preset cards, object managers, services/storage panels, shortcut capture,
  // multiline editors) are injected via the `complexField` snippet; with no
  // snippet (website) a static stand-in renders, since those are not shown in the
  // showcase.

  const noop = (): void => {};

  let {
    field,
    value,
    onChange = noop,
    error = null,
    editable = true,
    horizontal = false,
    showReset = false,
    onReset,
    isDark = false,
    onOpenColorPicker,
    selectOptions,
    placeholder,
    complexField,
  }: {
    field: SettingField;
    value: unknown;
    onChange?: (value: unknown) => void;
    error?: string | null;
    editable?: boolean;
    horizontal?: boolean;
    showReset?: boolean;
    onReset?: () => void;
    /** Dark theme, so the color swatch/input render the theme-inverted preview. */
    isDark?: boolean;
    /** Open the client color picker anchored on the swatch (client only). */
    onOpenColorPicker?: (anchor: HTMLElement) => void;
    /** Resolved options for a select whose options are runtime-sourced
     *  (monitors / fonts / tts voices). Falls back to the field's static options. */
    selectOptions?: SettingOption[];
    /** Placeholder override (e.g. the password "saved" hint). */
    placeholder?: string;
    /** Renders a type the shared view delegates (preset*, object_management,
     *  services, storage, command_preview, shortcut, multiline). */
    complexField?: Snippet<[SettingField]>;
  } = $props();

  const DELEGATED = new Set([
    "preset",
    "model_preset",
    "stt_preset",
    "tts_preset",
    "object_management",
    "services",
    "storage",
    "command_preview",
    "shortcut",
    "multiline",
  ]);
  const isDelegated = $derived(DELEGATED.has(field.type));

  const inputType = $derived(
    field.type === "password"
      ? "password"
      : field.type === "number" || field.type === "float"
        ? "number"
        : "text",
  );
  const isNumeric = $derived(field.type === "number" || field.type === "float");

  function defaultSelectOptions(): SettingOption[] {
    if (field.type !== "select") return [];
    if (field.optionsSource === "monitors") return [{ value: "primary", label: "Primary Monitor" }];
    if (field.optionsSource === "fonts") return [{ value: "default", label: "Default" }];
    if (field.optionsSource === "tts_voices")
      return [{ value: String(value), label: String(value) }];
    return field.options ?? [];
  }
  const resolvedOptions = $derived(selectOptions ?? defaultSelectOptions());

  // --- preset stand-in (website only; client injects real cards) ---
  const presetCards = $derived(
    field.type === "preset" ||
      field.type === "model_preset" ||
      field.type === "stt_preset" ||
      field.type === "tts_preset"
      ? [...field.presetConfig.options, ...(field.presetConfig.secondaryOptions ?? [])]
      : [],
  );
  const shortcutSegments = $derived(
    field.type === "shortcut" && typeof value === "string" && value ? value.split("+") : [],
  );

  // --- color ---
  // Gate on the color type: this derived runs for every field, so a non-color
  // value like a slider's `8` must not reach `storedToDisplay` (it would parse
  // it as a hex and throw `invalid hex: 8`).
  const isColor = $derived(field.type === "color");
  const lockedLightness = $derived(field.type === "color" ? field.lockedLightness : undefined);
  const displayedColor = $derived(
    isColor ? storedToDisplay(String(value ?? ""), isDark, lockedLightness) : "",
  );
  let colorInput = $state("");
  let colorFocused = $state(false);
  let colorInvalid = $state(false);
  let swatchEl = $state<HTMLButtonElement>();
  $effect(() => {
    if (!colorFocused) colorInput = displayedColor;
  });
  function commitColor(displayed: string): void {
    onChange(displayToStored(displayed, isDark, lockedLightness));
  }
</script>

{#if isDelegated}
  {#if complexField}
    {@render complexField(field)}
  {:else if field.type === "preset" || field.type === "model_preset" || field.type === "stt_preset" || field.type === "tts_preset"}
    <div class="flex flex-col gap-2">
      {#each presetCards as opt (opt.id)}
        <OptionCard
          selected={opt.id === value}
          icon={opt.icon}
          title={opt.title ?? opt.label}
          description={opt.description}
          onclick={noop}
          ariaLabel={opt.title ?? opt.label}
        />
      {/each}
    </div>
  {:else if field.type === "object_management"}
    <div
      class="bg-surface-inset rounded-large p-4 text-sm text-default-500 min-h-24 flex items-center justify-center"
    >
      {field.name}
    </div>
  {:else if field.type === "shortcut"}
    <FormField label={field.name} description={field.description} {horizontal}>
      <div
        class="flex-1 min-h-8 px-2 py-1 rounded-medium flex flex-row items-center gap-1 flex-wrap bg-surface-inset"
      >
        {#if shortcutSegments.length === 0}
          <span class="text-default-500 text-sm italic">Disabled</span>
        {:else}
          {#each shortcutSegments as seg, i (i)}
            {#if i > 0}<span class="text-default-500 text-xs">+</span>{/if}
            <kbd
              class="bg-surface-inset-strong text-default-800 px-1.5 py-0.5 rounded text-xs font-mono uppercase tracking-wide"
            >
              {seg}
            </kbd>
          {/each}
        {/if}
      </div>
    </FormField>
  {:else if field.type === "multiline"}
    <FormField label={field.name} description={field.description} {horizontal}>
      <div
        class="bg-surface-inset rounded-medium px-3 py-2 text-sm whitespace-pre-wrap break-words max-h-40 overflow-hidden {field.mono
          ? 'font-mono'
          : ''}"
      >
        {String(value ?? "")}
      </div>
    </FormField>
  {:else}
    <FormField label={field.name} description={field.description} {horizontal}>
      <div class="bg-surface-inset rounded-medium px-3 py-2 text-sm text-default-500">
        {field.name}
      </div>
    </FormField>
  {/if}
{:else}
  <FormField
    fieldId={field.id}
    label={field.name}
    description={field.description}
    descriptionTier={field.descriptionTier}
    {horizontal}
    {error}
    {showReset}
    {onReset}
  >
    {#if field.type === "boolean"}
      <Toggle
        checked={Boolean(value)}
        disabled={!editable}
        ariaLabel={field.name}
        onchange={(v) => onChange(v)}
      />
    {:else if field.type === "select"}
      <Select
        value={value as string | number}
        options={resolvedOptions}
        disabled={!editable}
        ariaLabel={field.name}
        onchange={(v) => onChange(v)}
      />
    {:else if field.type === "number_slider"}
      <Slider
        value={Number(value)}
        min={field.min}
        max={field.max}
        step={field.step ?? 1}
        pairedInput
        suffix={field.suffix}
        disabled={!editable}
        error={!!error}
        ariaLabel={field.name}
        onchange={(v) => onChange(v)}
      />
    {:else if field.type === "color"}
      <div
        class="flex flex-row items-center gap-2 w-full {!editable
          ? 'opacity-60 pointer-events-none'
          : ''}"
      >
        <button
          bind:this={swatchEl}
          type="button"
          class="alpha-checkerboard relative h-8 w-8 border-4 border-default-300 shrink-0 rounded-medium overflow-hidden {onOpenColorPicker
            ? 'cursor-pointer'
            : ''}"
          title="Open color picker"
          aria-label="Open color picker"
          disabled={!editable}
          onclick={() => swatchEl && onOpenColorPicker?.(swatchEl)}
        >
          <span class="absolute inset-[-4px]" style:background-color={displayedColor}></span>
        </button>
        <Input
          value={colorInput}
          placeholder="oklch(L C H / A) or #hex"
          maxlength={40}
          spellcheck={false}
          autocomplete="off"
          disabled={!editable}
          error={colorInvalid}
          mono
          ariaLabel="{field.name} value"
          onfocus={() => (colorFocused = true)}
          oninput={(v) => {
            colorInput = v.trim();
            if (isValidColor(colorInput)) {
              colorInvalid = false;
              commitColor(colorInput);
            } else {
              colorInvalid = true;
            }
          }}
          onblur={() => {
            colorFocused = false;
            if (!isValidColor(colorInput)) {
              colorInput = displayedColor;
              colorInvalid = false;
            }
          }}
        />
      </div>
    {:else}
      <Input
        type={inputType}
        value={value as string | number}
        spinner={isNumeric}
        step={field.type === "float" ? 0.1 : 1}
        suffix={"suffix" in field ? field.suffix : undefined}
        placeholder={placeholder ?? ("placeholder" in field ? field.placeholder : undefined)}
        disabled={!editable}
        error={!!error}
        ariaLabel={field.name}
        onchange={(v) => {
          if (isNumeric) {
            const parsed = field.type === "float" ? parseFloat(v) : parseInt(v, 10);
            onChange(Number.isFinite(parsed) ? parsed : null);
          } else {
            onChange(v);
          }
        }}
      />
    {/if}
  </FormField>
{/if}
