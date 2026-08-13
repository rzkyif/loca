import { Hono } from "hono";
import { join } from "@std/path";
import { z } from "zod";
import {
  errMessage,
  type Grant,
  parseExtensionManifest,
  permissionKey,
  seededExtensionById,
  type Tool,
} from "@loca/shared";
import {
  flattenPermissions,
  type InstallEventSink,
  registerLocalDownloaded,
  startDownload,
  startInstallDeps,
  startUpdate,
} from "../../extensions/installer.ts";
import { extensionsRegistry } from "../../extensions/registry.ts";
import { hashExtension } from "../../extensions/hash.ts";
import { workerPool } from "../../extensions/worker-pool.ts";
import { deleteExtension, uninstallExtension } from "../../extensions/uninstall.ts";
import { channel, paths } from "../../paths.ts";
import {
  resolveLatestVersion,
  resolveVersion,
  searchPackages,
} from "../../extensions/npm-registry.ts";
import { loadSeededManifest } from "../../extensions/seeded-manifest.ts";
import { requestBuiltinInstall } from "../../extensions/seeding.ts";
import { embed, isEmbeddingModelReady } from "@loca/core-engine/services/embedding";
import { embedWithHash, toolEmbedText } from "@loca/core-engine/services/relevance";
import { toolFilter } from "@loca/core-engine/services/tool-filter";
import { sha256Hex } from "../../shared/hash.ts";
import { AppError } from "@loca/core-engine";
import { parseBody, readJson } from "@loca/core-engine/http/body";
import { bearerMiddleware } from "../middleware/auth.ts";
import { wsHub } from "../../ws/hub.ts";
import { getLogger } from "../../shared/log.ts";

const log = getLogger("extensions.routes");

// Sink that broadcasts install_log + install_done frames to every paired
// client. The client's extensions state subscribes to these and renders
// streamed progress in the install modal. install_done also triggers a
// `extension.snapshot` so the installed list refreshes without polling.
export const BROADCAST_SINK: InstallEventSink = {
  log(jobId, id, stream, line) {
    wsHub().broadcastAll({
      kind: "extension.install_log",
      jobId,
      id,
      stream,
      line,
    });
  },
  done(jobId, id, ok, code) {
    wsHub().broadcastAll({
      kind: "extension.install_done",
      jobId,
      id,
      ok,
      code,
    });
    wsHub().broadcastAll({ kind: "extension.snapshot" });
  },
};

