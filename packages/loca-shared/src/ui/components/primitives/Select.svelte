<script lang="ts">
  // A native <select> styled as a recessed inset well (or invisible overlay for
  // custom triggers). Shared so client and website render selects identically.
  type Variant = "default" | "invisible";
  type OptionValue = string | number;
  type Option = { value: OptionValue; label: string; disabled?: boolean };

  let {
    value,
    options,
    onchange,
    disabled = false,
    error = false,
    variant = "default",
    ariaLabel,
    title,
    class: extraClass = "",
  }: {
    value: OptionValue;
    options: Option[];
    onchange?: (v: string) => void;
    disabled?: boolean;
    error?: boolean;
    variant?: Variant;
    ariaLabel?: string;
    title?: string;
    class?: string;
  } = $props();

  // Dropdown trigger: rest inset fill darkens one shade on hover, two on press,
  // matching the shared button interaction standard. An errored select keeps the
  // neutral fill and reads as a red inset outline (the `loca-error-ring` helper,
  // matching the field's error message), the same as an errored Input.
  const surfaceClass = "bg-surface-inset hov:bg-surface-inset-strong act:bg-default-400";
  const errorClass = $derived(error ? "loca-error-ring" : "");
</script>

{#if variant === "invisible"}
  <select
    {disabled}
    {value}
    {title}
    aria-label={ariaLabel}
    onchange={(e) => onchange?.((e.target as HTMLSelectElement).value)}
    class="absolute inset-0 w-full h-full opacity-0 cursor-pointer text-base {extraClass}"
  >
    {#each options as option (option.value)}
      <option value={option.value} disabled={option.disabled}>{option.label}</option>
    {/each}
  </select>
{:else}
  <div class="relative w-full {disabled ? 'opacity-60' : ''} {extraClass}">
    <select
      {disabled}
      {value}
      {title}
      aria-label={ariaLabel}
      onchange={(e) => onchange?.((e.target as HTMLSelectElement).value)}
      class="appearance-none {surfaceClass} {errorClass} text-default-800 rounded-medium block w-full h-8 px-2 pr-7 outline-none hov:cursor-pointer transition-interactive text-sm"
    >
      {#each options as option (option.value)}
        <option value={option.value} disabled={option.disabled}>{option.label}</option>
      {/each}
    </select>
    <i
      class="i-material-symbols-expand-more-rounded absolute right-1.5 top-1/2 -translate-y-1/2 text-default-600 pointer-events-none"
    ></i>
  </div>
{/if}
