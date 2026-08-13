import type { ComponentProps } from "svelte";
import type { OmitSnippetProps } from "./types.ts";
import type ExtensionsFieldView from "../components/settings/ExtensionsFieldView.svelte";

// The extensions field's empty-state, by mode. The client owns the surrounding
// ObjectManager (list + cards + detail); this View renders only the empty
// message, so its samples are the four query-derived modes: an installed list
// with no items vs no matches, and an npm search prompt vs no results.
export const extensionsFieldSamples = {
  none: {
    mode: "none",
  },
  noMatches: {
    mode: "noMatches",
  },
  npmEmpty: {
    mode: "npmEmpty",
  },
  npmNoResults: {
    mode: "npmNoResults",
  },
} satisfies Record<string, OmitSnippetProps<ComponentProps<typeof ExtensionsFieldView>>>;
