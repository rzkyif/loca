// Extensions + Tools + Grants registry (DB-backed).
//
// Aggregates queries against the `extensions`, `tools`, `tool_embeddings`, and
// `grants` tables. Higher-level orchestration (installer, worker pool) lives
// in sibling files.

import { join } from "@std/path";
import type {
  Extension,
  ExtensionSource,
  ExtensionStatus,
  Grant,
  GrantState,
  PermissionDecl,
  Tool,
  UndeclaredPolicy,
} from "@loca/shared";
import { db } from "@loca/core-engine";
import { paths } from "../paths.ts";
import { AppError } from "@loca/core-engine";
import { hostPlatforms } from "../shared/platform.ts";
import { errMessage, permissionKey, toolPlatformSupported } from "@loca/shared";
import { hashExtension, statSignature } from "./hash.ts";
import { getLogger } from "../shared/log.ts";

export interface ExtensionInsertInput {
  id: string;
  source: ExtensionSource;
  displayName: string;
  description?: string;
  version: string;
  installedPath: string;
  manifestHash: string;
  // Empty string until the extension is installed; download leaves it unpinned.
  contentHash: string;
  // Defaults to "downloaded" when omitted (the download path never installs).
  status?: ExtensionStatus;
  // Whether the extension declares dependencies. Defaults to false.
  hasDeps?: boolean;
  // Whether loca.json declares `database: true`. Defaults to false.
  hasDatabase?: boolean;
}

export interface ToolInsertInput {
  extensionId: string;
  name: string;
  description: string;
  parameters: Record<string, unknown>;
  triggers: string[];
  fnExport: string;
  alwaysAvailable: boolean;
  platforms: string[];
  requiredPermissions: PermissionDecl[];
}

export class ExtensionsRegistry {
  // Per-call hash-verify skip cache: extensionId -> { sig, hash }. When the
  // stat signature (count + total size + newest mtime) across a extension's files
  // is unchanged from a prior successful verify, the content is very unlikely to
  // have changed, so the per-call re-hash is skipped. (mtime alone was forgeable
  // by a tool resetting its own files' mtime; see statSignature.)
  private hashVerifiedCache = new Map<string, { sig: string; hash: string }>();

  // --- extension-level ------------------------------------------------------

  list(): Extension[] {
    const rows = db()
      .prepare(`
      SELECT id, source, display_name, description, version, installed_path,
             manifest_hash, content_hash, status, has_deps, has_database,
             undeclared_policy,
             (SELECT COUNT(*) FROM tools WHERE extension_id = extensions.id) AS tool_count,
             (SELECT COUNT(*) FROM tools WHERE extension_id = extensions.id AND enabled = 1)
               AS enabled_tool_count,
             installed_at_ms, updated_at_ms
      FROM extensions
      ORDER BY display_name COLLATE NOCASE ASC
    `)
      .all() as Array<Record<string, unknown>>;
    return rows.map(rowToExtension);
  }

  get(id: string): Extension | undefined {
    const row = db()
      .prepare(`
      SELECT id, source, display_name, description, version, installed_path,
             manifest_hash, content_hash, status, has_deps, has_database,
             undeclared_policy,
             (SELECT COUNT(*) FROM tools WHERE extension_id = extensions.id) AS tool_count,
             (SELECT COUNT(*) FROM tools WHERE extension_id = extensions.id AND enabled = 1)
               AS enabled_tool_count,
             installed_at_ms, updated_at_ms
      FROM extensions WHERE id = ?
    `)
      .get(id) as Record<string, unknown> | undefined;
    return row ? rowToExtension(row) : undefined;
  }

  getOrThrow(id: string): Extension {
    const tk = this.get(id);
    if (!tk) {
      throw new AppError("extension_not_found", `extension ${id} not found`);
    }
    return tk;
  }