export function extensionsRoutes(): Hono {
  const r = new Hono();
  r.use("*", bearerMiddleware());

  r.get("/", (c) => c.json(extensionsRegistry().list()));

  r.get("/search", async (c) => {
    const q = c.req.query("q") ?? "";
    const limit = numQuery(c, "limit", 50);
    const offset = numQuery(c, "offset", 0);
    return c.json({ results: await searchPackages(q, limit, offset) });
  });

  // Phase 1: acquire a extension's files (npm tarball / built-in / local folder).
  // Deps are NOT installed here; the extension lands in status 'downloaded'.
  r.post("/download", async (c) => {
    const body = (await readJson(c)) as
      | { source: "npm"; name: string; version?: string }
      | { source: "local"; path: string; slug?: string }
      | { source: "seeded"; id: string };
    if (body.source === "npm") {
      return c.json(startDownload(body, BROADCAST_SINK));
    } else if (body.source === "local") {
      if (!body.slug) {
        throw new AppError("validation_error", "slug required for local");
      }
      return c.json(
        startDownload({ source: "local", path: body.path, slug: body.slug }, BROADCAST_SINK),
      );
    } else if (body.source === "seeded") {
      const ext = seededExtensionById(body.id);
      if (!ext) {
        throw new AppError("validation_error", `unknown seeded extension "${body.id}"`);
      }
      if (ext.devOnly && channel() !== "dev") {
        throw new AppError(
          "validation_error",
          `${ext.id} is a dev-only extension and cannot be installed on the ${channel()} channel`,
        );
      }
      return c.json(startDownload({ source: "seeded", id: body.id }, BROADCAST_SINK));
    }
    throw new AppError("validation_error", "unknown source");
  });

  // Phase 2: install a downloaded extension's dependencies, pin its content hash,
  // and flip it to status 'installed' so its tools can be enabled.
  r.post("/:id/install", (c) => {
    const id = c.req.param("id");
    const tk = extensionsRegistry().getOrThrow(id);
    if (tk.status === "drift") {
      throw new AppError("extension_hash_drift", `${id} content drift; confirm re-enable first`);
    }
    return c.json(startInstallDeps(id, BROADCAST_SINK));
  });

  // Install the built-in's tools at the user's explicit request (the Tools
  // prompt). Installs now if the deno worker runtime is present, otherwise
  // queues the install in memory until it lands. `{ queued: true }` tells the
  // client the install is pending the runtime download. A single-segment path so
  // it doesn't collide with the `/:id/install` route.
  r.post("/builtin-install", async (c) => c.json(await requestBuiltinInstall(BROADCAST_SINK)));

  // Check installed extensions for newer versions (npm `dist-tags.latest`, the
  // built-in's signed manifest; local extensions have no upstream). Per-extension
  // errors are isolated so one unreachable registry doesn't fail the batch.
  r.post("/check-updates", async (c) => c.json(await checkUpdates(await readJson(c))));

  // Reconcile the extensions directory with the registry: register folders the
  // user dropped in (as 'downloaded'), re-register ones whose loca.json changed
  // (resetting them to 'downloaded' so the user re-Installs), and prune local
  // extensions whose folder is gone. Rescan never installs deps or pins a hash;
  // that is the per-extension Install step. The extension.snapshot broadcast repaints
  // the client.
  r.post("/rescan", async (c) => c.json(await rescanExtensions()));

  // Delete: remove the extension's files + rows entirely. Allowed once a extension is
  // "uninstalled" (status 'downloaded'), in a 'drift' state, or for a no-dep
  // extension (which is delete-only). The snapshot repaints the client list.
  r.delete("/:id", async (c) => {
    const id = c.req.param("id");
    await deleteExtension(id);
    wsHub().broadcastAll({ kind: "extension.snapshot" });
    return c.body(null, 204);
  });

  // Uninstall: revert an installed, deps-bearing extension to 'downloaded' (drop
  // node_modules + deno.lock, unpin the hash). Source files stay for a re-Install.
  r.post("/:id/uninstall", async (c) => {
    const id = c.req.param("id");
    await uninstallExtension(id);
    wsHub().broadcastAll({ kind: "extension.snapshot" });
    return c.json({ id });
  });

  r.post("/:id/update", async (c) => {
    const id = c.req.param("id");
    const extension = extensionsRegistry().getOrThrow(id);
    const body = (await readJson(c)) as { version?: string };
    if (extension.source === "seeded") {
      // Re-download + re-install from the latest manifest (CDN for the built-in,
      // codebase in dev), same id. The install path re-pins the hash, so a
      // legitimate update never trips drift.
      return c.json(startUpdate({ source: "seeded", id }, BROADCAST_SINK));
    }
    if (extension.source !== "npm") {
      throw new AppError("validation_error", "only npm and seeded extensions can be updated");
    }
    const npmName = id.replace("__", "/");
    const resolved = await resolveVersion(npmName, body.version);
    return c.json(
      startUpdate({ source: "npm", name: npmName, version: resolved.version }, BROADCAST_SINK),
    );
  });

  r.get("/:id/tools", async (c) => {
    const id = c.req.param("id");
    const tools = extensionsRegistry().listTools(id);
    const enriched = await Promise.all(tools.map((t) => attachRequiredPermissions(t)));
    return c.json({ tools: enriched });
  });

  r.post("/:id/tools/:tool/enable", async (c) => {
    const id = c.req.param("id");
    const name = c.req.param("tool");
    const tk = extensionsRegistry().getOrThrow(id);
    if (tk.status === "drift") {
      throw new AppError("extension_hash_drift", `${id} content drift; confirm re-enable first`);
    }
    const tool = extensionsRegistry()
      .listTools(id)
      .find((t) => t.name === name);
    if (!tool) throw new AppError("tool_not_found", `${id}::${name}`);
    // Enabling is allowed even with ungranted required permissions: the tool is
    // then "enabled but not exposed" (a warning state in the UI). chat.ts's
    // exposure gate is the authority on what the model actually sees.
    extensionsRegistry().setToolEnabled(id, name, true);
    await workerPool().refreshPermissions(id);
    // Repaint the list so the "N enabled" badge reflects the change.
    wsHub().broadcastAll({ kind: "extension.snapshot" });
    return c.json({ ok: true });
  });

  r.post("/:id/tools/:tool/disable", async (c) => {
    const id = c.req.param("id");
    const name = c.req.param("tool");
    extensionsRegistry().setToolEnabled(id, name, false);
    await workerPool().refreshPermissions(id);
    // Repaint the list so the "N enabled" badge reflects the change.
    wsHub().broadcastAll({ kind: "extension.snapshot" });
    return c.json({ ok: true });
  });

  // Per-tool override for "offer every turn, skip relevance selection". No
  // worker refresh: this only affects tool selection, not sandbox grants.
  r.post("/:id/tools/:tool/always-available", async (c) => {
    const id = c.req.param("id");
    const name = c.req.param("tool");
    const body = (await readJson(c)) as { value?: unknown };
    if (typeof body.value !== "boolean") {
      throw new AppError("validation_error", "value must be a boolean");
    }
    const tool = extensionsRegistry()
      .listTools(id)
      .find((t) => t.name === name);
    if (!tool) throw new AppError("tool_not_found", `${id}::${name}`);
    extensionsRegistry().setToolAlwaysAvailable(id, name, body.value);
    // Repaint the list so the tool detail reflects the change.
    wsHub().broadcastAll({ kind: "extension.snapshot" });
    return c.json({ ok: true });
  });

  // Confirm-reenable: the user reviewed an out-of-band content change and trusts
  // the current on-disk content. Re-pin the hash, clear drift, return to
  // 'installed'. Tools were disabled when drift was flagged; the user re-enables.
  r.post("/:id/confirm-reenable", async (c) => {
    const id = c.req.param("id");
    const tk = extensionsRegistry().getOrThrow(id);
    const hash = await hashExtension(tk.installedPath);
    extensionsRegistry().repinAndClear(id, hash);
    return c.json({ id });
  });

  r.post("/:id/tools/:tool/grants", async (c) => {
    const id = c.req.param("id");
    const name = c.req.param("tool");
    const tool = extensionsRegistry()
      .listTools(id)
      .find((t) => t.name === name);
    if (!tool) throw new AppError("tool_not_found", `${id}::${name}`);
    const body = (await readJson(c)) as {
      grants: Array<{ key: string; state: Grant["state"] }>;
    };
    if (!Array.isArray(body.grants)) {
      throw new AppError("validation_error", "grants array required");
    }
    const enriched = await attachRequiredPermissions(tool);
    const decoded = body.grants.map((g) => {
      const decl = enriched.requiredPermissions.find((p) => permissionKey(p) === g.key);
      if (!decl) {
        throw new AppError(
          "validation_error",
          `grant key ${g.key} not in this tool's required permissions`,
        );
      }
      if (g.state !== "granted" && g.state !== "ask" && g.state !== "denied") {
        throw new AppError("validation_error", `invalid grant state ${g.state}`);
      }
      return { key: g.key, kind: decl.kind, state: g.state };
    });
    const next = extensionsRegistry().setGrants(tool.id, decoded);
    await workerPool().refreshPermissions(id);
    return c.json({ grants: next });
  });

  r.post("/:id/undeclared-policy", async (c) => {
    const id = c.req.param("id");
    extensionsRegistry().getOrThrow(id);
    const body = (await readJson(c)) as { policy?: string };
    if (body.policy !== "deny" && body.policy !== "ask") {
      throw new AppError("validation_error", "policy must be 'deny' or 'ask'");
    }
    extensionsRegistry().setUndeclaredPolicy(id, body.policy);
    await workerPool().refreshPermissions(id);
    return c.json(extensionsRegistry().getOrThrow(id));
  });

  r.post("/reindex", async (c) => {
    // Tool-relevance RAG needs the embed model. If it isn't downloaded yet,
    // skip cleanly (embedded: 0) rather than letting embed() throw an opaque
    // worker error; the requirements flow prompts the download separately.
    if (!(await isEmbeddingModelReady())) {
      return c.json({ embedded: 0, skipped: true });
    }
    let count = 0;
    const tools = allEnabledTools();
    for (const t of tools) {
      const text = toolEmbedText(t);
      const result = await embedWithHash(text);
      if (result) {
        extensionsRegistry().storeEmbedding(t.id, result.vector, result.sourceHash);
        count++;
      }
    }
    return c.json({ embedded: count });
  });

  r.post("/filter", async (c) => {
    const body = (await readJson(c)) as { vector: number[]; topK?: number };
    if (!Array.isArray(body.vector)) {
      throw new AppError("validation_error", "vector array required");
    }
    const result = toolFilter().phase1(new Float32Array(body.vector), {
      topK: body.topK,
      includeAlwaysAvailable: true,
    });
    return c.json(result);
  });

  const toolSchemasBodySchema = z
    .object({
      ids: z.array(z.string().min(1)).max(512),
    })
    .strict();

  r.post("/tool-schemas", async (c) => {
    const body = parseBody(toolSchemasBodySchema, await readJson(c));
    const tools = body.ids.map((id) => {
      const tool = extensionsRegistry().getTool(id);
      if (!tool) throw new AppError("tool_not_found", id);
      return {
        type: "function" as const,
        function: {
          name: tool.name,
          description: tool.description,
          parameters: tool.parameters,
        },
      };
    });
    return c.json({ tools });
  });

  r.post("/embed", async (c) => {
    const body = (await readJson(c)) as { texts: string[] };
    if (!Array.isArray(body.texts)) {
      throw new AppError("validation_error", "texts array required");
    }
    if (!(await isEmbeddingModelReady())) {
      throw new AppError("server_unavailable", "embedding model not downloaded yet");
    }
    const vectors = await embed(body.texts);
    return c.json({ vectors: vectors.map((v) => Array.from(v)) });
  });

  return r;
}

