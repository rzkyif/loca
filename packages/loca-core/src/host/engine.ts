// The Deno shell's handle on @loca/core-engine. It builds the engine once with
// the DenoHost and the shell's bearer-token resolver, then main.ts serves the
// engine's app-domain routes by dispatching matching requests into
// engine.handleHttp (and the WS hub shares the engine FrameBus for delivery).
//
// The resolver is where the shell keeps ownership of authentication: it reads
// the Authorization header and resolves it against authService exactly as the
// old per-route bearerMiddleware did, then hands the engine only the client id.

import type { Context } from "hono";
import { AppError, type EngineInstance, init } from "@loca/core-engine";
import { denoHost } from "./deno-host.ts";
import { authService } from "../services/auth.ts";

async function resolveClient(c: Context): Promise<{ id: string }> {
  const header = c.req.header("authorization") ?? "";
  const match = /^Bearer\s+(.+)$/i.exec(header);
  if (!match) {
    throw new AppError("missing_token", "missing Authorization: Bearer header");
  }
  const client = await authService().authenticate(match[1].trim());
  return { id: client.id };
}

let _engine: Promise<EngineInstance> | null = null;

/** The process-wide engine instance for the Deno shell. Memoized: the app is
 *  stateless (services read the current DB per call), so one instance is safe
 *  across the test env swaps that repoint LOCA_CORE_HOME. */
export function engine(): Promise<EngineInstance> {
  if (!_engine) _engine = init(denoHost(), { resolveClient });
  return _engine;
}

/** Test-only: drop the memoized instance so a suite that needs a fresh build
 *  (e.g. after changing the route set) gets one. */
export function __resetEngineForTesting(): void {
  _engine = null;
}
