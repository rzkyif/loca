<script lang="ts">
  import { tick, type Snippet } from "svelte";
  import Bubble from "../primitives/Bubble.svelte";
  import SettingsHeaderView from "./SettingsHeaderView.svelte";
  import SettingsSidebarView from "./SettingsSidebarView.svelte";
  import { slideSwap } from "../../animations.ts";
  import { useUiContext } from "../../context.ts";
  import type { BubbleCorner } from "../../merge.ts";

  const noop = (): void => {};

  // A focused, reusable settings panel: a collapsible group sidebar beside a
  // scrollable content area that shows either the selected group's fields or
  // search results, with the app's vertical group-swap slide. Composed entirely
  // from the shared primitives so it renders identically to the client settings
  // UI. The website showcase drives it via the exported `selectGroup` /
  // `setSearch`; a future manual page can render a single group statically.
  const ui = useUiContext();

  type Group = { id: string; name: string; icon: string; iconInactive?: string };

  let {
    groups,
    selectedGroupId = $bindable(groups[0]?.id ?? ""),
    sidebarCollapsed = $bindable(false),
    searchValue = $bindable(""),
    searchMode = $bindable(false),
    searchEl = $bindable<HTMLInputElement | undefined>(undefined),
    searchPlaceholder = "Search settings...",
    searchDisabled = false,
    onSearchInput,
    onSearchFocus,
    onSearchClear,
    onQuickSettings = noop,
    onShare = noop,
    onClose = noop,
    isGroupDisabled,
    onReselectGroup,
    onToggleSidebar,
    sizeClass = "w-[760px] max-w-[calc(100vw-5rem)] h-80vh",
    flatCorners = [],
    belowHeader,
    groupContent,
    searchContent,
    sidebarFooter,
  }: {
    groups: Group[];
    selectedGroupId?: string;
    sidebarCollapsed?: boolean;
    searchValue?: string;
    searchMode?: boolean;
    searchEl?: HTMLInputElement;
    searchPlaceholder?: string;
    searchDisabled?: boolean;
    onSearchInput?: (v: string) => void;
    onSearchFocus?: (e: FocusEvent) => void;
    onSearchClear?: () => void;
    onQuickSettings?: () => void;
    onShare?: () => void;
    onClose?: () => void;
    isGroupDisabled?: (id: string) => boolean;
    /** Fired when the already-selected group is re-clicked (no slide). The
     *  client uses it to restore that group's default section expand state. */
    onReselectGroup?: (id: string) => void;
    /** Overrides the sidebar collapse toggle. When omitted, `sidebarCollapsed`
     *  is toggled internally; the client passes this to persist it as a setting
     *  (with scroll anchoring). */
    onToggleSidebar?: () => void;
    /** Sizing for the panel Bubble (default: the app's 760px x 80vh). */
    sizeClass?: string;
    /** Corners to square off when the CoreBar merges flush onto the panel's
     *  bottom (the settings view pins it there); defaults to none. Desktop only. */
    flatCorners?: BubbleCorner[];
    /** Rendered between the header row and the sidebar/content split (e.g. the
     *  multi-core picker). */
    belowHeader?: Snippet;
    /** Renders the fields for a group id. */
    groupContent: Snippet<[string]>;
    /** Renders the search results. */
    searchContent?: Snippet;
    /** Pinned bottom of the sidebar (status chips, Downloads, version). The
     *  boolean is the current collapsed state, so rows can collapse to icons. */
    sidebarFooter?: Snippet<[boolean]>;
  } = $props();

  let layerEl: HTMLDivElement | undefined = $state();
  let scrollEl: HTMLDivElement | undefined = $state();
  let showTopFade = $state(false);
  let showBottomFade = $state(false);
  let transitioning = false;

  // Mobile uses Android-style nested navigation instead of the side-by-side
  // sidebar + content: a full-screen group LIST, and tapping a group pushes its
  // fields as a DETAIL screen with a back affordance. `mobileDetail` tracks
  // which screen is showing. The single-source rule keeps both layouts in this
  // one shared shell; the desktop split is untouched.
  const stacked = $derived(ui.platform === "mobile");
  let mobileDetail = $state(false);

  // While a group detail or the search results are open, the host's back gesture
  // (Android) returns to the group list first, instead of leaving Settings
  // outright. Registered through the UiContext bridge so this shared shell never
  // reaches into client back state; inert on the website and on desktop.
  $effect(() => {
    if (!stacked || (!mobileDetail && !searchMode)) return;
    return ui.registerBack(() => {
      mobileBack();
      return true;
    });
  });

  function updateFades(): void {
    const el = scrollEl;
    if (!el) return;
    showTopFade = el.scrollTop > 1;
    showBottomFade = el.scrollTop + el.clientHeight < el.scrollHeight - 1;
  }

  /** On mobile, return from a group's detail screen to the group list. */
  export function mobileBack(): void {
    mobileDetail = false;
    searchMode = false;
  }

  /** Slide to a group (or out of search). Vertical swap mirroring the app. */
  export async function selectGroup(id: string): Promise<void> {
    if (transitioning) return;
    // Mobile: tapping a group in the list pushes its detail screen (no slide;
    // the list/detail swap is a screen change, not the desktop vertical slide).
    if (stacked) {
      selectedGroupId = id;
      searchMode = false;
      searchValue = "";
      mobileDetail = true;
      await tick();
      if (scrollEl) scrollEl.scrollTop = 0;
      updateFades();
      return;
    }
    // Re-clicking the active group (when not searching) is not a slide; let the
    // client restore that group's default section state instead.
    if (id === selectedGroupId && !searchMode) {
      onReselectGroup?.(id);
      return;
    }
    transitioning = true;
    const fromIdx = searchMode ? -1 : groups.findIndex((g) => g.id === selectedGroupId);
    const toIdx = groups.findIndex((g) => g.id === id);
    // Earlier group leaves upward (outSign 1), later group downward.
    const outSign: 1 | -1 = toIdx < fromIdx ? 1 : -1;
    await slideSwap(layerEl, {
      axis: "y",
      outSign,
      durationMs: ui.animationDurationMs(),
      swap: async () => {
        searchMode = false;
        searchValue = "";
        selectedGroupId = id;
        await tick();
        if (scrollEl) scrollEl.scrollTop = 0;
        updateFades();
      },
    });
    transitioning = false;
  }

  /** Enter or leave search-results mode with the same vertical swap. */
  export async function setSearch(active: boolean): Promise<void> {
    if (transitioning || searchMode === active) return;
    transitioning = true;
    await slideSwap(layerEl, {
      axis: "y",
      outSign: active ? 1 : -1,
      durationMs: ui.animationDurationMs(),
      swap: async () => {
        searchMode = active;
        await tick();
        if (scrollEl) scrollEl.scrollTop = 0;
        updateFades();
      },
    });
    transitioning = false;
  }

  /** The scroll viewport, so a caller can animate `scrollTop`. */
  export function getScrollEl(): HTMLDivElement | undefined {
    return scrollEl;
  }

  /** Snap back to the initial state with NO animation (used by the showcase when
   *  re-locking a stage to time 0). Synchronous, so it never collides with the
   *  `transitioning` guard the way replaying `selectGroup`/`setSearch` would. */
  export function reset(): void {
    transitioning = false;
    searchMode = false;
    searchValue = "";
    selectedGroupId = groups[0]?.id ?? "";
    if (layerEl) {
      layerEl.style.transition = "";
      layerEl.style.transform = "";
    }
    if (scrollEl) scrollEl.scrollTop = 0;
    showTopFade = false;
    showBottomFade = false;
  }
