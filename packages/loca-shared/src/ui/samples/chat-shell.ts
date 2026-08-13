import type { ComponentProps } from "svelte";
import type { OmitSnippetProps } from "./types.ts";
import type ChatShellView from "../components/chat/ChatShellView.svelte";

// The shell's regions (core bar, session bar, composer, transcript) are snippet
// props the gallery supplies, so the only data prop is the desktop z-stacking
// base. A realistic transcript has a handful of rows.
export const chatShellSamples = {
  default: {
    stackDepth: 3,
  },
} satisfies Record<string, OmitSnippetProps<ComponentProps<typeof ChatShellView>>>;
