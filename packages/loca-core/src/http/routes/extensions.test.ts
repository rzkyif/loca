// HTTP contract for /api/v1/extensions. Avoids exercising npm install
// (network); the local-install path is verified by installer.t1. Here we
// only assert routing, auth, validation, and the read-only views over a
// tempdir registry.

import { assertEquals } from "@std/assert";
import { join } from "@std/path";
import { buildApp } from "../server.ts";
import { pairClient } from "../../../tests/helpers/pairing.ts";
import { setupTestEnv } from "../../../tests/helpers/db.ts";
import { extensionsRegistry } from "../../extensions/registry.ts";

async function pairOne(): Promise<{ token: string }> {
  const { token } = await pairClient("t2", "127.0.0.1");
  return { token };
}

function bearer(token: string): Record<string, string> {
  return { authorization: `Bearer ${token}` };
}

function postJson(token: string, path: string): Request {
  return new Request(`http://x${path}`, {
    method: "POST",
    headers: { ...bearer(token), "content-type": "application/json" },
    body: "{}",
  });
}

// Seed an INSTALLED extension (hash pinned, on-disk folder present) with one tool
// that declares an ungranted net permission. Returns the temp install dir.
async function seedInstalledExtension(id: string, hasDeps = false): Promise<string> {
  const dir = await Deno.makeTempDir({ prefix: "loca-tk-route-" });
  await Deno.writeTextFile(join(dir, "index.ts"), "export const x = 1;\n");
  const r = extensionsRegistry();
  r.upsertExtension({
    id,
    source: "local",
    displayName: id,
    description: "",
    version: "local",
    installedPath: dir,
    manifestHash: "x",
    contentHash: "pinned",
    status: "installed",
    hasDeps,
  });
  r.replaceTools(id, [
    {
      extensionId: id,
      name: "t",
      description: "d",
      parameters: { type: "object", properties: {} },
      triggers: [],
      fnExport: "t",
      alwaysAvailable: false,
      platforms: [],
      requiredPermissions: [
        {
          kind: "net",
          host: "x",
          ports: [443],
          reason: "y",
        },
      ],
    },
  ]);
  return dir;
}

Deno.test("GET /api/v1/extensions: requires bearer (401)", async () => {
  const env = await setupTestEnv();
  try {
    const app = buildApp();
    const res = await app.fetch(new Request("http://x/api/v1/extensions"));
    assertEquals(res.status, 401);
  } finally {
    await env.teardown();
  }
});

Deno.test("GET /api/v1/extensions: empty list before any install", async () => {
  const env = await setupTestEnv();
  try {
    const { token } = await pairOne();
    const app = buildApp();
    const res = await app.fetch(
      new Request("http://x/api/v1/extensions", { headers: bearer(token) }),
    );
    assertEquals(res.status, 200);
    assertEquals(await res.json(), []);
  } finally {
    await env.teardown();
  }
});

Deno.test("POST /api/v1/extensions/embed: 503 server_unavailable when the embed model is absent", async () => {
  const env = await setupTestEnv();
  try {
    const { token } = await pairOne();
    const app = buildApp();
    // Fresh tempdir: no embed model on disk, so the gate trips before the
    // worker is ever spawned (a clean 503 instead of an opaque 500).
    const res = await app.fetch(
      new Request("http://x/api/v1/extensions/embed", {
        method: "POST",
        headers: { ...bearer(token), "content-type": "application/json" },
        body: JSON.stringify({ texts: ["hello"] }),
      }),
    );
    assertEquals(res.status, 503);
    assertEquals((await res.json()).error.code, "server_unavailable");
  } finally {
    await env.teardown();
  }
});

Deno.test("DELETE /api/v1/extensions/:id: 404 with extension_not_found when id is unknown", async () => {
  const env = await setupTestEnv();
  try {
    const { token } = await pairOne();
    const app = buildApp();
    const res = await app.fetch(
      new Request("http://x/api/v1/extensions/no-such-id", {
        method: "DELETE",
        headers: bearer(token),
      }),
    );
    assertEquals(res.status, 404);
    const body = await res.json();
    assertEquals(body.error.code, "extension_not_found");
  } finally {
    await env.teardown();
  }
});

Deno.test("DELETE /api/v1/extensions/:id: removes the extension row", async () => {
  const env = await setupTestEnv();
  try {
    const { token } = await pairOne();
    const id = "tk-del";
    const dir = await seedInstalledExtension(id);
    try {
      const app = buildApp();
      const res = await app.fetch(
        new Request(`http://x/api/v1/extensions/${id}`, {
          method: "DELETE",
          headers: bearer(token),
        }),
      );
      assertEquals(res.status, 204);
      assertEquals(extensionsRegistry().get(id), undefined);
    } finally {
      await Deno.remove(dir, { recursive: true }).catch(() => {});
    }
  } finally {
    await env.teardown();
  }
});

Deno.test("POST /:id/uninstall: reverts an installed deps extension to 'downloaded'", async () => {
  const env = await setupTestEnv();
  try {
    const { token } = await pairOne();
    const id = "tk-uninstall";
    const dir = await seedInstalledExtension(id, true);
    try {
      const app = buildApp();
      const res = await app.fetch(postJson(token, `/api/v1/extensions/${id}/uninstall`));
      assertEquals(res.status, 200);
      const tk = extensionsRegistry().get(id);
      assertEquals(tk?.status, "downloaded");
      assertEquals(tk?.contentHash, "");
    } finally {
      await Deno.remove(dir, { recursive: true }).catch(() => {});
    }
  } finally {
    await env.teardown();
  }
});

