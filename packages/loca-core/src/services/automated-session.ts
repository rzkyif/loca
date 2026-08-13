// Core-initiated ("automated") sessions: the one backend behind scheduled
// prompts and greetings. Creates a fresh session for the owner client,
// persists the instruction as an `automated: true` user message, announces
// it over WS (`session.created`), and starts a normal chat turn on it.
//
// The turn deliberately runs the standard pipeline: no systemPrompt on the
// frame means chat.ts falls back to the `prompts.defaultSystemPrompt` core
// setting, and the shared DEFAULT_TOOLS_HINT is appended iff the turn
// actually exposes tools. Everything persists even when the owner client is
// offline; `broadcastToClient` simply drops frames nobody is listening for,
// and the client catches up from the session list on its next connect.

import type { Session, UserMessage } from "@loca/shared";
import { applyTitleDatetime, DEFAULT_TOOLS_HINT, errMessage } from "@loca/shared";
import { newMessageId, newStreamId } from "@loca/core-engine";
import { getLogger } from "../shared/log.ts";
import { wsHub } from "../ws/hub.ts";
import { chatService } from "@loca/core-engine/services/chat";
import { sessionsRepo } from "@loca/core-engine/services/sessions-store";

const log = getLogger("automated-session");

export interface AutomatedSessionInput {
  ownerClientId: string;
  title: string;
  /** The automated user prompt that opens the session. */
  instruction: string;
  reason: "schedule" | "greeting";
  scheduledPromptId?: string;
  /** How the owner client should surface the new session; see the
   *  `session.created` frame in @loca/shared api/ws.ts. */
  focus: "show" | "show_when_done";
}

export async function runAutomatedSession(input: AutomatedSessionInput): Promise<Session> {
  // Stamp `{datetime}` in the title with the creation time, so a greeting (or
  // scheduled prompt) titled "Greeting {datetime}" reads "Greeting 2026-06-15
  // 14:30", matching the default-title form before LLM title generation.
  const session = await sessionsRepo().create({
    ownerClientId: input.ownerClientId,
    title: applyTitleDatetime(input.title, Date.now()),
  });
  const message: UserMessage = {
    id: newMessageId(),
    ord: 0, // appendMessage assigns the real ord
    role: "user",
    content: input.instruction,
    automated: true,
    createdAtMs: Date.now(),
  };
  await sessionsRepo().appendMessage(session.id, message);
  wsHub().broadcastToClient(input.ownerClientId, {
    kind: "session.created",
    session,
    reason: input.reason,
    ...(input.scheduledPromptId ? { scheduledPromptId: input.scheduledPromptId } : {}),
    focus: input.focus,
  });
  log.info(`automated session ${session.id} (${input.reason}): "${input.title}"`);
  // Fire-and-forget the turn: the session is already created and returned.
  void chatService()
    .start(input.ownerClientId, {
      kind: "chat.start",
      streamId: newStreamId(),
      sessionId: session.id,
      route: "default",
      toolsHint: DEFAULT_TOOLS_HINT,
    })
    .catch((err) => log.warn(`automated session ${session.id} start failed: ${errMessage(err)}`));
  return session;
}
