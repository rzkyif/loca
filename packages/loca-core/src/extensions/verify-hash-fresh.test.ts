// Per-call content-hash re-verification (registry.verifyHashFresh): a extension
// tampered after boot must be refused at tool-call time, not only flagged at
// startup by verifyAllOnBoot.

import { assertEquals, assertRejects } from "@std/assert";
import { join } from "@std/path";
import { setupTestEnv } from "../../tests/helpers/db.ts";
import { extensionsRegistry } from "./registry.ts";
import { hashExtension } from "./hash.ts";
import { AppError } from "@loca/core-engine";

async function makeExtensionDir(): Promise<string> {
  const dir = await Deno.makeTempDir({ prefix: "loca-tk-verify-" });
  await Deno.writeTextFile(join(dir, "index.ts"), "export const x = 1;\n");
  return dir;
}

function upsert(id: string, dir: string, contentHash: string): void {
  // verifyHashFresh only acts on installed extensions; seed status accordingly.
  extensionsRegistry().upsertExtension({
    id,
    source: "npm",
    displayName: id,
    description: "",
    version: "1.0.0",
    installedPath: dir,
    manifestHash: "x",
    contentHash,
    status: "installed",
  });
}

function addEnabledTool(id: string, name: string): void {
  const r = extensionsRegistry();
  r.replaceTools(id, [
    {
      extensionId: id,
      name,
      description: "d",
      parameters: { type: "object", properties: {} },
      triggers: [],
      fnExport: name,
      alwaysAvailable: false,
      platforms: [],
      requiredPermissions: [],
    },
  ]);
  r.setToolEnabled(id, name, true);
}

Deno.test("verifyHashFresh: resolves when on-disk content matches the trusted hash", async () => {
  const env = await setupTestEnv();
  try {
    const dir = await makeExtensionDir();
    upsert("tk-ok", dir, await hashExtension(dir));
    await extensionsRegistry().verifyHashFresh("tk-ok"); // must not throw
    await Deno.remove(dir, { recursive: true });
  } finally {
    await env.teardown();
  }
});

Deno.test("verifyHashFresh: drift flips status to 'drift' and disables tools", async () => {
  const env = await setupTestEnv();
  try {
    const dir = await makeExtensionDir();
    upsert("tk-drift", dir, await hashExtension(dir));
    addEnabledTool("tk-drift", "t");
    // Tamper after "install": the content no longer matches the trusted hash.
    await Deno.writeTextFile(join(dir, "index.ts"), "export const x = 999; // changed\n");
    await assertRejects(
      () => extensionsRegistry().verifyHashFresh("tk-drift"),
      AppError,
      "content changed",
    );
    assertEquals(extensionsRegistry().get("tk-drift")?.status, "drift");
    // Tools are auto-disabled so a drifted extension can never be LLM-exposed.
    assertEquals(extensionsRegistry().listTools("tk-drift")[0].enabled, false);
    await Deno.remove(dir, { recursive: true });
  } finally {
    await env.teardown();
  }
});
