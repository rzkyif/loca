// Source-string parsing and HEAD-probing.
//
// Supported source format: `@user/repo/branch/filename`. The relative on-disk
// path under the destination root is `user/repo/filename`; the URL points at
// the HF resolve endpoint with ?download=true.
//
// Bare absolute paths are accepted in `parseSource` for compatibility, but
// since the rework rejects user-supplied local-only paths in the API the
// production caller should always pass an HF spec.

import { join } from "@std/path";
import { AppError } from "@loca/core-engine";
import { HF_BASE_URL } from "../config.ts";
import type { DownloadPlan } from "@loca/shared";

export interface ParsedSource {
  relPath: string;
  url: string;
  filename: string;
}

// Reject a path component that would let `relPath` escape the destination root
// when joined: traversal (`.`/`..`), empties, null bytes, or a path separator
// where a single segment is expected.
function assertSafeComponent(c: string, source: string): void {
  if (c === "" || c === "." || c === ".." || c.includes("\0") || c.includes("\\")) {
    throw new AppError("validation_error", `unsafe source component in ${source}`);
  }
}

export function parseSource(source: string): ParsedSource {
  if (!source.startsWith("@")) {
    // Absolute path; not downloadable.
    const filename = source.split("/").pop() ?? source;
    return { relPath: source, url: "", filename };
  }
  const parts = source.slice(1).split("/");
  if (parts.length < 4) {
    throw new AppError("validation_error", `invalid source format: ${source}`);
  }
  const [username, reponame, branchname, ...fileParts] = parts;
  assertSafeComponent(username, source);
  assertSafeComponent(reponame, source);
  assertSafeComponent(branchname, source);
  // filename may legitimately be a nested repo path (e.g. `subdir/model.gguf`);
  // validate each segment so none is `..`.
  const filename = fileParts.join("/");
  for (const seg of filename.split("/")) assertSafeComponent(seg, source);
  const relPath = `${username}/${reponame}/${filename}`;
  const url = `${HF_BASE_URL}/${username}/${reponame}/resolve/${branchname}/${filename}?download=true`;
  const baseName = filename.split("/").pop() ?? filename;
  return { relPath, url, filename: baseName };
}

export interface ProbeResult {
  source: string;
  url: string;
  filename: string;
  sizeBytes?: number;
  alreadyHave: boolean;
  absPath: string;
}

// HF resolve URLs 302-redirect to a CDN that often omits Content-Length on
// HEAD. The 302 itself carries the size in `x-linked-size`, so probe without
// following redirects first and fall back to a normal HEAD if needed.
export async function probeSource(
  source: string,
  destinationRoot: string,
  opts: { network?: boolean } = {},
): Promise<ProbeResult> {
  // `network` gates the outbound size HEAD. Presence (`alreadyHave`) is always a
  // local check; the HEAD to HuggingFace is only for the size hint of a MISSING
  // file, so callers that must not touch the network (the always-on requirements
  // snapshot, per the no-non-consented-network rule) pass network:false and get
  // presence without a size estimate.
  const network = opts.network ?? true;
  const parsed = parseSource(source);
  const absPath = join(destinationRoot, parsed.relPath);
  const alreadyHave = await pathExists(absPath);

  if (!source.startsWith("@") || alreadyHave || !network) {
    return {
      source,
      url: parsed.url,
      filename: parsed.filename,
      absPath,
      alreadyHave,
    };
  }

  let sizeBytes: number | undefined;
  try {
    const noRedirect = await fetch(parsed.url, {
      method: "HEAD",
      redirect: "manual",
    });
    await noRedirect.body?.cancel();
    const linked = noRedirect.headers.get("x-linked-size");
    if (linked) {
      const n = Number(linked);
      if (Number.isFinite(n)) sizeBytes = n;
    } else {
      const cl = noRedirect.headers.get("content-length");
      if (cl) {
        const n = Number(cl);
        // Zero means the REDIRECT's own body, not the resource's. A 302 carries
        // no body and plenty of servers say so explicitly, so accepting 0 here
        // reports the file as empty and skips the followed-HEAD fallback that
        // would have found its real size. `if (cl)` does not catch this: the
        // string "0" is truthy.
        if (Number.isFinite(n) && n > 0) sizeBytes = n;
      }
    }
    if (sizeBytes === undefined) {
      // Followed redirect HEAD as a fallback.
      const follow = await fetch(parsed.url, { method: "HEAD" });
      await follow.body?.cancel();
      const cl = follow.headers.get("content-length");
      if (cl) {
        const n = Number(cl);
        if (Number.isFinite(n)) sizeBytes = n;
      }
    }
  } catch {
    // Probe failures are non-fatal: callers display the row without a size
    // estimate. The actual download still validates content if checksummed.
  }

  return {
    source,
    url: parsed.url,
    filename: parsed.filename,
    sizeBytes,
    alreadyHave,
    absPath,
  };
}

export function probeResultToPlan(probe: ProbeResult): DownloadPlan {
  return {
    source: probe.source,
    alreadyHave: probe.alreadyHave,
    sizeHint: probe.sizeBytes,
  };
}

async function pathExists(path: string): Promise<boolean> {
  try {
    await Deno.stat(path);
    return true;
  } catch {
    return false;
  }
}
