/**
 * Per-message expansion state, keyed by `msg.id`. Shared between the small
 * bubble components (their root `<Expandable>` binds to this map) and
 * `+page.svelte`, which reads it to decide whether a small bubble should
 * break the horizontal stack chain (an expanded bubble pops out of its row
 * because its content has grown).
 */

import { SvelteMap } from "svelte/reactivity";

export const expansionState = new SvelteMap<string, boolean>();

export function isExpanded(id: string | undefined, fallback = false): boolean {
  if (!id) return fallback;
  return expansionState.get(id) ?? fallback;
}
