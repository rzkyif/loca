// Extension, Tool, Permission, Grant: shared shapes used by core + client.
// The on-disk source of truth for permissions is the extension's tomat.json
// (see validation/tomat-json.ts). These types describe the DB-projected
// view that the API serves.

// "seeded" covers every extension Tomat ships and installs on a fresh Core (the
// built-in and the dev-only samples). Seeded bytes resolve from the codebase in
// dev and from install-script-planted / CDN artifacts in prod; npm + local are
// the user-supplied sources.
export type ExtensionSource = "npm" | "local" | "seeded";

// Gated lifecycle status. `downloaded`: files on disk, deps not installed,
// content hash not pinned. `installed`: deps installed, content hash pinned,
// tools enable-able + (if perms granted) LLM-exposed. `drift`: on-disk content
// no longer matches the pinned hash; all tools auto-disabled until the user
// re-confirms (which re-pins the current content).
export type ExtensionStatus = "downloaded" | "installed" | "drift";

// Seeded extensions Tomat ships and installs on a fresh Core. Each id doubles as
// the on-disk folder name under the extensions dir and as the package directory
// name under packages/; ids satisfy the local-slug charset, so no flattening is
// applied. The built-in is released + planted in prod; samples is dev-only (never
// released, never planted) and exists purely as the capability showcase.
export const BUILTIN_EXTENSION_ID = "tomat-extension-builtin";
export const SAMPLES_EXTENSION_ID = "tomat-extension-samples";

// One seeded extension: its id (also the extensions-dir folder name) + the
// packages/ directory that is both its release source and its dev byte source.
// `devOnly` extensions are skipped by the prod seed path, the release pipeline,
// and the install-script planting; they are only ever codebase-installed in dev.
export interface SeededExtension {
  id: string;
  dir: string;
  devOnly: boolean;
}

export const SEEDED_EXTENSIONS: readonly SeededExtension[] = [
  { id: BUILTIN_EXTENSION_ID, dir: "tomat-extension-builtin", devOnly: false },
  { id: SAMPLES_EXTENSION_ID, dir: "tomat-extension-samples", devOnly: true },
];

/** The seeded-extension descriptor for an id, or undefined if the id is not a
 *  seeded extension. */
export function seededExtensionById(id: string): SeededExtension | undefined {
  return SEEDED_EXTENSIONS.find((e) => e.id === id);
}

// Single source of truth for the permission kinds: the type is derived from
// this tuple, and runtime validators (e.g. the WS permission-request schema)
// reuse the same array via `z.enum(PERMISSION_KINDS)` so the two can't drift.
export const PERMISSION_KINDS = [
  "net",
  "read",
  "write",
  "run",
  "env",
  "ffi",
  "sys",
  "memories",
  "llm",
  "tts",
  "stt",
] as const;

export type PermissionKind = (typeof PERMISSION_KINDS)[number];

// Wire shape mirroring tomat.json permission entries. Cardinality varies by
// kind: net entries declare host/ports; read/write declare paths; run declares
// binaries; env declares variable names. ffi/sys are all-or-nothing keys.
// memories/llm/tts/stt are core-module permissions enforced by the module
// broker (not Deno flags); memories splits into read/write access.
export type PermissionDecl =
  | {
      kind: "net";
      host: string;
      ports: ReadonlyArray<number | "*">;
      reason: string;
      optional?: boolean;
    }
  | { kind: "read"; path: string; reason: string; optional?: boolean }
  | { kind: "write"; path: string; reason: string; optional?: boolean }
  | { kind: "run"; binary: string; reason: string; optional?: boolean }
  | { kind: "env"; key: string; reason: string; optional?: boolean }
  | { kind: "ffi"; reason: string; optional?: boolean }
  | { kind: "sys"; flag: string; reason: string; optional?: boolean }
  | {
      kind: "memories";
      access: "read" | "write";
      reason: string;
      optional?: boolean;
    }
  | { kind: "llm"; reason: string; optional?: boolean }
  | { kind: "tts"; reason: string; optional?: boolean }
  | { kind: "stt"; reason: string; optional?: boolean };

