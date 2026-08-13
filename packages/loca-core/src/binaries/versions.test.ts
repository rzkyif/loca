// host-triple + canonical binary-name helpers.

import { assertEquals } from "@std/assert";
import { binaryName, hostTriple, platformExe } from "./versions.ts";

Deno.test("hostTriple: reports the running Deno target", () => {
  const t = hostTriple();
  // We can't assert a specific triple (depends on the test host), but it
  // must match Deno.build.target verbatim.
  assertEquals(t, Deno.build.target);
});

Deno.test("platformExe: '' on unix, '.exe' on windows", () => {
  const ext = platformExe();
  if (Deno.build.os === "windows") assertEquals(ext, ".exe");
  else assertEquals(ext, "");
});

Deno.test("binaryName: composes kind + platformExe()", () => {
  const name = binaryName("llama-server" as never);
  if (Deno.build.os === "windows") assertEquals(name, "llama-server.exe");
  else assertEquals(name, "llama-server");
});
