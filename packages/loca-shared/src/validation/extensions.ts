// Zod schemas for the extension routes' request bodies.
//
// These live here rather than beside the routes that read them because
// `scripts/contract/emit-zod.ts` walks this directory and nothing else: a schema
// declared inside a route file generates no Rust type, and the core silently
// falls back to the constraint-free shape.

import { z } from "zod";

/** The `POST /api/v1/extensions/tool-schemas` body.
 *
 *  `.min(1)` on the item is the load-bearing part and the reason this schema is
 *  worth generating: an empty id names no tool, and without the constraint the
 *  lookup runs and reports "no such tool" for a string that was never a tool
 *  name in the first place. The `.max(512)` caps a body that would otherwise
 *  fan out into that many registry lookups. */
export const toolSchemasBodySchema = z
  .object({
    ids: z.array(z.string().min(1)).max(512),
  })
  .strict();
