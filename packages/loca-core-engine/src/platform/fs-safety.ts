// Filesystem path-safety guard: the single canonical check for any path built
// from user- or remote-influenced input before it is read/written. Pure path
// math (@std/path), no filesystem access.

import { isAbsolute, relative, resolve, SEPARATOR } from "@std/path";

/** True if `candidate` resolves to a path inside `root` (or root itself).
 *  Rejects path-traversal (`..`), absolute re-roots, and sibling escapes
 *  (e.g. zip-slip tarball entries, a `..`-bearing download source). */
export function isWithin(root: string, candidate: string): boolean {
  const rel = relative(resolve(root), resolve(candidate));
  return rel !== ".." && !rel.startsWith(".." + SEPARATOR) && !isAbsolute(rel);
}
