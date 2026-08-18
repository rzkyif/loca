// Which routes each target actually serves.
//
// Out of scope is a property of the TARGET, not of the language: the desktop
// Rust core serves everything the Deno core serves, extensions and
// /api/v1/update included, because D12 and D13 are funded packages. Only the
// mobile build drops them, and it drops them because Android W^X and iOS forbid
// executing a subprocess, not because the port is incomplete. Keying on target
// is what keeps this correct when one binary serves both.
//
// Skipping is always explicit and always counted. A suite that silently drops
// cases is worse than no suite.

import type { Target } from "../harness/home.ts";

export interface ScopeRule {
  /** Route prefix, matched against "METHOD /path" entries in a case's `routes`. */
  prefix: string;
  reason: string;
}

const MOBILE_UNSERVED: ScopeRule[] = [
  { prefix: "/api/v1/update", reason: "updates ship through the app store, not self-update" },
  { prefix: "/api/v1/extensions", reason: "no subprocess execution under Android W^X or iOS" },
  { prefix: "/api/v1/tools", reason: "extension-backed tools are absent" },
  { prefix: "/api/v1/binaries", reason: "no downloadable executables" },
  { prefix: "/api/v1/sidecars", reason: "inference and speech are absorbed in-process" },
];

/** The reason this case is out of scope for the target, or null if it is in
 *  scope. A case is skipped when ANY of its routes is unserved: a case spanning
 *  a served and an unserved route cannot pass, and splitting it is the author's
 *  job. */
export function outOfScope(routes: string[], target: Target): string | null {
  if (target !== "rust-mobile") return null;
  for (const route of routes) {
    const path = route.includes(" ") ? route.split(" ")[1] : route;
    for (const rule of MOBILE_UNSERVED) {
      if (path.startsWith(rule.prefix)) return `${rule.prefix}: ${rule.reason}`;
    }
  }
  return null;
}

/** Every rule, for `--strict-scope` reporting and for the M3 capability
 *  assertion, which checks the core's own declaration against this table. */
export function scopeRules(target: Target): ScopeRule[] {
  return target === "rust-mobile" ? [...MOBILE_UNSERVED] : [];
}
