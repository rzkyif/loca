<script lang="ts" generics="T">
  import type { Snippet } from "svelte";
  import SearchInput from "../primitives/SearchInput.svelte";
  import IconButton from "../primitives/IconButton.svelte";
  import IconText from "../primitives/IconText.svelte";

  // Generic, presentational shell for the object-management settings UI:
  // search bar + optional filter/sort + optional triple-dot, a batched
  // infinite-scroll list, and a list<->detail swap. It holds ZERO state and
  // ZERO domain logic; the client wrapper owns search/selection/filtering/
  // loading and the lifecycle (subscribe, infinite-scroll observer, Esc-to-back),
  // feeding this View the live data, callbacks, and card/detail/empty snippets.
  // `scrollEl`/`sentinelEl` are bound back out so the wrapper can attach its
  // IntersectionObserver without this View running any side-effecting lifecycle.
  let {
    items,
    idOf = (item) => JSON.stringify(item),
    selectedItem = null,
    live = null,
    searchPlaceholder,
    error = null,
    emptyVisible = false,
    loading = false,
    hasFilterSort = false,
    hasMenu = false,
    menuBusy = false,
    onClearQuery,
    onSelect,
    onBack,
    onFilterSort,
    onMenu,
    card,
    detailPane,
    empty,
    query = $bindable(""),
    scrollEl = $bindable(),
    sentinelEl = $bindable(),
  }: {
    items: T[];
    /** Keys the list rows. Defaults to a structural key so samples can omit it;
     *  the client always passes a stable domain id. */
    idOf?: (item: T) => string;
    /** null = list mode; an item = detail mode (the wrapper's selection). */
    selectedItem?: T | null;
    /** The fresh selected item to render in the detail pane (or null). */
    live?: T | null;
    searchPlaceholder: string;
    error?: string | null;
    emptyVisible?: boolean;
    loading?: boolean;
    hasFilterSort?: boolean;
    hasMenu?: boolean;
    /** When true the triple-dot swaps to a spinner + disables. */
    menuBusy?: boolean;
    onClearQuery?: () => void;
    onSelect?: (item: T) => void;
    onBack?: () => void;
    onFilterSort?: () => void;
    onMenu?: () => void;
    card: Snippet<[T, () => void]>;
    detailPane: Snippet<[T, () => void]>;
    empty: Snippet;
    query?: string;
    scrollEl?: HTMLElement;
    sentinelEl?: HTMLElement;
  } = $props();

  const noop = (): void => {};
</script>

<div class="h-full flex flex-col gap-2">
  <!-- Toolbar stays visible in both list and detail mode. Editing the search
       (or inserting a filter/sort token) exits detail; the triple-dot does not. -->
  <div class="flex items-center gap-1 shrink-0">
    <div class="flex-1 min-w-0">
      <SearchInput
        bind:value={query}
        placeholder={searchPlaceholder}
        onclear={onClearQuery ?? noop}
      />
    </div>
    {#if hasFilterSort}
      <IconButton
        icon="i-material-symbols-filter-alt"
        title="Filter and sort"
        size="lg"
        surface="filled"
        rounded="rounded-large"
        onclick={() => onFilterSort?.()}
      />
    {/if}
    {#if hasMenu}
      <IconButton
        icon={menuBusy
          ? "i-material-symbols-progress-activity animate-spin"
          : "i-material-symbols-more-vert"}
        title="More actions"
        size="lg"
        surface="filled"
        rounded="rounded-large"
        disabled={menuBusy}
        onclick={() => onMenu?.()}
      />
    {/if}
  </div>

  <!-- Only this area swaps between list and detail. The list stays mounted but
       hidden under detail so its scroll / query / loaded pages survive. -->
  <div class="flex-1 min-h-0 flex flex-col">
    <div
      bind:this={scrollEl}
      class="loca-scroll flex-1 min-h-0 overflow-y-auto pr-2 {selectedItem !== null
        ? 'hidden'
        : ''}"
    >
      {#if error}
        <div class="px-3 py-2">
          <IconText icon="i-material-symbols-error-rounded" color="text-accent-red-700">
            {error}
          </IconText>
        </div>
      {/if}
      {#if emptyVisible}
        {@render empty()}
      {:else}
        <div class="flex flex-col">
          {#each items as item (idOf(item))}
            {@render card(item, () => onSelect?.(item))}
          {/each}
        </div>
      {/if}
      <div bind:this={sentinelEl} class="h-px"></div>
      {#if loading}
        <div class="flex justify-center py-3 text-default-500">
          <i class="i-material-symbols-progress-activity animate-spin text-lg"></i>
        </div>
      {/if}
    </div>

    {#if selectedItem !== null && live !== undefined && live !== null}
      <!-- Back button is a left gutter spanning the detail, so the title (in
           ObjectDetailHeader) and the form body below share one left edge. -->
      <div class="flex-1 min-h-0 flex gap-2">
        <IconButton
          icon="i-material-symbols-arrow-back-rounded"
          title="Back to list"
          size="md"
          surface="none"
          onclick={() => onBack?.()}
          class="self-start shrink-0"
        />
        <div class="flex flex-col gap-3 flex-1 min-w-0 min-h-0">
          {@render detailPane(live, () => onBack?.())}
        </div>
      </div>
    {/if}
  </div>
</div>
