// Client-context middleware for the engine's HTTP routes. The engine does not
// own authentication: the embedder resolves each request to a client and injects
// that resolver at init. The desktop shell's resolver reads the bearer token and
// calls its authService (identical to the old bearerMiddleware); a future
// in-process mobile transport injects a resolver that returns its fixed,
// pre-authenticated local client. Either way the authed client lands in
// c.var.client and routes read it via requireClient().

import type { Context, MiddlewareHandler } from "hono";

// The only field the routes use is the client id (session ownership, per-client
// settings). Kept minimal so the engine never depends on the shell's richer
// AuthedClient shape.
export interface EngineClient {
  id: string;
}

// Resolves one request to its authenticated client, throwing an AppError
// (missing_token / unauthorized) on failure so the app's onError renders the
// same error envelope the shell's bearerMiddleware produced.
export type ClientResolver = (c: Context) => Promise<EngineClient>;

// A distinct key from the shell's own `client` context var (which carries its
// richer AuthedClient): the two apps run in one TS graph, so a shared key with
// different types would collide.
declare module "hono" {
  interface ContextVariableMap {
    engineClient: EngineClient;
  }
}

export function clientContextMiddleware(resolve: ClientResolver): MiddlewareHandler {
  return async (c, next) => {
    c.set("engineClient", await resolve(c));
    await next();
  };
}

/** Read the authed client set by clientContextMiddleware. */
export function requireClient(c: Context): EngineClient {
  return c.get("engineClient");
}
