// The byte transfer itself: resolve the integrity hash, resume a partial via a
// Range request when one is trustworthy, stream to a `.tmp` with a stall
// watchdog and a runaway-size ceiling, verify the sha256, and rename into place.
// Progress + size discoveries are reported through callbacks so the manager
// owns persistence/broadcast; this module owns only the network + filesystem.

import { dirname } from "@std/path";
import { AppError } from "@loca/core-engine";
import { getLogger } from "../shared/log.ts";
import { Sha256Stream } from "../shared/hash.ts";
import { modelCatalogSha256 } from "../models/catalog.ts";
import { parseSource } from "./sources.ts";
import { HF_BASE_URL } from "../config.ts";
import type { EnqueueSpec } from "./manager.ts";

const log = getLogger("downloads");

// The HuggingFace host whose resolve redirect carries the LFS content sha256.
// Derived from HF_BASE_URL so a test/mirror host (LOCA_HF_BASE_URL) is honored
// rather than the literal "huggingface.co".
const HF_HOST = new URL(HF_BASE_URL).host;

// Abort a download that receives no bytes for this long. A silently-stalled
// connection (server wedged, network black-holed) otherwise leaves `for await`
// blocked forever with the UI gated on a download that will never finish or
// error. The window is generous so a slow-but-progressing transfer never trips
// it; it only fires on true silence.
const STALL_TIMEOUT_MS = 60_000;

export interface StreamTransferOpts {
  spec: EnqueueSpec;
  absPath: string;
  signal: AbortSignal;
  // Throttled progress tick (every ~250ms of streaming): persist + broadcast.
  onProgress: (downloaded: number) => void;
  // Fired once the exact total size is known from content-length: persist it.
  onSizeKnown: (total: number) => void;
}

/** Download `spec` to `absPath`, returning the final byte count. Throws an
 *  AppError on a terminal failure (bad HTTP, checksum mismatch, stall); a
 *  transient failure leaves the `.tmp` partial behind for a later resume. */
export async function streamTransfer(opts: StreamTransferOpts): Promise<number> {
  const { spec, absPath, signal, onProgress, onSizeKnown } = opts;
  const url = spec.url ?? parseSource(spec.source).url;
  if (!url) {
    throw new AppError("validation_error", "non-downloadable source");
  }
  await Deno.mkdir(dirname(absPath), { recursive: true });
  const tmpPath = absPath + ".tmp";

  const expectedSha = await resolveExpectedSha(spec, url, signal);
  if (!expectedSha) {
    // No trustworthy content hash is available (no pinned sha256, and HF did
    // not return an x-linked-etag for this file). The bytes are then accepted
    // on TLS trust alone, so make that explicit instead of silently trusting a
    // possibly mis-served file. Weights with an LFS sha256 still verify below.
    log.warn(`downloading ${url} without integrity verification (no sha256 available)`);
  }

  let { resumeFrom, sha } = await openResume(tmpPath, expectedSha);

  let downloaded = resumeFrom;
  let total: number | undefined = spec.sizeHint;
  let lastEmit = 0;

  // Stall watchdog: abort if no bytes arrive for STALL_TIMEOUT_MS. Combined
  // with the external signal (user cancel / shutdown) so either can stop the
  // transfer. Reset on every chunk, so it only fires on true silence.
  const stall = new AbortController();
  let stalled = false;
  const combined = AbortSignal.any([signal, stall.signal]);
  let stallTimer: ReturnType<typeof setTimeout> | null = null;
  const armStall = (): void => {
    if (stallTimer !== null) clearTimeout(stallTimer);
    stallTimer = setTimeout(() => {
      stalled = true;
      stall.abort();
    }, STALL_TIMEOUT_MS);
  };

  let file: Deno.FsFile | null = null;
  try {
    armStall();
    const headers = resumeFrom > 0 ? { Range: `bytes=${resumeFrom}-` } : undefined;
    const res = await fetch(url, { signal: combined, headers });

    // The partial is larger than the resource itself (corrupt/stale .tmp): drop
    // it so the next retry starts clean rather than 416-looping.
    if (res.status === 416) {
      await Deno.remove(tmpPath).catch(() => {});
      throw new AppError(
        "provider_error",
        `partial file for ${url} is unusable (HTTP 416); restarting on next retry`,
      );
    }
    if (!res.ok) {
      throw new AppError(
        "manifest_fetch_failed",
        `HTTP ${res.status} ${res.statusText} for ${url}`,
      );
    }
    // 206 means the server honored our Range and is sending only the tail, so
    // append; anything else (200) is the whole file, so rewrite from scratch
    // (reset the hasher + truncate the partial).
    const resuming = resumeFrom > 0 && res.status === 206;
    if (!resuming) {
      resumeFrom = 0;
      downloaded = 0;
      if (expectedSha) sha = new Sha256Stream();
    }
    file = await Deno.open(
      tmpPath,
      resuming
        ? { create: true, write: true, append: true }
        : { create: true, write: true, truncate: true },
    );

    // content-length is the BODY length (the remaining tail when resuming), so
    // the full size is the offset we started at plus it.
    const cl = res.headers.get("content-length");
    let exactTotal: number | undefined;
    if (cl) {
      const n = Number(cl);
      if (Number.isFinite(n)) {
        exactTotal = downloaded + n;
        total = exactTotal;
      }
    }
    if (total !== undefined && total !== spec.sizeHint) {
      onSizeKnown(total);
    }
    const body = res.body;
    if (!body) {
      throw new AppError("provider_error", "empty response body");
    }
    for await (const chunk of body) {
      if (signal.aborted) {
        throw new Error("cancelled");
      }
      // Guard against a wrong/hostile URL that streams far more than it
      // declared (filling the disk). The ceiling is generous (2x + 64MB) so a
      // transfer-encoding quirk never trips a legitimate download, but an
      // unbounded stream is still cut off.
      if (exactTotal !== undefined && downloaded > exactTotal * 2 + 64 * 1024 * 1024) {
        throw new AppError(
          "provider_error",
          `download for ${url} far exceeded its declared size (${exactTotal}); aborting`,
        );
      }
      await file.write(chunk);
      if (sha) sha.update(chunk);
      downloaded += chunk.byteLength;
      armStall(); // progress: reset the stall watchdog
      const now = Date.now();
      if (now - lastEmit > 250) {
        lastEmit = now;
        onProgress(downloaded);
      }
    }
  } catch (err) {
    // A watchdog abort surfaces as a generic AbortError; rewrite it into a
    // clear, retryable reason so the UI explains the stall instead of a bare
    // "operation aborted".
    if (stalled) {
      throw new AppError(
        "server_unavailable",
        `download for ${url} stalled (no data for ${STALL_TIMEOUT_MS / 1000}s); aborting`,
      );
    }
    throw err;
  } finally {
    if (stallTimer !== null) clearTimeout(stallTimer);
    try {
      file?.close();
    } catch {
      /* fine */
    }
  }

  if (sha && expectedSha) {
    const actual = await sha.hexDigest();
    if (actual !== expectedSha.toLowerCase()) {
      try {
        await Deno.remove(tmpPath);
      } catch {
        /* fine */
      }
      throw new AppError(
        "checksum_mismatch",
        `sha256 mismatch: want ${expectedSha}, got ${actual}`,
      );
    }
  }

  await Deno.rename(tmpPath, absPath);
  return downloaded;
}

