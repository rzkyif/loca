import type { ComponentProps } from "svelte";
import type { OmitSnippetProps } from "./types.ts";
import type ObjectManagerView from "../components/objects/ObjectManagerView.svelte";

// The generic master/detail shell: search toolbar, an infinite-scroll list of
// rows, and a list<->detail swap. The `card`/`detail`/`empty` snippets are
// supplied by the renderer; these samples drive the surrounding chrome and the
// list/detail/empty states with plain data. (T is generic, so the registered
// type resolves the item to `unknown`; the scripted items below are structural
// stand-ins for the client's domain objects.)
const items = [
  { id: "a", name: "Code Search" },
  { id: "b", name: "filesystem" },
  { id: "c", name: "Daily summary" },
];

export const objectManagerSamples = {
  populated: {
    items,
    searchPlaceholder: "Search extensions",
    hasMenu: true,
  },
  empty: {
    items: [],
    searchPlaceholder: "Search extensions",
    emptyVisible: true,
  },
  loading: {
    items,
    searchPlaceholder: "Search extensions",
    loading: true,
  },
  selected: {
    items,
    selectedItem: items[0],
    live: items[0],
    searchPlaceholder: "Search extensions",
    hasMenu: true,
  },
  error: {
    items: [],
    searchPlaceholder: "Search extensions",
    error: "Could not reach the Core",
  },
  menuBusy: {
    items,
    searchPlaceholder: "Search extensions",
    hasMenu: true,
    menuBusy: true,
  },
  filterSort: {
    items,
    searchPlaceholder: "Search memories",
    hasFilterSort: true,
  },
} satisfies Record<string, OmitSnippetProps<ComponentProps<typeof ObjectManagerView>>>;
