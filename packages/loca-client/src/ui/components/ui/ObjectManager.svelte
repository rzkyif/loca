<script lang="ts" generics="T">
  import { onMount, type Snippet, untrack } from "svelte";
  import { errMessage } from "@loca/shared";
  import ObjectManagerView from "@loca/shared/ui/components/objects/ObjectManagerView.svelte";
  import { type ParsedQuery, parseQuery } from "$lib/objects/query";

  // Client wrapper for the object-management settings UI: owns all state
  // (search query, selection, filtering, loading, infinite-scroll, and the
  // subscribe/Esc lifecycle) and feeds ObjectManagerView the live data,
  // callbacks, and the card/detail/empty snippets. It holds ZERO domain logic;
  // each per-type field supplies data (load/idOf/subscribe), the toolbar menu
  // handlers, and the card/detail/empty snippets.
  let {
    load,
    idOf,
    getById,
    searchPlaceholder,
    subscribe,
    query = $bindable(""),
    selectedItem = $bindable(null),
    reloadKey = $bindable(0),
    hasFilterSort = false,
    onFilterSort,
    hasMenu = false,
    onMenu,
    menuBusy = false,
    card,
    detail,
    empty,
  }: {
    load: (ctx: {
      offset: number;
      limit: number;
      query: ParsedQuery;
    }) => Promise<{ items: T[]; done: boolean }>;
    idOf: (item: T) => string;
    /** Live lookup for detail freshness; omit for async-only sources (cores).
     *  When present and it returns undefined, the detail auto-closes. */
    getById?: (id: string) => T | undefined;
    searchPlaceholder: string;
    /** Reactivity bridge (WS / store change) -> reload from offset 0. */
    subscribe?: (onChange: () => void) => () => void;
    query?: string;
    /** null = list mode; an item = detail mode. Bindable so a field can open a
     *  freshly-created item, and the shell can clear it on back/Esc. */
    selectedItem?: T | null;
    /** Bump to force a reload from offset 0 (after add/delete with no subscribe). */
    reloadKey?: number;
    hasFilterSort?: boolean;
    onFilterSort?: () => void;
    hasMenu?: boolean;
    onMenu?: () => void;
    /** When true the triple-dot swaps to a spinner + disables (e.g. while a
     *  field-level action like "Check for Updates" runs). */
    menuBusy?: boolean;
    card: Snippet<[T, () => void]>;
    detail: Snippet<[T, () => void]>;
    empty: Snippet;
  } = $props();

  const PAGE = 30;

  let items = $state<T[]>([]);
  let done = $state(false);
  let loading = $state(false);
  let error = $state<string | null>(null);
  let sentinelEl = $state<HTMLElement>();
  let scrollEl = $state<HTMLElement>();

  // Discards stale async loads (the query changed or a reset happened while a
  // fetch was in flight).
  let loadSeq = 0;

  // The fresh selected item (from the store when getById is available) or the
  // snapshot captured on card click.
  const live = $derived(
    selectedItem === null ? null : getById ? getById(idOf(selectedItem)) : selectedItem,
  );
  // The selected item vanished (deleted/unpaired elsewhere): return to the list.
  $effect(() => {
    if (selectedItem !== null && getById && live === undefined) selectedItem = null;
  });

  async function loadMore() {
    if (loading || done) return;
    loading = true;
    const seq = ++loadSeq;
    try {
      const res = await load({ offset: items.length, limit: PAGE, query: parseQuery(query) });
      if (seq !== loadSeq) return; // superseded by a newer load/reset
      items = items.concat(res.items);
      done = res.done;
      error = null;
    } catch (e) {
      if (seq === loadSeq) error = errMessage(e);
    } finally {
      if (seq === loadSeq) loading = false;
    }
  }

  function reset() {
    loadSeq++; // invalidate any in-flight load
    items = [];
    done = false;
    loading = false;
    error = null;
    void loadMore();
  }

  // Reload on query (debounced) or reloadKey (immediate). Plain locals are
  // intentionally non-reactive so only query / reloadKey are effect deps.
  let started = false;
  let lastQuery = untrack(() => query);
  let lastReloadKey = untrack(() => reloadKey);
  let debounceTimer: ReturnType<typeof setTimeout> | null = null;
  $effect(() => {
    const q = query;
    const rk = reloadKey;
    if (!started) {
      started = true;
      lastQuery = q;
      lastReloadKey = rk;
      reset();
      return;
    }
    if (rk !== lastReloadKey) {
      lastReloadKey = rk;
      lastQuery = q;
      reset();
      return;
    }
    if (q !== lastQuery) {
      lastQuery = q;
      // Editing the query (typing, clearing, or inserting a filter/sort token)
      // exits detail back to the list. Field-level actions (the triple-dot)
      // don't touch the query, so they leave the detail open.
      selectedItem = null;
      if (debounceTimer) clearTimeout(debounceTimer);
      debounceTimer = setTimeout(reset, 200);
    }
  });

  // Esc returns from detail to list (mirrors Modal/Popover). Active only in
  // detail mode; stops propagation so it never closes the Settings panel.
  $effect(() => {
    if (selectedItem === null) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        selectedItem = null;
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  });

  onMount(() => {
    const unsub = subscribe?.(() => reset());
    let observer: IntersectionObserver | undefined;
    if (sentinelEl && scrollEl) {
      observer = new IntersectionObserver(
        (entries) => {
          if (entries.some((e) => e.isIntersecting)) void loadMore();
        },
        { root: scrollEl, rootMargin: "200px" },
      );
      observer.observe(sentinelEl);
    }
    return () => {
      unsub?.();
      observer?.disconnect();
      if (debounceTimer) clearTimeout(debounceTimer);
    };
  });

  const emptyVisible = $derived(items.length === 0 && !loading && !error);
</script>

<ObjectManagerView
  {items}
  {idOf}
  {selectedItem}
  live={live ?? null}
  {searchPlaceholder}
  {error}
  {emptyVisible}
  {loading}
  {hasFilterSort}
  {hasMenu}
  {menuBusy}
  bind:query
  bind:scrollEl
  bind:sentinelEl
  onClearQuery={() => (query = "")}
  onSelect={(item) => (selectedItem = item)}
  onBack={() => (selectedItem = null)}
  onFilterSort={() => onFilterSort?.()}
  onMenu={() => onMenu?.()}
  {card}
  detailPane={detail}
  {empty}
/>
