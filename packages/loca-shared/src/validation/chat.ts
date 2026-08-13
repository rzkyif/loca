// Zod schemas for chat-related request bodies (the client->server WS frames).
//
// These use `.passthrough()`, NOT `.strict()`: they still validate every known
// field (required, min-length, types, kind literal), but IGNORE unknown fields
// instead of rejecting the whole frame. This mirrors the server->client
// forward-compat policy (serverToClientFrameSchema in validation/ws.ts also
// passes unknown fields through) and is what makes version skew graceful - a
// newer client that adds a field to chat.start is processed by an older core on
// its known fields rather than being silently dropped (the hub logs + returns on
// a parse failure, so a rejected frame just hangs the client). Required-field
// checks still catch genuine malformed frames.

import { z } from "zod";
import { scheduledPromptDraftSchema } from "./scheduled-prompt.ts";

export const chatStartWsSchema = z
  .object({
    kind: z.literal("chat.start"),
    streamId: z.string().min(1),
    sessionId: z.string().min(1),
    route: z.enum(["default", "secondary"]).default("default"),
    // Per-turn effective system prompt composed client-side; see the frame
    // type in api/ws.ts. Empty string is meaningful ("send no system
    // prompt"), absent means "fall back to prompts.defaultSystemPrompt".
    systemPrompt: z.string().optional(),
    // Rendered tools-availability hint, appended by core iff the turn
    // exposes tools to the model; see the frame type in api/ws.ts.
    toolsHint: z.string().optional(),
    // User message anchoring this turn (regenerate / edit-and-resend);
    // see the frame type in api/ws.ts.
    anchorMessageId: z.string().min(1).optional(),
  })
  .passthrough();

export type ChatStartFrame = z.infer<typeof chatStartWsSchema>;

export const chatInterruptWsSchema = z
  .object({
    kind: z.literal("chat.interrupt"),
    streamId: z.string().min(1),
  })
  .passthrough();

export type ChatInterruptFrame = z.infer<typeof chatInterruptWsSchema>;

export const chatSubscribeWsSchema = z
  .object({
    kind: z.literal("chat.subscribe"),
    sessionId: z.string().min(1),
  })
  .passthrough();

export type ChatSubscribeFrame = z.infer<typeof chatSubscribeWsSchema>;

export const toolAskUserResponseSchema = z
  .object({
    kind: z.literal("tool.askuser_response"),
    callId: z.string().min(1),
    requestId: z.string().min(1),
    // Per-question answer: choice/files/image are strings (string[] when
    // multiselect), table is the edited rows as column-keyed records.
    answers: z.array(
      z.union([z.string(), z.array(z.string()), z.array(z.record(z.string(), z.string()))]),
    ),
  })
  .passthrough();

export type ToolAskUserResponseFrame = z.infer<typeof toolAskUserResponseSchema>;

export const scheduleConfirmResponseSchema = z
  .object({
    kind: z.literal("schedule.confirm_response"),
    callId: z.string().min(1),
    requestId: z.string().min(1),
    accepted: z.boolean(),
    // The (possibly user-edited) draft; required when accepted.
    draft: scheduledPromptDraftSchema.optional(),
  })
  .passthrough()
  .superRefine((value, ctx) => {
    if (value.accepted && !value.draft) {
      ctx.addIssue({
        code: z.ZodIssueCode.custom,
        path: ["draft"],
        message: "draft is required when accepted",
      });
    }
  });

export type ScheduleConfirmResponseFrame = z.infer<typeof scheduleConfirmResponseSchema>;

export const toolPermissionResponseSchema = z
  .object({
    kind: z.literal("tool.permission_response"),
    callId: z.string().min(1),
    requestId: z.string().min(1),
    allow: z.boolean(),
  })
  .passthrough();

export type ToolPermissionResponseFrame = z.infer<typeof toolPermissionResponseSchema>;

export const toolCancelSchema = z
  .object({
    kind: z.literal("tool.cancel"),
    callId: z.string().min(1),
  })
  .passthrough();

export type ToolCancelFrame = z.infer<typeof toolCancelSchema>;

// Minimal envelope for the CLIENT->server receive path on the WS hub
// (ws/hub.ts), where only `kind` routing matters: parse object + non-empty
// kind, pass unknown fields through, and log + drop only when the shape is
// unmistakably wrong.
//
// NOTE: the server->client direction IS validated per-variant by
// `serverToClientFrameSchema` in `validation/ws.ts` (the client uses it in
// core/client.ts); those variants use `.passthrough()` for forward-compat (the
// same policy the client->server variants above now follow), and a test in
// `validation/ws.test.ts` asserts the Zod union covers every TS
// `ServerToClientFrame` kind so the two can't drift.
export const wsFrameEnvelopeSchema = z
  .object({
    kind: z.string().min(1),
  })
  .passthrough();

export type WsFrameEnvelope = z.infer<typeof wsFrameEnvelopeSchema>;
