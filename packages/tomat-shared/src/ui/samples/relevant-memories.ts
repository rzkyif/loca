import type { ComponentProps } from "svelte";
import type { OmitSnippetProps } from "./types.ts";
import type RelevantMemoriesView from "../components/chat/messages/RelevantMemoriesView.svelte";

export const relevantMemoriesSamples = {
  found: {
    defaultExpanded: true,
    memories: [
      {
        memoryId: "1",
        title: "Installing Tomat",
        score: 0.92,
        summary: "One-line installer + launch.",
      },
      { memoryId: "2", title: "Your first chat", score: 0.81 },
    ],
  },
  none: { defaultExpanded: true, memories: [] },
  collapsed: {
    defaultExpanded: false,
    memories: [{ memoryId: "1", title: "Installing Tomat", score: 0.92 }],
  },
  error: {
    defaultExpanded: true,
    status: "error",
    errorMessage: "Embedding model timed out after 30s.",
    memories: [],
  },
} satisfies Record<string, OmitSnippetProps<ComponentProps<typeof RelevantMemoriesView>>>;
