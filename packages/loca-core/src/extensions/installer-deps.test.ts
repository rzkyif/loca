// The `deno install` argument vector, and the setting that opens it.
//
// Both assertions are about a flag whose presence looks harmless:
// `--allow-scripts` makes more packages install successfully, so nothing about
// adding it reads as wrong. What it actually does is execute whatever a
// transitive dependency's author put in `postinstall`, with the user's
// privileges, during what the user asked for as an install.

import { assertEquals } from "@std/assert";
import { allowPostinstallScripts, denoInstallArgs } from "./installer-deps.ts";

Deno.test("denoInstallArgs: lifecycle scripts are off unless the flag is passed", () => {
  assertEquals(denoInstallArgs(false), ["install", "--node-modules-dir=auto", "--quiet"]);
  assertEquals(denoInstallArgs(true), [
    "install",
    "--node-modules-dir=auto",
    "--quiet",
    "--allow-scripts",
  ]);
});

// The setting is phrased as BLOCKING, so the flag is its inverse, and getting
// that backwards is the one mistake here that fails open. The absent case is
// asserted separately from the explicit `true`: they are the same answer for
// different reasons, and a reader returning its fallback unconditionally would
// pass on either one alone.
Deno.test("allowPostinstallScripts: unset and hostile records land on blocked", () => {
  assertEquals(allowPostinstallScripts({}), false);
  assertEquals(allowPostinstallScripts({ "extensions.ignorePostinstallScripts": true }), false);
  assertEquals(
    allowPostinstallScripts({ "extensions.ignorePostinstallScripts": "yes please" }),
    false,
  );
  // And the one value that opens it, which is the user saying so.
  assertEquals(allowPostinstallScripts({ "extensions.ignorePostinstallScripts": false }), true);
});
