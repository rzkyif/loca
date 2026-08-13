import { Hono } from "hono";
import { applyUpdate, checkForUpdate } from "../../update/self-updater.ts";
import { bearerMiddleware } from "../middleware/auth.ts";

export function updateRoutes(): Hono {
  const r = new Hono();
  r.use("*", bearerMiddleware());

  r.get("/check", async (c) => c.json(await checkForUpdate()));

  r.post("/apply", async (c) => {
    let version: string | undefined;
    try {
      const body = (await c.req.json()) as { version?: string };
      version = body.version;
    } catch {
      /* */
    }
    // applyUpdate hands off to the updater binary and calls Deno.exit; the
    // 204 response is never delivered if everything works. Return it for
    // the typecheck.
    await applyUpdate(version);
    return c.body(null, 204);
  });

  return r;
}
