// Health-check helpers for the sidecar supervisor.
//
// HTTP URLs are validated to be loopback-only - external endpoints are
// rejected. This mirrors the existing Rust supervisor's guarantee that
// sidecar supervision stays on the user's machine.
//
// The stdout-marker readiness mode is implemented directly in manager.ts:
// captureOutput pumps the child's stdout/stderr and flips an _markerSeen
// flag the moment the marker is seen, and runReadiness busy-polls that flag
// until the deadline. Keeping the marker logic in manager.ts means we don't
// have to tee streams just to expose them externally.

import { AppError } from "@loca/core-engine";

export const HEALTH_CHECK_ATTEMPTS = 30;
export const HEALTH_CHECK_INTERVAL_MS = 1_000;
export const STARTUP_WARMUP_MS = 2_000;

export function validateHealthCheckUrl(url: string): void {
  let parsed: URL;
  try {
    parsed = new URL(url);
  } catch {
    throw new AppError("validation_error", `invalid health-check URL: ${url}`);
  }
  if (parsed.protocol !== "http:") {
    throw new AppError("validation_error", "health-check URL must use http://");
  }
  if (parsed.hostname !== "127.0.0.1" && parsed.hostname !== "localhost") {
    throw new AppError("validation_error", "health-check URL must point to 127.0.0.1 or localhost");
  }
}

export interface PollOptions {
  attempts?: number;
  intervalMs?: number;
  signal?: AbortSignal;
}

// Polls an HTTP health-check URL. Resolves true on first 2xx, false on
// timeout. Returns false immediately if the signal aborts.
export async function pollHttpHealth(url: string, options: PollOptions = {}): Promise<boolean> {
  const attempts = options.attempts ?? HEALTH_CHECK_ATTEMPTS;
  const intervalMs = options.intervalMs ?? HEALTH_CHECK_INTERVAL_MS;
  for (let i = 0; i < attempts; i++) {
    if (options.signal?.aborted) return false;
    try {
      const res = await fetch(url, { signal: options.signal });
      await res.body?.cancel();
      if (res.ok) return true;
    } catch {
      // Network errors are expected during startup; retry.
    }
    if (i < attempts - 1) {
      try {
        await sleep(intervalMs, options.signal);
      } catch {
        return false;
      }
    }
  }
  return false;
}

export function sleep(ms: number, signal?: AbortSignal): Promise<void> {
  return new Promise((resolve, reject) => {
    if (signal?.aborted) {
      reject(new Error("aborted"));
      return;
    }
    const t = setTimeout(() => {
      signal?.removeEventListener("abort", onAbort);
      resolve();
    }, ms);
    const onAbort = () => {
      clearTimeout(t);
      reject(new Error("aborted"));
    };
    signal?.addEventListener("abort", onAbort, { once: true });
  });
}
