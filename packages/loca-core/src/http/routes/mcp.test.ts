// HTTP contract for /api/v1/mcp: bearer gating, create/patch validation, the
// write-only authToken semantics (stored in the vault, never echoed, "" clears),
// and per-tool enable/disable. Servers are created disabled so no real
// connection is attempted.

import { assertEquals } from "@std/assert";
import { engine } from "../../host/engine.ts";
import type { EngineInstance } from "@loca/core-engine";
import { pairClient } from "../../../tests/helpers/pairing.ts";
import { setupTestEnv } from "../../../tests/helpers/db.ts";
import { getSecret } from "@loca/core-engine/services/secrets";
import { mcpAuthSecretName } from "../../mcp/secret-key.ts";

const bearer = (token: string) => ({ authorization: `Bearer ${token}` });
const json = (token: string) => ({ ...bearer(token), "content-type": "application/json" });

async function setup(): Promise<{ token: string; app: EngineInstance }> {
  const { token } = await pairClient("mcp-test", "127.0.0.1");
  return { token, app: await engine() };
}

Deno.test("GET /api/v1/mcp: requires bearer (401)", async () => {
  const env = await setupTestEnv();
  try {
    const app = await engine();
    const res = await app.handleHttp(new Request("http://x/api/v1/mcp"));
    assertEquals(res.status, 401);
  } finally {
    await env.teardown();
  }
});

Deno.test("POST /api/v1/mcp: rejects an unknown transport kind (400)", async () => {
  const env = await setupTestEnv();
  try {
    const { token, app } = await setup();
    const res = await app.handleHttp(
      new Request("http://x/api/v1/mcp", {
        method: "POST",
        headers: json(token),
        body: JSON.stringify({ name: "X", kind: "ftp" }),
      }),
    );
    assertEquals(res.status, 400);
  } finally {
    await env.teardown();
  }
});

Deno.test("authToken is write-only: stored in the vault, never echoed, '' clears it", async () => {
  const env = await setupTestEnv();
  try {
    const { token, app } = await setup();

    // Create a disabled remote server with a bearer token.
    const created = await app.handleHttp(
      new Request("http://x/api/v1/mcp", {
        method: "POST",
        headers: json(token),
        body: JSON.stringify({
          name: "Remote",
          kind: "remote",
          url: "https://example.com/mcp",
          enabled: false,
          authToken: "sk-secret",
        }),
      }),
    );
    assertEquals(created.status, 201);
    const server = await created.json();
    assertEquals(server.hasAuth, true);
    // The token is never part of the projection.
    assertEquals("authToken" in server, false);
    assertEquals(JSON.stringify(server).includes("sk-secret"), false);
    // It lives in the vault under the server's key.
    assertEquals(await getSecret(mcpAuthSecretName(server.id)), "sk-secret");

    // PATCH without authToken leaves it untouched.
    await app.handleHttp(
      new Request(`http://x/api/v1/mcp/${server.id}`, {
        method: "PATCH",
        headers: json(token),
        body: JSON.stringify({ name: "Renamed" }),
      }),
    );
    assertEquals(await getSecret(mcpAuthSecretName(server.id)), "sk-secret");
    assertEquals((await (await get(app, token, server.id)).json()).hasAuth, true);

    // PATCH authToken "" clears it and flips hasAuth false.
    await app.handleHttp(
      new Request(`http://x/api/v1/mcp/${server.id}`, {
        method: "PATCH",
        headers: json(token),
        body: JSON.stringify({ authToken: "" }),
      }),
    );
    assertEquals(await getSecret(mcpAuthSecretName(server.id)), undefined);
    assertEquals((await (await get(app, token, server.id)).json()).hasAuth, false);
  } finally {
    await env.teardown();
  }
});

Deno.test("per-tool enable/disable toggles the row; bad action is 400", async () => {
  const env = await setupTestEnv();
  try {
    const { token, app } = await setup();
    const created = await app.handleHttp(
      new Request("http://x/api/v1/mcp", {
        method: "POST",
        headers: json(token),
        body: JSON.stringify({ name: "S", kind: "stdio", enabled: false }),
      }),
    );
    const { id } = await created.json();

    const enabled = await app.handleHttp(
      new Request(`http://x/api/v1/mcp/${id}/tools/search/enable`, {
        method: "POST",
        headers: bearer(token),
      }),
    );
    assertEquals(enabled.status, 200);
    assertEquals((await enabled.json()).toolEnabled, ["search"]);

    const bad = await app.handleHttp(
      new Request(`http://x/api/v1/mcp/${id}/tools/search/frobnicate`, {
        method: "POST",
        headers: bearer(token),
      }),
    );
    assertEquals(bad.status, 400);
  } finally {
    await env.teardown();
  }
});

function get(app: EngineInstance, token: string, id: string): Promise<Response> {
  return app.handleHttp(new Request(`http://x/api/v1/mcp/${id}`, { headers: bearer(token) }));
}
