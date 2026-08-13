// permissionKey is security-relevant: grants are looked up by this key,
// so the function must be deterministic and unambiguous across all 11
// permission kinds.

import { assertEquals } from "@std/assert";
import { type PermissionDecl, permissionKey } from "./extension.ts";

Deno.test("permissionKey: net packs host + comma-joined ports", () => {
  const decl: PermissionDecl = {
    kind: "net",
    host: "api.example.com",
    ports: [80, 443, "*"],
    reason: "fetch updates",
  };
  assertEquals(permissionKey(decl), "net:api.example.com:80,443,*");
});

Deno.test("permissionKey: read/write include the path verbatim", () => {
  assertEquals(permissionKey({ kind: "read", path: "/etc/hosts", reason: "x" }), "read:/etc/hosts");
  assertEquals(
    permissionKey({ kind: "write", path: "~/.cache/foo", reason: "x" }),
    "write:~/.cache/foo",
  );
});

Deno.test("permissionKey: run uses the binary name as the discriminator", () => {
  assertEquals(permissionKey({ kind: "run", binary: "ffmpeg", reason: "x" }), "run:ffmpeg");
});

Deno.test("permissionKey: env keys are the variable name", () => {
  assertEquals(
    permissionKey({ kind: "env", key: "OPENAI_API_KEY", reason: "x" }),
    "env:OPENAI_API_KEY",
  );
});

Deno.test("permissionKey: ffi is the bare 'ffi' constant", () => {
  assertEquals(permissionKey({ kind: "ffi", reason: "x" }), "ffi");
});

Deno.test("permissionKey: sys carries the flag", () => {
  assertEquals(permissionKey({ kind: "sys", flag: "hostname", reason: "x" }), "sys:hostname");
});

Deno.test("permissionKey: module kinds key by access (memories) or bare kind", () => {
  assertEquals(permissionKey({ kind: "memories", access: "read", reason: "x" }), "memories:read");
  assertEquals(permissionKey({ kind: "memories", access: "write", reason: "x" }), "memories:write");
  assertEquals(permissionKey({ kind: "llm", reason: "x" }), "llm");
  assertEquals(permissionKey({ kind: "tts", reason: "x" }), "tts");
  assertEquals(permissionKey({ kind: "stt", reason: "x" }), "stt");
});

Deno.test("permissionKey: identical decls produce identical keys (determinism)", () => {
  const a: PermissionDecl = {
    kind: "net",
    host: "x",
    ports: [443],
    reason: "first",
  };
  const b: PermissionDecl = {
    kind: "net",
    host: "x",
    ports: [443],
    reason: "second-with-different-reason",
  };
  // Reason is not part of the key: only the access surface is. Two decls
  // with the same access but different reasons must dedupe in the grants
  // table.
  assertEquals(permissionKey(a), permissionKey(b));
});

Deno.test("permissionKey: differing decls produce differing keys", () => {
  const keys = [
    permissionKey({ kind: "read", path: "/a", reason: "" }),
    permissionKey({ kind: "read", path: "/b", reason: "" }),
    permissionKey({ kind: "write", path: "/a", reason: "" }),
    permissionKey({ kind: "net", host: "a", ports: [443], reason: "" }),
    permissionKey({ kind: "net", host: "a", ports: [80], reason: "" }),
    permissionKey({ kind: "ffi", reason: "" }),
  ];
  assertEquals(new Set(keys).size, keys.length);
});
