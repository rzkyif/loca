// Timed fetch + stall-guarded streaming for the install / self-update network
// path.
//
// Deno's `fetch` has NO default idle timeout: a stalled TCP read (corporate
// proxy, TLS-inspection middlebox, flaky link) makes a bare `await fetch(...)`
// or `for await (chunk of body)` hang forever, stranding an install-service
// bootstrap or a self-update mid-phase. Every install/update fetch routes
// through here so a dead connection fails within a bounded idle window with a
// clear error. Mirrors the stall-timer + AbortSignal.any pattern already used
// by downloads/transfer.ts.

/** Default overall deadline for a SMALL response (manifest / JSON): the caller
 *  reads the whole body immediately, so one wall-clock cap is safe. */
export const FETCH_TIMEOUT_MS = 30_000;

/** Default idle window for a streamed download: aborted only when NO data
 *  arrives for this long, so a slow-but-progressing large transfer is never
 *  killed while a dead socket fails fast. */
export const DOWNLOAD_STALL_MS = 60_000;

/** fetch() that aborts the whole request if the server hasn't responded within
 *  `timeoutMs`. For small responses the caller drains the body right after, so a
 *  single overall deadline (not a stall timer) is the simplest correct bound. */
export async function fetchWithTimeout(
  url: string,
  init: RequestInit = {},
  timeoutMs = FETCH_TIMEOUT_MS,
): Promise<Response> {
  const deadline = AbortSignal.timeout(timeoutMs);
  const signal = init.signal ? AbortSignal.any([init.signal, deadline]) : deadline;
  try {
    return await fetch(url, { ...init, signal });
  } catch (err) {
    if (deadline.aborted) {
      throw new Error(`request to ${url} timed out after ${timeoutMs}ms`);
    }
    throw err;
  }
}

/** Stream a URL's body to `onChunk`, aborting if the connection STALLS (no
 *  progress for `stallMs`) rather than on a total wall-clock cap. The stall
 *  timer is armed before the connect and re-armed on every chunk, so it also
 *  bounds a hung handshake. Optionally gzip-decompresses the body first (core
 *  artifacts ship gzipped and are hashed decompressed); the built-in extension
 *  tarball is hashed as-is, so its caller passes decompress: false. */
export async function streamDownload(
  url: string,
  onChunk: (chunk: Uint8Array) => Promise<void> | void,
  opts: { stallMs?: number; decompress?: boolean; signal?: AbortSignal } = {},
): Promise<void> {
  const stallMs = opts.stallMs ?? DOWNLOAD_STALL_MS;
  const stall = new AbortController();
  const signal = opts.signal ? AbortSignal.any([opts.signal, stall.signal]) : stall.signal;
  let timer: ReturnType<typeof setTimeout> | null = null;
  const arm = () => {
    if (timer !== null) clearTimeout(timer);
    timer = setTimeout(() => stall.abort(), stallMs);
  };
  arm();
  try {
    const res = await fetch(url, { signal });
    if (!res.ok || !res.body) {
      throw new Error(`download HTTP ${res.status} for ${url}`);
    }
    const body = opts.decompress ? res.body.pipeThrough(new DecompressionStream("gzip")) : res.body;
    for await (const chunk of body) {
      arm();
      await onChunk(chunk);
    }
  } catch (err) {
    // A stall-triggered abort surfaces as an AbortError; translate it to a
    // legible message. An externally-supplied signal aborting is the caller's
    // own cancel and passes through untouched.
    if (stall.signal.aborted && !opts.signal?.aborted) {
      throw new Error(`download from ${url} stalled (no data for ${stallMs}ms)`);
    }
    throw err;
  } finally {
    if (timer !== null) clearTimeout(timer);
  }
}
