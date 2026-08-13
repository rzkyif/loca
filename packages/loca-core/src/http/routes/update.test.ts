// HTTP contract for /api/v1/update. Both endpoints go through the
// real self-updater which fetches/signature-verifies the manifest from
// the live CDN. We only assert that auth is enforced.

import { assertEquals } from "@std/assert";
import { buildApp } from "../server.ts";
import { setupTestEnv } from "../../../tests/helpers/db.ts";

Deno.test("GET /api/v1/update/check: requires bearer (401)", async () => {
  const env = await setupTestEnv();
  try {
    const app = buildApp();
    const res = await app.fetch(new Request("http://x/api/v1/update/check"));
    assertEquals(res.status, 401);
  } finally {
    await env.teardown();
  }
});

Deno.test("POST /api/v1/update/apply: requires bearer (401)", async () => {
  const env = await setupTestEnv();
  try {
    const app = buildApp();
    const res = await app.fetch(new Request("http://x/api/v1/update/apply", { method: "POST" }));
    assertEquals(res.status, 401);
  } finally {
    await env.teardown();
  }
});
