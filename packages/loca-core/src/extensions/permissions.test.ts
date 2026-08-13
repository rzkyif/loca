// permission flag-set composition. Pure logic. No I/O, no DB.
// Verifies the union semantics that drive `--allow-*` args at worker spawn.

import { assertEquals } from "@std/assert";
import type { Grant, PermissionDecl } from "@loca/shared";
import { permissionKey } from "@loca/shared";
import {
  emptyFlagSet,
  expandPath,
  flagSetToArgs,
  type PathTemplates,
  unionFlags,
} from "./permissions.ts";

const templates: PathTemplates = {
  home: "/home/u",
  downloads: "/home/u/Downloads",
  models: "/home/u/.loca/core/models",
  sessions: "/home/u/.loca/core/sessions",
  extension: "/home/u/.loca/core/extensions/x",
};

function grantedFor(decl: PermissionDecl): Grant {
  return {
    toolId: "t",
    permissionKey: permissionKey(decl),
    permissionKind: decl.kind,
    state: "granted",
    grantedAtMs: 0,
  };
}

Deno.test("emptyFlagSet: all sets empty, ffi false", () => {
  const f = emptyFlagSet();
  assertEquals(f.netAll, false);
  assertEquals(f.net.size, 0);
  assertEquals(f.read.size, 0);
  assertEquals(f.write.size, 0);
  assertEquals(f.run.size, 0);
  assertEquals(f.env.size, 0);
  assertEquals(f.sys.size, 0);
  assertEquals(f.ffi, false);
});

Deno.test("unionFlags: only granted decls contribute; ungranted are skipped", () => {
  const granted: PermissionDecl = {
    kind: "net",
    host: "api.example.com",
    ports: [443],
    reason: "x",
  };
  const ungranted: PermissionDecl = {
    kind: "read",
    path: "/etc/hosts",
    reason: "x",
  };
  const flags = unionFlags(
    [{ required: [granted, ungranted], grants: [grantedFor(granted)] }],
    templates,
  );
  assertEquals(flags.net.has("api.example.com:443"), true);
  assertEquals(flags.read.size, 0);
});

Deno.test("unionFlags: net expands one entry per (host, port)", () => {
  const decl: PermissionDecl = {
    kind: "net",
    host: "api.example.com",
    ports: [80, 443],
    reason: "x",
  };
  const flags = unionFlags([{ required: [decl], grants: [grantedFor(decl)] }], templates);
  assertEquals(flags.net.has("api.example.com:80"), true);
  assertEquals(flags.net.has("api.example.com:443"), true);
});

Deno.test("unionFlags: wildcard port grants all ports of the host (bare host, valid Deno syntax)", () => {
  const decl: PermissionDecl = {
    kind: "net",
    host: "api.example.com",
    ports: ["*"],
    reason: "x",
  };
  const flags = unionFlags([{ required: [decl], grants: [grantedFor(decl)] }], templates);
  // Deno spells "all ports of a host" as the bare host (no :port); `host:*` is
  // rejected at parse time and would crash the worker at spawn.
  assertEquals(flags.net.has("api.example.com"), true);
  assertEquals(flagSetToArgs(flags).includes("--allow-net=api.example.com"), true);
});

Deno.test("unionFlags: wildcard host grants all hosts (bare --allow-net, valid Deno syntax)", () => {
  const decl: PermissionDecl = {
    kind: "net",
    host: "*",
    ports: [80, 443],
    reason: "x",
  };
  const flags = unionFlags([{ required: [decl], grants: [grantedFor(decl)] }], templates);
  assertEquals(flags.netAll, true);
  const args = flagSetToArgs(flags);
  // The bare `--allow-net` (any host/port) supersedes any host:port entries;
  // `*:80` would be rejected at parse time and crash the worker.
  assertEquals(args.includes("--allow-net"), true);
  assertEquals(
    args.some((a) => a.startsWith("--allow-net=")),
    false,
  );
});

Deno.test("unionFlags: read/write apply path templates", () => {
  const decl: PermissionDecl = {
    kind: "write",
    path: "$downloads/out",
    reason: "x",
  };
  const flags = unionFlags([{ required: [decl], grants: [grantedFor(decl)] }], templates);
  assertEquals(flags.write.has("/home/u/Downloads/out"), true);
});

