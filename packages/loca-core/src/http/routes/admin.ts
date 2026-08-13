// Admin routes. Today: setting the admin password used by the remote-pairing
// flow. Guarded by the admin token (X-Admin-Token), i.e. device access: only
// someone who can read ~/.loca/<channel>/core/.admin-token may set or replace
// the password, so a LAN peer on a 0.0.0.0-bound core can't overwrite it. The
// client's Settings > Cores panel calls this with the on-disk token and the
// password the user chose. See services/auth.ts for the verify side.

import { Hono } from "hono";
import { adminPasswordSetRequestSchema } from "@loca/shared";
import { authService } from "../../services/auth.ts";
import { parseBody, readJson } from "@loca/core-engine/http/body";
import { adminTokenMiddleware } from "../middleware/auth.ts";

export function adminRoutes(): Hono {
  const r = new Hono();

  r.post("/password", adminTokenMiddleware(), async (c) => {
    const body = parseBody(adminPasswordSetRequestSchema, await readJson(c));
    await authService().setAdminPassword(body.password);
    return c.body(null, 204);
  });

  return r;
}
