// ExtensionsRegistry CRUD + the grant/tool relationship that the
// permission system depends on. Skip the hash-drift / verifyAllOnBoot path.
// That's exercised separately under hash.test.ts plus an installer
// integration test.

import { assertEquals, assertThrows } from "@std/assert";
import type { PermissionDecl } from "@loca/shared";
import { permissionKey } from "@loca/shared";
import { setupTestEnv } from "../../tests/helpers/db.ts";
import { type ExtensionInsertInput, extensionsRegistry, type ToolInsertInput } from "./registry.ts";
import { AppError } from "@loca/core-engine";

function tk(overrides: Partial<ExtensionInsertInput> = {}): ExtensionInsertInput {
  return {
    id: "example-extension",
    source: "npm",
    displayName: "Example",
    description: "demo",
    version: "1.0.0",
    installedPath: "/tmp/example",
    manifestHash: "deadbeef",
    contentHash: "cafebabe",
    ...overrides,
  };
}

function tool(overrides: Partial<ToolInsertInput> = {}): ToolInsertInput {
  return {
    extensionId: "example-extension",
    name: "do_thing",
    description: "does",
    parameters: { type: "object", properties: {} },
    triggers: [],
    fnExport: "doThing",
    alwaysAvailable: false,
    platforms: [],
    requiredPermissions: [],
    ...overrides,
  };
}

Deno.test("upsertExtension: persists the database declaration as hasDatabase", async () => {
  const env = await setupTestEnv();
  try {
    const r = extensionsRegistry();
    r.upsertExtension(tk({ id: "with-db", hasDatabase: true }));
    r.upsertExtension(tk({ id: "no-db" }));
    assertEquals(r.getOrThrow("with-db").hasDatabase, true);
    assertEquals(r.getOrThrow("no-db").hasDatabase, false);
  } finally {
    await env.teardown();
  }
});

Deno.test("upsertExtension + status transitions: downloaded -> installed -> drift", async () => {
  const env = await setupTestEnv();
  try {
    const r = extensionsRegistry();
    // Download leaves the extension unpinned + 'downloaded'.
    r.upsertExtension(tk({ contentHash: "" }));
    assertEquals(r.get("example-extension")?.status, "downloaded");
    assertEquals(r.get("example-extension")?.contentHash, "");
    // Install pins the hash + flips to 'installed'.
    r.markInstalled("example-extension", "cafebabe");
    assertEquals(r.get("example-extension")?.status, "installed");
    assertEquals(r.get("example-extension")?.contentHash, "cafebabe");
    // Uninstall reverts to 'downloaded' and unpins the hash.
    r.markDownloaded("example-extension");
    assertEquals(r.get("example-extension")?.status, "downloaded");
    assertEquals(r.get("example-extension")?.contentHash, "");
    // A re-download resets to 'downloaded' + clears the pin (re-install needed).
    r.upsertExtension(tk({ version: "1.0.1", contentHash: "" }));
    assertEquals(r.get("example-extension")?.status, "downloaded");
    assertEquals(r.get("example-extension")?.contentHash, "");
    assertEquals(r.get("example-extension")?.version, "1.0.1");
    // Drift keeps the pinned hash but flips status.
    r.markInstalled("example-extension", "feedface");
    r.markDrift("example-extension");
    assertEquals(r.get("example-extension")?.status, "drift");
    assertEquals(r.get("example-extension")?.contentHash, "feedface");
  } finally {
    await env.teardown();
  }
});

Deno.test("listAllTools: only aggregates tools of installed extensions", async () => {
  const env = await setupTestEnv();
  try {
    const r = extensionsRegistry();
    // Installed extension: its tool shows in the aggregate.
    r.upsertExtension(tk({ id: "live" }));
    r.markInstalled("live", "cafebabe");
    r.replaceTools("live", [tool({ extensionId: "live", name: "live_tool" })]);
    // Downloaded-but-not-installed: its tool must stay out.
    r.upsertExtension(tk({ id: "pending", contentHash: "" }));
    r.replaceTools("pending", [tool({ extensionId: "pending", name: "pending_tool" })]);
    // Drifted: also out.
    r.upsertExtension(tk({ id: "bad" }));
    r.markInstalled("bad", "cafebabe");
    r.replaceTools("bad", [tool({ extensionId: "bad", name: "bad_tool" })]);
    r.markDrift("bad");

    const names = r.listAllTools().map((t) => t.name);
    assertEquals(names, ["live_tool"]);
  } finally {
    await env.teardown();
  }
});

Deno.test("getOrThrow: throws extension_not_found on missing id", async () => {
  const env = await setupTestEnv();
  try {
    const r = extensionsRegistry();
    const err = assertThrows(() => r.getOrThrow("nope"), AppError);
    assertEquals(err.code, "extension_not_found");
  } finally {
    await env.teardown();
  }
});

