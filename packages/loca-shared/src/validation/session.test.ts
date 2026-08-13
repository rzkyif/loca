// Message discriminated union: one parse per role, strict-mode
// rejection of cross-role fields, and PATCH schemas that don't accept
// `role` updates. This file is the contract test that prevents silent
// drift between core's appendMessage and the client's typed senders.

import { assertEquals } from "@std/assert";
import {
  assistantMessageInputSchema,
  errorMessageInputSchema,
  memoryFilterMessageInputSchema,
  messageInputSchema,
  messagePatchSchemaByRole,
  reasoningMessageInputSchema,
  systemMessageInputSchema,
  toolFilterMessageInputSchema,
  toolMessageInputSchema,
  userMessageInputSchema,
} from "./session.ts";

// One minimal valid body per MessageRole. Adding a role to the domain union
// without adding it here (and to the schemas) makes the coverage test below
// fail, which is the drift guard.
const MINIMAL_BY_ROLE: Record<string, Record<string, unknown>> = {
  user: { role: "user", content: "hi" },
  assistant: { role: "assistant", content: "hi" },
  system: { role: "system", content: "sys" },
  tool: {
    role: "tool",
    callId: "c1",
    extensionId: "t1",
    toolName: "do",
    arguments: "{}",
    status: "completed",
  },
  reasoning: { role: "reasoning", content: "think" },
  tool_filter: { role: "tool_filter", status: "complete" },
  memory_filter: { role: "memory_filter", status: "complete" },
  display: { role: "display", content: { type: "markdown", markdown: "x" } },
  error: { role: "error", content: "boom" },
};

Deno.test("messageInputSchema: parses each of the 9 roles", () => {
  for (const c of Object.values(MINIMAL_BY_ROLE)) {
    const r = messageInputSchema.safeParse(c);
    assertEquals(r.success, true, `should parse role=${c.role}`);
  }
});

Deno.test("contract: union variants and PATCH schemas cover the SAME role set", () => {
  // Every role with a PATCH schema must have a full-input union variant (its
  // minimal body parses), and vice versa, so neither can silently lag the
  // domain MessageRole union.
  const patchRoles = Object.keys(messagePatchSchemaByRole).sort();
  assertEquals(patchRoles, Object.keys(MINIMAL_BY_ROLE).sort());
  for (const role of patchRoles) {
    assertEquals(
      messageInputSchema.safeParse(MINIMAL_BY_ROLE[role]).success,
      true,
      `role ${role} has a PATCH schema but no parseable union variant`,
    );
  }
});

Deno.test("messageInputSchema: accepts automated (user) and truncated (assistant)", () => {
  assertEquals(
    messageInputSchema.safeParse({
      role: "user",
      content: "hi",
      automated: true,
    }).success,
    true,
  );
  assertEquals(
    messageInputSchema.safeParse({
      role: "assistant",
      content: "hi",
      truncated: true,
    }).success,
    true,
  );
});

Deno.test("userMessageInputSchema: accepts string content and multipart array", () => {
  assertEquals(userMessageInputSchema.safeParse({ role: "user", content: "hi" }).success, true);
  assertEquals(
    userMessageInputSchema.safeParse({
      role: "user",
      content: [{ type: "text", text: "hello" }],
    }).success,
    true,
  );
});

Deno.test("userMessageInputSchema: rejects assistant-only fields (strict mode)", () => {
  const r = userMessageInputSchema.safeParse({
    role: "user",
    content: "hi",
    streaming: true,
  });
  assertEquals(r.success, false);
});

Deno.test("assistantMessageInputSchema: rejects unknown fields and tool-only fields", () => {
  assertEquals(
    assistantMessageInputSchema.safeParse({
      role: "assistant",
      content: "ok",
      callId: "c1",
    }).success,
    false,
  );
});

Deno.test("messagePart: image_url requires nested url object (no bare string)", () => {
  assertEquals(
    userMessageInputSchema.safeParse({
      role: "user",
      content: [{ type: "image_url", image_url: "https://x" }],
    }).success,
    false,
  );
  assertEquals(
    userMessageInputSchema.safeParse({
      role: "user",
      content: [{ type: "image_url", image_url: { url: "https://x" } }],
    }).success,
    true,
  );
});

Deno.test("toolMessageInputSchema: requires status from the documented enum", () => {
  assertEquals(
    toolMessageInputSchema.safeParse({
      role: "tool",
      callId: "c",
      extensionId: "t",
      toolName: "n",
      arguments: "{}",
      status: "not-a-state",
    }).success,
    false,
  );
});

Deno.test("messagePatchSchemaByRole: rejects `role` in patch bodies", () => {
  const r = messagePatchSchemaByRole.user.safeParse({ role: "user" });
  assertEquals(r.success, false);
});

Deno.test("messagePatchSchemaByRole: rejects cross-role fields per role", () => {
  // Patch a UserMessage with an assistant-only field, which must fail.
  assertEquals(messagePatchSchemaByRole.user.safeParse({ streaming: true }).success, false);
  // Patch an AssistantMessage with a user-only field, which must fail.
  assertEquals(
    messagePatchSchemaByRole.assistant.safeParse({
      systemPromptOverride: "x",
    }).success,
    false,
  );
});

Deno.test("messagePatchSchemaByRole: accepts a partial of fields valid for the role", () => {
  assertEquals(messagePatchSchemaByRole.assistant.safeParse({ content: "edited" }).success, true);
  assertEquals(messagePatchSchemaByRole.assistant.safeParse({ interrupted: true }).success, true);
});

Deno.test("reasoning + tool_filter + memory_filter + error: each parses its minimum body", () => {
  for (const [schema, body] of [
    [reasoningMessageInputSchema, { role: "reasoning", content: "" }],
    [
      toolFilterMessageInputSchema,
      {
        role: "tool_filter",
        status: "complete",
      },
    ],
    [
      memoryFilterMessageInputSchema,
      {
        role: "memory_filter",
        status: "complete",
      },
    ],
    [errorMessageInputSchema, { role: "error", content: "boom" }],
    [systemMessageInputSchema, { role: "system", content: "s" }],
  ] as const) {
    assertEquals(schema.safeParse(body).success, true);
  }
});
