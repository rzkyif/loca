// Storage view for the Settings "Storage" field: a full per-category view of
// everything Tomat keeps on disk (downloaded models, sidecar binaries, the
// sessions JSON store, installed extensions, caches, logs, settings) plus the
// clear/delete operations for each. The core is the sole authority on what may
// be removed: every node is tagged with a `lock_reason` when it's in use, and a
// delete request re-derives that lock server-side and refuses anything locked
// (never trusting the client's view).
//
// In-use ("locked") rules per category:
//   - models:   required by the current settings (requiredModelRefs)
//   - binaries: required by settings OR its sidecar is running
//   - cache:    never (regenerable)
//   - logs:     the live core.log only
//   - sessions: an agent is mid-turn on it (chat activeSessionIds)
//   - extensions: it has an enabled tool, or it's the built-in extension
//   - settings: never; "clearing" it is a factory reset (defaults + wipe secrets)

import { basename, dirname, isAbsolute, join, normalize, relative } from "@std/path";
import type { BinaryKind, StorageCategory, StorageCategoryId, StorageNode } from "@tomat/shared";
import { BUILTIN_EXTENSION_ID, requiredBinaryKinds, requiredModelRefs } from "@tomat/shared";
import { extensionDir, paths, sessionDir } from "../paths.ts";
import { AppError } from "@tomat/core-engine";
import { getLogger } from "../shared/log.ts";
import { resolveHfPath } from "../models/manager.ts";
import { binariesManager } from "../binaries/manager.ts";
import { sidecarManager } from "../sidecars/manager.ts";
import { extensionsRegistry } from "../extensions/registry.ts";
import { deleteExtension } from "../extensions/uninstall.ts";
import { sessionsRepo } from "@tomat/core-engine/services/sessions-store";
import { chatService } from "@tomat/core-engine/services/chat";
import {
  loadCoreSettingsResolved,
  resetAllClientSettings,
  resetCoreSettings,
} from "@tomat/core-engine/services/core-settings";
import { clearAllSecrets } from "@tomat/core-engine/services/secrets";
import { notifyRequirementsChanged } from "./requirements.ts";

const log = getLogger("storage");

// Extensions that count as model / support files: gguf (llama.cpp), onnx (sherpa
// Whisper + Kokoro), txt (sherpa tokens + the Kokoro voices token table), bin
// (Kokoro voices pack + GGUF siblings), json + pt (legacy support files).
const MODEL_FILE_EXTS = new Set(["gguf", "bin", "onnx", "json", "txt", "pt"]);

// Running sidecar kind -> the binary it runs. deno is always required (it runs
// the tool worker), so it's covered by requiredBinaryKinds. llama-embed runs a
// second llama-server instance (embeddings), so it locks the same binary; speech
// runs tomat-core-speech (Whisper STT + Kokoro TTS).
const SIDECAR_BINARY: Partial<Record<string, BinaryKind>> = {
  llama: "llama-server",
  "llama-embed": "llama-server",
  speech: "tomat-core-speech",
};

// --- lock context ----------------------------------------------------------

interface LockContext {
  requiredModelPaths: Set<string>;
  lockedBinaryKinds: Set<BinaryKind>;
  activeSessionIds: Set<string>;
  enabledExtensionIds: Set<string>;
}

async function buildLockContext(): Promise<LockContext> {
  const settings = await loadCoreSettingsResolved();

  const requiredModelPaths = new Set(
    requiredModelRefs(settings).map((r) => normalize(resolveHfPath(r.source))),
  );

  const lockedBinaryKinds = new Set<BinaryKind>(requiredBinaryKinds(settings));
  for (const s of sidecarManager().getStatuses()) {
    if (s.status !== "Running") continue;
    const bin = SIDECAR_BINARY[s.kind];
    if (bin) lockedBinaryKinds.add(bin);
  }

  const enabledExtensionIds = new Set<string>();
  try {
    for (const tk of extensionsRegistry().list()) {
      if (
        extensionsRegistry()
          .listTools(tk.id)
          .some((t) => t.enabled)
      ) {
        enabledExtensionIds.add(tk.id);
      }
    }
  } catch (err) {
    // A extension-registry read failure (e.g. a stale DB schema) must not blank
    // the whole storage view; fall back to "nothing enabled" (the extensions
    // category itself degrades to empty below).
    log.warn(`extension lock scan failed: ${err instanceof Error ? err.message : err}`);
  }

  return {
    requiredModelPaths,
    lockedBinaryKinds,
    activeSessionIds: chatService().activeSessionIds(),
    enabledExtensionIds,
  };
}

// --- node helpers ----------------------------------------------------------