  upsertExtension(input: ExtensionInsertInput): void {
    const now = Date.now();
    const existing = this.get(input.id);
    if (existing) {
      db()
        .prepare(`
        UPDATE extensions
           SET source = ?, display_name = ?, description = ?, version = ?,
               installed_path = ?, manifest_hash = ?, content_hash = ?,
               status = ?, has_deps = ?, has_database = ?, updated_at_ms = ?
         WHERE id = ?
      `)
        .run(
          input.source,
          input.displayName,
          input.description ?? null,
          input.version,
          input.installedPath,
          input.manifestHash,
          input.contentHash,
          input.status ?? "downloaded",
          input.hasDeps ? 1 : 0,
          input.hasDatabase ? 1 : 0,
          now,
          input.id,
        );
    } else {
      db()
        .prepare(`
        INSERT INTO extensions
          (id, source, display_name, description, version, installed_path,
           manifest_hash, content_hash, status, has_deps, has_database,
           installed_at_ms, updated_at_ms)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
      `)
        .run(
          input.id,
          input.source,
          input.displayName,
          input.description ?? null,
          input.version,
          input.installedPath,
          input.manifestHash,
          input.contentHash,
          input.status ?? "downloaded",
          input.hasDeps ? 1 : 0,
          input.hasDatabase ? 1 : 0,
          now,
          now,
        );
    }
  }

  setStatus(id: string, status: ExtensionStatus): void {
    db()
      .prepare(`UPDATE extensions SET status = ?, updated_at_ms = ? WHERE id = ?`)
      .run(status, Date.now(), id);
  }

  setUndeclaredPolicy(id: string, policy: UndeclaredPolicy): void {
    db()
      .prepare(`UPDATE extensions SET undeclared_policy = ?, updated_at_ms = ? WHERE id = ?`)
      .run(policy, Date.now(), id);
  }

  // Install (deps done): pin the content hash, flip to 'installed'. Invalidates
  // the per-call hash skip-cache so the next verify compares against the freshly
  // pinned hash.
  markInstalled(id: string, contentHash: string): void {
    db()
      .prepare(
        `UPDATE extensions SET status = 'installed', content_hash = ?, updated_at_ms = ? WHERE id = ?`,
      )
      .run(contentHash, Date.now(), id);
    this.hashVerifiedCache.delete(id);
  }

  // Out-of-band content change detected. Flip to 'drift' (keeping the pinned
  // hash for reference) and leave it for the user to confirm. Callers also call
  // disableAllTools(id) so a drifted extension can never be LLM-exposed.
  markDrift(id: string): void {
    db()
      .prepare(`UPDATE extensions SET status = 'drift', updated_at_ms = ? WHERE id = ?`)
      .run(Date.now(), id);
    this.hashVerifiedCache.delete(id);
  }

  // Flag drift AND disable every tool, the two steps every drift detection
  // takes together so a tampered extension can never stay LLM-exposed.
  private flagDrift(id: string): void {
    this.markDrift(id);
    this.disableAllTools(id);
  }

  // Confirm-reenable: re-pin the CURRENT on-disk content as trusted, clear the
  // drift state, return to 'installed'. The user re-enables tools afterward
  // (they were disabled when drift was flagged).
  repinAndClear(id: string, contentHash: string): void {
    db()
      .prepare(
        `UPDATE extensions SET status = 'installed', content_hash = ?, updated_at_ms = ? WHERE id = ?`,
      )
      .run(contentHash, Date.now(), id);
    this.hashVerifiedCache.delete(id);
  }

  // Uninstall (revert): drop an installed extension back to 'downloaded' and unpin
  // the hash, so its tools stop being exposed and the user must re-Install. Tool
  // enabled flags are kept so a later re-Install restores the prior selection.
  markDownloaded(id: string): void {
    db()
      .prepare(
        `UPDATE extensions SET status = 'downloaded', content_hash = '', updated_at_ms = ? WHERE id = ?`,
      )
      .run(Date.now(), id);
    this.hashVerifiedCache.delete(id);
  }

