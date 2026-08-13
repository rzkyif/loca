// Removes build artifacts. Explicit allowlist only. Never a glob walk and
// never `git clean` (which would also wipe gitignored secrets like .env).
//
//   deno task clean                build outputs (dist, target, .svelte-kit, …)
//   deno task clean --deep         also node_modules + the Deno cache
//   deno task clean --dev-state    also ~/.loca/dev (isolated dev channel only)
//   deno task clean --latest-state   also ~/.loca/latest (isolated latest channel only)
//
// `--dev-state` / `--latest-state` only ever touch that channel's ~/.loca/<ch>
// subtree; a stable install (~/.loca/stable) and the shared ~/.loca/models
// are never removed. Channel state nests our binaries by suffix
// (loca-core-latest), so build outputs in dist/ never need a per-channel clean.

import { parseArgs } from "@std/cli/parse-args";
import { fromFileUrl, join } from "@std/path";

const ROOT = fromFileUrl(new URL("..", import.meta.url));

// Build outputs, relative to the repo root. Every entry is gitignored.
const ARTIFACTS = [
  "dist",
  "packages/loca-website/dist",
  "packages/loca-website/.astro",
  "packages/loca-website/.wrangler",
  "packages/loca-client/build",
  "packages/loca-client/.svelte-kit",
  "target",
];

// Removed only with --deep; re-run `deno install` afterwards.
const DEEP = ["node_modules", ".deno-cache"];

function homeDir(): string {
  const home = Deno.env.get("HOME") ?? Deno.env.get("USERPROFILE");
  if (!home) {
    throw new Error("could not determine home directory (no HOME or USERPROFILE)");
  }
  return home;
}

async function rm(absPath: string, label: string): Promise<void> {
  try {
    await Deno.remove(absPath, { recursive: true });
    console.log(`removed  ${label}`);
  } catch (err) {
    if (err instanceof Deno.errors.NotFound) {
      console.log(`skip     ${label} (absent)`);
    } else if (err instanceof Deno.errors.PermissionDenied) {
      console.warn(
        `WARN     ${label}: permission denied. Close your editor / stop cargo, then retry`,
      );
    } else {
      throw err;
    }
  }
}

const args = parseArgs(Deno.args, {
  boolean: ["deep", "dev-state", "latest-state"],
});

const targets: Array<{ path: string; label: string }> = ARTIFACTS.map((p) => ({
  path: join(ROOT, p),
  label: p,
}));

if (args.deep) {
  for (const p of DEEP) targets.push({ path: join(ROOT, p), label: p });
}

if (args["dev-state"]) {
  const devDir = join(homeDir(), ".loca", "dev");
  targets.push({ path: devDir, label: "~/.loca/dev" });
}

if (args["latest-state"]) {
  const latestDir = join(homeDir(), ".loca", "latest");
  targets.push({ path: latestDir, label: "~/.loca/latest" });
}

for (const { path, label } of targets) {
  await rm(path, label);
}
