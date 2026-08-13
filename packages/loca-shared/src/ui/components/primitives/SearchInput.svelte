<script lang="ts">
  // A search field: an inset pill with a magnifier that swaps to a clear button
  // when there is a value. Shared so the client and website render search
  // identically. The clear button uses `hov:` for demo-cursor parity.
  let {
    value = $bindable(""),
    oninput,
    onfocus,
    onclear,
    placeholder = "Search...",
    ariaLabel,
    disabled = false,
    el = $bindable<HTMLInputElement | undefined>(undefined),
    class: extraClass = "",
  }: {
    value?: string;
    oninput?: (v: string) => void;
    onfocus?: (e: FocusEvent) => void;
    /** When set, a clear button appears while `value` is non-empty; the handler
     *  runs on click and the consumer clears `value`. */
    onclear?: () => void;
    placeholder?: string;
    ariaLabel?: string;
    disabled?: boolean;
    el?: HTMLInputElement | undefined;
    class?: string;
  } = $props();
</script>

<div
  class="loca-focus-wrap relative h-10 bg-surface-inset rounded-large overflow-hidden w-full flex items-center px-4 pr-8 {extraClass}"
  class:opacity-50={disabled}
>
  <input
    bind:this={el}
    bind:value
    type="text"
    {placeholder}
    {disabled}
    aria-label={ariaLabel}
    class="bg-transparent outline-none text-base text-default-600 w-full disabled:cursor-not-allowed"
    oninput={(e) => oninput?.((e.target as HTMLInputElement).value)}
    {onfocus}
  />
  {#if value && onclear}
    <button
      class="flex absolute right-3 top-1/2 -translate-y-1/2 text-default-400 hov:text-default-600 text-lg cursor-pointer transition-colors"
      onclick={onclear}
      title="Clear search"
      aria-label="Clear search"
    >
      <i class="flex i-material-symbols-close-rounded"></i>
    </button>
  {:else}
    <i
      class="flex i-material-symbols-search-rounded absolute right-3 top-1/2 -translate-y-1/2 text-default-400 text-lg pointer-events-none"
    ></i>
  {/if}
</div>