/** Extension directory ids present on disk, skipping the `.new` / `.old` staging
 *  dirs the installer creates during an atomic swap. */
async function listExtensionDirIds(): Promise<string[]> {
  const ids: string[] = [];
  try {
    for await (const entry of Deno.readDir(paths().extensionsDir)) {
      if (!entry.isDirectory) continue;
      if (entry.name.endsWith(".new") || entry.name.endsWith(".old")) continue;
      ids.push(entry.name);
    }
  } catch {
    // extensionsDir does not exist yet: treat as empty.
  }
  return ids;
}

async function readExtensionManifest(id: string): Promise<string | null> {
  try {
    return await Deno.readTextFile(join(paths().extensionsDir, id, "loca.json"));
  } catch {
    return null;
  }
}

async function checkUpdates(
  body: unknown,
): Promise<{ results: import("@loca/shared").ExtensionUpdateStatus[] }> {
  const ids = (body as { ids?: unknown })?.ids;
  const all = extensionsRegistry().list();
  const targets = Array.isArray(ids) ? all.filter((t) => (ids as unknown[]).includes(t.id)) : all;

  const results: import("@loca/shared").ExtensionUpdateStatus[] = [];
  // Sequential to stay polite to the npm registry; only installed npm/seeded
  // extensions hit the network, and a per-extension failure is isolated.
  for (const tk of targets) {
    const base = { id: tk.id, installedVersion: tk.version };
    const seeded = tk.source === "seeded" ? seededExtensionById(tk.id) : undefined;
    // No upstream to check: local extensions, and any seeded row whose id is no
    // longer a known seeded extension (don't let it fall through to an npm lookup).
    if (tk.source === "local" || (tk.source === "seeded" && !seeded)) {
      results.push({ ...base, latestVersion: null, updateAvailable: false });
      continue;
    }
    try {
      const latestVersion = seeded
        ? (await loadSeededManifest(seeded, { force: true })).version
        : await resolveLatestVersion(tk.id.replace("__", "/"));
      results.push({
        ...base,
        latestVersion,
        updateAvailable: latestVersion !== tk.version,
      });
    } catch (err) {
      results.push({
        ...base,
        latestVersion: null,
        updateAvailable: false,
        error: errMessage(err),
      });
    }
  }
  return { results };
}

