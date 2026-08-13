/**
 * Client logger. Every UI module logs through `getLogger("scope")` instead of
 * `console.*`, so lines flow to the Rust backend (lib/platform logging) and
 * reach the `deno task dev` terminal + the persisted client.log (WARN/ERROR).
 * Mirrors loca-core's getLogger("scope") shape.
 *
 *   const log = getLogger("ws");
 *   log.warn("rejected frame: schema mismatch", err);
 *
 * Levels: warn/error always forward; debug/info forward only in dev (they're
 * terminal-only diagnostics and would be wasted IPC in prod). A logging failure
 * - including being called before setPlatform() during early boot - falls back
 * to console and never throws into the caller.
 */

import { dev } from "$app/environment";
import { platform } from "$lib/platform";
import type { LogLevel } from "$lib/platform";

export interface Logger {
  debug(message: string, extra?: unknown): void;
  info(message: string, extra?: unknown): void;
  warn(message: string, extra?: unknown): void;
  error(message: string, extra?: unknown): void;
}

/** Turn an optional error/context arg into a trailing string fragment, mirroring
 *  core's `errMessage`: Error -> its message, string -> itself, else JSON (with
 *  a String() fallback for circular/unserializable values). */
function extraToString(extra: unknown): string {
  if (extra === undefined) return "";
  if (extra instanceof Error) return ` ${extra.message}`;
  if (typeof extra === "string") return ` ${extra}`;
  try {
    return ` ${JSON.stringify(extra)}`;
  } catch {
    return ` ${String(extra)}`;
  }
}

function emit(level: LogLevel, scope: string, message: string, extra?: unknown): void {
  // debug/info are dev-only; in prod they aren't even forwarded over IPC.
  if ((level === "debug" || level === "info") && !dev) return;
  const line = `${message}${extraToString(extra)}`;
  try {
    platform().logging.log(level, scope, line);
  } catch {
    // platform() not installed yet (early boot) or the sink threw: never lose
    // the line and never throw into the caller.
    const prefixed = scope ? `[${scope}] ${line}` : line;
    (console[level] ?? console.log)(prefixed);
  }
}

/** Build a scoped logger. `scope` is a short lowercase namespace (e.g. "ws",
 *  "boot") rendered as the module column by the backend. */
export function getLogger(scope: string): Logger {
  return {
    debug: (message, extra) => emit("debug", scope, message, extra),
    info: (message, extra) => emit("info", scope, message, extra),
    warn: (message, extra) => emit("warn", scope, message, extra),
    error: (message, extra) => emit("error", scope, message, extra),
  };
}
