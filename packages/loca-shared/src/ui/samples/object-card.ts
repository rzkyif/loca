import type { ComponentProps } from "svelte";
import type { OmitSnippetProps } from "./types.ts";
import type ObjectCardView from "../components/objects/ObjectCardView.svelte";

// A flush list row: label / description / meta on the left, badges pinned top
// right and the triple-dot pinned bottom right. Covers a minimal row, a fully
// populated row with badges + meta + menu, and an error-state row.
export const objectCardSamples = {
  minimal: { label: "Untitled snippet" },
  full: {
    label: "Code Search",
    description: "Searches the codebase for symbols, references, and text matches.",
    meta: "Built-in extension",
    badges: [
      { label: "Enabled", accent: "green" },
      { label: "Updated", accent: "blue" },
    ],
    hasMenu: true,
  },
  error: {
    label: "filesystem",
    description: "Local (stdio)",
    meta: "node ./server.js",
    badges: [{ label: "Error", accent: "red", title: "Failed to start" }],
    hasMenu: true,
  },
} satisfies Record<string, OmitSnippetProps<ComponentProps<typeof ObjectCardView>>>;
