// Zod schemas for the extension routes' request bodies.
//
// These live here rather than beside the routes that read them so the contract
// bridge can see them: `scripts/contract/emit-zod.ts` walks this directory and
// nothing else, and a schema declared inside a route file is generated into no
// Rust type at all. The core then falls back to the constraint-free shape from
// the TypeScript half, and the port silently accepts what the oracle refuses.
// That is not hypothetical: it is exactly how the Rust core came to answer
// `tool_not_found` for an empty tool id the oracle rejects as a validation
// error. See D-093.

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