  // Boot-time integrity check. Walks every INSTALLED extension, recomputes its
  // content hash (per hash.ts: excludes node_modules + deno.lock + .gitignore-
  // listed paths; includes .gitignore itself), and on a mismatch flips the row
  // to status='drift' AND disables all its tools so a tampered extension can never
  // be LLM-exposed. A 'downloaded' extension has no pinned hash to check; a 'drift'
  // extension is already flagged. There is NO auto-clear on match: leaving drift
  // requires the user's explicit confirm-reenable (which re-pins the hash).
  //
  // Returns the IDs that drifted (for boot logging).
  async verifyAllOnBoot(): Promise<string[]> {
    const log = getLogger("extensions.verify");
    const drifted: string[] = [];
    for (const tk of this.list()) {
      if (tk.status !== "installed") continue;
      let actualHash: string;
      try {
        actualHash = await hashExtension(tk.installedPath);
      } catch (err) {
        const msg = errMessage(err);
        log.warn(`[${tk.id}] hash failed: ${msg}; flagging drift`);
        this.flagDrift(tk.id);
        drifted.push(tk.id);
        continue;
      }
      if (actualHash !== tk.contentHash) {
        log.warn(
          `[${tk.id}] content drift: stored=${tk.contentHash.slice(
            0,
            12,
          )} actual=${actualHash.slice(0, 12)}; disabling tools`,
        );
        this.flagDrift(tk.id);
        drifted.push(tk.id);
      }
    }
    return drifted;
  }

  // Per-call integrity check: re-verify a extension's content hash at tool-call
  // time so a extension tampered AFTER boot can't execute (verifyAllOnBoot only
  // runs at startup). Cheap when unchanged via the stat skip-cache. On mismatch
  // (or hash failure) it flips the extension to status='drift', disables all its
  // tools, and throws extension_hash_drift. There is no auto-clear: re-enabling
  // requires the user's explicit confirm-reenable.
  async verifyHashFresh(extensionId: string): Promise<void> {
    const tk = this.get(extensionId);
    if (!tk) return; // unknown extension; the tool lookup already handled this
    // Defense in depth: a call should only reach here for an installed extension
    // (the chat-exposure gate excludes non-installed). Refuse otherwise.
    if (tk.status !== "installed") {
      throw new AppError(
        "extension_hash_drift",
        `extension ${extensionId} is not installed (status ${tk.status})`,
      );
    }
    const sig = await statSignature(tk.installedPath);
    const cached = this.hashVerifiedCache.get(extensionId);
    if (cached && cached.sig === sig && cached.hash === tk.contentHash) return;
    let actual: string;
    try {
      actual = await hashExtension(tk.installedPath);
    } catch (err) {
      this.flagDrift(extensionId);
      throw new AppError(
        "extension_hash_drift",
        `extension ${extensionId} hash failed: ${errMessage(err)}`,
      );
    }
    if (actual === tk.contentHash) {
      this.hashVerifiedCache.set(extensionId, { sig, hash: actual });
      return;
    }
    this.flagDrift(extensionId);
    throw new AppError(
      "extension_hash_drift",
      `extension ${extensionId} content changed since it was trusted`,
    );
  }

  delete(id: string): void {
    // CASCADE drops tools, tool_embeddings, grants.
    db().prepare(`DELETE FROM extensions WHERE id = ?`).run(id);
  }

  // --- tool-level ---------------------------------------------------------

