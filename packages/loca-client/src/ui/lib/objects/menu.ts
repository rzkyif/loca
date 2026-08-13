/**
 * Native context menus for the object-management UI (the per-card and
 * field-level triple-dot, and the filter/sort button). Mirrors the chat
 * message-bubble menus in `message-menu.ts`: build a `ContextMenuItem[]` +
 * route the chosen id back to a callback via `platform().menu.showContextMenu`,
 * gated by `isTauri()` (no-op on the web build, like the message menus).
 */

import { type ContextMenuItem, platform } from "$lib/platform";
import { isTauri } from "$lib/util/env";
import { parseQuery, setSortToken, toggleFilterToken } from "./query.ts";

/** One row of an action menu (per-card actions or a field-level triple-dot). */
export interface MenuRow {
  id: string;
  label: string;
  disabled?: boolean;
  onSelect: () => void;
}

/** A named group of filter options shown under a (disabled) header. */
export interface FilterGroup {
  label?: string;
  options: { token: string; label: string }[];
}

/** One sort option; clicking inserts/replaces the single `@sort:<value>` token. */
export interface SortOption {
  value: string;
  label: string;
}

/** Show an action menu and run the chosen row's callback. */
export async function showObjectActionMenu(rows: MenuRow[]): Promise<void> {
  if (!isTauri() || rows.length === 0) return;
  const items: ContextMenuItem[] = rows.map((r) => ({
    id: r.id,
    label: r.label,
    enabled: !r.disabled,
  }));
  const chosen = await platform().menu.showContextMenu(items);
  if (chosen) rows.find((r) => r.id === chosen)?.onSelect();
}

/** Show the filter/sort menu: filters and the active sort render as checkable
 *  items under disabled group headers; the chosen id maps back to a token op. */
export async function showFilterSortMenu(opts: {
  filters: FilterGroup[];
  sorts: SortOption[];
  query: string;
  onQueryChange: (next: string) => void;
}): Promise<void> {
  if (!isTauri()) return;
  const { filters, sorts, query, onQueryChange } = opts;
  const parsed = parseQuery(query);
  const items: ContextMenuItem[] = [];
  for (const group of filters) {
    if (group.label) {
      items.push({
        id: `hdr:${group.label}`,
        label: group.label,
        enabled: false,
      });
    }
    for (const opt of group.options) {
      items.push({
        id: `filter:${opt.token}`,
        label: opt.label,
        checked: parsed.filters.has(opt.token),
      });
    }
  }
  if (filters.length > 0 && sorts.length > 0) items.push({ separator: true });
  if (sorts.length > 0) {
    items.push({ id: "hdr:sort", label: "Sort by", enabled: false });
    for (const opt of sorts) {
      items.push({
        id: `sort:${opt.value}`,
        label: opt.label,
        checked: parsed.sort === opt.value,
      });
    }
  }
  const chosen = await platform().menu.showContextMenu(items);
  if (!chosen) return;
  if (chosen.startsWith("filter:")) {
    onQueryChange(toggleFilterToken(query, chosen.slice("filter:".length)));
  } else if (chosen.startsWith("sort:")) {
    onQueryChange(setSortToken(query, chosen.slice("sort:".length)));
  }
}
