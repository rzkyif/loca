// Zod schemas for the memory routes' request bodies.
//
// These live here rather than beside the routes that read them so the contract
// bridge can see them: `scripts/contract/emit-zod.ts` walks this directory and
// nothing else, and a schema declared inside a route file is generated into no
// Rust type at all. The core then falls back to the constraint-free shape from
// the TypeScript half, and the port silently accepts what the oracle refuses.
// See D-093.

import { z } from "zod";

/** The `POST /api/v1/memories` body. */
export const memoryCreateBodySchema = z
  .object({
    kind: z.enum(["knowledge", "skill"]).default("knowledge"),
    title: z.string().min(1),
    content: z.string().default(""),
  })
  .strict();

/** The `PUT /api/v1/memories/:id/files/:name` body. */
export const memoryFileBodySchema = z
  .object({
    content: z.string().default(""),
  })
  .strict();

/** The `PATCH /api/v1/memories/:id` body.
 *
 *  Every field is optional, so a patch carrying nothing is a no-op that still
 *  returns the memory. `title`'s `.min(1)` has to be rejected before any field
 *  is applied: the handler writes `enabled` first, so a body that pairs a valid
 *  `enabled` with an empty `title` would otherwise half-apply. */
export const memoryPatchBodySchema = z
  .object({
    title: z.string().min(1).optional(),
    content: z.string().optional(),
    enabled: z.boolean().optional(),
  })
  .strict();
