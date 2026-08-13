<script lang="ts" module>
  // Plain presentational shapes for the storage browser. The client owns the
  // live `StorageTree`, the IPC calls, and the selection/expansion logic; it
  // maps that tree into these display shapes (every byte size pre-formatted via
  // its `formatBytes` helper, every render flag precomputed) so this View stays
  // pure: it renders rows and emits the raw interaction back to the client,
  // which decides what is selectable, what gets deleted, and how locks apply.
  export interface StorageRowView {
    path: string;
    name: string;
    kind: "file" | "folder";
    /** Pre-formatted size (e.g. "4.7 GB"). */
    sizeText: string;
    /** This item is in use; rendered dim with a lock icon and not selectable. */
    locked: boolean;
    /** Human-readable lock reason, shown as the row tooltip. */
    lockReason?: string;
    /** Whether clicking the row toggles selection (client decides; locked rows
     *  and settings rows are never selectable, but folders still expand). */
    selectable: boolean;
    /** A folder with at least one child (gets a chevron toggle). */
    hasChildren: boolean;
    /** Direct children, rendered when this folder is expanded. */
    children: StorageRowView[];
  }

  export interface StorageCategoryView {
    id: string;
    label: string;
    /** Pre-formatted total size for the category. */
    sizeText: string;
    /** Whether the Clear/Reset action button is shown. */
    canClear: boolean;
    /** The settings category clears via a Reset icon; everything else a trash
     *  icon. Precomputed icon class + accessible labels so the View stays pure. */
    clearIcon: string;
    clearAriaLabel: string;
    clearTitle: string;
    /** Rendered with a no-row-selection cursor (settings is reset, not picked). */
    settings: boolean;
    nodes: StorageRowView[];
  }

  export interface StorageTreeView {
    categories: StorageCategoryView[];
    /** Pre-formatted grand total. */
    totalSizeText: string;
  }
</script>

<script lang="ts">
  // Full on-disk storage browser body. The provider (paired core or local
  // client) serves a tree; the client maps it to `StorageTreeView`, this renders
  // it. Category headers toggle expansion; rows select (multi/range) and folders
  // expand; the Clear button and Delete key route back to the client, which owns
  // every decision about what is safe to remove.
  import ErrorDetailView from "../chat/messages/ErrorDetailView.svelte";

  const categoryKey = (id: string): string => `__cat__:${id}`;

  let {
    loading = false,
    loadError = null,
    tree = null,
    expanded = new Set<string>(),
    selected = new Set<string>(),
    onToggle,
    onRowClick,
    onClearCategory,
    onKeyDown,
  }: {
    loading?: boolean;
    loadError?: string | null;
    tree?: StorageTreeView | null;
    /** Expanded category group keys (`__cat__:<id>`) + expanded folder paths. */
    expanded?: ReadonlySet<string>;
    /** Currently-selected node paths. */
    selected?: ReadonlySet<string>;
    onToggle?: (key: string) => void;
    onRowClick?: (e: MouseEvent, path: string, catId: string) => void;
    onClearCategory?: (catId: string) => void;
    onKeyDown?: (e: KeyboardEvent) => void;
  } = $props();

  const noop = (): void => {};
</script>

<div
  class="flex flex-col gap-0.5 outline-none"
  tabindex="0"
  role="tree"
  onkeydown={onKeyDown ?? noop}
