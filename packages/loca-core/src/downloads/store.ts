// Persistence layer for the download manager: every read/write of the
// `downloads` table lives here, so the manager itself only orchestrates
// (concurrency, transfer, fanout, broadcast). Pure SQLite + the row<->entry
// mapping; no network, no broadcast, no in-flight state.

import { db } from "@loca/core-engine";
import type { DownloadEntry, DownloadStatus } from "@loca/shared";
import { parseSource } from "./sources.ts";
import type { EnqueueSpec } from "./manager.ts";

const ROW_COLUMNS = `id, source, destination, rel_path, abs_path, filename, group_id,
       size_bytes, downloaded_bytes, status, error, added_at_ms`;

export class DownloadStore {
  upsertPending(id: string, spec: EnqueueSpec, absPath: string): void {
    const existing = this.getRow(id);
    const now = Date.now();
    if (existing) {
      db()
        .prepare(`
        UPDATE downloads
           SET status = 'Pending',
               error = NULL,
               downloaded_bytes = 0,
               size_bytes = COALESCE(?, size_bytes)
         WHERE id = ?
      `)
        .run(spec.sizeHint ?? null, id);
      return;
    }
    const meta = resolveMeta(spec);
    db()
      .prepare(`
      INSERT INTO downloads
        (id, source, destination, rel_path, abs_path, filename, group_id,
         size_bytes, downloaded_bytes, status, error, added_at_ms)
      VALUES (?, ?, ?, ?, ?, ?, ?, ?, 0, 'Pending', NULL, ?)
    `)
      .run(
        id,
        spec.source,
        spec.destination,
        meta.relPath,
        absPath,
        meta.filename,
        spec.groupId,
        spec.sizeHint ?? null,
        now,
      );
  }

  async upsertCompleted(id: string, spec: EnqueueSpec, absPath: string): Promise<void> {
    const existing = this.getRow(id);
    if (existing) {
      db()
        .prepare(`
        UPDATE downloads
           SET status = 'Completed',
               error = NULL,
               abs_path = ?,
               downloaded_bytes = COALESCE(size_bytes, downloaded_bytes)
         WHERE id = ?
      `)
        .run(absPath, id);
      return;
    }
    let sizeOnDisk: number | undefined;
    try {
      sizeOnDisk = (await Deno.stat(absPath)).size;
    } catch {
      /* ignore */
    }
    const meta = resolveMeta(spec);
    db()
      .prepare(`
      INSERT INTO downloads
        (id, source, destination, rel_path, abs_path, filename, group_id,
         size_bytes, downloaded_bytes, status, error, added_at_ms)
      VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 'Completed', NULL, ?)
    `)
      .run(
        id,
        spec.source,
        spec.destination,
        meta.relPath,
        absPath,
        meta.filename,
        spec.groupId,
        sizeOnDisk ?? null,
        sizeOnDisk ?? 0,
        Date.now(),
      );
  }

  setStatus(
    id: string,
    status: DownloadStatus,
    opts: { error?: string; downloadedBytes?: number } = {},
  ): void {
    if (opts.downloadedBytes !== undefined) {
      db()
        .prepare(`
        UPDATE downloads
           SET status = ?, error = ?, downloaded_bytes = ?
         WHERE id = ?
      `)
        .run(status, opts.error ?? null, opts.downloadedBytes, id);
    } else {
      db()
        .prepare(`
        UPDATE downloads
           SET status = ?, error = ?
         WHERE id = ?
      `)
        .run(status, opts.error ?? null, id);
    }
  }

  updateProgress(id: string, downloadedBytes: number): void {
    db().prepare(`UPDATE downloads SET downloaded_bytes = ? WHERE id = ?`).run(downloadedBytes, id);
  }

  updateSize(id: string, sizeBytes: number): void {
    db().prepare(`UPDATE downloads SET size_bytes = ? WHERE id = ?`).run(sizeBytes, id);
  }

  getRow(id: string): DownloadEntry | undefined {
    const row = db().prepare(`SELECT ${ROW_COLUMNS} FROM downloads WHERE id = ?`).get(id) as
      | Record<string, unknown>
      | undefined;
    return row ? rowToEntry(row) : undefined;
  }

  snapshot(): DownloadEntry[] {
    const rows = db()
      .prepare(`SELECT ${ROW_COLUMNS} FROM downloads ORDER BY added_at_ms DESC`)
      .all() as Array<Record<string, unknown>>;
    return rows.map(rowToEntry);
  }

  /** Drop a row outright (used by the manager's `remove`). */
  delete(id: string): void {
    db().prepare("DELETE FROM downloads WHERE id = ?").run(id);
  }

  /** Drop Completed rows whose file is no longer on disk (deleted in-app or
   *  externally), so a stale "done" entry self-clears. Only removes on a
   *  definite NotFound; other stat errors (permissions, etc.) leave the row.
   *  Returns whether any row was dropped. */
  async reconcileCompleted(): Promise<boolean> {
    const rows = db()
      .prepare(`SELECT id, abs_path FROM downloads WHERE status = 'Completed'`)
      .all() as Array<{ id: string; abs_path: string }>;
    let changed = false;
    for (const row of rows) {
      try {
        await Deno.stat(row.abs_path);
      } catch (err) {
        if (err instanceof Deno.errors.NotFound) {
          db().prepare("DELETE FROM downloads WHERE id = ?").run(row.id);
          changed = true;
        }
      }
    }
    return changed;
  }

  // On construction: drop persisted Completed rows whose file vanished;
  // flip persisted Downloading rows to Pending (the resume loop will pick
  // them up).
  // Boot-time pass that runs once at construction, so the sync stat here is
  // intentional (a few hundred sync stats during startup is cheaper than
  // restructuring the constructor to be async).
  normalizePersistedRows(): void {
    const all = db().prepare(`SELECT id, abs_path, status FROM downloads`).all() as Array<{
      id: string;
      abs_path: string;
      status: string;
    }>;
    for (const row of all) {
      if (row.status === "Completed") {
        try {
          Deno.statSync(row.abs_path);
        } catch {
          db().prepare(`DELETE FROM downloads WHERE id = ?`).run(row.id);
        }
      } else if (row.status === "Downloading") {
        db()
          .prepare(`
          UPDATE downloads SET status = 'Pending', downloaded_bytes = 0
          WHERE id = ?
        `)
          .run(row.id);
      }
    }
  }
}

function resolveMeta(spec: EnqueueSpec): { relPath: string; filename: string } {
  if (spec.relPath) {
    const filename = spec.filename ?? spec.relPath.split("/").pop() ?? spec.relPath;
    return { relPath: spec.relPath, filename };
  }
  const parsed = parseSource(spec.source);
  return {
    relPath: parsed.relPath,
    filename: spec.filename ?? parsed.filename,
  };
}

function rowToEntry(row: Record<string, unknown>): DownloadEntry {
  return {
    id: String(row.id),
    source: String(row.source),
    destination: String(row.destination) as DownloadEntry["destination"],
    relPath: String(row.rel_path),
    absPath: String(row.abs_path),
    filename: String(row.filename),
    groupId: String(row.group_id),
    sizeBytes: row.size_bytes == null ? undefined : Number(row.size_bytes),
    downloadedBytes: Number(row.downloaded_bytes ?? 0),
    status: String(row.status) as DownloadStatus,
    error: row.error == null ? undefined : String(row.error),
    addedAtMs: Number(row.added_at_ms),
  };
}
