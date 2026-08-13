import { assertEquals } from "@std/assert";
import type { Grant, PermissionDecl } from "@loca/shared";
import { permissionKey } from "@loca/shared";
import { decidePrompt, type PromptContext } from "./prompt-matcher.ts";
import type { PathTemplates } from "./permissions.ts";

const templates: PathTemplates = {
  home: "/Users/u",
  downloads: "/Users/u/Downloads",
  models: "/Users/u/.loca/models",
  sessions: "/Users/u/.loca/core/sessions",
  extension: "/Users/u/.loca/core/extensions/tk",
};

function grant(decl: PermissionDecl, state: Grant["state"]): Grant {
  return {
    toolId: "tk::t",
    permissionKey: permissionKey(decl),
    permissionKind: decl.kind,
    state,
    grantedAtMs: 0,
  };
}

function ctx(
  required: PermissionDecl[],
  grants: Grant[] = [],
  undeclaredPolicy: "deny" | "ask" = "deny",
): PromptContext {
  return { required, grants, undeclaredPolicy, templates };
}

Deno.test("matcher: declared ask-state net permission forwards", () => {
  const decl: PermissionDecl = {
    kind: "net",
    host: "api.example.com",
    ports: [443],
    reason: "r",
  };
  const d = decidePrompt(
    { permission: "net", resource: "api.example.com:443" },
    ctx([decl], [grant(decl, "ask")]),
  );
  assertEquals(d, {
    action: "forward",
    declared: true,
    reason: "r",
    permissionKind: "net",
  });
});

Deno.test("matcher: absent grant row behaves as ask", () => {
  const decl: PermissionDecl = { kind: "env", key: "HOME", reason: "home" };
  const d = decidePrompt({ permission: "env", resource: "HOME" }, ctx([decl]));
  assertEquals(d?.action, "forward");
  assertEquals(d?.declared, true);
});

Deno.test("matcher: declared denied permission auto-denies", () => {
  const decl: PermissionDecl = { kind: "run", binary: "ls", reason: "list" };
  const d = decidePrompt(
    { permission: "run", resource: "ls" },
    ctx([decl], [grant(decl, "denied")]),
  );
  assertEquals(d?.action, "deny");
  assertEquals(d?.declared, true);
});

Deno.test("matcher: net wildcard host and port", () => {
  const decl: PermissionDecl = {
    kind: "net",
    host: "*",
    ports: ["*"],
    reason: "any",
  };
  const d = decidePrompt({ permission: "net", resource: "evil.example:8080" }, ctx([decl]));
  assertEquals(d?.declared, true);
  const wrongPort = decidePrompt(
    { permission: "net", resource: "a.example:9999" },
    ctx([{ kind: "net", host: "a.example", ports: [443], reason: "tls" }]),
  );
  assertEquals(wrongPort?.declared, false);
});

Deno.test("matcher: read path template expands and prefix-matches", () => {
  const decl: PermissionDecl = {
    kind: "read",
    path: "$downloads",
    reason: "dl",
  };
  const inside = decidePrompt(
    { permission: "read", resource: "/Users/u/Downloads/file.zip" },
    ctx([decl]),
  );
  assertEquals(inside?.declared, true);
  const outside = decidePrompt(
    { permission: "read", resource: "/Users/u/Documents/x" },
    ctx([decl]),
  );
  assertEquals(outside?.declared, false);
  // /Users/u/DownloadsEvil must NOT prefix-match /Users/u/Downloads.
  const sibling = decidePrompt(
    { permission: "read", resource: "/Users/u/DownloadsEvil/x" },
    ctx([decl]),
  );
  assertEquals(sibling?.declared, false);
});

Deno.test("matcher: run matches resolved absolute path by basename", () => {
  const decl: PermissionDecl = {
    kind: "run",
    binary: "ffmpeg",
    reason: "encode",
  };
  const d = decidePrompt(
    {
      permission: "run",
      resource: "/opt/homebrew/bin/ffmpeg",
    },
    ctx([decl]),
  );
  assertEquals(d?.declared, true);
});

Deno.test("matcher: path-pinned run declaration does not match same name elsewhere", () => {
  // Declaring "/usr/bin/git" must not bless "/tmp/evil/git" via basename match.
  const decl: PermissionDecl = {
    kind: "run",
    binary: "/usr/bin/git",
    reason: "vcs",
  };
  const exact = decidePrompt({ permission: "run", resource: "/usr/bin/git" }, ctx([decl]));
  assertEquals(exact?.declared, true);
  const imposter = decidePrompt(
    {
      permission: "run",
      resource: "/tmp/evil/git",
    },
    ctx([decl]),
  );
  // Falls through to undeclared (default policy deny).
  assertEquals(imposter, {
    action: "deny",
    declared: false,
    permissionKind: "run",
  });
});

Deno.test("matcher: ffi declaration matches any library path", () => {
  const decl: PermissionDecl = { kind: "ffi", reason: "native" };
  const d = decidePrompt(
    { permission: "ffi", resource: "/usr/lib/libSystem.B.dylib" },
    ctx([decl]),
  );
  assertEquals(d?.declared, true);
});

Deno.test("matcher: undeclared access follows extension policy", () => {
  const denied = decidePrompt({ permission: "read", resource: "/secret" }, ctx([], [], "deny"));
  assertEquals(denied, {
    action: "deny",
    declared: false,
    permissionKind: "read",
  });
  const asked = decidePrompt({ permission: "read", resource: "/secret" }, ctx([], [], "ask"));
  assertEquals(asked, {
    action: "forward",
    declared: false,
    permissionKind: "read",
  });
});

Deno.test("matcher: granted-state match still forwards (access beyond flag scope)", () => {
  // The flag covered $downloads, yet Deno prompted: the concrete access fell
  // outside what the flag granted, so the user decides.
  const decl: PermissionDecl = {
    kind: "read",
    path: "$downloads",
    reason: "dl",
  };
  const d = decidePrompt(
    { permission: "read", resource: "/Users/u/Downloads/sub/f" },
    ctx([decl], [grant(decl, "granted")]),
  );
  assertEquals(d?.action, "forward");
});

Deno.test("matcher: unknown prompt kind fails closed", () => {
  const d = decidePrompt(
    { permission: "import", resource: "https://x.example/mod.ts" },
    ctx([{ kind: "net", host: "*", ports: ["*"], reason: "any" }], [], "ask"),
  );
  assertEquals(d, null);
});
