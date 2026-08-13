// Derive the Deno `--allow-*` flag set from the granted permissions for the
// set of currently-enabled tools in a extension.
//
// Per the plan: spawn flags are computed from the UNION of grants for
// enabled tools. Optional ungranted permissions are silently absent (the
// tool sees a NotCapable at runtime and handles or fails gracefully).

import type { Grant, PermissionDecl, PermissionKind } from "@loca/shared";
import { isWithin } from "@loca/core-engine";
import { getLogger } from "../shared/log.ts";

const log = getLogger("permissions");

export interface FlagSet {
  netAll: boolean; // bare --allow-net (any host/port); set by a wildcard host grant
  net: Set<string>; // host:port entries (or a bare host for all-ports)
  read: Set<string>;
  write: Set<string>;
  run: Set<string>;
  env: Set<string>;
  ffi: boolean;
  sys: Set<string>;
}

export function emptyFlagSet(): FlagSet {
  return {
    netAll: false,
    net: new Set(),
    read: new Set(),
    write: new Set(),
    run: new Set(),
    env: new Set(),
    ffi: false,
    sys: new Set(),
  };
}

export interface ToolGrantContext {
  // Declared permissions for the tool (parsed from loca.json).
  required: PermissionDecl[];
  // Stored grants for the tool (only entries with state="granted" contribute).
  grants: Grant[];
}

// Compose the union flag set across an array of (decl, grant) contexts.
// Per-permission keys must appear in both `required` and `grants` (granted
// state) to contribute. Unknown grant keys are ignored. They're stale
// holdovers from a previous loca.json version.
export function unionFlags(tools: ToolGrantContext[], templates: PathTemplates): FlagSet {
  const out = emptyFlagSet();
  for (const tool of tools) {
    const grantedKeys = new Set(
      tool.grants.filter((g) => g.state === "granted").map((g) => g.permissionKey),
    );
    for (const decl of tool.required) {
      const key = declKey(decl);
      if (!grantedKeys.has(key)) continue;
      applyDecl(out, decl, templates);
    }
  }
  return out;
}

export function flagSetToArgs(flags: FlagSet): string[] {
  const args: string[] = [];
  if (flags.read.size > 0) {
    args.push(`--allow-read=${[...flags.read].join(",")}`);
  }
  if (flags.write.size > 0) {
    args.push(`--allow-write=${[...flags.write].join(",")}`);
  }
  // A wildcard-host grant means "any host" (Deno can't express "any host, only
  // these ports"), which is the bare `--allow-net` and supersedes any entries.
  if (flags.netAll) args.push("--allow-net");
  else if (flags.net.size > 0) {
    args.push(`--allow-net=${[...flags.net].join(",")}`);
  }
  if (flags.env.size > 0) args.push(`--allow-env=${[...flags.env].join(",")}`);
  if (flags.run.size > 0) args.push(`--allow-run=${[...flags.run].join(",")}`);
  if (flags.ffi) args.push("--allow-ffi");
  if (flags.sys.size > 0) args.push(`--allow-sys=${[...flags.sys].join(",")}`);
  return args;
}

// Template substitutions applied to read/write paths at spawn time.
// `$home`, `$downloads`, `$models`, `$sessions`, `$extension`, `$env.VAR`.
export interface PathTemplates {
  home: string;
  downloads: string;
  models: string;
  sessions: string;
  extension: string;
}

// `$env.VAR` in a path template resolves ONLY against this allowlist of
// path-shaped, non-secret variables. Resolving against the full core
// environment would let a declared path reference a core-only secret-bearing
// variable (e.g. an operator's API key) and smuggle its value into a granted
// read/write path. A non-path secret never belongs in a path anyway, so any
// off-list key resolves to "" (same as an unset var).
const PATH_ENV_ALLOWLIST = new Set([
  "HOME",
  "USERPROFILE",
  "APPDATA",
  "LOCALAPPDATA",
  "TMPDIR",
  "TMP",
  "TEMP",
  "XDG_CONFIG_HOME",
  "XDG_DATA_HOME",
  "XDG_CACHE_HOME",
]);