  replaceTools(extensionId: string, tools: ToolInsertInput[]): void {
    // Atomically replace the extension's tools. Across a re-download we preserve
    // each surviving tool's `enabled` flag (by name) so the user's tool
    // selection isn't lost on an update, and we preserve its permission grants
    // ONLY when the required-permission SET is also unchanged. A changed perm
    // set drops the grants so the user must re-review (the tool stays
    // enabled-but-not-exposed until re-granted). Grants would otherwise be lost
    // entirely: the DELETE below cascades them away (grants.tool_id FK), so we
    // snapshot + re-insert. Tool ids are deterministic (`extensionId::name`), so a
    // re-inserted tool keeps the same id its grants reference.
    const existing = db()
      .prepare(`
      SELECT id, name, enabled, always_available, required_permissions_json FROM tools WHERE extension_id = ?
    `)
      .all(extensionId) as Array<{
      id: string;
      name: string;
      enabled: number;
      always_available: number;
      required_permissions_json: string;
    }>;
    const existingByName = new Map(existing.map((r) => [r.name, r]));
    const grantsByToolId = new Map<string, Grant[]>();
    for (const r of existing) {
      grantsByToolId.set(r.id, this.listGrantsForTool(r.id));
    }

    db().exec("BEGIN");
    try {
      db().prepare(`DELETE FROM tools WHERE extension_id = ?`).run(extensionId);
      for (const t of tools) {
        const id = toolId(extensionId, t.name);
        const prior = existingByName.get(t.name);
        db()
          .prepare(`
          INSERT INTO tools
            (id, extension_id, name, description, parameters_json, triggers_json,
             fn_export, always_available, platforms_json, enabled, required_permissions_json)
          VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        `)
          .run(
            id,
            extensionId,
            t.name,
            t.description,
            JSON.stringify(t.parameters),
            JSON.stringify(t.triggers),
            t.fnExport,
            // Both enabled and always_available are user-overridable, so a
            // re-download preserves the prior choice by tool name; a brand-new
            // tool falls back to its manifest default (enabled off; always as
            // the manifest declares).
            prior ? prior.always_available : t.alwaysAvailable ? 1 : 0,
            JSON.stringify(t.platforms),
            prior ? prior.enabled : 0,
            JSON.stringify(t.requiredPermissions),
          );
        if (
          prior &&
          permKeySet(prior.required_permissions_json) ===
            permKeySet(JSON.stringify(t.requiredPermissions))
        ) {
          for (const g of grantsByToolId.get(prior.id) ?? []) {
            db()
              .prepare(`
              INSERT INTO grants (tool_id, permission_key, permission_kind, state, granted_at_ms)
              VALUES (?, ?, ?, ?, ?)
            `)
              .run(id, g.permissionKey, g.permissionKind, g.state, g.grantedAtMs);
          }
        }
      }
      db().exec("COMMIT");
    } catch (err) {
      db().exec("ROLLBACK");
      throw err;
    }
  }

  // Excludes tools the host OS doesn't support (toolPlatformSupported), so an
  // OS-gated tool is invisible to every consumer: chat selection, the route
  // layer, and the Tools UI.
  listTools(extensionId: string): Tool[] {
    const rows = db()
      .prepare(`
      SELECT id, extension_id, name, description, parameters_json, triggers_json,
             fn_export, always_available, platforms_json, enabled, required_permissions_json
      FROM tools WHERE extension_id = ?
      ORDER BY name ASC
    `)
      .all(extensionId) as Array<Record<string, unknown>>;
    const host = hostPlatforms();
    return rows
      .map((r) => rowToTool(r))
      .filter((t) => toolPlatformSupported(t.platforms, host))
      .map((t) => this.attachGrants(t));
  }

  /** Every tool from every installed extension, flat, each tagged with its
   *  provider's display name. Backs the Tools management UI, which aggregates
   *  tools across all providers (MCP servers union in separately). */
  listAllTools(): Tool[] {
    const out: Tool[] = [];
    for (const ext of this.list()) {
      // A downloaded-but-not-installed (or drifted) extension is not active, so its
      // tools stay out of the aggregate list the way its chat exposure does.
      if (ext.status !== "installed") continue;
      for (const tool of this.listTools(ext.id)) {
        out.push({ ...tool, providerName: ext.displayName });
      }
    }
    return out;
  }

  // Returns undefined for a tool the host OS doesn't support, so dispatch
  // refuses to execute an OS-gated tool even if its id is referenced directly.
  getTool(toolId: string): Tool | undefined {
    const row = db()
      .prepare(`
      SELECT id, extension_id, name, description, parameters_json, triggers_json,
             fn_export, always_available, platforms_json, enabled, required_permissions_json
      FROM tools WHERE id = ?
    `)
      .get(toolId) as Record<string, unknown> | undefined;
    if (!row) return undefined;
    const tool = rowToTool(row);
    if (!toolPlatformSupported(tool.platforms, hostPlatforms())) return undefined;
    return this.attachGrants(tool);
  }

  setToolEnabled(extensionId: string, name: string, enabled: boolean): void {
    db()
      .prepare(`UPDATE tools SET enabled = ? WHERE extension_id = ? AND name = ?`)
      .run(enabled ? 1 : 0, extensionId, name);
  }