// Stable string key derived from a permission decl, used as the primary key
// in the grants table. See plan §7 for the format per kind.
export function permissionKey(decl: PermissionDecl): string {
  switch (decl.kind) {
    case "net": {
      const portStr = decl.ports.map((p) => String(p)).join(",");
      return `net:${decl.host}:${portStr}`;
    }
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

// Per-permission decision. `granted` (Always Allow) bakes the permission into
// the worker's --allow-* spawn flags; `ask` leaves it out so Deno prompts at
// the moment of access and the user decides in chat; `denied` auto-rejects the
// prompt. A permission with no grant row behaves as `ask`.
export type GrantState = "granted" | "ask" | "denied";

// Extension-level policy for runtime permission prompts that match none of the
// tool's declared permissions: auto-reject or forward to the user.
export type UndeclaredPolicy = "deny" | "ask";

export interface Grant {
  toolId: string;
  permissionKey: string;
  permissionKind: PermissionKind;
  state: GrantState;
  grantedAtMs: number;
}

// Where a tool comes from. Extensions run in the local sandbox (permission
// grants apply); MCP-server tools run on the server (no local grants, just an
// enable toggle). The Tools UI aggregates both, so a tool carries its provider.
export type ToolProviderKind = "extension" | "mcp";

// Platforms a tool can declare it works on. `linux` matches any Linux session;
// `linux_x11` / `linux_wayland` narrow to a display server. Tomat resolves the
// host's tokens centrally (Core reads the display server once) so authors never
// detect it themselves. A tool whose declared list doesn't intersect the host's
// resolved tokens is dropped from every listing path: the relevance filter, the
// model (even with filtering off), and the Tools UI.
export type ToolPlatform = "darwin" | "windows" | "linux" | "linux_x11" | "linux_wayland";

// True when a tool with the given declared platforms is supported on a host
// whose resolved platform tokens are `hostPlatforms`. An empty/absent declared
// list means "all platforms"; otherwise the lists must intersect (host
// ["linux","linux_x11"] satisfies a tool declaring ["linux"] OR ["linux_x11"]).
export function toolPlatformSupported(
  declared: string[] | undefined,
  hostPlatforms: string[],
): boolean {
  return !declared || declared.length === 0 || declared.some((p) => hostPlatforms.includes(p));
}

export interface Tool {
  id: string; // `${extensionId}::${name}`
  // Id of the provider that supplies this tool (an extension id, or an MCP
  // server id). Named for the extension case it began as; holds either.
  extensionId: string;
  providerKind: ToolProviderKind;
  // Human-readable provider name for the Tools UI (extension displayName or MCP
  // server name). Populated by the flat all-tools projection.
  providerName?: string;
  name: string;
  description: string;
  parameters: Record<string, unknown>; // JSON Schema draft 2020-12
  triggers: string[];
  fnExport: string;
  alwaysAvailable: boolean;
  // OS gating. Empty = every platform. See toolPlatformSupported. MCP tools
  // leave this empty (they run on their server, not the local host).
  platforms: string[];
  enabled: boolean;
  requiredPermissions: PermissionDecl[];
  // Indices into `requiredPermissions` that have no grant row yet. Purely
  // informational for the UI: an absent row behaves as `ask`.
  missingRequired: number[];
  grants: Grant[];
}

export interface Extension {
  id: string; // npm pkg name (with `@scope/name` flattened to `@scope__name`) or local slug
  source: ExtensionSource;
  displayName: string;
  description?: string;
  version: string;
  installedPath: string;
  manifestHash: string;
  // The pinned trust anchor. Empty string until the extension is installed.
  contentHash: string;
  status: ExtensionStatus;
  // Whether the extension declares dependencies (deno.json/package.json). Drives
  // the Uninstall vs Delete choice: a no-dep extension is installed on download
  // and can only be deleted (there is nothing to uninstall).
  hasDeps: boolean;
  // Whether tomat.json declares `database: true`: the core provisions a
  // private per-extension SQLite database its tools reach via ctx.db.
  hasDatabase: boolean;
  undeclaredPolicy: UndeclaredPolicy;
  // Tool counts from the list/get projection, so a card can show "N enabled"
  // without lazy-loading the full tool list.
  toolCount: number;
  enabledToolCount: number;
  installedAtMs: number;
  updatedAtMs: number;
  // Convenience embedding of tools when fetched via /extensions/:id/tools.
  tools?: Tool[];
}

// Search-result entry as returned by /api/v1/extensions/search.
export interface ExtensionSearchResult {
  name: string;
  description: string;
  version: string;
  weeklyDownloads?: number;
  source: "npm";
  homepage?: string;
  license?: string;
}

// OpenAI-format tool schema as returned by /api/v1/extensions/tool-schemas.
export interface OpenAIToolDef {
  type: "function";
  function: {
    name: string;
    description: string;
    parameters: Record<string, unknown>;
  };
}

export interface ToolDescriptor {
  toolId: string;
  extensionId: string;
  name: string;
  description: string;
  similarity?: number;
}
