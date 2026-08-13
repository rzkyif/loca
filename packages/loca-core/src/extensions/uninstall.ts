// Extension removal + uninstall.
//
// `deleteExtension` drops the DB rows (tools/grants/embeddings cascade) and removes
// the installed files entirely. Shared by the extensions route (DELETE /:id) and
// the Storage settings view (deleting a extension).
//
// `uninstallExtension` reverts an installed, deps-bearing extension back to
// 'downloaded' by removing the generated dependency artifacts (node_modules +
// deno.lock) and unpinning the hash, keeping the source files so it can be
// re-Installed. A no-dep extension has nothing to uninstall (it is delete-only).
//
// Both are idempotent on the filesystem side (a missing path is fine).

import { join } from "@std/path";
import { deleteExtensionData } from "../services/module-broker.ts";
import { memoriesStore } from "@loca/core-engine/services/memories-store";
import { AppError } from "@loca/core-engine";
import { extensionsRegistry } from "./registry.ts";
import { workerPool } from "./worker-pool.ts";

export async function deleteExtension(id: string): Promise<void> {
  const extension = extensionsRegistry().getOrThrow(id);
  // Refresh the worker's permission view so an in-flight worker drops this
  // extension's grants before its rows vanish.
  await workerPool().refreshPermissions(id);
  extensionsRegistry().delete(id);
  // Drop the read-only memories this extension provided.
  memoriesStore().removeExtensionMemories(id);
  // Drop the extension's private data (its broker-proxied SQLite) with it.
  deleteExtensionData(id);
  try {
    await Deno.remove(extension.installedPath, { recursive: true });
  } catch {
    /* already gone */
  }
}

export async function uninstallExtension(id: string): Promise<void> {
  const tk = extensionsRegistry().getOrThrow(id);
  if (tk.status !== "installed" || !tk.hasDeps) {
    throw new AppError(
      "validation_error",
      `extension ${id} has nothing to uninstall (delete it instead)`,
    );
  }
  await workerPool().refreshPermissions(id);
  // Remove the generated dependency artifacts; the source files stay so the user
  // can re-Install. Both are hash-excluded, so a later re-Install re-pins the
  // same content hash.
  for (const rel of ["node_modules", "deno.lock"]) {
    try {
      await Deno.remove(join(tk.installedPath, rel), { recursive: true });
    } catch {
      /* not present */
    }
  }
  extensionsRegistry().markDownloaded(id);
}
