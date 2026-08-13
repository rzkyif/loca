// HTTP contract for /api/v1/settings and the secrets sub-resource.
// Asserts the "names only, never values" rule that the secrets endpoint
// promises.

import { assertEquals } from "@std/assert";
import { engine } from "../../host/engine.ts";
import { pairClient } from "../../../tests/helpers/pairing.ts";
import { setupTestEnv } from "../../../tests/helpers/db.ts";

async function pairOne(): Promise<string> {
  const { token } = await pairClient("settings-t", "127.0.0.1");
  return token;
}

function bearer(token: string) {
  return { authorization: `Bearer ${token}` };
}

Deno.test("GET /api/v1/settings: returns {} for a fresh core", async () => {
  const env = await setupTestEnv();
  try {
    const token = await pairOne();
    const app = await engine();
    const res = await app.handleHttp(
      new Request("http://x/api/v1/settings", { headers: bearer(token) }),
    );
    assertEquals(res.status, 200);
    assertEquals(await res.json(), {});
  } finally {
    await env.teardown();
  }
});

Deno.test("PATCH /api/v1/settings: persists keys and returns the merged object", async () => {
  const env = await setupTestEnv();
  try {
    const token = await pairOne();
    const app = await engine();
    const res = await app.handleHttp(
      new Request("http://x/api/v1/settings", {
        method: "PATCH",
        headers: { ...bearer(token), "content-type": "application/json" },
        body: JSON.stringify({ "llm.host": "0.0.0.0" }),
      }),
    );
    assertEquals(res.status, 200);
    const body = await res.json();
    assertEquals(body["llm.host"], "0.0.0.0");
  } finally {
    await env.teardown();
  }
});

Deno.test("PATCH /api/v1/settings: rejects an unknown key with 400", async () => {
  const env = await setupTestEnv();
  try {
    const token = await pairOne();
    const app = await engine();
    const res = await app.handleHttp(
      new Request("http://x/api/v1/settings", {
        method: "PATCH",
        headers: { ...bearer(token), "content-type": "application/json" },
        body: JSON.stringify({ "ui.theme": "dark" }),
      }),
    );
    assertEquals(res.status, 400);
    assertEquals((await res.json()).error.code, "validation_error");
  } finally {
    await env.teardown();
  }
});

Deno.test("PATCH /api/v1/settings: rejects a client-destination key with 400", async () => {
  const env = await setupTestEnv();
  try {
    const token = await pairOne();
    const app = await engine();
    const res = await app.handleHttp(
      new Request("http://x/api/v1/settings", {
        method: "PATCH",
        headers: { ...bearer(token), "content-type": "application/json" },
        body: JSON.stringify({ "appearance.theme": "dark" }),
      }),
    );
    assertEquals(res.status, 400);
    assertEquals((await res.json()).error.code, "validation_error");
  } finally {
    await env.teardown();
  }
});

Deno.test("GET /api/v1/settings: drops non-schema keys from the response", async () => {
  const env = await setupTestEnv();
  try {
    const token = await pairOne();
    const app = await engine();
    // Simulate a stray key sitting in settings.json by writing through the
    // settings service directly (the route guard would reject it).
    const { patchCoreSettings } = await import("@loca/core-engine/services/core-settings");
    await patchCoreSettings({ "internal.junk": 1, "llm.host": "0.0.0.0" });
    const res = await app.handleHttp(
      new Request("http://x/api/v1/settings", { headers: bearer(token) }),
    );
    assertEquals(res.status, 200);
    const body = await res.json();
    assertEquals("internal.junk" in body, false);
    assertEquals(body["llm.host"], "0.0.0.0");
  } finally {
    await env.teardown();
  }
});

