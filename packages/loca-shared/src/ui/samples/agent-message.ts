import type { ComponentProps } from "svelte";
import type { OmitSnippetProps } from "./types.ts";
import type AgentMessageView from "../components/chat/messages/AgentMessageView.svelte";

// The reasoning/answer copy the gallery and chat demos render into the `body`
// snippet. Exported so a renderer can feed the matching prose per scenario.
export const AGENT_REASONING =
  "The user is on macOS, so the one-line installer script is the simplest path; point them at the landing page command and the launch step.";
export const AGENT_ANSWER =
  "Run the one-line installer from the landing page, then launch Loca from Applications. Want the per-OS steps?";

export const agentMessageSamples = {
  content: { kind: "content", bgClass: "bubble-agent" },
  active: { kind: "content", bgClass: "bubble-agent", active: true },
  reasoningDone: {
    kind: "reasoning",
    isStreaming: false,
    reasoningDurationMs: 3200,
  },
  reasoningStreaming: { kind: "reasoning", isStreaming: true },
} satisfies Record<string, OmitSnippetProps<ComponentProps<typeof AgentMessageView>>>;
