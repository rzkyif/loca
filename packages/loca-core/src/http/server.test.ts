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
  } finally {
    await env.teardown();
  }
});
