// Models manager: list / download / delete / probe HF model files.
// Backed by the centralized DownloadManager so progress surfaces over WS.

import { join, relative } from "@std/path";
import type { DownloadEntry, ModelEntry, ProbeModelsResponse } from "@loca/shared";
import { downloadManager } from "../downloads/manager.ts";
import { probeSource } from "../downloads/sources.ts";
import { paths } from "../paths.ts";
import { AppError } from "@loca/core-engine";

export type ModelGroup = "llm" | "stt" | "tts" | "embed";

export class ModelsManager {
  // Recursive walk of ~/.loca/core/models/ returning a flat list of files.
  async list(): Promise<ModelEntry[]> {
    const root = paths().modelsDir;
    const out: ModelEntry[] = [];
    await walk(root, root, out);
    out.sort((a, b) => a.relPath.localeCompare(b.relPath));
    return out;
  }

  // Enqueue downloads for one or more HF specs. Returns each spec's queue-row
  // id (one row per file), the id cancel/retry/remove accept.
  download(items: Array<{ source: string; group?: ModelGroup }>): string[] {
    const ids: string[] = [];
    for (const item of items) {
      const spec = {
        source: item.source,
        destination: "models" as const,
        groupId: item.group ?? "llm",
      };
      const id = downloadManager().idFor(spec);
      void downloadManager()
        .enqueue(spec)
        .catch(() => {
          // Errors surface on the WS via the downloads.snapshot broadcast; we
          // don't need to propagate them here because the caller already
          // received the row ids and is subscribed.
        });
      ids.push(id);
    }
    return ids;
  }

  async delete(relPath: string): Promise<void> {
    const safe = sanitizeRelPath(relPath);
    const abs = join(paths().modelsDir, safe);
    try {
      await Deno.remove(abs);
    } catch (err) {
      if (err instanceof Deno.errors.NotFound) {
        throw new AppError("model_not_found", `model not on disk: ${relPath}`);
      }
      throw err;
    }
    // Drop any Completed download row for the file we just removed so the list
    // stays in sync with disk.
    await downloadManager().reconcileCompleted();
  }

  async probe(sources: string[], opts: { network?: boolean } = {}): Promise<ProbeModelsResponse> {
    const root = paths().modelsDir;
    const out: ProbeModelsResponse = [];
    for (const source of sources) {
      const r = await probeSource(source, root, { network: opts.network });
      out.push({
        source,
        alreadyHave: r.alreadyHave,
        sizeHint: r.sizeBytes,
      });
    }
    return out;
  }

  // Convenience pass-throughs for the model download UI.
  downloads(): DownloadEntry[] {
    return downloadManager()
      .snapshot()
      .filter((d) => d.destination === "models");
  }

  cancel(id: string): void {
    downloadManager().cancel(id);
  }
  retry(id: string): void {
    downloadManager().retry(id);
  }
  remove(id: string): void {
    downloadManager().remove(id);
  }
}

let _instance: ModelsManager | null = null;
export function modelsManager(): ModelsManager {
  if (!_instance) _instance = new ModelsManager();
  return _instance;
}

export function __resetForTesting(): void {
  _instance = null;
}

/** Resolve an HF spec (`@user/repo/branch/file`) to its absolute on-disk
 *  path under the models root. Branch is dropped on disk so the same file
 *  isn't redundantly tracked per branch. Non-`@` specs are returned as-is
 *  (treated as already-absolute paths). */
export function resolveHfPath(spec: string): string {
  if (!spec.startsWith("@")) return spec;
  const m = spec.match(/^@([^/]+)\/([^/]+)\/([^/]+)\/(.+)$/);
  if (!m) return spec;
  const [, user, repo, , file] = m;
  return join(paths().modelsDir, user, repo, file);
}

// Reject any rel-path that escapes the models root.
function sanitizeRelPath(relPath: string): string {
  if (relPath.includes("\0") || relPath.includes("..")) {
    throw new AppError("validation_error", `invalid model relPath: ${relPath}`);
  }
  return relPath.replace(/^\/+/, "");
}

async function walk(root: string, dir: string, out: ModelEntry[]): Promise<void> {
  let entries: AsyncIterable<Deno.DirEntry>;
  try {
    entries = Deno.readDir(dir);
  } catch (err) {
    if (err instanceof Deno.errors.NotFound) return;
    throw err;
  }
  for await (const entry of entries) {
    const full = join(dir, entry.name);
    if (entry.isDirectory) {
      await walk(root, full, out);
    } else if (entry.isFile) {
      try {
        const st = await Deno.stat(full);
        const relPath = relative(root, full).replaceAll("\\", "/");
        out.push({
          source: `@${relPath.replace(/^([^/]+)\/([^/]+)\//, "$1/$2/main/")}`,
          relPath,
          absPath: full,
          sizeBytes: st.size,
        });
      } catch {
        /* skip unreadable */
      }
    }
  }
}