function fileNode(name: string, path: string, size: number, lock?: string): StorageNode {
  return lock
    ? { kind: "file", name, path, size, lock_reason: lock }
    : { kind: "file", name, path, size };
}

function folderNode(
  name: string,
  path: string,
  size: number,
  children: StorageNode[] = [],
  lock?: string,
): StorageNode {
  return lock
    ? { kind: "folder", name, path, size, children, lock_reason: lock }
    : { kind: "folder", name, path, size, children };
}

async function statSize(path: string): Promise<number> {
  try {
    return (await Deno.stat(path)).size;
  } catch {
    return 0;
  }
}

async function dirSize(path: string): Promise<number> {
  let total = 0;
  let entries: AsyncIterable<Deno.DirEntry>;
  try {
    entries = Deno.readDir(path);
  } catch {
    return 0;
  }
  for await (const e of entries) {
    const full = join(path, e.name);
    if (e.isDirectory) total += await dirSize(full);
    else if (e.isFile) total += await statSize(full);
  }
  return total;
}

// --- models ----------------------------------------------------------------

async function collectModelFiles(base: string, dir: string, out: StorageNode[]): Promise<void> {
  let entries: AsyncIterable<Deno.DirEntry>;
  try {
    entries = Deno.readDir(dir);
  } catch {
    return;
  }
  for await (const e of entries) {
    const p = join(dir, e.name);
    if (e.isDirectory) {
      await collectModelFiles(base, p, out);
    } else if (e.isFile) {
      const ext = (e.name.includes(".") ? e.name.split(".").pop()! : "").toLowerCase();
      if (!MODEL_FILE_EXTS.has(ext)) continue;
      const size = await statSize(p);
      out.push({
        kind: "file",
        name: relative(base, p).replaceAll("\\", "/"),
        path: p,
        size,
      });
    }
  }
}

function applyModelLock(node: StorageNode, ctx: LockContext): boolean {
  // Annotate a model file/folder with a lock reason in place. Returns whether
  // the node ended up locked (a folder is locked iff every child is).
  if (node.kind === "file") {
    if (ctx.requiredModelPaths.has(normalize(node.path))) {
      node.lock_reason = "Required by current settings";
      return true;
    }
    return false;
  }
  if (node.children.length === 0) return false;
  let allLocked = true;
  for (const c of node.children) if (!applyModelLock(c, ctx)) allLocked = false;
  if (allLocked) node.lock_reason = "Required by current settings";
  return allLocked;
}

async function buildModelNodes(ctx: LockContext): Promise<StorageNode[]> {
  const modelsDir = paths().modelsDir;
  const out: StorageNode[] = [];
  let users: AsyncIterable<Deno.DirEntry>;
  try {
    users = Deno.readDir(modelsDir);
  } catch {
    return out;
  }
  for await (const user of users) {
    if (!user.isDirectory) continue;
    const userPath = join(modelsDir, user.name);
    let repos: AsyncIterable<Deno.DirEntry>;
    try {
      repos = Deno.readDir(userPath);
    } catch {
      continue;
    }
    for await (const repo of repos) {
      if (!repo.isDirectory) continue;
      const repoPath = join(userPath, repo.name);
      const display = `${user.name}/${repo.name}`;
      const files: StorageNode[] = [];
      await collectModelFiles(repoPath, repoPath, files);
      files.sort((a, b) => a.name.localeCompare(b.name));
      if (files.length === 0) continue;
      const hasMmproj = files.some((f) => basename(f.name).toLowerCase().startsWith("mmproj"));
      if (hasMmproj || files.length > 1) {
        const size = files.reduce((s, f) => s + f.size, 0);
        out.push(folderNode(display, repoPath, size, files));
      } else {
        const f = files[0];
        out.push(fileNode(`${display}/${f.name}`, f.path, f.size));
      }
    }
  }
  out.sort((a, b) => a.name.localeCompare(b.name));
  for (const n of out) applyModelLock(n, ctx);
  return out;
}

// --- category descriptors --------------------------------------------------

interface CategoryDescriptor {
  id: StorageCategoryId;
  label: string;
  deletable: boolean;
  buildNodes(ctx: LockContext): Promise<StorageNode[]>;
  deleteNode(node: StorageNode): Promise<void>;
}