Deno.test("POST /:id/uninstall: 400 for a no-dep extension (delete-only)", async () => {
  const env = await setupTestEnv();
  try {
    const { token } = await pairOne();
    const id = "tk-nodep";
    const dir = await seedInstalledExtension(id, false);
    try {
      const app = buildApp();
      const res = await app.fetch(postJson(token, `/api/v1/extensions/${id}/uninstall`));
      assertEquals(res.status, 400);
      // Unchanged: still installed.
      assertEquals(extensionsRegistry().get(id)?.status, "installed");
    } finally {
      await Deno.remove(dir, { recursive: true }).catch(() => {});
    }
  } finally {
    await env.teardown();
  }
});

Deno.test("POST /api/v1/extensions/download: unknown source kind returns 400", async () => {
  const env = await setupTestEnv();
  try {
    const { token } = await pairOne();
    const app = buildApp();
    const res = await app.fetch(
      new Request("http://x/api/v1/extensions/download", {
        method: "POST",
        headers: { ...bearer(token), "content-type": "application/json" },
        body: JSON.stringify({ source: "ftp", name: "x" }),
      }),
    );
    assertEquals(res.status, 400);
  } finally {
    await env.teardown();
  }
});

Deno.test("POST /api/v1/extensions/download: local source requires slug (400)", async () => {
  const env = await setupTestEnv();
  try {
    const { token } = await pairOne();
    const app = buildApp();
    const res = await app.fetch(
      new Request("http://x/api/v1/extensions/download", {
        method: "POST",
        headers: { ...bearer(token), "content-type": "application/json" },
        body: JSON.stringify({ source: "local", path: "/tmp/x" }),
      }),
    );
    assertEquals(res.status, 400);
  } finally {
    await env.teardown();
  }
});

Deno.test("POST /api/v1/extensions/:id/install: 404 for an unknown id", async () => {
  const env = await setupTestEnv();
  try {
    const { token } = await pairOne();
    const app = buildApp();
    const res = await app.fetch(postJson(token, "/api/v1/extensions/nope/install"));
    assertEquals(res.status, 404);
  } finally {
    await env.teardown();
  }
});

Deno.test("POST /:id/tools/:tool/enable: succeeds even with ungranted required perms", async () => {
  const env = await setupTestEnv();
  try {
    const { token } = await pairOne();
    const id = "tk-enable";
    const dir = await seedInstalledExtension(id);
    try {
      const app = buildApp();
      const res = await app.fetch(postJson(token, `/api/v1/extensions/${id}/tools/t/enable`));
      assertEquals(res.status, 200);
      // Enabling is no longer permission-gated; the tool flips on (the
      // chat-exposure gate withholds it from the model until granted).
      assertEquals(extensionsRegistry().listTools(id)[0].enabled, true);
    } finally {
      await Deno.remove(dir, { recursive: true });
    }
  } finally {
    await env.teardown();
  }
});

Deno.test("POST /:id/tools/:tool/always-available: overrides the flag; rejects a non-boolean", async () => {
  const env = await setupTestEnv();
  try {
    const { token } = await pairOne();
    const id = "tk-always";
    const dir = await seedInstalledExtension(id);
    try {
      const app = buildApp();
      const url = `/api/v1/extensions/${id}/tools/t/always-available`;
      const post = (body: string): Request =>
        new Request(`http://x${url}`, {
          method: "POST",
          headers: { ...bearer(token), "content-type": "application/json" },
          body,
        });
      // Seeded tool declares alwaysAvailable: false; the override flips it on.
      assertEquals(extensionsRegistry().listTools(id)[0].alwaysAvailable, false);
      assertEquals((await app.fetch(post(`{"value":true}`))).status, 200);
      assertEquals(extensionsRegistry().listTools(id)[0].alwaysAvailable, true);
      assertEquals((await app.fetch(post(`{"value":false}`))).status, 200);
      assertEquals(extensionsRegistry().listTools(id)[0].alwaysAvailable, false);
      // A missing/non-boolean value is a validation error.
      assertEquals((await app.fetch(post(`{}`))).status, 400);
    } finally {
      await Deno.remove(dir, { recursive: true });
    }
  } finally {
    await env.teardown();
  }
});

Deno.test("POST /:id/confirm-reenable: re-pins hash + clears drift -> installed", async () => {
  const env = await setupTestEnv();
  try {
    const { token } = await pairOne();
    const id = "tk-confirm";
    const dir = await seedInstalledExtension(id);
    try {
      extensionsRegistry().markDrift(id);
      assertEquals(extensionsRegistry().get(id)?.status, "drift");
      const app = buildApp();
      const res = await app.fetch(postJson(token, `/api/v1/extensions/${id}/confirm-reenable`));
      assertEquals(res.status, 200);
      assertEquals(extensionsRegistry().get(id)?.status, "installed");
    } finally {
      await Deno.remove(dir, { recursive: true });
    }
  } finally {
    await env.teardown();
  }
});

Deno.test("POST /api/v1/extensions/filter: vector array required (400 when missing)", async () => {
  const env = await setupTestEnv();
  try {
    const { token } = await pairOne();
    const app = buildApp();
    const res = await app.fetch(
      new Request("http://x/api/v1/extensions/filter", {
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

Deno.test("POST /api/v1/extensions/embed: texts array required (400 when missing)", async () => {
  const env = await setupTestEnv();
  try {
    const { token } = await pairOne();
    const app = buildApp();
    const res = await app.fetch(
      new Request("http://x/api/v1/extensions/embed", {
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
