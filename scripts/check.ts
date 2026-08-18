// Type-checks the repo's root-level tooling scripts (release, website, and the
// package dispatcher). Per-package type-checks live in each package's own
// `check` task and are fanned out by scripts/pkg.ts.

import { fromFileUrl } from "@std/path";

// Repo root as an OS path. `new URL("..").pathname` yields "/C:/work/" on Windows
// (an invalid cwd); fromFileUrl produces the correct native path on every OS.
const ROOT = fromFileUrl(new URL("..", import.meta.url));

const cmd = new Deno.Command("deno", {
  args: [
    "check",
    "--quiet",
    "scripts/build.ts",
    "scripts/release/ci-build.ts",
    "scripts/release/ci-publish.ts",
    "scripts/website/dev.ts",
    "scripts/website/build.ts",
    "scripts/pkg.ts",
    "scripts/fmt-web.ts",
  ],
  cwd: ROOT,
  stdout: "inherit",
  stderr: "inherit",
});
const { code } = await cmd.output();
if (code !== 0) {
  console.error("!! root script type-check failed");
  Deno.exit(1);
}
console.log("root scripts type-checked.");
