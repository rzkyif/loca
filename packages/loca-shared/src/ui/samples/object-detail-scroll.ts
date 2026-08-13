import type { ComponentProps } from "svelte";
import type { OmitSnippetProps } from "./types.ts";
import type ObjectDetailScrollView from "../components/objects/ObjectDetailScrollView.svelte";

// The scrollable detail body. Renders the object's description (when present)
// above the type-specific content (the `children` snippet, supplied by the
// renderer). Covers with and without a description.
export const objectDetailScrollSamples = {
  withDescription: {
    description: "Searches the codebase for symbols, references, and text matches.",
  },
  noDescription: {},
} satisfies Record<string, OmitSnippetProps<ComponentProps<typeof ObjectDetailScrollView>>>;
