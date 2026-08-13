<script lang="ts">
  import { CSS_EASING } from "../../animations.ts";
  import { useUiContext } from "../../context.ts";

  // The settings switch: a bg-surface-inset groove with an inverted knob that
  // clip-path slides to the selected cell (binary on/off labels, or a segmented
  // N-way set of options). Shared so client and website render switches
  // identically; the slide duration comes from the UI context (settings-aware
  // client, BASE_MS website). Hover bits use `hov:` for demo-cursor parity.
  const ui = useUiContext();

  type Option = { value: string; label: string };

  let {
    checked = false,
    onchange,
    labels = { on: "on", off: "off" },
    options,
    value,
    onselect,
    disabled = false,
    compact = false,
    ariaLabel,
    class: extraClass = "",
  }: {
    checked?: boolean;
    onchange?: (value: boolean) => void;
    labels?: { on: string; off: string };
    options?: Option[];
    value?: string;
    onselect?: (value: string) => void;
    disabled?: boolean;
    compact?: boolean;
    ariaLabel?: string;
    class?: string;
  } = $props();

  let groupEl: HTMLDivElement | undefined = $state();

  const count = $derived(options?.length ?? 2);
  const selectedIndex = $derived(
    options ? options.findIndex((o) => o.value === value) : checked ? 1 : 0,
  );
  const knobStyle = $derived.by(() => {
    const pad = "0.25rem";
    const cell = `(100% - 2 * ${pad}) / ${count}`;
    const clip =
      selectedIndex < 0
        ? `inset(${pad} calc(100% - ${pad}) ${pad} ${pad})`
        : `inset(${pad} calc(${pad} + ${cell} * ${count - 1 - selectedIndex}) ${pad} calc(${pad} + ${cell} * ${selectedIndex}) round var(--rounded-medium))`;
    return `clip-path: ${clip}; transition: clip-path ${ui.animationDurationMs()}ms ${CSS_EASING};`;
  });
  const cellClass = $derived(
    `flex-1 min-w-0 flex items-center justify-center px-2 uppercase tracking-wide ${
      compact ? "text-[0.625rem]" : "text-xs"
    }`,
  );

  function onKeydown(e: KeyboardEvent): void {
    if (!options) return;
    const idx = options.findIndex((o) => o.value === value);
    if (idx < 0) return;
    let next = idx;
    if (e.key === "ArrowRight" || e.key === "ArrowDown") next = (idx + 1) % options.length;
    else if (e.key === "ArrowLeft" || e.key === "ArrowUp") {
      next = (idx - 1 + options.length) % options.length;
    } else if (e.key === "Home") next = 0;
    else if (e.key === "End") next = options.length - 1;
    else return;
    e.preventDefault();
    if (options[next].value !== value) onselect?.(options[next].value);
    groupEl?.querySelectorAll<HTMLButtonElement>('[role="radio"]')[next]?.focus();
  }
</script>

{#snippet knob(texts: string[])}
  <span
    aria-hidden="true"
    class="absolute inset-0 flex p-1 bg-default-inverted-300 text-default-inverted-900 pointer-events-none"
    style={knobStyle}
  >
    {#each texts as text, i (i)}
      <span class={cellClass}>{text}</span>
    {/each}
  </span>
{/snippet}

{#if options}
  <div
    bind:this={groupEl}
    class="groove relative flex w-full h-8 rounded-medium bg-surface-inset p-1 {disabled
      ? 'opacity-60 pointer-events-none'
      : ''} {extraClass}"
    role="radiogroup"
    aria-label={ariaLabel}
    tabindex={-1}
    onkeydown={onKeydown}
  >
    {#each options as opt (opt.value)}
      <button
        type="button"
        role="radio"
        aria-checked={value === opt.value}
        tabindex={value === opt.value ? 0 : -1}
        class="{cellClass} text-default-600 transition-interactive hov:text-default-800 act:text-default-900 hov:cursor-pointer"
        onclick={() => onselect?.(opt.value)}
      >
        {opt.label}
      </button>
    {/each}
    {@render knob(options.map((o) => o.label))}
  </div>
{:else}
  <label
    class="relative flex w-full items-center cursor-pointer {disabled
      ? 'opacity-60 pointer-events-none'
      : ''} {extraClass}"
  >
    <input
      type="checkbox"
      aria-label={ariaLabel}
      class="sr-only"
      {checked}
      {disabled}
      onchange={(e) => onchange?.((e.target as HTMLInputElement).checked)}
    />
    <span
      class="groove relative flex w-full h-8 rounded-medium bg-surface-inset p-1 text-default-600"
    >
      <span class={cellClass}>{labels.off}</span>
      <span class={cellClass}>{labels.on}</span>
      {@render knob([labels.off, labels.on])}
    </span>
  </label>
{/if}

<style>
  /* The selected control sits under the opaque knob, where the UA focus ring
     would be painted over; resurface keyboard focus on the groove itself. */
  .groove:has(:focus-visible),
  label:has(:focus-visible) .groove {
    outline: var(--focus-ring);
    outline-offset: var(--focus-ring-offset);
  }
</style>
