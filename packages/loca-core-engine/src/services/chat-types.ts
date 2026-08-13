// Internal types shared across the chat orchestrator's collaborators
// (ChatService, TurnWriter, and the tool-dispatch path).

import type { Message, ServerToClientFrame } from "@loca/shared";

export interface ActiveStream {
  streamId: string;
  sessionId: string;
  clientId: string;
  abort: AbortController;
  // Born-but-not-yet-finalized messages of this turn, in birth order, each with
  // the afterId it was born at. Their `message` refs are the live objects the
  // run loop mutates, so their `content`/`status` reflect the stream so far.
  // A client that (re)subscribes mid-turn gets these re-emitted as born
  // snapshots for full live catch-up. Cleared per message on finalize.
  liveMessages: Map<string, { message: Message; afterId: string | null }>;
  // Outstanding tool prompts (askuser / permission / schedule-confirm) keyed by
  // callId, retained so a (re)subscribing client gets the open prompt re-sent.
  // Dropped when the prompt is answered or its call ends. See Phase 5.
  outstandingPrompts: Map<string, ServerToClientFrame>;
}

export interface ResolvedPendingCall {
  callId: string;
  extensionId: string;
  toolName: string;
  arguments: string;
  unknown?: boolean;
}
