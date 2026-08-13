// Bearer-token middleware. Routes that need authentication import this and
// install it before their handlers. The authed client lands in c.var.client.

import type { Context, MiddlewareHandler } from "hono";
import { type AuthedClient, authService } from "../../services/auth.ts";
import { AppError } from "@loca/core-engine";

declare module "hono" {
  interface ContextVariableMap {
    client: AuthedClient;
  }
}

export function bearerMiddleware(): MiddlewareHandler {
  return async (c, next) => {
    const header = c.req.header("authorization") ?? "";
    const match = /^Bearer\s+(.+)$/i.exec(header);
    if (!match) {
      throw new AppError("missing_token", "missing Authorization: Bearer header");
    }
    const token = match[1].trim();
    const client = await authService().authenticate(token);
    c.set("client", client);
    await next();
  };
}

// Admin-token guard for the pairing-code mint endpoint. Reads
// X-Admin-Token from the request and compares constant-time against
// the on-disk admin token.
export function adminTokenMiddleware(): MiddlewareHandler {
  return async (c, next) => {
    const provided = c.req.header("x-admin-token");
    await authService().verifyAdminToken(provided ?? null);
    await next();
  };
}

// Helper for routes to read the authed client without re-parsing.
export function requireClient(c: Context): AuthedClient {
  return c.get("client");
}
