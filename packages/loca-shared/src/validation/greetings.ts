// Zod schema for the greeting trigger's request body.
//
// It lives here rather than beside the route that reads it so the contract
// bridge can see it: `scripts/contract/emit-zod.ts` walks this directory and
// nothing else, and a schema declared inside a route file is generated into no
// Rust type at all. The core then falls back to the constraint-free shape from
// the TypeScript half, and the port silently accepts what the oracle refuses.
// See D-093.

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
