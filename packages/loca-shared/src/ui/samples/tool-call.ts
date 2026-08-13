import type { ComponentProps } from "svelte";
import type { OmitSnippetProps } from "./types.ts";
import type ToolCallView from "../components/chat/messages/ToolCallView.svelte";

// One tool call per status (including the form-free awaiting-input bubble,
// which reads yellow while its askUser form shows in the composer) plus a
// well-known memory result. `memoryContent` is a snippet prop, so the gallery
// supplies the markdown renderer; `OmitSnippetProps` strips it here.
export const toolCallSamples = {
  completed: {
    toolName: "feed_cat",
    status: "completed",
    args: { portion: "1 scoop", bowl: "kitchen" },
    result: { fed: true, dispensedGrams: 45 },
    expanded: true,
  },
  running: {
    toolName: "close_garage",
    status: "running",
    description: "Closing the garage door",
    progress: 0.4,
    expanded: true,
  },
  failed: {
    toolName: "open",
    status: "failed",
    error: "only http(s) URLs are allowed",
    expanded: true,
  },
  labelled: {
    toolName: "feed_cat",
    status: "completed",
    label: "Fed the cat",
    args: { portion: "1 scoop" },
    result: { fed: true },
    expanded: true,
  },
  cancelled: {
    toolName: "close_garage",
    status: "cancelled",
    description: "Closing the garage door",
    expanded: true,
  },
  awaiting: {
    toolName: "feed_cat",
    status: "awaiting_user",
    args: { portion: "1 scoop" },
    expanded: true,
  },
  memoryResult: {
    toolName: "show_todos",
    status: "completed",
    expanded: true,
    result: {
      kind: "memory_content",
      title: "Today",
      content: "# Today\n\n- water the plants\n- call the vet",
    },
  },
} satisfies Record<string, OmitSnippetProps<ComponentProps<typeof ToolCallView>>>;
