// shared-library search-path prepending. Per-platform behavior is
// the one place where a wrong env var would break llama / whisper
// silently. It uses `DYLD_LIBRARY_PATH` on macOS, `LD_LIBRARY_PATH` on Linux,
// and `PATH` + cwd-override on Windows. The function takes `platform`
// as an injected arg so tests don't have to spoof `Deno.build.os`.

import { assertEquals } from "@std/assert";
import { libraryEnvFor } from "./library-path.ts";

Deno.test("libraryEnvFor: macOS sets DYLD_LIBRARY_PATH only", () => {
  const out = libraryEnvFor("/usr/local/lib", "darwin");
  assertEquals(Object.keys(out.env), ["DYLD_LIBRARY_PATH"]);
  assertEquals(out.env.DYLD_LIBRARY_PATH.startsWith("/usr/local/lib"), true);
  assertEquals(out.cwd, undefined);
});

Deno.test("libraryEnvFor: Linux sets LD_LIBRARY_PATH only", () => {
  const out = libraryEnvFor("/opt/lib", "linux");
  assertEquals(out.env.LD_LIBRARY_PATH.startsWith("/opt/lib"), true);
  assertEquals(out.cwd, undefined);
});

Deno.test("libraryEnvFor: Windows prepends PATH with ';' and sets cwd override", () => {
  const out = libraryEnvFor("C:\\lib", "windows");
  assertEquals(out.env.PATH.startsWith("C:\\lib"), true);
  assertEquals(out.env.PATH.includes(";"), true);
  assertEquals(out.cwd, "C:\\lib");
});

Deno.test("libraryEnvFor: unknown OS produces empty env (no crash, no cwd)", () => {
  const out = libraryEnvFor("/anywhere", "aix" as typeof Deno.build.os);
  assertEquals(out.env, {});
  assertEquals(out.cwd, undefined);
});

Deno.test("libraryEnvFor: prepends dir BEFORE existing env var contents", () => {
  const priorEnv = Deno.env.get("DYLD_LIBRARY_PATH");
  Deno.env.set("DYLD_LIBRARY_PATH", "/already/here");
  try {
    const out = libraryEnvFor("/new", "darwin");
    assertEquals(out.env.DYLD_LIBRARY_PATH, "/new:/already/here");
  } finally {
    if (priorEnv === undefined) Deno.env.delete("DYLD_LIBRARY_PATH");
    else Deno.env.set("DYLD_LIBRARY_PATH", priorEnv);
  }
});
