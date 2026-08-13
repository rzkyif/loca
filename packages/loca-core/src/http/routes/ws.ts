import { Hono } from "hono";
import { authService } from "../../services/auth.ts";
import { bearerMiddleware, requireClient } from "../middleware/auth.ts";

// WS upgrade helpers. The upgrade itself (/ws/v1) is served by the hub off the
// Deno.serve hook (it needs the raw Request to call Deno.upgradeWebSocket); this
// bearer-authed route only mints the short-lived ticket the client then presents
// on that upgrade, so the long-lived bearer never travels in the ws:// URL.
export function wsRoutes(): Hono {
  const r = new Hono();
  r.use("*", bearerMiddleware());

  // Mint a single-use, short-lived ticket for this authenticated client to use
  // on the very next WS upgrade. The bearer rides in this call's Authorization
  // header (which a reverse proxy does not log by default); only the throwaway
  // ticket lands in the ws:// query string.
  r.post("/ticket", (c) => {
    const client = requireClient(c);
    return c.json({ ticket: authService().mintWsTicket(client.id) });
  });

  return r;
}
