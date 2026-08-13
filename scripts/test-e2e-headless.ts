// Launches the headless integration E2E suite (vitest browser mode + Playwright
// Chromium). Manual only. Not invoked by CI.
//
// This lane mounts the real Svelte app in a real Chromium and drives it against
// a real loca-core subprocess over TLS, with outbound LLM/STT/TTS/downloads
// mocked locally. See tests/e2e/headless/README.md for the architecture and the
// behaviour delta versus the tauri-driver lane.
//
// One-time setup the agent / dev must complete before this runs cleanly:
//   cd tests/e2e/headless && npm install && npx playwright install chromium
//   (kept out of the workspace deno.json so the heavy browser toolchain stays
//    opt-in; mirrors the tauri-driver lane.)
//
// It also needs a dev install present (the four helper binaries + deno +
// llama-server symlinked under ~/.loca/dev/core/bin, produced by `deno task
// dev` at least once) and the Rust helper binaries built in target/debug.

import { fromFileUrl, join } from "@std/path";

const repoRoot = fromFileUrl(new URL("..", import.meta.url));
const dir = join(repoRoot, "tests", "e2e", "headless");
const vitestBin = join(dir, "node_modules", ".bin", "vitest");

try {
  await Deno.stat(vitestBin);
} catch {
  console.error(
    `vitest not installed at ${vitestBin}.\n` +
      `One-time setup (opt-in, heavy browser toolchain):\n` +
      `  cd tests/e2e/headless && npm install && npx playwright install chromium\n` +
      `See tests/e2e/headless/README.md.`,
  );
  Deno.exit(2);
}

const cmd = new Deno.Command(vitestBin, {
  args: ["run", ...Deno.args],
  cwd: dir,
  stdout: "inherit",
  stderr: "inherit",
});
const status = await cmd.output();
Deno.exit(status.code);
