/**
 * Pure tree algorithms for the Storage settings field. The component owns the
 * reactive `tree` / `expanded` / `selected` state and the IPC calls; everything
 * that's a function of the tree lives here so it stays easy to follow and
 * unit-testable.
 *
 * Lock state ("is this item in use?") is decided by the core and shipped on each
 * node as `lock_reason`; the client only renders it. There is no client-side
 * lock computation.
 */

import type { StorageCategory, StorageNode, StorageTree } from "@loca/shared";

// The tree shapes live in @loca/shared (core/client wire types); re-export so
// importers of these from this module keep working.
export type {
  StorageCategory,
  StorageFile,
  StorageFolder,
  StorageNode,
  StorageTree,
} from "@loca/shared";

/** Stable key for a category's group header in the `expanded` set. */
export function categoryKey(id: string): string {
  return `__cat__:${id}`;
}

export function collectPaths(node: StorageNode, into: Set<string>): void {
  into.add(node.path);
  if (node.kind === "folder") {
    for (const c of node.children) collectPaths(c, into);
  }
}

/** Find a node anywhere in the tree by its path. */
export function findNode(tree: StorageTree, path: string): StorageNode | null {
  const visit = (n: StorageNode): StorageNode | null => {
    if (n.path === path) return n;
    if (n.kind === "folder") {
      for (const c of n.children) {
        const hit = visit(c);
        if (hit) return hit;
      }
    }
    return null;
  };
  for (const cat of tree.categories) {
    for (const n of cat.nodes) {
      const hit = visit(n);
      if (hit) return hit;
    }
  }
  return null;
}

/** Expand the given paths into the concrete paths that get deleted: a file is
 *  itself; a folder with file children expands to those files (so a partially
 *  locked repo can be cleaned file-by-file); a folder with no children (a
 *  summary dir, session, or extension) deletes as a whole. */
export function expandToFiles(tree: StorageTree, paths: string[]): string[] {
  const out: string[] = [];
  for (const p of paths) {
    const n = findNode(tree, p);
    if (!n) continue;
    if (n.kind === "file") {
      out.push(n.path);
    } else {
      const fileChildren = n.children.filter((c) => c.kind === "file");
      if (fileChildren.length > 0) {
        for (const c of fileChildren) out.push(c.path);
      } else out.push(n.path);
    }
  }
  return out;
}

/** Flat list of currently-visible rows given the expanded set (category group
 *  keys + expanded folder paths). Used for shift-range selection. */
export function visibleRows(tree: StorageTree, expanded: ReadonlySet<string>): StorageNode[] {
  const rows: StorageNode[] = [];
  for (const cat of tree.categories) {
    if (!expanded.has(categoryKey(cat.id))) continue;
    for (const n of cat.nodes) {
      rows.push(n);
      if (n.kind === "folder" && expanded.has(n.path)) {
        for (const c of n.children) rows.push(c);
      }
    }
  }
  return rows;
}

/** A node's own lock reason or, for a folder, the first locked descendant.
 *  Used to decide whether a whole-category Clear can include this node. */
export function subtreeLockReason(node: StorageNode): string | undefined {
  if (node.lock_reason) return node.lock_reason;
  if (node.kind === "folder") {
    for (const c of node.children) {
      const r = subtreeLockReason(c);
      if (r) return r;
    }
  }
  return undefined;
}

/** Top-level nodes of a category that a Clear would actually delete (nothing in
 *  the subtree is locked). */
export function clearableNodes(cat: StorageCategory): StorageNode[] {
  return cat.nodes.filter((n) => !subtreeLockReason(n));
}
