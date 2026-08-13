import type { ComponentProps } from "svelte";
import type { OmitSnippetProps } from "./types.ts";
import type ToolsFieldView from "../components/settings/ToolsFieldView.svelte";

// The Tools list empty/error state. Covers no tools at all, an active search
// with no matches, and a load failure.
export const toolsFieldSamples = {
  empty: {},
  noMatches: {
    hasQuery: true,
  },
  loadError: {
    loadError: "Could not reach the Core.",
  },
} satisfies Record<string, OmitSnippetProps<ComponentProps<typeof ToolsFieldView>>>;