const CATEGORIES: CategoryDescriptor[] = [
  {
    id: "models",
    label: "Models",
    deletable: true,
    buildNodes: buildModelNodes,
    deleteNode: async (node) => {
      assertUnder(node.path, paths().modelsDir);
      await removePath(node.path);
    },
  },
  {
    id: "binaries",
    label: "Binaries",
    deletable: true,
    async buildNodes(ctx) {
      const list = await binariesManager()
        .list()
        .catch(() => []);
      const out: StorageNode[] = [];
      for (const b of list) {
        if (!b.installed || !b.path) continue;
        const lock = ctx.lockedBinaryKinds.has(b.kind)
          ? "Required by an enabled feature"
          : undefined;
        out.push(fileNode(b.kind, b.path, await statSize(b.path), lock));
      }
      return out;
    },
    deleteNode: async (node) => {
      assertUnder(node.path, paths().binDir);
      await removePath(node.path);
    },
  },
  {
    id: "cache",
    label: "Cache",
    deletable: true,
    async buildNodes() {
      const p = paths();
      const dirs: Array<[string, string]> = [
        ["cache", p.cacheDir],
        ["deno-cache", p.denoCacheDir],
        ["staging", p.stagingDir],
      ];
      const out: StorageNode[] = [];
      for (const [name, dir] of dirs) {
        out.push(folderNode(name, dir, await dirSize(dir)));
      }
      return out;
    },
    deleteNode: async (node) => {
      await removePath(node.path);
      // Cache dirs are long-lived (created by ensureDirs); recreate empty.
      await Deno.mkdir(node.path, { recursive: true });
    },
  },
  {
    id: "logs",
    label: "Logs",
    deletable: true,
    async buildNodes() {
      const p = paths();
      const out: StorageNode[] = [];
      let entries: AsyncIterable<Deno.DirEntry>;
      try {
        entries = Deno.readDir(p.logsDir);
      } catch {
        return out;
      }
      for await (const e of entries) {
        if (!e.isFile) continue;
        const path = join(p.logsDir, e.name);
        // All logs are clearable: the active core.log is truncated (not removed)
        // so the running core keeps logging to it; rotated backups are removed.
        out.push(fileNode(e.name, path, await statSize(path)));
      }
      out.sort((a, b) => a.name.localeCompare(b.name));
      return out;
    },
    deleteNode: async (node) => {
      assertUnder(node.path, paths().logsDir);
      if (normalize(node.path) === normalize(paths().logFile)) {
        // Active log: truncate in place. Deleting the open file wouldn't reclaim
        // space on Unix (the handler keeps the inode) and fails on Windows;
        // truncation empties it now and the logger keeps appending from 0.
        await Deno.truncate(node.path, 0).catch(() => {});
      } else {
        await removePath(node.path);
      }
    },
  },
  {
    id: "sessions",
    label: "Sessions",
    deletable: true,
    async buildNodes(ctx) {
      const sessions = await sessionsRepo().listAll();
      return sessions
        .sort((a, b) => b.updatedAtMs - a.updatedAtMs)
        .map((s) =>
          folderNode(
            s.title || "Untitled session",
            sessionDir(s.id),
            s.sizeBytes,
            [],
            ctx.activeSessionIds.has(s.id) ? "Session is active" : undefined,
          ),
        );
    },
    deleteNode: async (node) => {
      await sessionsRepo().deleteById(basename(node.path));
    },
  },
  {
    id: "extensions",
    label: "Extensions",
    deletable: true,
    async buildNodes(ctx) {
      const out: StorageNode[] = [];
      for (const tk of extensionsRegistry().list()) {
        const dir = extensionDir(tk.id);
        const lock =
          tk.source === "seeded"
            ? tk.id === BUILTIN_EXTENSION_ID
              ? "Built-in extension"
              : "Seeded extension"
            : ctx.enabledExtensionIds.has(tk.id)
              ? "Has enabled tools"
              : undefined;
        out.push(folderNode(tk.displayName || tk.id, dir, await dirSize(dir), [], lock));
      }
      out.sort((a, b) => a.name.localeCompare(b.name));
      return out;
    },
    deleteNode: async (node) => {
      await deleteExtension(basename(node.path));
    },
  },
  {
    id: "settings",
    label: "Settings",
    deletable: true,
    async buildNodes() {
      const p = paths();
      const size = (await statSize(p.settingsFile)) + (await statSize(p.secretsEncFile));
      // Never locked; deleting it is a factory reset, not a file removal.
      return [fileNode("Settings + secrets", p.settingsFile, size)];
    },
    deleteNode: async () => {
      await resetCoreSettings();
      await resetAllClientSettings();
      await clearAllSecrets();
    },
  },
];

const CATEGORY_BY_ID = new Map(CATEGORIES.map((c) => [c.id, c]));

// --- public API ------------------------------------------------------------

/** Build one category's nodes, degrading to an empty list (logged) on failure
 *  so a single broken category can never blank the whole storage view. */
