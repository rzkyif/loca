// HTTP contract for /api/v1/llm. We only assert auth + body validation;
// the singleShot path is exercised at the provider level (llm-provider).

import { assertEquals } from "@std/assert";
import { engine } from "../../host/engine.ts";
import { pairClient } from "../../../tests/helpers/pairing.ts";
import { setupTestEnv } from "../../../tests/helpers/db.ts";

async function pair(): Promise<{ token: string }> {
  const { token } = await pairClient("t2", "127.0.0.1");
  return { token };
}
const bearer = (token: string) => ({ authorization: `Bearer ${token}` });

Deno.test("POST /api/v1/llm/autocorrect: requires bearer (401)", async () => {
  const env = await setupTestEnv();
  try {
    const app = await engine();
    const res = await app.handleHttp(
      new Request("http://x/api/v1/llm/autocorrect", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: "{}",
      }),
    );
    assertEquals(res.status, 401);
  } finally {
    await env.teardown();
  }
});

Deno.test("POST /api/v1/llm/autocorrect: empty/missing text returns 400", async () => {
  const env = await setupTestEnv();
  try {
    const { token } = await pair();
    const app = await engine();
    const res = await app.handleHttp(
      new Request("http://x/api/v1/llm/autocorrect", {
        method: "POST",
        headers: { ...bearer(token), "content-type": "application/json" },
        body: JSON.stringify({ text: "" }),
      }),
    );
    assertEquals(res.status, 400);
  } finally {
    await env.teardown();
  }
});

Deno.test("POST /api/v1/llm/merge: missing existing/next returns 400", async () => {
  const env = await setupTestEnv();
  try {
    const { token } = await pair();
    const app = await engine();
    const res = await app.handleHttp(
      new Request("http://x/api/v1/llm/merge", {
        method: "POST",
        headers: { ...bearer(token), "content-type": "application/json" },
        body: JSON.stringify({ existing: "x" }),
      }),
    );
    assertEquals(res.status, 400);
  } finally {
    await env.teardown();
  }
});
