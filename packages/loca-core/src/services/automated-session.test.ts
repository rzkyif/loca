// runAutomatedSession is the shared backend behind scheduled prompts and
// greetings: it creates a session, persists the instruction as an
// automated:true user message, broadcasts session.created, and starts a chat
// turn. Stub the chat turn + WS hub so the test asserts the durable effects
// without running an LLM.

import { assertEquals } from "@std/assert";
import type { ServerToClientFrame } from "@loca/shared";
import { createTestClient, setupTestEnv } from "../../tests/helpers/db.ts";
import { chatService } from "@loca/core-engine/services/chat";
import { sessionsRepo } from "@loca/core-engine/services/sessions-store";
import { wsHub } from "../ws/hub.ts";
import { runAutomatedSession } from "./automated-session.ts";

Deno.test("runAutomatedSession: persists an automated message, broadcasts, and starts a turn", async () => {
  const env = await setupTestEnv();
  try {
    const clientId = createTestClient();

    // Capture the chat.start frame instead of running a real LLM turn.
    const starts: Array<{ clientId: string; sessionId: string }> = [];
    chatService().start = (cid, frame) => {
      starts.push({ clientId: cid, sessionId: frame.sessionId });
      return Promise.resolve();
    };
    // Capture session.created broadcasts.
    const broadcasts: Array<{ clientId: string; frame: ServerToClientFrame }> = [];
    wsHub().broadcastToClient = (cid, frame) => {
      broadcasts.push({ clientId: cid, frame });
    };

    const session = await runAutomatedSession({
      ownerClientId: clientId,
      title: "Morning brief",
      instruction: "Summarize my day",
      reason: "schedule",
      scheduledPromptId: "sched-1",
      focus: "show",
    });

    // The session's automated opening message persists.
    const messages = await sessionsRepo().listMessages(session.id);
    assertEquals(messages.length, 1);
    const opening = messages[0];
    if (opening.role !== "user") throw new Error("expected a user message");
    assertEquals(opening.automated, true);
    assertEquals(opening.content, "Summarize my day");

    // session.created was broadcast to the owner with the right metadata.
    assertEquals(broadcasts.length, 1);
    assertEquals(broadcasts[0].clientId, clientId);
    const frame = broadcasts[0].frame;
    if (frame.kind !== "session.created") {
      throw new Error("expected session.created");
    }
    assertEquals(frame.reason, "schedule");
    assertEquals(frame.focus, "show");
    assertEquals(frame.scheduledPromptId, "sched-1");
    assertEquals(frame.session.id, session.id);

    // A normal chat turn was started on the new session.
    assertEquals(starts, [{ clientId, sessionId: session.id }]);
  } finally {
    await env.teardown();
  }
});
