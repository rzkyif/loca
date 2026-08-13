// Allowlisted CORS. loca-core's only browser caller is the Tauri webview
// (origins `tauri://localhost` on mac/linux, `https://tauri.localhost` on
// windows) and the Vite dev server on a loopback port. Native HTTP clients
// (curl, the Tauri rust side, anyone using a raw `fetch` from node) don't
// enforce CORS at all, so omitting CORS headers for them is harmless.
//
// Why this matters even on loopback: we accept `X-Admin-Token` here, which
// is a custom header. Without an Origin allowlist, a malicious browser
// page could trigger a CORS preflight to a core bound on 0.0.0.0 and then
// send the token-stealing request. Reflecting `*` plus `X-Admin-Token` in
// Allow-Headers was effectively a wildcard credential grant.

import type { MiddlewareHandler } from "hono";

const ALLOWED_ORIGINS = new Set<string>(["tauri://localhost", "https://tauri.localhost"]);

const LOOPBACK_RE = /^https?:\/\/(localhost|127\.0\.0\.1)(:\d+)?$/;

export function isOriginAllowed(origin: string): boolean {
  if (ALLOWED_ORIGINS.has(origin)) return true;
  return LOOPBACK_RE.test(origin);
}

export function corsMiddleware(): MiddlewareHandler {
  return async (c, next) => {
    const origin = c.req.header("origin");
    const allowed = !!origin && isOriginAllowed(origin);

    // Preflight: answer directly with the CORS headers.
    if (c.req.method === "OPTIONS") {
      if (!allowed) return c.body(null, 204);
      return new Response(null, { status: 204, headers: corsHeaders(origin!) });
    }

    await next();

    // Apply CORS headers to the final response. Setting them on `c.res` here
    // (rather than via c.header() before next()) means they survive even when a
    // handler returns its own `new Response(...)` (e.g. the binary TTS / blob
    // endpoints), which would otherwise drop context-set headers and make the
    // response unreadable to a browser cross-origin caller.
    if (allowed) {
      const h = corsHeaders(origin!);
      for (const [k, v] of Object.entries(h)) c.res.headers.set(k, v);
    }
    // No Origin header (curl, native HTTP, server-to-server) -> no CORS headers;
    // CORS only gates browser-driven cross-origin fetches.
  };
}

function corsHeaders(origin: string): Record<string, string> {
  return {
    "Access-Control-Allow-Origin": origin,
    Vary: "Origin",
    "Access-Control-Allow-Methods": "GET,POST,PUT,PATCH,DELETE,OPTIONS",
    "Access-Control-Allow-Headers": "Authorization,Content-Type,X-Admin-Token",
    "Access-Control-Max-Age": "86400",
  };
}
