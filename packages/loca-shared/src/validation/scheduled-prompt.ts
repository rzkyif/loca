// Zod schemas for scheduled-prompt payloads: the REST routes' bodies and
// the schedule confirm WS frames both embed the same draft shape.

import { z } from "zod";

const hourField = z.number().int().min(0).max(23);
const minuteField = z.number().int().min(0).max(59);

export const scheduleSpecSchema = z.discriminatedUnion("kind", [
  z.object({ kind: z.literal("once"), atMs: z.number().int().positive() }).passthrough(),
  z
    .object({
      kind: z.literal("interval"),
      everyMinutes: z.number().int().min(1),
    })
    .passthrough(),
  z
    .object({
      kind: z.literal("weekly"),
      weekdays: z.array(z.number().int().min(0).max(6)).min(1),
      hour: hourField,
      minute: minuteField,
    })
    .passthrough(),
  z
    .object({
      kind: z.literal("monthly"),
      day: z.number().int().min(1).max(31),
      hour: hourField,
      minute: minuteField,
    })
    .passthrough(),
  z
    .object({
      kind: z.literal("yearly"),
      month: z.number().int().min(1).max(12),
      day: z.number().int().min(1).max(31),
      hour: hourField,
      minute: minuteField,
    })
    .passthrough(),
]);

// Length caps: the title renders in lists and notifications; the instruction
// becomes an automated user message, so it is bounded like one. Declared once
// and shared by BOTH the create and patch bodies below, so a title refused on
// create can never be accepted on update.
const titleField = z.string().min(1).max(200);
const instructionField = z.string().min(1).max(10_000);

export const scheduledPromptDraftSchema = z
  .object({
    title: titleField,
    instruction: instructionField,
    schedule: scheduleSpecSchema,
    runMissed: z.boolean(),
  })
  .passthrough();

/** The `PATCH /api/v1/scheduled-prompts/:id` body: every field of the draft,
 *  optional, strict rather than passthrough, and sharing the draft's length
 *  caps so a client cannot create a schedule inside the limits and immediately
 *  widen it past them. */
export const scheduledPromptPatchBodySchema = z
  .object({
    title: titleField.optional(),
    instruction: instructionField.optional(),
    schedule: scheduleSpecSchema.optional(),
    runMissed: z.boolean().optional(),
    enabled: z.boolean().optional(),
  })
  .strict();
