<script lang="ts">
  import type { Snippet } from "svelte";
  import SidebarItem from "../primitives/SidebarItem.svelte";
  import { useUiContext } from "../../context.ts";

  const ui = useUiContext();
  // The mobile group list renders each row as a tappable card (see SidebarItem),
  // so the rows need real spacing between them; desktop keeps its compact gap.
  const listGap = "gap-2";

  // THE single settings sidebar for both the client and the website
  // (single-source rule, AGENTS.md): the collapse toggle, the scrollable group
  // list (with top/bottom fades), and a pinned footer slot. The client wraps it
  // feeding live groups + the real footer (status chips, Downloads, version); the
  // website wraps it feeding the schema groups + a static footer.

  type Group = { id: string; name: string; icon: string; iconInactive?: string };

  let {
    groups,
    selectedGroupId,
    searchMode = false,
    collapsed,
    onToggleCollapse,
    onSelectGroup,
    isGroupDisabled,
    showCollapse = true,
    footer,
  }: {
    groups: Group[];
    selectedGroupId: string;
    /** When searching, no group is highlighted as selected. */
    searchMode?: boolean;
    collapsed: boolean;
    onToggleCollapse: () => void;
    onSelectGroup: (id: string) => void;
    isGroupDisabled?: (id: string) => boolean;
    /** Whether to render the collapse toggle. Hidden in the mobile stacked nav,
     *  where the list is full-screen and there is no sidebar to collapse. */
    showCollapse?: boolean;
    /** Pinned bottom of the sidebar; the boolean is the collapsed state. */
    footer?: Snippet<[boolean]>;
  } = $props();

  let scrollEl = $state<HTMLDivElement>();
  let contentEl = $state<HTMLDivElement>();
  let showTopFade = $state(false);
  let showBottomFade = $state(false);
  function updateFades(): void {
    const el = scrollEl;
    if (!el) return;
    showTopFade = el.scrollTop > 1;
    showBottomFade = el.scrollTop + el.clientHeight < el.scrollHeight - 1;
  }
  // `onscroll` keeps the fades live while scrolling; this sets their initial
  // state and recomputes when the viewport or the group list resizes (the list
  // overflows differently as groups change, the sidebar collapses, or the panel
  // resizes), so the bottom fade shows as soon as content overflows rather than
  // only after the first scroll.
  $effect(() => {
    const el = scrollEl;
    if (!el) return;
    const ro = new ResizeObserver(updateFades);
    ro.observe(el);
    if (contentEl) ro.observe(contentEl);
    updateFades();
    return () => ro.disconnect();
  });
</script>

<div class="flex flex-col gap-2 h-full min-h-0">
  <!-- h-6.5 (matches the sticky group header) so the icon's vertical center lines
       up with the group header text when scrolled to the top. -->
  {#if showCollapse}
    <button
      class="shrink-0 hov:cursor-pointer text-default-500 hov:text-default-700 hov:bg-surface-inset w-fit flex items-center gap-2 h-6.5 pl-1.5 pr-1.5 rounded-medium transition-colors"
      onclick={onToggleCollapse}
      title={collapsed ? "Expand sidebar" : "Collapse sidebar"}
      aria-label={collapsed ? "Expand sidebar" : "Collapse sidebar"}
      data-demo="collapse"
    >
      <i
        class="flex text-xl shrink-0 {collapsed
          ? 'i-material-symbols-keyboard-double-arrow-right-rounded'
          : 'i-material-symbols-keyboard-double-arrow-left-rounded'}"
      ></i>
    </button>
  {/if}

  <div class="relative flex-1 min-h-0">
    <div
      bind:this={scrollEl}
      onscroll={updateFades}
      class="loca-scroll loca-scroll-hover h-full overflow-y-auto pr-2"
    >
      <div bind:this={contentEl} class="flex flex-col {listGap}">
        {#each groups as g (g.id)}
          <!-- The mobile list pushes a full-screen detail on tap, so it carries
               no persistent "selected group" highlight (the desktop split does). -->
          {@const selected = !searchMode && selectedGroupId === g.id && ui.platform !== "mobile"}
          <SidebarItem
            icon={selected ? g.icon : (g.iconInactive ?? g.icon)}
            label={g.name}
            {collapsed}
            {selected}
            disabled={isGroupDisabled?.(g.id) ?? false}
            title={collapsed ? g.name : undefined}
            ariaLabel={g.name}
            onclick={() => onSelectGroup(g.id)}
            class="settings-group-{g.id}"
          />
        {/each}
      </div>
    </div>
    <div
      class="absolute left-0 right-0 top-0 h-6 pointer-events-none z-1 bg-gradient-to-b from-default-50 to-transparent transition-opacity duration-100 {showTopFade
        ? 'opacity-100'
        : 'opacity-0'}"
    ></div>
    <div
      class="absolute left-0 right-0 bottom-0 h-6 pointer-events-none z-1 bg-gradient-to-t from-default-50 to-transparent transition-opacity duration-100 {showBottomFade
        ? 'opacity-100'
        : 'opacity-0'}"
    ></div>
  </div>

  {#if footer}
    <div class="shrink-0 flex flex-col gap-1.5 pr-2">
      {@render footer(collapsed)}
    </div>
  {/if}
</div>
