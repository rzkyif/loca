// HTTP contract for /api/v1/sidecars. Only covers the read-side
// (/status) and the invalid-kind guard. Actual start/stop calls would
// spawn real binaries; those are covered separately by sidecars/manager.test.ts.

import { assertEquals } from "@std/assert";
import { buildApp } from "../server.ts";
import { pairClient } from "../../../tests/helpers/pairing.ts";
import { setupTestEnv } from "../../../tests/helpers/db.ts";

async function pairOne(): Promise<string> {
  const { token } = await pairClient("sidecars-t", "127.0.0.1");
  return token;
}

Deno.test("GET /api/v1/sidecars/status: returns sidecar statuses + core process metrics", async () => {
  const env = await setupTestEnv();
  try {
    const token = await pairOne();
    const app = buildApp();
    const res = await app.fetch(
      new Request("http://x/api/v1/sidecars/status", {
        headers: { authorization: `Bearer ${token}` },
      }),
    );
    assertEquals(res.status, 200);
    const body = await res.json();
    assertEquals(Array.isArray(body.sidecars), true);
    // The core process is always sampled (its own PID), so metrics are present.
    assertEquals(typeof body.core.pid, "number");
    assertEquals(typeof body.core.rssMb, "number");
    assertEquals(typeof body.core.cpuPct, "number");
  } finally {
    await env.teardown();
  }
});

Deno.test("POST /api/v1/sidecars/:kind/stop: rejects unknown sidecar kind with 400", async () => {
  const env = await setupTestEnv();
  try {
    const token = await pairOne();
    const app = buildApp();
    const res = await app.fetch(
      new Request("http://x/api/v1/sidecars/not-a-real-kind/stop", {
        method: "POST",
        headers: { authorization: `Bearer ${token}` },
      }),
    );
    assertEquals(res.status, 400);
    const body = await res.json();
    assertEquals(body.error.code, "validation_error");
  } finally {
    await env.teardown();
  }
});