Deno.test("PATCH /api/v1/settings: rejects a wrong-typed known key with 400", async () => {
  const env = await setupTestEnv();
  try {
    const token = await pairOne();
    const app = await engine();
    // llm.host is a string setting; a number is a type mismatch.
    const res = await app.handleHttp(
      new Request("http://x/api/v1/settings", {
        method: "PATCH",
        headers: { ...bearer(token), "content-type": "application/json" },
        body: JSON.stringify({ "llm.host": 1234 }),
      }),
    );
    assertEquals(res.status, 400);
    assertEquals((await res.json()).error.code, "validation_error");
  } finally {
    await env.teardown();
  }
});

Deno.test("PATCH /api/v1/settings: rejects a secret-typed key (must use the vault)", async () => {
  const env = await setupTestEnv();
  try {
    const token = await pairOne();
    const app = await engine();
    const res = await app.handleHttp(
      new Request("http://x/api/v1/settings", {
        method: "PATCH",
        headers: { ...bearer(token), "content-type": "application/json" },
        body: JSON.stringify({
          "llm.external.apiKey": "sk-should-be-rejected",
        }),
      }),
    );
    assertEquals(res.status, 400);
    assertEquals((await res.json()).error.code, "validation_error");
  } finally {
    await env.teardown();
  }
});

Deno.test("GET /api/v1/settings: never returns secret-typed values", async () => {
  const env = await setupTestEnv();
  try {
    const token = await pairOne();
    const app = await engine();
    // Simulate a plaintext key sitting in settings.json by writing it through
    // the settings service directly (bypassing the route guard).
    const { patchCoreSettings } = await import("@loca/core-engine/services/core-settings");
    await patchCoreSettings({ "llm.external.apiKey": "sk-must-be-redacted" });
    const res = await app.handleHttp(
      new Request("http://x/api/v1/settings", { headers: bearer(token) }),
    );
    assertEquals(res.status, 200);
    const body = await res.json();
    assertEquals("llm.external.apiKey" in body, false);
    assertEquals(JSON.stringify(body).includes("sk-must-be-redacted"), false);
  } finally {
    await env.teardown();
  }
});

Deno.test("PATCH /api/v1/settings: rejects non-object body with 400", async () => {
  const env = await setupTestEnv();
  try {
    const token = await pairOne();
    const app = await engine();
    const res = await app.handleHttp(
      new Request("http://x/api/v1/settings", {
        method: "PATCH",
        headers: { ...bearer(token), "content-type": "application/json" },
        body: JSON.stringify([1, 2, 3]),
      }),
    );
    assertEquals(res.status, 400);
    const body = await res.json();
    assertEquals(body.error.code, "validation_error");
  } finally {
    await env.teardown();
  }
});

Deno.test("PUT /api/v1/settings/secrets/:name: stores secret then GET returns name only (never value)", async () => {
  const env = await setupTestEnv();
  try {
    const token = await pairOne();
    const app = await engine();
    const put = await app.handleHttp(
      new Request("http://x/api/v1/settings/secrets/openai-api-key", {
        method: "PUT",
        headers: { ...bearer(token), "content-type": "application/json" },
        body: JSON.stringify({ value: "sk-secret-do-not-leak" }),
      }),
    );
    assertEquals(put.status, 204);

    const list = await app.handleHttp(
      new Request("http://x/api/v1/settings/secrets", {
        headers: bearer(token),
      }),
    );
    const body = await list.json();
    assertEquals(body, { names: ["openai-api-key"] });
    // No value field anywhere in the response.
    assertEquals(JSON.stringify(body).includes("sk-secret"), false);
  } finally {
    await env.teardown();
  }
});

Deno.test("DELETE /api/v1/settings/secrets/:name: 404 when the name was never set", async () => {
  const env = await setupTestEnv();
  try {
    const token = await pairOne();
    const app = await engine();
    const res = await app.handleHttp(
      new Request("http://x/api/v1/settings/secrets/absent", {
        method: "DELETE",
        headers: bearer(token),
      }),
    );
    assertEquals(res.status, 404);
  } finally {
    await env.teardown();
  }
});
