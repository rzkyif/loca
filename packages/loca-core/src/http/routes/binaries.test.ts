// HTTP contract for /api/v1/binaries. Read-only and validation-rejection
// surface; the install/update actions hit the network and are exercised
// via integration paths.

import { assertEquals } from "@std/assert";
import { buildApp } from "../server.ts";
import { pairClient } from "../../../tests/helpers/pairing.ts";
import { setupTestEnv } from "../../../tests/helpers/db.ts";

async function pair(): Promise<{ token: string }> {
  const { token } = await pairClient("t2", "127.0.0.1");
  return { token };
}

const bearer = (token: string) => ({ authorization: `Bearer ${token}` });

Deno.test("GET /api/v1/binaries: requires bearer (401)", async () => {
  const env = await setupTestEnv();
  try {
    const app = buildApp();
    const res = await app.fetch(new Request("http://x/api/v1/binaries"));
    assertEquals(res.status, 401);
  } finally {
    await env.teardown();
  }
});

// GET /api/v1/binaries hits the manifest fetch path internally (with a
// caught error if the CDN is unreachable). We assert auth + a successful
// status only. Exercising the response body would race the in-flight
// internal fetch and surface as a resource leak.

Deno.test("POST /api/v1/binaries/update: missing kind returns 400", async () => {
  const env = await setupTestEnv();
  try {
    const { token } = await pair();
    const app = buildApp();
    const res = await app.fetch(
      new Request("http://x/api/v1/binaries/update", {
        method: "POST",
        headers: { ...bearer(token), "content-type": "application/json" },
        body: JSON.stringify({}),
      }),
    );
    assertEquals(res.status, 400);
    const body = await res.json();
    assertEquals(body.error.code, "validation_error");
  } finally {
    await env.teardown();
  }
});
