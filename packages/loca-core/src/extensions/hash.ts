// Extension content hash, used as the trust anchor against tampering.
//
// Algorithm (deterministic across platforms):
//   1. Walk the folder; for each regular file, decide whether to include it.
//   2. Always exclude `node_modules/...` and the root `deno.lock`.
//   3. If `<root>/.gitignore` exists, exclude any path it matches (but
//      the .gitignore file itself is INCLUDED so changes to ignore rules
//      drift the hash too).
//   4. Sort included paths lexicographically (forward-slash form).
//   5. Hash the concatenation of `path\0sizeBytes\0content` for each file
//      into a single SHA-256 digest, hex-encoded lowercase.
//
// node_modules + deno.lock are excluded because `deno install` generates them
// AFTER the hash is pinned (and a later `deno run` can rewrite deno.lock), so
// including them would read benign install churn as content drift. The
// `deno.json` they derive from IS hashed, so tampering with declared imports is
// still detected.
//
// Uses npm:ignore for gitignore parsing (battle-tested by ESLint/Prettier).

import { join, relative } from "@std/path";
import ignore from "ignore";
import { Sha256Stream } from "../shared/hash.ts";

const NODE_MODULES = "node_modules";
const DENO_LOCK = "deno.lock";

/** The sorted, forward-slash relative paths that the hash covers (node_modules
 *  and .gitignore-listed paths excluded; .gitignore itself included). Shared by
 *  `hashExtension` and `newestIncludedMtimeMs`. */
export async function listIncludedFiles(rootDir: string): Promise<string[]> {
  const ig = await loadGitignore(rootDir);
  const files: string[] = [];
  await walk(rootDir, rootDir, files, ig);
  files.sort();
  return files;
}

/** Newest mtime (ms) across the hashed file set, or 0 when empty/unreadable. A
 *  cheap change signal: when it matches a previously-verified value the content
 *  hash is unchanged, so a per-call re-hash can be skipped. */
export async function newestIncludedMtimeMs(rootDir: string): Promise<number> {
  let files: string[];
  try {
    files = await listIncludedFiles(rootDir);
  } catch {
    return 0;
  }
  let newest = 0;
  for (const rel of files) {
    try {
      const st = await Deno.stat(join(rootDir, rel));
      if (st.mtime) newest = Math.max(newest, st.mtime.getTime());
    } catch {
      /* vanished mid-walk; ignore */
    }
  }
  return newest;
}

/** A cheap stat-only fingerprint of the hashed file set: `count:totalSize:newestMtime`.
 *  Used as the per-call re-hash skip key instead of mtime alone, which a tool
 *  with write access to its own folder could reset to the previously-verified
 *  value to bypass the integrity re-check. Matching count + total size as well
 *  is far harder to forge while actually changing content, so a tampered file
 *  re-triggers a full re-hash. */
export async function statSignature(rootDir: string): Promise<string> {
  let files: string[];
  try {
    files = await listIncludedFiles(rootDir);
  } catch {
    return "0:0:0";
  }
  let count = 0;
  let totalSize = 0;
  let newest = 0;
  for (const rel of files) {
    try {
      const st = await Deno.stat(join(rootDir, rel));
      count++;
      totalSize += st.size;
      if (st.mtime) newest = Math.max(newest, st.mtime.getTime());
    } catch {
      /* vanished mid-walk; ignore */
    }
  }
  return `${count}:${totalSize}:${newest}`;
}

export async function hashExtension(rootDir: string): Promise<string> {
  const files = await listIncludedFiles(rootDir);

  const digestStream = new Sha256Stream();
  for (const rel of files) {
    const abs = join(rootDir, rel);
    let size: number;
    try {
      size = (await Deno.stat(abs)).size;
    } catch {
      // File vanished mid-walk; skip it (next call will get a different hash).
      continue;
    }
    digestStream.update(new TextEncoder().encode(rel + "\0" + size + "\0"));
    const file = await Deno.open(abs, { read: true });
    try {
      for await (const chunk of file.readable) {
        digestStream.update(chunk);
      }
    } finally {
      try {
        file.close();
      } catch {
        /* already closed via readable */
      }
    }
    digestStream.update(new Uint8Array([0]));
  }
  return await digestStream.hexDigest();
}

async function loadGitignore(rootDir: string): Promise<ignore.Ignore> {
  const ig = ignore();
  try {
    const text = await Deno.readTextFile(join(rootDir, ".gitignore"));
    ig.add(text);
  } catch {
    /* no .gitignore */
  }
  return ig;
}

async function walk(root: string, dir: string, out: string[], ig: ignore.Ignore): Promise<void> {
  for await (const entry of Deno.readDir(dir)) {
    const full = join(dir, entry.name);
    const rel = relative(root, full).replaceAll("\\", "/");
    if (entry.isDirectory) {
      if (entry.name === NODE_MODULES) continue;
      if (rel && ig.ignores(rel + "/")) continue;
      await walk(root, full, out, ig);
    } else if (entry.isFile) {
      if (rel === DENO_LOCK) continue;
      if (rel && ig.ignores(rel)) continue;
      out.push(rel);
    }
    // symlinks and other entries intentionally skipped to keep hashing deterministic
  }
}
