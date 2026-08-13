<script lang="ts">
  import Select from "./Select.svelte";
  import { useUiContext } from "../../context.ts";

  // A dropdown that reads as part of the bubble background rather than a button
  // or card: just an icon + the selected label, with a transparent native
  // <select> overlaid (Select's "invisible" variant). Sits dim (placeholder
  // tone) until hovered, matching the input's other muted controls. Used by the
  // quick model controls, where horizontal space is tight. The `hov:` variant
  // also lights up under the demo cursor's `data-hover`, so the website showcase
  // and the app render the same hover state.

  type OptionValue = string | number;
  // `label` is shown in the open dropdown; `display` (when set) is the shorter
  // text shown collapsed in the bar.
  type Option = {
    value: OptionValue;
    label: string;
    display?: string;
    disabled?: boolean;
  };

  let {
    value,
    options,
    onchange,
    ariaLabel,
    title,
    icon,
    iconOnly = false,
    disabled = false,
    rounded = "rounded-medium",
    textClass,
    class: extraClass = "",
  }: {
    value: OptionValue;
    options: Option[];
    onchange: (v: string) => void;
    ariaLabel: string;
    title?: string;
    icon?: string;
    /** Render as an icon-only control (no selected-value label), so the dropdown
     *  reads as an icon button that opens the native picker on tap. The `icon` is
     *  centered and the invisible <select> overlays the whole square; size + fill
     *  come from `class` (e.g. an IconButton-matching "p-2 text-xl bg-..."). */
    iconOnly?: boolean;
    disabled?: boolean;
    /** Corner radius utility; override to match a surrounding pill. */
    rounded?: string;
    /** Resting + hover text tone. Defaults to the dim flush tone on a fine
     *  pointer (brightening on hover); on a coarse pointer (touch, which can't
     *  hover) it rests at the brighter shade so it never reads as too dim.
     *  Override to sit at a solid pill's text lightness (e.g. "text-default-700"). */
    textClass?: string;
    /** Extra classes on the root (e.g. an inset pill background + padding so the
     *  whole control reads as a solid pill rather than flush text). */
    class?: string;
  } = $props();

  const ui = useUiContext();
  const resolvedText = $derived(
    textClass ??
      (ui.pointer === "coarse" ? "text-default-700" : "text-default-400 hov:text-default-700"),
  );

  const current = $derived(options.find((o) => o.value === value));
</script>

{#if iconOnly}
  <div
    class="loca-focus-wrap {rounded} relative flex items-center justify-center shrink-0 transition-colors {disabled
      ? 'text-default-400 opacity-50'
      : resolvedText} {extraClass}"
    {title}
  >
    {#if icon}
      <i class="flex shrink-0 {icon}"></i>
    {/if}
    <Select variant="invisible" {value} {options} {onchange} {ariaLabel} {disabled} />
  </div>
{:else}
  <div
    class="loca-focus-wrap {rounded} relative flex items-center gap-1 min-w-0 text-sm transition-colors {disabled
      ? 'text-default-400 opacity-50'
      : resolvedText} {extraClass}"
    {title}
  >
    {#if icon}
      <i class="flex shrink-0 text-base {icon}"></i>
    {/if}
    <span class="truncate tracking-wide">
      {current?.display ?? current?.label ?? value}
    </span>
    <Select variant="invisible" {value} {options} {onchange} {ariaLabel} {disabled} />
  </div>
{/if}
