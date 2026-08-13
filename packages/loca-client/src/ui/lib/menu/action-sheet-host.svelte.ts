// In-app action-sheet host backing platform().menu.showContextMenu on the touch
// shell. The desktop impl pops a native context menu and resolves with the
// chosen item id; the mobile impl has no native menu, so it presents a shared
// ActionSheet here and resolves the same way. Routing through the existing menu
// builders (chat message menu, object menus) means every context menu gains a
// mobile presentation with no per-call-site change; the call sites only need a
// long-press trigger since touch has no right-click.

import type { ContextMenuItem } from "$lib/platform";

interface SheetState {
  open: boolean;
  title?: string;
  items: ContextMenuItem[];
}

const state = $state<SheetState>({ open: false, items: [] });
let resolver: ((id: string | null) => void) | null = null;

function settle(id: string | null): void {
  const r = resolver;
  resolver = null;
  state.open = false;
  state.items = [];
  state.title = undefined;
  r?.(id);
}

export const actionSheetHost = {
  get open(): boolean {
    return state.open;
  },
  get items(): ContextMenuItem[] {
    return state.items;
  },
  get title(): string | undefined {
    return state.title;
  },
  /** Resolve the pending presentation with a chosen id, or null to dismiss. */
  select(id: string | null): void {
    settle(id);
  },
};

/** Present an in-app action sheet for the given menu items and resolve with the
 *  chosen item id (or null if dismissed). If a sheet is already open, the
 *  previous presentation is dismissed (resolves null) before this one shows. */
export function presentActionSheet(
  items: ContextMenuItem[],
  title?: string,
): Promise<string | null> {
  if (resolver) settle(null);
  state.items = items;
  state.title = title;
  state.open = true;
  return new Promise((resolve) => {
    resolver = resolve;
  });
}