async function rescanExtensions(): Promise<{ added: number; updated: number; removed: number }> {
  const ids = await listExtensionDirIds();
  const onDisk = new Set(ids);
  let added = 0;
  let updated = 0;
  let removed = 0;

  for (const id of ids) {
    const manifestText = await readExtensionManifest(id);
    if (!manifestText) continue; // a stray folder without a loca.json: ignore.
    const existing = extensionsRegistry().get(id);
    // Only (re)register when new or when loca.json changed (cheap heuristic; a
    // code-only edit with an unchanged loca.json is left alone). Re-registering
    // resets the row to 'downloaded' so the user must re-Install before its tools
    // run. One folder with invalid loca.json must not fail the whole rescan.
    if (existing && (await sha256Hex(manifestText)) === existing.manifestHash) continue;
    try {
      await registerLocalDownloaded(id);
      if (existing) updated++;
      else added++;
    } catch (err) {
      log.warn(`rescan: skipping ${id}: ${errMessage(err)}`);
    }
  }

  // Prune local extensions whose folder was removed from disk.
  for (const tk of extensionsRegistry().list()) {
    if (tk.source === "local" && !onDisk.has(tk.id)) {
      await workerPool().refreshPermissions(tk.id);
      extensionsRegistry().delete(tk.id);
      removed++;
    }
  }

  // Repaint clients now; each started install also fires its own snapshot on done.
  wsHub().broadcastAll({ kind: "extension.snapshot" });
  return { added, updated, removed };
}

