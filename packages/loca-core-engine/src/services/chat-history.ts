// Transcript scan helpers shared by the chat orchestrator and the tool-dispatch
// path: locate the newest user message's text (the turn's query / a tool's
// chatContext) or its id (the turn anchor). Both scan from the tail, so
// assistant/tool messages inserted during a turn don't change the result.

import type { Message } from "@loca/shared";
import { contentToText } from "@loca/shared";

export function lastUserText(history: Message[]): string | undefined {
  for (let i = history.length - 1; i >= 0; i--) {
    const m = history[i];
    if (m.role === "user") return contentToText(m.content);
  }
  return undefined;
}

export function lastUserId(history: Message[]): string | null {
  for (let i = history.length - 1; i >= 0; i--) {
    if (history[i].role === "user") return history[i].id;
  }
  return null;
}