async function safeBuildNodes(desc: CategoryDescriptor, ctx: LockContext): Promise<StorageNode[]> {
  try {
    return await desc.buildNodes(ctx);
  } catch (err) {
    log.warn(
      `storage category ${desc.id} failed to build: ${err instanceof Error ? err.message : err}`,
    );
    return [];
  }
}

export async function buildStorageTree() {
  const ctx = await buildLockContext();
  const categories: StorageCategory[] = [];
  for (const desc of CATEGORIES) {
    const nodes = await safeBuildNodes(desc, ctx);
    const size = nodes.reduce((s, n) => s + n.size, 0);
    categories.push({
      id: desc.id,
      label: desc.label,
      deletable: desc.deletable,
      nodes,
      size,
    });
  }
  return {
    categories,
    total_size: categories.reduce((s, c) => s + c.size, 0),
    // `<root_path>/models` is the models dir, so the client can still map an HF
    // spec to disk if needed.
    root_path: dirname(paths().modelsDir),
  };
}

/** Delete the given paths. Each must match a node in the freshly-built tree,
 *  belong to a deletable category, and not be locked (nor contain any locked
 *  descendant). The lock is recomputed here server-side, so a stale or tampered
 *  client view can't force a locked delete. Unknown paths are skipped
 *  (idempotent). */
export async function deleteStoragePaths(targets: string[]): Promise<void> {
  const { byPath } = await indexTree();
  const ops: Array<{ desc: CategoryDescriptor; node: StorageNode }> = [];
  for (const t of targets) {
    const hit = byPath.get(normalize(t));
    if (!hit) continue; // already gone
    const { desc, node } = hit;
    if (!desc.deletable) {
      throw new AppError("validation_error", `category ${desc.id} is not deletable`);
    }
    if (subtreeLockReason(node)) {
      throw new AppError("validation_error", `cannot delete in-use item: ${node.path}`);
    }
    ops.push({ desc, node });
  }
  for (const { desc, node } of ops) {
    try {
      await desc.deleteNode(node);
    } catch (err) {
      log.warn(`delete ${node.path} failed: ${err instanceof Error ? err.message : err}`);
      throw err;
    }
  }
  void notifyRequirementsChanged();
}

/** Clear an entire category: delete every non-locked top-level node in it. For
 *  `settings` this performs the factory reset. */
export async function clearStorageCategory(categoryId: string): Promise<void> {
  const desc = CATEGORY_BY_ID.get(categoryId as StorageCategoryId);
  if (!desc) {
    throw new AppError("validation_error", `unknown storage category: ${categoryId}`);
  }
  if (!desc.deletable) {
    throw new AppError("validation_error", `category ${desc.id} is not deletable`);
  }
  const ctx = await buildLockContext();
  const nodes = await safeBuildNodes(desc, ctx);
  for (const node of nodes) {
    if (subtreeLockReason(node)) continue;
    try {
      await desc.deleteNode(node);
    } catch (err) {
      log.warn(`clear ${node.path} failed: ${err instanceof Error ? err.message : err}`);
    }
  }
  void notifyRequirementsChanged();
}

// --- internals -------------------------------------------------------------

async function indexTree(): Promise<{
  byPath: Map<string, { desc: CategoryDescriptor; node: StorageNode }>;
}> {
  const ctx = await buildLockContext();
  const byPath = new Map<string, { desc: CategoryDescriptor; node: StorageNode }>();
  const add = (desc: CategoryDescriptor, node: StorageNode) => {
    byPath.set(normalize(node.path), { desc, node });
    if (node.kind === "folder") {
      for (const c of node.children) add(desc, c);
    }
  };
  for (const desc of CATEGORIES) {
    for (const node of await safeBuildNodes(desc, ctx)) add(desc, node);
  }
  return { byPath };
}

/** The lock reason for a node or, for a folder, the first locked descendant.
 *  Used to refuse deleting anything in use. */
function subtreeLockReason(node: StorageNode): string | undefined {
  if (node.lock_reason) return node.lock_reason;
  if (node.kind === "folder") {
    for (const c of node.children) {
      const r = subtreeLockReason(c);
      if (r) return r;
    }
  }
  return undefined;
}

async function removePath(path: string): Promise<void> {
  try {
    await Deno.remove(path, { recursive: true });
  } catch (err) {
    if (!(err instanceof Deno.errors.NotFound)) throw err;
  }
}

// Reject any path that doesn't sit strictly inside `root` (no `..` escape, no
// absolute re-root, not the root itself).
function assertUnder(target: string, root: string): void {
  const rel = relative(normalize(root), normalize(target));
  if (rel === "" || rel === "." || rel.startsWith("..") || isAbsolute(rel)) {
    throw new AppError("validation_error", `path is not under ${root}: ${target}`);
  }
}
