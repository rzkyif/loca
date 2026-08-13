// Open a WebSocket to a test server the way the real client does: mint a
// single-use WS ticket for the bearer (the hub authenticates the ticket in the
// URL, not the long-lived bearer) and connect with `?ticket=`.

import { authService } from "../../src/services/auth.ts";

export async function dialWs(port: number, token: string): Promise<WebSocket> {
  const me = await authService().authenticate(token);
  const ticket = authService().mintWsTicket(me.id);
  return new WebSocket(`ws://127.0.0.1:${port}/ws/v1?ticket=${encodeURIComponent(ticket)}`);
}