  // The per-tool override for "offer this tool every turn without going through
  // relevance selection". Defaults to the manifest's declared value at install
  // and is preserved across re-downloads (see replaceTools); this is the write
  // path for the user flipping it in the tool detail view.
  setToolAlwaysAvailable(extensionId: string, name: string, alwaysAvailable: boolean): void {
    db()
      .prepare(`UPDATE tools SET always_available = ? WHERE extension_id = ? AND name = ?`)
      .run(alwaysAvailable ? 1 : 0, extensionId, name);
  }

  // Bulk toggles backing the "Enable all / Disable all" extension-detail action.
  // Used by drift handling (verifyAllOnBoot / verifyHashFresh) to take a
  // tampered extension's tools out of LLM exposure in one statement.
  disableAllTools(extensionId: string): void {
    db().prepare(`UPDATE tools SET enabled = 0 WHERE extension_id = ?`).run(extensionId);
  }

  // --- grants -------------------------------------------------------------

  setGrants(
    toolId: string,
    entries: Array<{ key: string; kind: PermissionDecl["kind"]; state: GrantState }>,
  ): Grant[] {
    const now = Date.now();
    db().exec("BEGIN");
    try {
      for (const entry of entries) {
        db()
          .prepare(`
          INSERT INTO grants (tool_id, permission_key, permission_kind, state, granted_at_ms)
          VALUES (?, ?, ?, ?, ?)
          ON CONFLICT(tool_id, permission_key) DO UPDATE SET
            state = excluded.state,
            granted_at_ms = excluded.granted_at_ms
        `)
          .run(toolId, entry.key, entry.kind, entry.state, now);
      }
      db().exec("COMMIT");
    } catch (err) {
      db().exec("ROLLBACK");
      throw err;
    }
    return this.listGrantsForTool(toolId);
  }

  listGrantsForTool(toolId: string): Grant[] {
    const rows = db()
      .prepare(`
      SELECT tool_id, permission_key, permission_kind, state, granted_at_ms
      FROM grants WHERE tool_id = ?
    `)
      .all(toolId) as Array<{
      tool_id: string;
      permission_key: string;
      permission_kind: string;
      state: string;
      granted_at_ms: number;
    }>;
    return rows.map((r) => ({
      toolId: r.tool_id,
      permissionKey: r.permission_key,
      permissionKind: r.permission_kind as Grant["permissionKind"],
      state: r.state as Grant["state"],
      grantedAtMs: r.granted_at_ms,
    }));
  }

  // --- embeddings ---------------------------------------------------------

  storeEmbedding(toolId: string, vector: Float32Array, sourceHash: string): void {
    db()
      .prepare(`
      INSERT INTO tool_embeddings (tool_id, dim, vector, source_hash)
      VALUES (?, ?, ?, ?)
      ON CONFLICT(tool_id) DO UPDATE SET
        dim = excluded.dim,
        vector = excluded.vector,
        source_hash = excluded.source_hash
    `)
      .run(toolId, vector.length, new Uint8Array(vector.buffer.slice(0)), sourceHash);
  }

  loadEmbedding(toolId: string): { vector: Float32Array; sourceHash: string } | undefined {
    const row = db()
      .prepare(`
      SELECT vector, dim, source_hash FROM tool_embeddings WHERE tool_id = ?
    `)
      .get(toolId) as { vector: Uint8Array; dim: number; source_hash: string } | undefined;
    if (!row) return undefined;
    return decodeEmbeddingRow(row);
  }

  // Batch-load every stored tool embedding in a single query. The relevance
  // filter (phase1) runs on every chat turn over all enabled tools; calling
  // loadEmbedding() per tool was N prepared statements + N round-trips. Keyed by
  // tool id; tools without an embedding are simply absent.
  loadAllEmbeddings(): Map<string, { vector: Float32Array; sourceHash: string }> {
    const rows = db()
      .prepare(`SELECT tool_id, vector, dim, source_hash FROM tool_embeddings`)
      .all() as Array<{
      tool_id: string;
      vector: Uint8Array;
      dim: number;
      source_hash: string;
    }>;
    const out = new Map<string, { vector: Float32Array; sourceHash: string }>();
    for (const row of rows) {
      out.set(String(row.tool_id), decodeEmbeddingRow(row));
    }
    return out;
  }