export function expandPath(template: string, templates: PathTemplates): string {
  return template
    .replaceAll("$home", templates.home)
    .replaceAll("$downloads", templates.downloads)
    .replaceAll("$models", templates.models)
    .replaceAll("$sessions", templates.sessions)
    .replaceAll("$extension", templates.extension)
    .replace(/\$env\.([A-Z_][A-Z0-9_]*)/g, (_, key) =>
      PATH_ENV_ALLOWLIST.has(key) ? (Deno.env.get(key) ?? "") : "",
    );
}

function applyDecl(flags: FlagSet, decl: PermissionDecl, templates: PathTemplates): void {
  switch (decl.kind) {
    case "net": {
      // Deno has no literal "*" token for a host or port. A wildcard host means
      // all hosts -> bare `--allow-net` (the built-in download/fetch tools rely
      // on this). A wildcard port means all ports of that host -> the bare host
      // with no `:port`. Emitting "*" verbatim produces a flag Deno rejects at
      // parse time, which crashes the worker at spawn.
      if (decl.host === "*") {
        flags.netAll = true;
        break;
      }
      for (const port of decl.ports) {
        flags.net.add(port === "*" ? decl.host : `${decl.host}:${port}`);
      }
      break;
    }
    case "read": {
      // An empty expansion (e.g. `$env.UNSET` or a non-allowlisted `$env.X`)
      // must NOT be added: an empty entry in the set serializes to `--allow-read=`
      // which Deno can read as a much broader grant than the author declared.
      // Drop it (the tool sees NotCapable at runtime, like any absent optional
      // permission) and warn so the author notices the unsatisfiable path.
      const readPath = expandPath(decl.path, templates);
      if (readPath) flags.read.add(readPath);
      else log.warn(`dropping read permission with empty path expansion: "${decl.path}"`);
      break;
    }
    case "write": {
      const writePath = expandPath(decl.path, templates);
      if (!writePath) {
        log.warn(`dropping write permission with empty path expansion: "${decl.path}"`);
        break;
      }
      // Never grant write into the extension's OWN installed code dir
      // ($extension / a path under it): that opens a TOCTOU where the tool could
      // rewrite its entry file in the window between the content-hash verify and
      // the worker's import (see chat-tool-dispatch verifyToolFresh). The
      // content-hash gate assumes the install dir is immutable, so a self-write
      // grant is dropped here rather than trusted.
      if (isWithin(templates.extension, writePath)) {
        log.warn(
          `dropping self-write grant into the extension install dir ($extension): "${decl.path}"`,
        );
        break;
      }
      flags.write.add(writePath);
      break;
    }
    case "run":
      flags.run.add(decl.binary);
      break;
    case "env":
      flags.env.add(decl.key);
      break;
    case "ffi":
      flags.ffi = true;
      break;
    case "sys":
      flags.sys.add(decl.flag);
      break;
    case "memories":
    case "llm":
    case "tts":
    case "stt":
      // Module-broker permissions carry no Deno sandbox flags; they are
      // enforced by the module broker at request time.
      break;
  }
}

function declKey(decl: PermissionDecl): string {
  switch (decl.kind) {
    case "net":
      return `net:${decl.host}:${decl.ports.map(String).join(",")}`;
    case "read":
      return `read:${decl.path}`;
    case "write":
      return `write:${decl.path}`;
    case "run":
      return `run:${decl.binary}`;
    case "env":
      return `env:${decl.key}`;
    case "ffi":
      return `ffi`;
    case "sys":
      return `sys:${decl.flag}`;
    case "memories":
      return `memories:${decl.access}`;
    case "llm":
      return `llm`;
    case "tts":
      return `tts`;
    case "stt":
      return `stt`;
  }
}

// Map between PermissionKind and the DB column representation.
export function permissionKindOf(decl: PermissionDecl): PermissionKind {
  return decl.kind;
}
