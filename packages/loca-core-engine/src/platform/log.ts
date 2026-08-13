// Host-backed logger for engine services. `getLogger(scope)` mirrors the call
// shape of core's shared/log.ts (`const log = getLogger("x"); log.info(...)`) so
// a service moved into the engine keeps its logging lines verbatim, but every
// line routes through `host().log(level, scope, message)` instead of touching
// Deno stderr / @std/log directly. The host owns formatting + secret scrubbing.

import type { LogLevel } from "../host.ts";
import { host } from "./runtime.ts";

export interface Logger {
  debug(message: string): void;
  info(message: string): void;
  warn(message: string): void;
  error(message: string): void;
}

export function getLogger(scope: string): Logger {
  const at =
    (level: LogLevel) =>
    (message: string): void =>
      host().log(level, scope, message);
  return {
    debug: at("debug"),
    info: at("info"),
    warn: at("warn"),
    error: at("error"),
  };
}

// Patterns ordered most-specific-first. The labeled-form patterns run before
// the bare-token patterns so we keep the label visible while masking the value.
const SCRUBBERS: Array<[RegExp, string]> = [
  // Authorization: Bearer <token>
  [/(bearer\s+)[A-Za-z0-9_\-.~+/=]{16,}/gi, "$1<REDACTED>"],
  // X-Admin-Token: <token>  /  x-admin-token=<token>
  [/(x-admin-token\s*[:=]\s*)\S+/gi, "$1<REDACTED>"],
  // "password":"<value>" in a logged JSON body (admin-password set / mint /
  // revoke). Tolerates whitespace; masks the value, keeps the key visible.
  [/("password"\s*:\s*)"(?:[^"\\]|\\.)*"/gi, '$1"<REDACTED>"'],
  // ?token=<value> in URLs / WS upgrade query strings.
  [/(\btoken=)[A-Za-z0-9_\-.]+/gi, "$1<REDACTED>"],
  // Bare base64url tokens. randomToken() in auth.ts emits 32 random bytes
  // base64url-encoded -> 43 chars. 40+ catches any reasonable token width
  // without over-matching short identifiers.
  [/[A-Za-z0-9_-]{40,}/g, "<REDACTED>"],
  // Bare hex strings: admin-token file is 32 hex chars, sha256 hashes are
  // 64 hex chars. Mask both.
  [/\b[a-f0-9]{32,}\b/gi, "<REDACTED>"],
];

/** Strip credential-shaped substrings from a string before it crosses the wire
 *  or is written to a log. Best-effort defense: a misplaced token still leaks at
 *  the call site, but at least it does not persist. The canonical copy lives
 *  here so the shell logger and the engine's HTTP error envelope scrub with one
 *  set of patterns; core's shared/log.ts re-exports it. */
export function scrubSecrets(input: string): string {
  let out = input;
  for (const [re, repl] of SCRUBBERS) out = out.replace(re, repl);
  return out;
}
