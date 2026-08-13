// On-disk storage view for the Settings "Storage" field. The core walks its
// whole persistence tree (models, the sessions DB + attachments, sidecar
// binaries, installed extensions, caches, logs, settings), tags anything that is
// currently in use with a `lock_reason`, and returns this grouped tree; the
// client renders it and routes deletes back through the storage API, which
// re-validates the lock server-side. Field names are snake_case to match the
// wire shape the UI's pure helpers (storage-tree.ts) consume.

export type StorageNodeBase = {
  name: string;
  path: string;
  size: number;
  /** Set by the core iff this item is currently in use and must not be
   *  deleted (e.g. a model a configured feature needs, a running sidecar's
   *  binary, an actively streaming session, the live settings file). The
   *  string is a human-readable reason shown as a tooltip. The core is the
   *  sole authority: it recomputes this on every delete request. */
  lock_reason?: string;
};

export type StorageFile = StorageNodeBase & { kind: "file" };
export type StorageFolder = StorageNodeBase & {
  kind: "folder";
  children: StorageNode[];
};
export type StorageNode = StorageFile | StorageFolder;

export type StorageCategoryId =
  | "models"
  | "sessions"
  | "binaries"
  | "extensions"
  | "cache"
  | "logs"
  | "settings";

export type StorageCategory = {
  id: StorageCategoryId;
  /** Display label for the group header (e.g. "Models", "Sessions"). */
  label: string;
  /** Whether this category supports deletion at all. Read-only categories
   *  (extensions, settings) are display-only; their rows are never selectable. */
  deletable: boolean;
  nodes: StorageNode[];
  /** Sum of `nodes[].size` for this category. */
  size: number;
};

export type StorageTree = {
  categories: StorageCategory[];
  total_size: number;
  /** Base dir whose `/models` child holds the model files, so the UI can map
   *  an HF spec to its on-disk path for in-use locking. */
  root_path: string;
};
