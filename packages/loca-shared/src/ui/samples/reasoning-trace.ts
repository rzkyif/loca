import type { ComponentProps } from "svelte";
import type { OmitSnippetProps } from "./types.ts";
import type ReasoningTraceView from "../components/chat/messages/ReasoningTraceView.svelte";

export const reasoningTraceSamples = {
  done: { isStreaming: false, reasoningDurationMs: 4200, expanded: true },
  collapsed: { isStreaming: false, reasoningDurationMs: 4200, expanded: false },
  streaming: { isStreaming: true },
} satisfies Record<string, OmitSnippetProps<ComponentProps<typeof ReasoningTraceView>>>;
