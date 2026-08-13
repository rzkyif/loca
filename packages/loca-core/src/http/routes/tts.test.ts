// HTTP contract for /api/v1/tts. Voices catalog + status are read-only;
// synthesize requires the kokoro sidecar (out of scope here).

import { assertEquals } from "@std/assert";
import { engine } from "../../host/engine.ts";
import { pairClient } from "../../../tests/helpers/pairing.ts";
import { setupTestEnv } from "../../../tests/helpers/db.ts";

async function pair(): Promise<{ token: string }> {
  const { token } = await pairClient("t2", "127.0.0.1");
  return { token };
}
const bearer = (token: string) => ({ authorization: `Bearer ${token}` });

Deno.test("GET /api/v1/tts/voices: requires bearer (401)", async () => {
  const env = await setupTestEnv();
  try {
    const app = await engine();
    const res = await app.handleHttp(new Request("http://x/api/v1/tts/voices"));
    assertEquals(res.status, 401);
  } finally {
    await env.teardown();
  }
});

Deno.test("GET /api/v1/tts/voices: returns the selected model's catalog voices", async () => {
  const env = await setupTestEnv();
  // The route reads the signed catalog; on the dev channel it is compiled from
  // the in-repo @loca/model-catalog (no network), so the default model's voices
  // (Kokoro) resolve here.
  const priorChannel = Deno.env.get("LOCA_CHANNEL");
  Deno.env.set("LOCA_CHANNEL", "dev");
  try {
    const { token } = await pair();
    const app = await engine();
    const res = await app.handleHttp(
      new Request("http://x/api/v1/tts/voices", { headers: bearer(token) }),
    );
    assertEquals(res.status, 200);
    const body = (await res.json()) as Array<{ id: string; label: string; lang?: string }>;
    assertEquals(Array.isArray(body), true);
    assertEquals(body.length > 0, true);
    // Every voice has the fields the UI reads.
    for (const v of body) {
      assertEquals(typeof v.id, "string");
      assertEquals(typeof v.label, "string");
    }
  } finally {
    if (priorChannel === undefined) Deno.env.delete("LOCA_CHANNEL");
    else Deno.env.set("LOCA_CHANNEL", priorChannel);
    await env.teardown();
  }
});

Deno.test("POST /api/v1/tts/synthesize: empty text returns 400", async () => {
  const env = await setupTestEnv();
  try {
    const { token } = await pair();
    const app = await engine();
    const res = await app.handleHttp(
      new Request("http://x/api/v1/tts/synthesize", {
        method: "POST",
        headers: { ...bearer(token), "content-type": "application/json" },
        body: JSON.stringify({}),
      }),
    );
    assertEquals(res.status, 400);
  } finally {
    await env.teardown();
  }
});

Deno.test("POST /api/v1/tts/synthesize: text over the length cap returns 400", async () => {
  const env = await setupTestEnv();
  try {
    const { token } = await pair();
    const app = await engine();
    const res = await app.handleHttp(
      new Request("http://x/api/v1/tts/synthesize", {
        method: "POST",
        headers: { ...bearer(token), "content-type": "application/json" },
        body: JSON.stringify({ text: "a".repeat(2_001) }),
      }),
    );
    assertEquals(res.status, 400);
    const body = await res.json();
    assertEquals(body.error.code, "validation_error");
  } finally {
    await env.teardown();
  }
});

Deno.test("GET /api/v1/tts/status: returns the controller status object", async () => {
  const env = await setupTestEnv();
  try {
    const { token } = await pair();
    const app = await engine();
    const res = await app.handleHttp(
      new Request("http://x/api/v1/tts/status", { headers: bearer(token) }),
    );
    assertEquals(res.status, 200);
    const body = await res.json();
    // Shape: at minimum the controller reports loaded boolean.
    assertEquals(typeof body, "object");
    assertEquals(body !== null, true);
  } finally {
    await env.teardown();
  }
});