Deno.test("replaceTools: drops old tools, inserts new, preserves enabled on matched names", async () => {
  const env = await setupTestEnv();
  try {
    const r = extensionsRegistry();
    r.upsertExtension(tk());
    r.replaceTools("example-extension", [tool({ name: "keep" }), tool({ name: "drop" })]);
    r.setToolEnabled("example-extension", "keep", true);
    // Re-install: "keep" stays, "drop" is gone, "new" arrives disabled.
    r.replaceTools("example-extension", [tool({ name: "keep" }), tool({ name: "new" })]);
    const tools = r.listTools("example-extension");
    const byName = new Map(tools.map((t) => [t.name, t]));
    assertEquals(byName.has("keep"), true);
    assertEquals(byName.has("drop"), false);
    assertEquals(byName.has("new"), true);
    // Preserved enabled.
    assertEquals(byName.get("keep")?.enabled, true);
    // New tool defaults to disabled (conservative re-install policy).
    assertEquals(byName.get("new")?.enabled, false);
  } finally {
    await env.teardown();
  }
});

Deno.test("always_available: defaults to the manifest, overridable, preserved across re-download", async () => {
  const env = await setupTestEnv();
  try {
    const r = extensionsRegistry();
    r.upsertExtension(tk());
    // Manifest default carries through: declared true stays on, declared false off.
    r.replaceTools("example-extension", [
      tool({ name: "declared_on", alwaysAvailable: true }),
      tool({ name: "declared_off", alwaysAvailable: false }),
    ]);
    let byName = new Map(r.listTools("example-extension").map((t) => [t.name, t]));
    assertEquals(byName.get("declared_on")?.alwaysAvailable, true);
    assertEquals(byName.get("declared_off")?.alwaysAvailable, false);

    // The user override wins over the declared default...
    r.setToolAlwaysAvailable("example-extension", "declared_on", false);
    r.setToolAlwaysAvailable("example-extension", "declared_off", true);
    // ...and survives a re-download even though the manifest still declares the
    // original values.
    r.replaceTools("example-extension", [
      tool({ name: "declared_on", alwaysAvailable: true }),
      tool({ name: "declared_off", alwaysAvailable: false }),
    ]);
    byName = new Map(r.listTools("example-extension").map((t) => [t.name, t]));
    assertEquals(byName.get("declared_on")?.alwaysAvailable, false);
    assertEquals(byName.get("declared_off")?.alwaysAvailable, true);
  } finally {
    await env.teardown();
  }
});

Deno.test("listTools: hides tools the host OS doesn't support", async () => {
  const env = await setupTestEnv();
  try {
    const r = extensionsRegistry();
    r.upsertExtension(tk());
    // A platform token the host can't be: macOS hosts use "darwin", so target
    // "windows" (and vice versa) to get a guaranteed-incompatible tool.
    const otherOs = Deno.build.os === "windows" ? "darwin" : "windows";
    r.replaceTools("example-extension", [
      tool({ name: "everywhere", platforms: [] }),
      tool({ name: "host_only", platforms: [Deno.build.os] }),
      tool({ name: "other_only", platforms: [otherOs] }),
    ]);
    const names = new Set(r.listTools("example-extension").map((t) => t.name));
    assertEquals(names.has("everywhere"), true);
    assertEquals(names.has("host_only"), true);
    assertEquals(names.has("other_only"), false);
    // getTool refuses the incompatible one too, so dispatch can't run it.
    assertEquals(r.getTool("example-extension::other_only"), undefined);
  } finally {
    await env.teardown();
  }
});

Deno.test("replaceTools: preserves grants when required perms unchanged, drops when changed", async () => {
  const env = await setupTestEnv();
  try {
    const r = extensionsRegistry();
    r.upsertExtension(tk());
    const net: PermissionDecl = {
      kind: "net",
      host: "x.example.com",
      ports: [443],
      reason: "y",
    };
    r.replaceTools("example-extension", [tool({ name: "t", requiredPermissions: [net] })]);
    const toolId = r.listTools("example-extension")[0].id;
    r.setGrants(toolId, [
      {
        key: permissionKey(net),
        kind: "net",
        state: "granted",
      },
    ]);

    // Re-download with the SAME required-permission set: grant survives.
    r.replaceTools("example-extension", [tool({ name: "t", requiredPermissions: [net] })]);
    assertEquals(r.listGrantsForTool(toolId).length, 1);
    assertEquals(r.listGrantsForTool(toolId)[0].state, "granted");

    // Re-download with a CHANGED required-permission set: grant dropped.
    const net2: PermissionDecl = {
      kind: "net",
      host: "y.example.com",
      ports: [443],
      reason: "y",
    };
    r.replaceTools("example-extension", [tool({ name: "t", requiredPermissions: [net2] })]);
    assertEquals(r.listGrantsForTool(toolId).length, 0);
  } finally {
    await env.teardown();
  }
});

