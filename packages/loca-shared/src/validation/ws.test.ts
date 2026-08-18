// Server->client WS frame validation. The client decodes every inbound frame
// through `serverToClientFrameSchema` and DROPS anything that fails the
// discriminated union. A `kind` present in the TS union but missing a Zod
// variant therefore vanishes silently at runtime: that is exactly how
// `requirements.snapshot` broadcasts were being discarded, freezing the
// pending-downloads popup on its boot-time HTTP snapshot.

import { assertEquals } from "@std/assert";
import { CLIENT_TO_SERVER_KINDS, SERVER_TO_CLIENT_KINDS } from "../api/ws.ts";
import { serverToClientFrameSchema } from "./ws.ts";
import {
  chatInterruptWsSchema,
  chatStartWsSchema,
  chatSubscribeWsSchema,
  scheduleConfirmResponseSchema,
  toolAskUserResponseSchema,
  toolCancelSchema,
  toolPermissionResponseSchema,
} from "./chat.ts";

Deno.test("serverToClientFrameSchema: accepts requirements.snapshot", () => {
  const frame = {
    kind: "requirements.snapshot",
    required: [
      {
        source: "@unsloth/Qwen3.5-2B-GGUF/main/Qwen3.5-2B-Q4_K_M.gguf",
        type: "model",
      },
    ],
    missing: [
      {
        source: "@unsloth/Qwen3.5-2B-GGUF/main/Qwen3.5-2B-Q4_K_M.gguf",
        type: "model",
      },
    ],
  };
  assertEquals(serverToClientFrameSchema.safeParse(frame).success, true);
});

// These two frames were silently dropped by the client because the Zod enum
// drifted from the TS union. They now share one const tuple per enum (see the
// "Wire enums" block in ../api/ws.ts), so these regressions can't recur.
Deno.test("serverToClientFrameSchema: accepts session.updated title_generating", () => {
  assertEquals(
    serverToClientFrameSchema.safeParse({
      kind: "session.updated",
      sessionId: "s1",
      op: "title_generating",
      payload: { generating: true },
    }).success,
    true,
  );
});

Deno.test("serverToClientFrameSchema: accepts chat.done with reason 'length'", () => {
  assertEquals(
    serverToClientFrameSchema.safeParse({
      kind: "chat.done",
      streamId: "s1",
      reason: "length",
    }).success,
    true,
  );
});

Deno.test("serverToClientFrameSchema: accepts settings.updated (with and without secretNames)", () => {
  const frame = {
    kind: "settings.updated",
    values: { "tts.enabled": true },
    deleted: ["llm.modelPath"],
  };
  assertEquals(serverToClientFrameSchema.safeParse(frame).success, true);
  assertEquals(
    serverToClientFrameSchema.safeParse({
      kind: "settings.updated",
      values: {},
      deleted: [],
      secretNames: ["llm.external.apiKey"],
    }).success,
    true,
  );
});

Deno.test("serverToClientFrameSchema: every server->client kind has a variant", () => {
  // SERVER_TO_CLIENT_KINDS is the one source for the kind set. The TS union in
  // ../api/ws.ts is checked against it at compile time; this checks the Zod
  // union against the same tuple, so a kind cannot exist on one side only.
  const covered = new Set(
    serverToClientFrameSchema.options.map((o) => o.shape.kind.value as string),
  );
  assertEquals(covered, new Set<string>(SERVER_TO_CLIENT_KINDS));
});

Deno.test("client->server: every kind has a validator", () => {
  // ping and pong carry nothing beyond `kind`, so the hub routes them off the
  // envelope schema alone and they need no per-kind validator.
  const envelopeOnly = new Set<string>(["ping", "pong"]);
  const validators = [
    chatStartWsSchema,
    chatInterruptWsSchema,
    chatSubscribeWsSchema,
    toolAskUserResponseSchema,
    toolPermissionResponseSchema,
    toolCancelSchema,
    scheduleConfirmResponseSchema,
  ];
  // scheduleConfirmResponseSchema is a ZodEffects (it has a .superRefine), so
  // its shape lives one level down under .innerType().
  const covered = new Set(
    validators.map((schema) => {
      // deno-lint-ignore no-explicit-any
      const s = schema as any;
      const shape = s.shape ?? s.innerType?.().shape ?? s._def?.schema?.shape;
      return shape.kind.value as string;
    }),
  );
  const expected = new Set(CLIENT_TO_SERVER_KINDS.filter((k) => !envelopeOnly.has(k)));
  assertEquals(covered, expected);
});
