// Launches the tauri-driver smoke lane (WebdriverIO + tauri-driver). Manual
// only. Not invoked by CI.
//
// Specs live under tests/e2e/tauri-driver/specs/ as `*.test.ts`; scratch specs
// (`*.tmp.test.ts`) are gitignored.
//
// One-time setup the agent / dev must complete before this runs cleanly:
//   1. cargo install tauri-driver --locked
//   2. deno task build:client          # produces packages/loca-client/...debug/loca
//   3. cd tests/e2e/tauri-driver && npm i @wdio/cli @wdio/local-runner @wdio/mocha-framework @wdio/spec-reporter webdriverio
//      (kept out of the workspace deno.json so E2E stays opt-in)
//
// Once that's done, this script exec's `wdio run wdio.conf.ts`.

import { fromFileUrl, join } from "@std/path";

const repoRoot = fromFileUrl(new URL("..", import.meta.url));
const e2eDir = join(repoRoot, "tests", "e2e", "tauri-driver");
const wdioBin = join(e2eDir, "node_modules", ".bin", "wdio");

try {
  await Deno.stat(wdioBin);
} catch {
  console.error(
    `wdio not installed at ${wdioBin}.\n` +
      `See tests/e2e/tauri-driver/README.md for the one-time setup. ` +
      `E2E is intentionally opt-in. Its toolchain is heavy and most ` +
      `regression coverage already lives in the co-located test files.`,
  );
  Deno.exit(2);
}

const cmd = new Deno.Command(wdioBin, {
  args: ["run", "wdio.conf.ts", ...Deno.args],
  cwd: e2eDir,
  stdout: "inherit",
  stderr: "inherit",
});
const status = await cmd.output();
Deno.exit(status.code);
