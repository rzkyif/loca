// Interactive OAuth 2.1 sign-in for a remote MCP server. Follows the RFC 8252
// native-app pattern: open a short-lived loopback HTTP listener as the redirect
// target (so the browser never has to trust core's self-signed cert), run the
// SDK's auth() to discover metadata, dynamically register, and build the PKCE
// authorization URL, then hand that URL back for the client to open in a
// browser. When the browser redirects to the loopback listener with the code,
// exchange it for tokens and notify the caller.
//
// This is a user-initiated action (a Sign in button), so opening a browser and
// reaching the authorization server is consented network, not background reach.

import { auth } from "@modelcontextprotocol/sdk/client/auth.js";
import { McpOAuthProvider } from "./oauth-provider.ts";
import { getLogger } from "../shared/log.ts";
import { errMessage } from "@loca/shared";

const log = getLogger("mcp");

// Abandon a half-finished sign-in (browser closed, never returned) after this.
const FLOW_TIMEOUT_MS = 5 * 60_000;

interface Pending {
  provider: McpOAuthProvider;
  serverUrl: string;
  server: Deno.HttpServer;
  ac: AbortController;
  timer: ReturnType<typeof setTimeout>;
  onComplete: (ok: boolean) => void;
}

const pending = new Map<string, Pending>();

export interface OAuthStartResult {
  /** URL the client opens in a browser, or null if already authorized. */
  authorizationUrl: string | null;
}

/** Begin the authorization-code flow. Resolves with the URL to open; the token
 *  exchange completes later when the browser hits the loopback listener, which
 *  calls `onComplete(true)`. A timeout or error calls `onComplete(false)`. */
export async function startMcpOAuth(
  serverId: string,
  serverUrl: string,
  onComplete: (ok: boolean) => void,
): Promise<OAuthStartResult> {
  cancel(serverId);

  const ac = new AbortController();
  const server = Deno.serve(
    { hostname: "127.0.0.1", port: 0, signal: ac.signal, onListen: () => {} },
    (req) => handleCallback(serverId, req),
  );
  const port = (server.addr as Deno.NetAddr).port;
  const provider = new McpOAuthProvider(serverId, `http://127.0.0.1:${port}/callback`);

  const result = await auth(provider, { serverUrl }).catch((err: unknown) => {
    ac.abort();
    throw new Error(`OAuth start failed: ${errMessage(err)}`);
  });

  if (result === "AUTHORIZED") {
    // Already had valid tokens (e.g. a stored refresh token still works).
    ac.abort();
    onComplete(true);
    return { authorizationUrl: null };
  }
  if (!provider.authorizationUrl) {
    ac.abort();
    throw new Error("OAuth start did not produce an authorization URL");
  }

  const timer = setTimeout(() => finish(serverId, false), FLOW_TIMEOUT_MS);
  pending.set(serverId, { provider, serverUrl, server, ac, timer, onComplete });
  return { authorizationUrl: provider.authorizationUrl.toString() };
}

async function handleCallback(serverId: string, req: Request): Promise<Response> {
  const entry = pending.get(serverId);
  if (!entry) return new Response("No sign-in in progress.", { status: 400 });
  const url = new URL(req.url);
  // Only the redirect lands on /callback. Ignore stray loopback hits (a browser
  // favicon probe, a prefetch, a port scan) so they don't abort a pending flow.
  if (url.pathname !== "/callback") return new Response("Not found", { status: 404 });
  const err = url.searchParams.get("error");
  const code = url.searchParams.get("code");
  if (err || !code) {
    finish(serverId, false);
    return htmlPage("Sign-in failed. You can close this tab and try again.");
  }
  try {
    const result = await auth(entry.provider, {
      serverUrl: entry.serverUrl,
      authorizationCode: code,
    });
    finish(serverId, result === "AUTHORIZED");
    return htmlPage("Signed in. You can close this tab.");
  } catch (e) {
    log.warn(`MCP OAuth exchange failed: ${errMessage(e)}`);
    finish(serverId, false);
    return htmlPage("Sign-in failed. You can close this tab and try again.");
  }
}

/** Resolve a pending flow once, run its callback, and tear down the listener. */
function finish(serverId: string, ok: boolean): void {
  const entry = pending.get(serverId);
  if (!entry) return;
  pending.delete(serverId);
  clearTimeout(entry.timer);
  entry.ac.abort();
  entry.onComplete(ok);
}

/** Drop any in-flight sign-in for a server (e.g. it was deleted / reconfigured). */
export function cancel(serverId: string): void {
  const entry = pending.get(serverId);
  if (!entry) return;
  pending.delete(serverId);
  clearTimeout(entry.timer);
  entry.ac.abort();
}

function htmlPage(message: string): Response {
  return new Response(
    `<!doctype html><meta charset="utf-8"><title>Loca</title>` +
      `<body style="font:16px system-ui;padding:3rem;text-align:center">${message}</body>`,
    { headers: { "content-type": "text/html; charset=utf-8" } },
  );
}