/** Pick the most trustworthy sha256 anchor for `spec`: an explicit spec hash,
 *  then (for a catalog model file) the sha256 pinned in the SIGNED model
 *  catalog, then HF's published sha256 (the `x-linked-etag` on the resolve
 *  redirect). The signed-catalog hash holds even if a proxy strips the etag or
 *  the file moves to a non-HF mirror. Small non-LFS files not in the catalog
 *  carry a git blob sha1 instead, which is not a content hash, so they stay
 *  unverified. */
async function resolveExpectedSha(
  spec: EnqueueSpec,
  url: string,
  signal: AbortSignal,
): Promise<string | undefined> {
  return (
    spec.sha256 ??
    (spec.destination === "models" ? await modelCatalogSha256(spec.source) : undefined) ??
    (await resolveHfSha256(url, signal))
  );
}

/** Detect a resumable `.tmp` partial and seed the hasher with its bytes so the
 *  streamed tail hashes continuously. Resume only when a content hash exists:
 *  the final sha256 verifies the WHOLE file, so a wrongly-resumed file is still
 *  caught and retried. Without a hash we can't tell a good partial from a bad
 *  one, so we always start clean. A read failure falls back to a clean restart. */
async function openResume(
  tmpPath: string,
  expectedSha: string | undefined,
): Promise<{ resumeFrom: number; sha: Sha256Stream | null }> {
  let sha = expectedSha ? new Sha256Stream() : null;
  let resumeFrom = 0;
  if (expectedSha) {
    try {
      const st = await Deno.stat(tmpPath);
      if (st.isFile && st.size > 0) resumeFrom = st.size;
    } catch {
      /* no partial on disk: start fresh */
    }
  }
  if (resumeFrom > 0 && sha) {
    try {
      const rf = await Deno.open(tmpPath, { read: true });
      for await (const chunk of rf.readable) sha.update(chunk);
    } catch {
      resumeFrom = 0;
      sha = new Sha256Stream();
    }
  }
  return { resumeFrom, sha };
}

/** Best-effort lookup of a HuggingFace file's published sha256. The resolve
 *  endpoint 302-redirects to a CDN; for git-LFS objects (the large model
 *  weights) the redirect carries the content sha256 in `x-linked-etag`. Returns
 *  it (lowercase hex) when present and shaped like a sha256, else undefined
 *  (e.g. small non-LFS files, whose etag is a git blob sha1, or a non-HF URL).
 *  Used to verify model downloads against HF + TLS. */
async function resolveHfSha256(url: string, signal: AbortSignal): Promise<string | undefined> {
  let host: string;
  try {
    host = new URL(url).host;
  } catch {
    return undefined;
  }
  if (host !== HF_HOST) return undefined;
  try {
    const res = await fetch(url, {
      method: "HEAD",
      redirect: "manual",
      signal,
    });
    await res.body?.cancel();
    const raw = res.headers.get("x-linked-etag") ?? res.headers.get("etag");
    if (!raw) return undefined;
    const cleaned = raw.replace(/^W\//, "").replace(/"/g, "").trim();
    return /^[0-9a-f]{64}$/i.test(cleaned) ? cleaned.toLowerCase() : undefined;
  } catch {
    return undefined;
  }
}
