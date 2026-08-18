// Zod schemas for the MCP admin routes' request bodies.
//
// These live here rather than beside the routes that read them so the contract
// bridge can see them: `scripts/contract/emit-zod.ts` walks this directory and
// nothing else, and a schema declared inside a route file is generated into no
// Rust type at all. The core then falls back to the constraint-free shape from
// the TypeScript half, and the port silently accepts what the oracle refuses.
// See D-093.

import { z } from "zod";

/** The `POST /api/v1/mcp` body. */
export const mcpServerBodySchema = z
  .object({
    name: z.string().min(1),
    kind: z.enum(["stdio", "remote"]),
    command: z.string().optional(),
    args: z.array(z.string()).optional(),
    runtime: z.enum(["custom", "deno"]).optional(),
    denoAllowAll: z.boolean().optional(),
    denoPermissions: z.array(z.string()).optional(),
    url: z.string().optional(),
    remoteAuth: z.enum(["none", "bearer", "oauth"]).optional(),
    enabled: z.boolean().optional(),
    // Remote bearer token: write-only. Omitted = leave as-is; "" = clear; a
    // value = store in the vault. Never echoed back (the projection only
    // exposes `hasAuth`).
    authToken: z.string().optional(),
  })
  .strict();

/** The `PATCH /api/v1/mcp/:id` body: every field of the create body, optional.
 *
 *  Declared here rather than derived at the route so it becomes a Rust type of
 *  its own. `name` keeps its `.min(1)` when present, which is what stops an
 *  empty rename being silently swallowed by the registry's
 *  "blank keeps the stored name" rule. */
export const mcpServerPatchBodySchema = mcpServerBodySchema.partial();

/** The `POST /api/v1/mcp/:id/prompts/:prompt/resolve` body: the arguments to
 *  fold into the prompt's messages. Absent means "no arguments", which is what
 *  the handler's `?? {}` reads. */
export const mcpPromptResolveBodySchema = z
  .object({ arguments: z.record(z.string(), z.string()).optional() })
  .strict();