Deno.test("unionFlags: a write grant into the extension's own install dir ($extension) is dropped", () => {
  // Self-write would open a TOCTOU vs the content-hash gate, so it must never
  // reach the worker's --allow-write set.
  const at: PermissionDecl = { kind: "write", path: "$extension", reason: "x" };
  const under: PermissionDecl = { kind: "write", path: "$extension/sub/f", reason: "x" };
  for (const decl of [at, under]) {
    const flags = unionFlags([{ required: [decl], grants: [grantedFor(decl)] }], templates);
    assertEquals(flags.write.size, 0);
  }
});

Deno.test("unionFlags: a write grant whose path expands empty is dropped, not added as an empty entry", () => {
  // A non-allowlisted $env expands to "", which must not become `--allow-write=`
  // (a much broader grant than declared).
  const decl: PermissionDecl = { kind: "write", path: "$env.LOCA_NOT_ALLOWLISTED", reason: "x" };
  const flags = unionFlags([{ required: [decl], grants: [grantedFor(decl)] }], templates);
  assertEquals(flags.write.size, 0);
  assertEquals(flags.write.has(""), false);
});

Deno.test("unionFlags: ffi flips the boolean, sys is a set", () => {
  const ffi: PermissionDecl = { kind: "ffi", reason: "x" };
  const sys: PermissionDecl = { kind: "sys", flag: "hostname", reason: "x" };
  const flags = unionFlags(
    [{ required: [ffi, sys], grants: [grantedFor(ffi), grantedFor(sys)] }],
    templates,
  );
  assertEquals(flags.ffi, true);
  assertEquals(flags.sys.has("hostname"), true);
});

Deno.test("unionFlags: identical grants across tools deduplicate", () => {
  const decl: PermissionDecl = { kind: "env", key: "FOO", reason: "x" };
  const flags = unionFlags(
    [
      { required: [decl], grants: [grantedFor(decl)] },
      { required: [decl], grants: [grantedFor(decl)] },
    ],
    templates,
  );
  assertEquals(flags.env.size, 1);
  assertEquals(flags.env.has("FOO"), true);
});

Deno.test("flagSetToArgs: emits one --allow-* per non-empty bucket", () => {
  const f = emptyFlagSet();
  f.net.add("a:80");
  f.net.add("b:443");
  f.read.add("/r");
  f.ffi = true;
  const args = flagSetToArgs(f);
  assertEquals(args.includes("--allow-net=a:80,b:443"), true);
  assertEquals(args.includes("--allow-read=/r"), true);
  assertEquals(args.includes("--allow-ffi"), true);
});

Deno.test("flagSetToArgs: omits buckets that are empty", () => {
  const args = flagSetToArgs(emptyFlagSet());
  assertEquals(args, []);
});

Deno.test("expandPath: substitutes $home/$downloads/$models/$sessions/$extension", () => {
  assertEquals(expandPath("$home/foo", templates), "/home/u/foo");
  assertEquals(expandPath("$downloads", templates), "/home/u/Downloads");
  assertEquals(expandPath("$models/x", templates), "/home/u/.loca/core/models/x");
  assertEquals(expandPath("$extension", templates), "/home/u/.loca/core/extensions/x");
});

Deno.test("expandPath: $env.VAR resolves only allowlisted path vars; off-list vars resolve empty", () => {
  const priorTmp = Deno.env.get("TMPDIR");
  Deno.env.set("TMPDIR", "/tmpx");
  // Secret-shaped, non-path var present in the core env: must NOT leak into a
  // granted path.
  Deno.env.set("LOCA_PERMISSIONS_SECRET", "leak");
  try {
    assertEquals(expandPath("$env.TMPDIR/x", templates), "/tmpx/x");
    assertEquals(expandPath("$env.LOCA_PERMISSIONS_SECRET/x", templates), "/x");
  } finally {
    if (priorTmp === undefined) Deno.env.delete("TMPDIR");
    else Deno.env.set("TMPDIR", priorTmp);
    Deno.env.delete("LOCA_PERMISSIONS_SECRET");
  }
});
