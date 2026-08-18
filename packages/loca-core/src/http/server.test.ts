// The shell app's own routes. /api/v1/health is open (no bearer) and reports
// the running version + core status; it stays in the shell because it reads
// transport-layer state (behindProxy, admin-password presence).

import { assertEquals } from "@std/assert";
import { buildApp } from "./server.ts";
import { setupTestEnv } from "../../tests/helpers/db.ts";

Deno.test("GET /api/v1/health: open without auth and returns the running version", async () => {
  const env = await setupTestEnv();
  try {
    const app = buildApp();
    const res = await app.fetch(new Request("http://x/api/v1/health"));
    assertEquals(res.status, 200);
    const body = await res.json();
    assertEquals(body.status, "ok");
    assertEquals(typeof body.version, "string");
    // The capability block a Client caches on connect. This binary only ever
    // runs as a desktop service, so every flag is true; the Rust core is where
    // they vary. Asserted here so a Client reading a field this Core never
    // sends is a failing test rather than a silently-undefined gate.
    assertEquals(body.capabilities, {
      extensions: true,
      localInference: true,
      localSpeech: true,
      scheduledPrompts: true,
      stdioMcp: true,
      pairing: true,
      coreUpdate: true,
    });
  } finally {
    await env.teardown();
  }
});
