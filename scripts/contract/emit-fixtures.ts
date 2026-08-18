// Emits wire fixtures for the Rust round-trip test.
//
// Every fixture is validated by the real Zod schema before it is written, so
// the Rust side is checked against the actual contract rather than against
// someone's reading of it. A fixture Zod rejects fails here, before Rust sees
// it: that has already caught six invented-from-memory frame shapes.
//
// The recorded `expected` is Zod's PARSED value, not the raw input. `.default()`
// means the decoded frame carries the default even when the sender omitted it,
// so Rust re-serializing with the default present is correct, not a round-trip
// failure. Comparing against the raw input instead reports a defect that is not
// there.
//
// Run via `deno task contract`, not directly.

import { messageInputSchema } from "../../packages/loca-shared/src/validation/session.ts";
import {
  chatStartWsSchema,
  chatSubscribeWsSchema,
  toolAskUserResponseSchema,
  toolPermissionResponseSchema,
} from "../../packages/loca-shared/src/validation/chat.ts";
import { pairingCodeRequestSchema } from "../../packages/loca-shared/src/validation/pairing.ts";
import { scheduledPromptDraftSchema } from "../../packages/loca-shared/src/validation/scheduled-prompt.ts";
import { serverToClientFrameSchema } from "../../packages/loca-shared/src/validation/ws.ts";

// deno-lint-ignore no-explicit-any
type Case = { type: string; value: any };

// `type` is the Rust type name, resolved by the match in
// packages/loca-core-rs/tests/contract_roundtrip.rs. Add a case here and a match
// arm there.
const cases: Case[] = [
  // Server -> client frames. The core serializes these, so the round trip
  // checks that what Rust emits is what the client's validator accepts.
  { type: "api::ServerToClientFrame", value: { kind: "pong" } },
  { type: "api::ServerToClientFrame", value: { kind: "ping" } },
  {
    type: "api::ServerToClientFrame",
    value: { kind: "chat.delta", streamId: "s1", messageId: "m1", delta: "hello" },
  },
  {
    type: "api::ServerToClientFrame",
    value: { kind: "chat.done", streamId: "s1", reason: "length" },
  },
  {
    type: "api::ServerToClientFrame",
    value: {
      kind: "chat.error",
      streamId: "s1",
      code: "context_window_exceeded",
      message: "too long",
    },
  },
  {
    type: "api::ServerToClientFrame",
    value: {
      kind: "session.updated",
      sessionId: "s1",
      op: "title_generating",
      payload: { generating: true },
    },
  },
  {
    type: "api::ServerToClientFrame",
    value: {
      kind: "requirements.snapshot",
      required: [
        {
          source: "@unsloth/Qwen3.5-2B-GGUF/main/x.gguf",
          type: "model",
          group: "llm",
          present: false,
        },
      ],
      missing: [
        {
          source: "@unsloth/Qwen3.5-2B-GGUF/main/x.gguf",
          type: "model",
          group: "llm",
          present: false,
        },
      ],
    },
  },
  {
    type: "api::ServerToClientFrame",
    value: {
      kind: "settings.updated",
      values: { "tts.enabled": true },
      deleted: ["llm.modelPath"],
    },
  },
  {
    type: "api::ServerToClientFrame",
    value: {
      kind: "tool.askuser_request",
      callId: "c1",
      requestId: "r1",
      questions: [{ question: "Pick one", kind: "choice", options: [{ label: "A", value: "a" }] }],
    },
  },
  // One per askUser kind, plus the legacy kind-less form. This union is
  // `#[serde(untagged)]` in Rust because `kind` is optional on the choice
  // variant, so the variants are told apart by their field sets alone. Get that
  // wrong and a files question silently deserializes as a choice question.
  {
    type: "api::ServerToClientFrame",
    value: {
      kind: "tool.askuser_request",
      callId: "c1",
      requestId: "r1",
      questions: [
        { question: "Legacy, no kind" },
        { kind: "diff", question: "Apply?", before: "a", after: "b", title: "patch" },
        {
          kind: "files",
          question: "Which files?",
          entries: [{ path: "src/main.rs", label: "main" }],
          multiselect: true,
        },
        {
          kind: "image",
          question: "Crop?",
          dataB64: "AAAA",
          mime: "image/png",
          actions: [{ label: "Yes", value: "yes" }],
        },
        { kind: "table", question: "Edit", columns: ["a", "b"], rows: [["1", "2"]] },
      ],
    },
  },
  {
    type: "api::ServerToClientFrame",
    value: {
      kind: "core.status",
      snapshot: { status: "idle", detail: "ok", subsystems: [] },
    },
  },
  {
    type: "api::ServerToClientFrame",
    value: { kind: "update.staged", version: "0.1.16" },
  },

  // Client -> server. The core deserializes these, so the round trip checks
  // that the constraints Zod enforces are the constraints Rust enforces.
  {
    type: "inbound::ChatStartWs",
    value: { kind: "chat.start", streamId: "s1", sessionId: "sess1" },
  },
  {
    type: "inbound::ChatStartWs",
    value: {
      kind: "chat.start",
      streamId: "s1",
      sessionId: "sess1",
      route: "secondary",
      systemPrompt: "",
      anchorMessageId: "m9",
    },
  },
  { type: "inbound::ChatSubscribeWs", value: { kind: "chat.subscribe", sessionId: "sess1" } },
  {
    type: "inbound::ToolPermissionResponse",
    value: { kind: "tool.permission_response", callId: "c1", requestId: "r1", allow: true },
  },
  {
    type: "inbound::ToolAskUserResponse",
    value: {
      kind: "tool.askuser_response",
      callId: "c1",
      requestId: "r1",
      answers: ["a", ["b", "c"], [{ col: "v" }]],
    },
  },
  { type: "inbound::PairingCodeRequest", value: {} },
  { type: "inbound::MessageInput", value: { role: "user", content: "hi" } },
  {
    type: "inbound::MessageInput",
    value: { role: "user", content: [{ type: "text", text: "hi" }] },
  },
  { type: "inbound::MessageInput", value: { role: "assistant", content: "hello" } },
  {
    type: "inbound::ScheduledPromptDraft",
    value: {
      title: "Daily standup",
      instruction: "Summarise yesterday",
      schedule: { kind: "weekly", weekdays: [1, 2, 3], hour: 9, minute: 0 },
      runMissed: false,
    },
  },
  {
    type: "inbound::ScheduledPromptDraft",
    value: {
      title: "One off",
      instruction: "Ping me",
      schedule: { kind: "once", atMs: 1760000000000 },
      runMissed: true,
    },
  },
];

