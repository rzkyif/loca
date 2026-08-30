// Zod schema for the greeting trigger's request body.
//
// It lives here rather than beside the route that reads it because
// `scripts/contract/emit-zod.ts` walks this directory and nothing else: a schema
// declared inside a route file generates no Rust type, and the core silently
// falls back to the constraint-free shape.

import { z } from "zod";

/** The `POST /api/v1/greetings/run` body.
 *
 *  Every field optional: the client owns the greeting policy and sends the
 *  title, instruction and dedup window it wants, so the core falls back to its
 *  own defaults for anything absent. `cooldownMs` is a bare `z.number()`, not an
 *  integer: a fractional window is accepted and floored server-side. */
export const greetingRunBodySchema = z
  .object({
    sessionTitle: z.string().optional(),
    instruction: z.string().optional(),
    cooldownMs: z.number().optional(),
  })
  .strict();