</script>

{#if stacked}
  <!-- Mobile fills the whole frame as a plain surface (mobile bubbles carry no
       shadow/halo, and Bubble's intrinsic w-fit would collapse the height chain
       a full-screen scroll needs). Slim, equal p-3 like the other mobile
       screens. The desktop panel keeps the Bubble. -->
  <div class="flex flex-col gap-3 overflow-hidden w-full h-full p-3 bg-surface relative">
    {@render shellBody()}
  </div>
{:else}
  <Bubble
    selectedAlignment={ui.getAlignment()}
    extraClass="flex flex-col gap-3 overflow-hidden {sizeClass} relative"
    {flatCorners}
  >
    {@render shellBody()}
  </Bubble>
{/if}

{#snippet shellBody()}
  <SettingsHeaderView
    bind:searchValue
    bind:searchEl
    {searchPlaceholder}
    {searchDisabled}
    {onSearchInput}
    {onSearchFocus}
    {onSearchClear}
    {onQuickSettings}
    {onShare}
    {onClose}
  />

  {#if belowHeader}
    {@render belowHeader()}
  {/if}

  {#if stacked}
    <!-- Mobile nested navigation: the group LIST, a group's DETAIL screen, or
         full-screen search results. The group detail's back affordance lives in
         the sticky group header (see SettingsContentView onBack), so only search
         (which has no group header) needs its own back row. -->
    <div class="flex flex-1 overflow-hidden min-h-0 -mr-2">
      {#if searchMode}
        <div class="flex flex-col flex-1 min-h-0 min-w-0">
          <!-- Search has no group header to host the back button. Where the OS
               owns back (Android) the system gesture leaves search (see the
               registerBack effect above); elsewhere it gets its own back row,
               otherwise clearing the field would be the only escape. -->
          {#if !ui.hasSystemBack}
            <button
              type="button"
              class="flex items-center gap-2 px-1 h-11 text-default-700 transition-interactive hov:text-default-900"
              onclick={mobileBack}
            >
              <i class="i-material-symbols-arrow-back-rounded text-xl"></i>
              <span class="font-medium">Search</span>
            </button>
          {/if}
          {@render contentArea()}
        </div>
      {:else if mobileDetail}
        <div class="flex flex-col flex-1 min-h-0 min-w-0">
          {@render contentArea()}
        </div>
      {:else}
        <div class="flex-1 min-h-0 min-w-0">
          <SettingsSidebarView
            {groups}
            {selectedGroupId}
            {searchMode}
            collapsed={false}
            onToggleCollapse={noop}
            onSelectGroup={selectGroup}
            {isGroupDisabled}
            showCollapse={false}
            footer={sidebarFooter}
          />
        </div>
      {/if}
    </div>
  {:else}
    <div class="flex flex-1 overflow-hidden min-h-0 -mr-2 gap-3">
      <!-- Sidebar -->
      <SettingsSidebarView
        {groups}
        {selectedGroupId}
        {searchMode}
        collapsed={sidebarCollapsed}
        onToggleCollapse={onToggleSidebar ?? (() => (sidebarCollapsed = !sidebarCollapsed))}
        onSelectGroup={selectGroup}
        {isGroupDisabled}
        footer={sidebarFooter}
      />

      {@render contentArea()}
    </div>
  {/if}
{/snippet}

{#snippet contentArea()}
  <!-- Content -->
  <div class="relative flex-1 min-h-0 min-w-0">
    <!-- overflow-y-scroll (not -auto): the 5px .loca-scroll bar takes layout
         space, so switching between a group tall enough to scroll and one that
         is not would add/remove that 5px and reflow the whole panel (a visible
         shift mid group-change). Always reserving the gutter keeps the width
         constant; the track is transparent and the thumb only paints when
         scrollable, so a short group shows nothing in the reserved strip. -->
    <div
      class="loca-scroll overflow-y-scroll pr-2 h-full"
      bind:this={scrollEl}
      onscroll={updateFades}
      data-demo="content"
    >
      <!-- h-full so the layer is exactly the scroll viewport: the group-change
             slide (translateY 100% of this layer) travels one panel height, and
             a group that fills the viewport (an object-management manager) can
             establish a definite height chain down to its own inner scroller.
             Taller groups overflow this layer (visible) and the viewport scrolls. -->
      <div bind:this={layerEl} class="h-full">
        {#if searchMode}
          {@render searchContent?.()}
        {:else}
          {@render groupContent(selectedGroupId)}
        {/if}
      </div>
    </div>
    <div
      class="absolute left-0 right-0 top-0 h-6 pointer-events-none z-1 bg-gradient-to-b from-default-50 to-transparent transition-opacity duration-100 {showTopFade &&
      searchMode
        ? 'opacity-100'
        : 'opacity-0'}"
    ></div>
    <div
      class="absolute left-0 right-0 bottom-0 h-6 pointer-events-none z-1 bg-gradient-to-t from-default-50 to-transparent transition-opacity duration-100 {showBottomFade
        ? 'opacity-100'
        : 'opacity-0'}"
    ></div>
  </div>
{/snippet}