  // --- helpers ------------------------------------------------------------

  // requiredPermissions for a tool isn't persisted in the schema (loca.json
  // is the source of truth). Higher-level callers re-parse from disk when
  // they need it. attachGrants() does NOT resolve required permissions here;
  // it only attaches grant state. The route layer is responsible for combining
  // the two.
  private attachGrants(tool: Tool): Tool {
    return { ...tool, grants: this.listGrantsForTool(tool.id) };
  }
}

// Decode a tool_embeddings row's BLOB into a Float32Array view, copying out of
// the SQLite-owned buffer at the right offset.
function decodeEmbeddingRow(row: { vector: Uint8Array; source_hash: string }): {
  vector: Float32Array;
  sourceHash: string;
} {
  const bytes =
    row.vector instanceof Uint8Array ? row.vector : new Uint8Array(row.vector as ArrayBuffer);
  const buf = bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength);
  return { vector: new Float32Array(buf), sourceHash: row.source_hash };
}

export function toolId(extensionId: string, toolName: string): string {
  return `${extensionId}::${toolName}`;
}

// Canonical, order-independent signature of a tool's required-permission set
// (by permission key), used by replaceTools to decide whether grants survive a
// re-download. Two perm declarations that produce the same key set are treated
// as unchanged.
function permKeySet(requiredPermissionsJson: string): string {
  try {
    const decls = JSON.parse(requiredPermissionsJson) as PermissionDecl[];
    return decls
      .map((d) => permissionKey(d))
      .sort()
      .join("|");
  } catch {
    return requiredPermissionsJson;
  }
}

// Re-export so external callers can compute permission keys without importing
// from @loca/shared directly.
export { permissionKey };

let _instance: ExtensionsRegistry | null = null;
export function extensionsRegistry(): ExtensionsRegistry {
  if (!_instance) _instance = new ExtensionsRegistry();
  return _instance;
}

export function __resetForTesting(): void {
  _instance = null;
}

// Resolve the on-disk install path for a extension id.
export function extensionInstallPath(id: string): string {
  return join(paths().extensionsDir, id);
}

// --- row mappers ----------------------------------------------------------

function rowToExtension(row: Record<string, unknown>): Extension {
  return {
    id: String(row.id),
    source: row.source as ExtensionSource,
    displayName: String(row.display_name),
    description: row.description == null ? undefined : String(row.description),
    version: String(row.version),
    installedPath: String(row.installed_path),
    manifestHash: String(row.manifest_hash),
    contentHash: String(row.content_hash),
    status: String(row.status) as ExtensionStatus,
    hasDeps: Number(row.has_deps) === 1,
    hasDatabase: Number(row.has_database) === 1,
    undeclaredPolicy: String(row.undeclared_policy) as UndeclaredPolicy,
    toolCount: Number(row.tool_count),
    enabledToolCount: Number(row.enabled_tool_count),
    installedAtMs: Number(row.installed_at_ms),
    updatedAtMs: Number(row.updated_at_ms),
  };
}

function rowToTool(row: Record<string, unknown>): Tool {
  return {
    id: String(row.id),
    extensionId: String(row.extension_id),
    providerKind: "extension",
    name: String(row.name),
    description: String(row.description),
    parameters: JSON.parse(String(row.parameters_json)),
    triggers: JSON.parse(String(row.triggers_json)),
    fnExport: String(row.fn_export),
    alwaysAvailable: Number(row.always_available) === 1,
    platforms: row.platforms_json ? JSON.parse(String(row.platforms_json)) : [],
    enabled: Number(row.enabled) === 1,
    requiredPermissions: row.required_permissions_json
      ? JSON.parse(String(row.required_permissions_json))
      : [],
    // missingRequired is derived by the route/service layer at read time;
    // it depends on current grant state.
    missingRequired: [],
    grants: [],
  };
}
