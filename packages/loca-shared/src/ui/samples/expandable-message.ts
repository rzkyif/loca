import type { ComponentProps } from "svelte";
import type { OmitSnippetProps } from "./types.ts";
import type ExpandableMessageView from "../components/chat/messages/ExpandableMessageView.svelte";

export const expandableMessageSamples = {
  systemPrompt: {
    title: "System Prompt",
    text: "You are a helpful local assistant running entirely on the user's machine.",
    applyColorOverride: true,
  },
  automated: {
    title: "Automated Prompt",
    text: "Summarize today's unread notifications and list anything urgent.",
  },
  expanded: {
    title: "Content",
    text: "Tool-provided content shown inline, expanded by default.",
    defaultExpanded: true,
  },
} satisfies Record<string, OmitSnippetProps<ComponentProps<typeof ExpandableMessageView>>>;