// deno-lint-ignore no-explicit-any
const validators: Record<string, any> = {
  "api::ServerToClientFrame": serverToClientFrameSchema,
  "inbound::ChatStartWs": chatStartWsSchema,
  "inbound::ChatSubscribeWs": chatSubscribeWsSchema,
  "inbound::ToolPermissionResponse": toolPermissionResponseSchema,
  "inbound::ToolAskUserResponse": toolAskUserResponseSchema,
  "inbound::PairingCodeRequest": pairingCodeRequestSchema,
  "inbound::MessageInput": messageInputSchema,
  "inbound::ScheduledPromptDraft": scheduledPromptDraftSchema,
};

// deno-lint-ignore no-explicit-any
const out: any[] = [];
let bad = 0;
for (const c of cases) {
  const validator = validators[c.type];
  if (!validator) {
    console.error(`  no validator registered for ${c.type}`);
    bad++;
    continue;
  }
  const parsed = validator.safeParse(c.value);
  if (!parsed.success) {
    console.error(`  Zod rejects ${c.type}: ${JSON.stringify(c.value)}`);
    console.error(`    ${JSON.stringify(parsed.error.issues)}`);
    bad++;
    continue;
  }
  out.push({ type: c.type, value: c.value, expected: parsed.data });
}

if (bad) {
  console.error(`${bad} invalid fixture(s); fix them before the Rust side runs`);
  Deno.exit(1);
}

const outPath = Deno.args[0];
if (!outPath) {
  console.error("usage: emit-fixtures.ts <out.json>");
  Deno.exit(2);
}
await Deno.writeTextFile(outPath, `${JSON.stringify(out, null, 2)}\n`);
console.log(`fix: ${out.length} fixtures accepted by Zod`);
