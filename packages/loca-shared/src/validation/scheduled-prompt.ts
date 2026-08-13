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
// becomes an automated user message, so it is bounded like one.
export const scheduledPromptDraftSchema = z
  .object({
    title: z.string().min(1).max(200),
    instruction: z.string().min(1).max(10_000),
    schedule: scheduleSpecSchema,
    runMissed: z.boolean(),
  })
  .passthrough();
