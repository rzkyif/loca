import { assertEquals } from "@std/assert";
import { Hono } from "hono";
import { corsMiddleware, isOriginAllowed } from "./cors.ts";

function appWithCors(): Hono {
  const app = new Hono();
  app.use("*", corsMiddleware());
  app.get("/ping", (c) => c.json({ ok: true }));
  // A handler that returns its OWN raw Response (like the binary TTS / blob
  // endpoints), bypassing Hono's context-built response.
  app.get("/blob", () => new Response(new Uint8Array([1, 2, 3]), { status: 200 }));
  return app;
}

Deno.test("isOriginAllowed: accepts tauri custom schemes", () => {
  assertEquals(isOriginAllowed("tauri://localhost"), true);
  assertEquals(isOriginAllowed("https://tauri.localhost"), true);
});

Deno.test("isOriginAllowed: accepts loopback http(s) on any port", () => {
  assertEquals(isOriginAllowed("http://localhost"), true);
  assertEquals(isOriginAllowed("http://localhost:1420"), true);
  assertEquals(isOriginAllowed("http://127.0.0.1:7800"), true);
  assertEquals(isOriginAllowed("https://localhost:8443"), true);
});

Deno.test("isOriginAllowed: rejects everything else", () => {
  assertEquals(isOriginAllowed("https://evil.example"), false);
  assertEquals(isOriginAllowed("http://attacker.local"), false);
  assertEquals(isOriginAllowed("null"), false);
  assertEquals(isOriginAllowed("file://"), false);
  // Subdomain attack on tauri.localhost.
  assertEquals(isOriginAllowed("https://x.tauri.localhost"), false);
  // 127.0.0.1 lookalike: only the canonical form matches.
  assertEquals(isOriginAllowed("http://127.0.0.2"), false);
});

Deno.test("CORS: reflects allowed origin and emits Vary", async () => {
  const app = appWithCors();
  const res = await app.fetch(
    new Request("http://x/ping", {
      headers: { origin: "tauri://localhost" },
    }),
  );
  assertEquals(res.status, 200);
  assertEquals(res.headers.get("access-control-allow-origin"), "tauri://localhost");
  assertEquals(res.headers.get("vary"), "Origin");
});

Deno.test("CORS: cross-origin request gets no ACAO header", async () => {
  // The browser then refuses to expose the response to the page. Critically,
  // X-Admin-Token is NOT in Allow-Headers either, so a preflight would fail
  // and the actual request would never be sent.
  const app = appWithCors();
  const res = await app.fetch(
    new Request("http://x/ping", {
      headers: { origin: "https://evil.example" },
    }),
  );
  assertEquals(res.status, 200);
  assertEquals(res.headers.get("access-control-allow-origin"), null);
  assertEquals(res.headers.get("access-control-allow-headers"), null);
});

Deno.test("CORS: preflight from cross-origin is 204 with no allow headers", async () => {
  const app = appWithCors();
  const res = await app.fetch(
    new Request("http://x/ping", {
      method: "OPTIONS",
      headers: {
        origin: "https://evil.example",
        "access-control-request-method": "POST",
        "access-control-request-headers": "X-Admin-Token",
      },
    }),
  );
  assertEquals(res.status, 204);
  assertEquals(res.headers.get("access-control-allow-origin"), null);
});

Deno.test("CORS: preflight from an allowed origin advertises every method a route uses", async () => {
  // The memories bundled-file route (PUT /memories/:id/files/:name) and the
  // secrets route (PUT /settings/secrets/:name) are PUT, so a browser preflight
  // must see PUT in Allow-Methods or the actual request never leaves the page.
  const app = appWithCors();
  const res = await app.fetch(
    new Request("http://x/ping", {
      method: "OPTIONS",
      headers: {
        origin: "http://localhost:1420",
        "access-control-request-method": "PUT",
      },
    }),
  );
  assertEquals(res.status, 204);
  const methods = res.headers.get("access-control-allow-methods") ?? "";
  for (const m of ["GET", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"]) {
    assertEquals(methods.includes(m), true, `Allow-Methods missing ${m}: ${methods}`);
  }
});

Deno.test("CORS: headers are applied to a handler returning a raw Response", async () => {
  // The binary endpoints (TTS WAV, attachment download) return their own
  // `new Response(...)`. CORS headers must still be present so a browser
  // cross-origin caller can read the body.
  const app = appWithCors();
  const res = await app.fetch(
    new Request("http://x/blob", { headers: { origin: "http://localhost:1420" } }),
  );
  assertEquals(res.status, 200);
  assertEquals(res.headers.get("access-control-allow-origin"), "http://localhost:1420");
  assertEquals(res.headers.get("vary"), "Origin");
});

Deno.test("CORS: no Origin header means no CORS headers, request proceeds", async () => {
  // curl / native http clients don't send Origin; CORS doesn't apply to
  // them anyway, so the middleware just lets the request through.
  const app = appWithCors();
  const res = await app.fetch(new Request("http://x/ping"));
  assertEquals(res.status, 200);
  assertEquals(res.headers.get("access-control-allow-origin"), null);
});
