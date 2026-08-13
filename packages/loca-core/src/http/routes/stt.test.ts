// HTTP contract for /api/v1/stt. Provider switching is settings-driven
// and external mode needs network, so we only assert auth + validation.

import { assertEquals } from "@std/assert";
import { engine } from "../../host/engine.ts";
import { pairClient } from "../../../tests/helpers/pairing.ts";
import { setupTestEnv } from "../../../tests/helpers/db.ts";
import { paths } from "../../paths.ts";
import { __resetForTesting as resetCoreSettings } from "@loca/core-engine/services/core-settings";

async function pair(): Promise<{ token: string }> {
  const { token } = await pairClient("t2", "127.0.0.1");
  return { token };
}
const bearer = (token: string) => ({ authorization: `Bearer ${token}` });

Deno.test("POST /api/v1/stt/transcribe: requires bearer (401)", async () => {
  const env = await setupTestEnv();
  try {
    const app = await engine();
    const res = await app.handleHttp(
      new Request("http://x/api/v1/stt/transcribe", { method: "POST" }),
    );
    assertEquals(res.status, 401);
  } finally {
    await env.teardown();
  }
});

Deno.test("POST /api/v1/stt/transcribe: missing audio file returns 400", async () => {
  const env = await setupTestEnv();
  try {
    const { token } = await pair();
    const app = await engine();
    const form = new FormData();
    // Intentionally no audio entry.
    form.set("language", "en");
    const res = await app.handleHttp(
      new Request("http://x/api/v1/stt/transcribe", {
        method: "POST",
        headers: bearer(token),
        body: form,
      }),
    );
    assertEquals(res.status, 400);
    const body = await res.json();
    assertEquals(body.error.code, "validation_error");
  } finally {
    await env.teardown();
  }
});

Deno.test("POST /api/v1/stt/transcribe: audio over the 25 MB cap returns 400", async () => {
  const env = await setupTestEnv();
  try {
    const { token } = await pair();
    const app = await engine();
    const form = new FormData();
    // One byte over the cap; never reaches the provider.
    const oversize = new File([new Uint8Array(25 * 1024 * 1024 + 1)], "big.wav", {
      type: "audio/wav",
    });
    form.set("audio", oversize);
    const res = await app.handleHttp(
      new Request("http://x/api/v1/stt/transcribe", {
        method: "POST",
        headers: bearer(token),
        body: form,
      }),
    );
    assertEquals(res.status, 400);
    const body = await res.json();
    assertEquals(body.error.code, "validation_error");
  } finally {
    await env.teardown();
  }
});

Deno.test("GET /api/v1/stt/status: reports the configured provider", async () => {
  const env = await setupTestEnv();
  try {
    const { token } = await pair();
    const app = await engine();
    const local = await (
      await app.handleHttp(new Request("http://x/api/v1/stt/status", { headers: bearer(token) }))
    ).json();
    assertEquals(local.provider, "local");

    await Deno.writeTextFile(paths().settingsFile, JSON.stringify({ "stt.provider": "external" }));
    resetCoreSettings();
    const external = await (
      await app.handleHttp(new Request("http://x/api/v1/stt/status", { headers: bearer(token) }))
    ).json();
    assertEquals(external.provider, "external");
    assertEquals(external.running, true);
  } finally {
    await env.teardown();
  }
});
