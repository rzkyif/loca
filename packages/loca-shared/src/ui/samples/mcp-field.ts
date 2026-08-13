import type { ComponentProps } from "svelte";
import type { OmitSnippetProps } from "./types.ts";
import type McpFieldView from "../components/settings/McpFieldView.svelte";

// The MCP servers list empty/error state. Covers no servers at all, an active
// search with no matches, and a load failure.
export const mcpFieldSamples = {
  empty: {},
  noMatches: {
    hasQuery: true,
  },
  loadError: {
    loadError: "Could not reach the Core.",
  },
} satisfies Record<string, OmitSnippetProps<ComponentProps<typeof McpFieldView>>>;