function allEnabledTools(): Array<{
  id: string;
  name: string;
  description: string;
  triggers: string[];
}> {
  // Cheap query straight to the tools table; toolFilter has a richer version.
  // Duplicated here to avoid importing tool-filter.ts internals.
  const list = extensionsRegistry()
    .list()
    .filter((t) => t.status === "installed");
  const out: Array<{ id: string; name: string; description: string; triggers: string[] }> = [];
  for (const tk of list) {
    for (const t of extensionsRegistry().listTools(tk.id)) {
      if (t.enabled) {
        out.push({
          id: t.id,
          name: t.name,
          description: t.description,
          triggers: t.triggers,
        });
      }
    }
  }
  return out;
}

// Resolve a tool's declared required permissions from its extension's manifest
// and which of them have no grant decision yet. Shared with the flat
// `/api/v1/tools` route, which projects the same enriched shape.
export async function attachRequiredPermissions(tool: Tool): Promise<Tool> {
  const extension = extensionsRegistry().getOrThrow(tool.extensionId);
  const manifestPath = join(extension.installedPath, "loca.json");
  try {
    const parsed = parseExtensionManifest(JSON.parse(await Deno.readTextFile(manifestPath)));
    if (!parsed.ok) throw new AppError("invalid_loca_json", parsed.message);
    const def = parsed.value.tools.find((t) => t.name === tool.name);
    if (!def) {
      return { ...tool, requiredPermissions: [], missingRequired: [] };
    }
    const required = flattenPermissions(def.permissions);
    // Informational only: indices with no explicit decision yet (no grant
    // row in any state). An absent row behaves as 'ask' at runtime.
    const decidedKeys = new Set(tool.grants.map((g) => g.permissionKey));
    const missing: number[] = [];
    for (let i = 0; i < required.length; i++) {
      if (required[i].optional) continue;
      if (!decidedKeys.has(permissionKey(required[i]))) missing.push(i);
    }
    return { ...tool, requiredPermissions: required, missingRequired: missing };
  } catch (err) {
    if (err instanceof AppError) throw err;
    return { ...tool, requiredPermissions: [], missingRequired: [] };
  }
}

function numQuery(c: import("hono").Context, key: string, def: number): number {
  const v = c.req.query(key);
  if (!v) return def;
  const n = Number(v);
  return Number.isFinite(n) ? n : def;
}