Deno.test("disableAllTools: clears every tool's enabled flag (drift auto-disable)", async () => {
  const env = await setupTestEnv();
  try {
    const r = extensionsRegistry();
    r.upsertExtension(tk());
    r.replaceTools("example-extension", [tool({ name: "a" }), tool({ name: "b" })]);
    r.setToolEnabled("example-extension", "a", true);
    r.setToolEnabled("example-extension", "b", true);
    r.disableAllTools("example-extension");
    assertEquals(
      r.listTools("example-extension").every((t) => !t.enabled),
      true,
    );
  } finally {
    await env.teardown();
  }
});

Deno.test("list/get project tool counts: total + enabled", async () => {
  const env = await setupTestEnv();
  try {
    const r = extensionsRegistry();
    r.upsertExtension(tk());
    r.replaceTools("example-extension", [
      tool({ name: "a" }),
      tool({ name: "b" }),
      tool({ name: "c" }),
    ]);
    r.setToolEnabled("example-extension", "a", true);
    r.setToolEnabled("example-extension", "b", true);
    const got = r.get("example-extension");
    assertEquals(got?.toolCount, 3);
    assertEquals(got?.enabledToolCount, 2);
    const listed = r.list().find((t) => t.id === "example-extension");
    assertEquals(listed?.toolCount, 3);
    assertEquals(listed?.enabledToolCount, 2);
  } finally {
    await env.teardown();
  }
});

Deno.test("setGrants + listGrantsForTool: records granted + denied, upserts by (tool, key)", async () => {
  const env = await setupTestEnv();
  try {
    const r = extensionsRegistry();
    r.upsertExtension(tk());
    r.replaceTools("example-extension", [tool()]);
    const toolId = r.listTools("example-extension")[0].id;
    const decl: PermissionDecl = {
      kind: "net",
      host: "x.example.com",
      ports: [443],
      reason: "y",
    };
    const key = permissionKey(decl);
    r.setGrants(toolId, [{ key, kind: "net", state: "granted" }]);
    let grants = r.listGrantsForTool(toolId);
    assertEquals(grants.length, 1);
    assertEquals(grants[0].state, "granted");
    // Upsert to denied must not duplicate the row.
    r.setGrants(toolId, [{ key, kind: "net", state: "denied" }]);
    grants = r.listGrantsForTool(toolId);
    assertEquals(grants.length, 1);
    assertEquals(grants[0].state, "denied");
  } finally {
    await env.teardown();
  }
});

Deno.test("delete: cascades to tools and grants", async () => {
  const env = await setupTestEnv();
  try {
    const r = extensionsRegistry();
    r.upsertExtension(tk());
    r.replaceTools("example-extension", [tool()]);
    const toolId = r.listTools("example-extension")[0].id;
    const decl: PermissionDecl = { kind: "ffi", reason: "x" };
    r.setGrants(toolId, [
      {
        key: permissionKey(decl),
        kind: "ffi",
        state: "granted",
      },
    ]);
    r.delete("example-extension");
    assertEquals(r.get("example-extension"), undefined);
    assertEquals(r.listTools("example-extension"), []);
    assertEquals(r.listGrantsForTool(toolId), []);
  } finally {
    await env.teardown();
  }
});

Deno.test("setGrants: round-trips the ask state", async () => {
  const env = await setupTestEnv();
  try {
    const r = extensionsRegistry();
    r.upsertExtension(tk());
    const net: PermissionDecl = {
      kind: "net",
      host: "api.example.com",
      ports: [443],
      reason: "x",
    };
    r.replaceTools("example-extension", [tool({ requiredPermissions: [net] })]);
    const toolId = r.listTools("example-extension")[0].id;
    const key = permissionKey(net);
    r.setGrants(toolId, [{ key, kind: "net", state: "ask" }]);
    assertEquals(r.listGrantsForTool(toolId)[0].state, "ask");
    // ask -> granted upserts in place.
    r.setGrants(toolId, [{ key, kind: "net", state: "granted" }]);
    const grants = r.listGrantsForTool(toolId);
    assertEquals(grants.length, 1);
    assertEquals(grants[0].state, "granted");
  } finally {
    await env.teardown();
  }
});

Deno.test("undeclared policy: defaults to deny, settable, survives re-download", async () => {
  const env = await setupTestEnv();
  try {
    const r = extensionsRegistry();
    r.upsertExtension(tk());
    assertEquals(r.get("example-extension")?.undeclaredPolicy, "deny");
    r.setUndeclaredPolicy("example-extension", "ask");
    assertEquals(r.get("example-extension")?.undeclaredPolicy, "ask");
    // A extension update (re-download upsert) must not reset the user's choice.
    r.upsertExtension(tk({ version: "1.0.1" }));
    assertEquals(r.get("example-extension")?.undeclaredPolicy, "ask");
  } finally {
    await env.teardown();
  }
});
