// Flat, provider-agnostic tools listing. Backs the Tools management UI, which
// aggregates every tool from every provider (extensions today; MCP servers
// union in here once configured). Per-tool enable/disable and permission grants
// stay on the provider's own routes (`/api/v1/extensions/:id/tools/...`), keyed
// by the tool's `extensionId`; this route is read-only.

import { Hono } from "hono";
import { extensionsRegistry } from "../../extensions/registry.ts";
import { mcpRegistry } from "../../mcp/registry.ts";
import { attachRequiredPermissions } from "./extensions.ts";
import { bearerMiddleware } from "../middleware/auth.ts";

export function toolsRoutes(): Hono {
  const r = new Hono();
  r.use("*", bearerMiddleware());

  r.get("/", async (c) => {
    const extensionTools = await Promise.all(
      extensionsRegistry()
        .listAllTools()
        .map((t) => attachRequiredPermissions(t)),
    );
    // MCP tools run on their server (no local sandbox permissions); union them
    // in so the Tools UI shows every tool from every provider.
    const tools = [...extensionTools, ...mcpRegistry().listAllTools()];
    return c.json({ tools });
  });

  return r;
}
