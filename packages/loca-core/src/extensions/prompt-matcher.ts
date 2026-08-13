// Decide what to do with a runtime permission prompt: auto-deny it or
// forward it to the user in chat. Pure function over the tool's declared
// permissions + grants and the extension's undeclared-access policy.
//
// Grant semantics (see db/schema.sql): "granted" permissions are baked into
// the spawn flags so they never prompt; "ask" (or no grant row) prompts and
// forwards here; "denied" auto-rejects. A prompt that matches no declared
// permission follows the extension's undeclared policy. A prompt CAN match a
// "granted" declaration when the actual access falls outside the granted
// flag's scope (e.g. a path resolving outside the declared prefix); that is
// effectively an ask, so it forwards.

import {
  type Grant,
  type PermissionDecl,
  permissionKey,
  type PermissionKind,
  type UndeclaredPolicy,
} from "@loca/shared";
import { expandPath, type PathTemplates } from "./permissions.ts";

export interface PromptContext {
  required: PermissionDecl[];
  grants: Grant[];
  undeclaredPolicy: UndeclaredPolicy;
  templates: PathTemplates;
}

export interface PromptDecision {
  action: "deny" | "forward";
  /** True when the prompt matched one of the tool's declared permissions. */
  declared: boolean;
  /** The matched declaration's reason, for the chat prompt UI. */
  reason?: string;
  permissionKind: PermissionKind;
}

const KNOWN_KINDS: ReadonlySet<string> = new Set([
  "net",
  "read",
  "write",
  "run",
  "env",
  "ffi",
  "sys",
]);

export function decidePrompt(
  prompt: { permission: string; resource: string },
  ctx: PromptContext,
): PromptDecision | null {
  // Prompt kinds outside the loca.json vocabulary (e.g. "import") cannot be
  // declared, granted, or rendered; fail closed.
  if (!KNOWN_KINDS.has(prompt.permission)) return null;
  const kind = prompt.permission as PermissionKind;

  const decl = ctx.required.find((d) => d.kind === kind && declMatches(d, prompt.resource, ctx));
  if (!decl) {
    return ctx.undeclaredPolicy === "ask"
      ? { action: "forward", declared: false, permissionKind: kind }
      : { action: "deny", declared: false, permissionKind: kind };
  }

  const state = ctx.grants.find((g) => g.permissionKey === permissionKey(decl))?.state ?? "ask";
  if (state === "denied") {
    return {
      action: "deny",
      declared: true,
      reason: decl.reason,
      permissionKind: kind,
    };
  }
  return {
    action: "forward",
    declared: true,
    reason: decl.reason,
    permissionKind: kind,
  };
}

function declMatches(decl: PermissionDecl, resource: string, ctx: PromptContext): boolean {
  switch (decl.kind) {
    case "net": {
      // Resource is "host:port" (IPv6 hosts arrive bracketed, so the last
      // colon is the port separator).
      const sep = resource.lastIndexOf(":");
      const host = sep === -1 ? resource : resource.slice(0, sep);
      const port = sep === -1 ? "" : resource.slice(sep + 1);
      if (decl.host !== "*" && decl.host !== host) return false;
      return decl.ports.some((p) => p === "*" || String(p) === port);
    }
    case "read":
    case "write": {
      const base = expandPath(decl.path, ctx.templates);
      if (base.length === 0) return false;
      if (resource === base) return true;
      const prefix = base.endsWith("/") || base.endsWith("\\") ? base : base + pathSep(base);
      return resource.startsWith(prefix);
    }
    case "run": {
      if (decl.binary === resource) return true;
      // Only fall back to basename matching when the declaration is itself a
      // bare command name (Deno resolves it to an absolute path at prompt
      // time, e.g. "ffmpeg" -> "/opt/homebrew/bin/ffmpeg"). A declaration that
      // pins a full path must match exactly, so a same-named binary planted in
      // another directory is treated as undeclared, not silently "declared".
      const declHasPath = decl.binary.includes("/") || decl.binary.includes("\\");
      if (declHasPath) return false;
      return basename(resource) === decl.binary;
    }
    case "env":
      return decl.key === resource;
    case "sys":
      return decl.flag === resource;
    case "ffi":
      return true;
    case "memories":
    case "llm":
    case "tts":
    case "stt":
      // Module-broker permissions never originate from Deno runtime
      // prompts (and KNOWN_KINDS excludes them), so they match nothing.
      return false;
  }
}

function pathSep(path: string): string {
  return path.includes("\\") && !path.includes("/") ? "\\" : "/";
}

function basename(path: string): string {
  const i = Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\"));
  return i === -1 ? path : path.slice(i + 1);
}
