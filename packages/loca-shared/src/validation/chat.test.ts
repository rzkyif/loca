// chat-related Zod schemas. The chat WS frames are the contract between
// the client and the chat orchestrator. Required-field + min-length + type
// checks catch the common shapes silently breaking; the frames are
// `.passthrough()` so a newer client's extra field is ignored (forward-compat),
// not rejected into a silent turn drop.

import { assertEquals } from "@std/assert";
import {
  chatInterruptWsSchema,
  chatStartWsSchema,
  toolAskUserResponseSchema,
  toolCancelSchema,
} from "./chat.ts";

Deno.test("chatStartWsSchema: requires kind, streamId, sessionId", () => {
  assertEquals(
    chatStartWsSchema.safeParse({
      kind: "chat.start",
      streamId: "s1",
      sessionId: "S1",
    }).success,
    true,
  );
  assertEquals(
    chatStartWsSchema.safeParse({
      kind: "chat.start",
      streamId: "",
      sessionId: "S1",
    }).success,
    false,
  );
  assertEquals(
    chatStartWsSchema.safeParse({
      kind: "chat.notstart",
      streamId: "s1",
      sessionId: "S1",
    }).success,
    false,
  );
});

Deno.test("chatStartWsSchema: ignores an unknown field (forward-compat, no silent drop)", () => {
  // A newer client adds a field an older core doesn't know. It must parse OK on
  // the known fields, not be rejected (which the hub turns into a silent drop).
  const parsed = chatStartWsSchema.safeParse({
    kind: "chat.start",
    streamId: "s1",
    sessionId: "S1",
    someFutureField: { anything: true },
  });
  assertEquals(parsed.success, true);
  // The unknown field is still carried through (passthrough), and known fields
  // keep their validated values.
  assertEquals(parsed.success && parsed.data.streamId, "s1");
});

Deno.test("chatInterruptWsSchema: requires non-empty streamId", () => {
  assertEquals(
    chatInterruptWsSchema.safeParse({ kind: "chat.interrupt", streamId: "x" }).success,
    true,
  );
  assertEquals(
    chatInterruptWsSchema.safeParse({ kind: "chat.interrupt", streamId: "" }).success,
    false,
  );
});

Deno.test("toolAskUserResponseSchema: accepts string OR string[] entries in answers", () => {
  assertEquals(
    toolAskUserResponseSchema.safeParse({
      kind: "tool.askuser_response",
      callId: "c1",
      requestId: "r1",
      answers: ["one", ["a", "b"]],
    }).success,
    true,
  );
  // Numbers are not allowed.
  assertEquals(
    toolAskUserResponseSchema.safeParse({
      kind: "tool.askuser_response",
      callId: "c1",
      requestId: "r1",
      answers: [42],
    }).success,
    false,
  );
});

Deno.test("toolAskUserResponseSchema: accepts table-edit answers (array of string records)", () => {
  assertEquals(
    toolAskUserResponseSchema.safeParse({
      kind: "tool.askuser_response",
      callId: "c1",
      requestId: "r1",
      // One table question's answer: the edited rows, each a column-keyed record.
      answers: [
        [
          { item: "coffee", amount: "4.50" },
          { item: "lunch", amount: "12.00" },
        ],
      ],
    }).success,
    true,
  );
  // Record values must be strings, not numbers.
  assertEquals(
    toolAskUserResponseSchema.safeParse({
      kind: "tool.askuser_response",
      callId: "c1",
      requestId: "r1",
      answers: [[{ item: "coffee", amount: 4.5 }]],
    }).success,
    false,
  );
});

Deno.test("toolCancelSchema: requires non-empty callId", () => {
  assertEquals(toolCancelSchema.safeParse({ kind: "tool.cancel", callId: "c1" }).success, true);
  assertEquals(toolCancelSchema.safeParse({ kind: "tool.cancel", callId: "" }).success, false);
});