>
  {#if loading && !tree}
    <div class="text-default-500 text-sm py-1 select-none">Loading…</div>
  {:else if loadError && !tree}
    <div class="py-1">
      <ErrorDetailView message="Couldn't load storage" detail={loadError} />
    </div>
  {:else if tree}
    {#each tree.categories as cat (cat.id)}
      {@const open = expanded.has(categoryKey(cat.id))}
      <!-- Category header: the whole row toggles expansion (like a settings
           section header). -->
      <!-- svelte-ignore a11y_click_events_have_key_events -->
      <!-- svelte-ignore a11y_no_static_element_interactions -->
      <div
        class="flex items-center gap-1 text-sm py-1 pr-2 select-none cursor-pointer"
        role="button"
        tabindex="-1"
        onclick={() => (onToggle ?? noop)(categoryKey(cat.id))}
      >
        <i
          class="flex shrink-0 w-4 justify-center text-default-500 {open
            ? 'i-material-symbols-expand-more-rounded'
            : 'i-material-symbols-chevron-right-rounded'}"
        ></i>
        <span class="text-default-800 font-medium truncate">{cat.label}</span>
        {#if cat.canClear}
          <button
            type="button"
            class="flex shrink-0 w-5 h-5 -ml-0.5 justify-center items-center text-default-500 hover:text-default-900 hover:cursor-pointer transition-colors"
            onclick={(e) => {
              e.stopPropagation();
              (onClearCategory ?? noop)(cat.id);
            }}
            aria-label={cat.clearAriaLabel}
            title={cat.clearTitle}
          >
            <i class="flex {cat.clearIcon}"></i>
          </button>
        {/if}
        <span class="flex-1"></span>
        <span class="text-default-500 text-xs tabular-nums shrink-0">{cat.sizeText}</span>
      </div>

      {#if open}
        {#if cat.nodes.length === 0}
          <div class="text-default-400 text-xs pl-5 py-1 select-none">Empty.</div>
        {/if}
        {#each cat.nodes as node (node.path)}
          {@const isSel = selected.has(node.path)}
          <!-- svelte-ignore a11y_click_events_have_key_events -->
          <!-- svelte-ignore a11y_no_static_element_interactions -->
          <div
            class="flex items-center gap-1 text-sm pl-5 pr-2 py-1 select-none {node.locked ||
            cat.settings
              ? node.kind === 'folder'
                ? 'cursor-pointer'
                : 'cursor-default'
              : 'cursor-pointer'} {node.locked ? 'opacity-60' : ''} {isSel ? 'bg-default-400' : ''}"
            title={node.lockReason ?? undefined}
            onclick={(e) => (onRowClick ?? noop)(e, node.path, cat.id)}
          >
            {#if node.kind === "folder" && node.hasChildren}
              <button
                class="flex shrink-0 w-4 justify-center text-default-500 hover:cursor-pointer"
                aria-label="Toggle folder"
                onclick={(e) => {
                  e.stopPropagation();
                  (onToggle ?? noop)(node.path);
                }}
              >
                <i
                  class="flex {expanded.has(node.path)
                    ? 'i-material-symbols-expand-more-rounded'
                    : 'i-material-symbols-chevron-right-rounded'}"
                ></i>
              </button>
            {:else}
              <span class="flex shrink-0 w-4"></span>
            {/if}
            <i
              class="flex shrink-0 w-4 justify-center {node.locked
                ? 'i-material-symbols-lock-outline'
                : node.kind === 'folder'
                  ? 'i-material-symbols-folder-outline-rounded'
                  : 'i-material-symbols-description-outline-rounded'} text-default-500"
            ></i>
            <span class="flex-1 truncate text-default-800">{node.name}</span>
            <span class="text-default-500 text-xs tabular-nums shrink-0">
              {node.sizeText}
            </span>
          </div>
          {#if node.kind === "folder" && expanded.has(node.path)}
            {#each node.children as child (child.path)}
              <!-- svelte-ignore a11y_click_events_have_key_events -->
              <!-- svelte-ignore a11y_no_static_element_interactions -->
              <div
                class="flex items-center gap-1 text-sm pl-10 pr-2 py-1 select-none {child.locked
                  ? 'opacity-60 cursor-default'
                  : 'cursor-pointer'} {selected.has(child.path) ? 'bg-default-400' : ''}"
                title={child.lockReason ?? undefined}
                onclick={(e) => (onRowClick ?? noop)(e, child.path, cat.id)}
              >
                <span class="flex shrink-0 w-4"></span>
                <i
                  class="flex shrink-0 w-4 justify-center {child.locked
                    ? 'i-material-symbols-lock-outline'
                    : 'i-material-symbols-description-outline-rounded'} text-default-500"
                ></i>
                <span class="flex-1 truncate text-default-800">{child.name}</span>
                <span class="text-default-500 text-xs tabular-nums shrink-0">
                  {child.sizeText}
                </span>
              </div>
            {/each}
          {/if}
        {/each}
      {/if}
    {/each}

    <!-- Total -->
    <div class="flex items-center gap-1 text-sm py-1 pr-2 select-none">
      <span class="flex shrink-0 w-4"></span>
      <span class="text-default-800 font-medium flex-1">Total</span>
      <span class="text-default-500 text-xs font-bold tabular-nums shrink-0">
        {tree.totalSizeText}
      </span>
    </div>
  {/if}
</div>
