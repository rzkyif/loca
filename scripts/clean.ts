// Removes build artifacts. Explicit allowlist only. Never a glob walk and
// never `git clean` (which would also wipe gitignored secrets like .env).
//
//   deno task clean                build outputs (dist, target, .svelte-kit, …)
//   deno task clean --deep         also node_modules + the Deno cache
//   deno task clean --dev-state    also ~/.loca/dev (isolated dev channel only)
//   deno task clean --latest-state   also ~/.loca/latest (isolated latest channel only)
//   deno task clean:stale          only the orphaned objects in target/*/deps
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

// Scanned by --stale. On macOS `split-debuginfo` defaults to "unpacked", so
// rustc writes one `.o` per codegen unit here and never reclaims the ones a
// later rebuild supersedes; a crate rebuilt in a loop leaves a copy behind
// every time. The rlib archives its own copy of those objects, so the loose
// ones only feed the debug map that symbolicates an already-linked binary, and
// cargo does not fingerprint them: removing one can never force a rebuild, and
// the next build writes it back. Age is the orphan test, since an object older
// than the cutoff cannot be an input to a build running now. Keep the cutoff
// comfortably longer than the slowest build so a paused one is never touched.
const STALE_DIRS = ["target/debug/deps", "target/release/deps"];
const STALE_DEFAULT_MINUTES = 180;

async function pruneStale(minutes: number): Promise<void> {
  const cutoff = Date.now() - minutes * 60_000;
  for (const dir of STALE_DIRS) {
    const abs = join(ROOT, dir);
    if (
      !(await Deno.stat(abs)
        .then((s) => s.isDirectory)
        .catch(() => false))
    ) {
      console.log(`skip     ${dir} (absent)`);
      continue;
    }
    let count = 0;
    let bytes = 0;
    for await (const entry of Deno.readDir(abs)) {
      if (!entry.isFile || !entry.name.endsWith(".o")) continue;
      const path = join(abs, entry.name);
      try {
        const info = await Deno.stat(path);
        if ((info.mtime?.getTime() ?? Date.now()) >= cutoff) continue;
        await Deno.remove(path);
        count += 1;
        bytes += info.size;
      } catch (err) {
        // A build running alongside us may replace an object mid-walk. That
        // one is not stale by definition, so leave it to the next run.
        if (!(err instanceof Deno.errors.NotFound)) throw err;
      }
    }
    const gib = (bytes / 1024 ** 3).toFixed(1);
    console.log(`pruned   ${dir} (${count} objects, ${gib} GiB, older than ${minutes}m)`);
  }
}

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
  boolean: ["deep", "dev-state", "latest-state", "stale"],
  string: ["older-than"],
});

// --stale prunes in place instead of removing trees, so it is safe to run
// while a build holds target/. It never combines with the wholesale removal.
if (args.stale) {
  const minutes = args["older-than"] ? Number(args["older-than"]) : STALE_DEFAULT_MINUTES;
  if (!Number.isFinite(minutes) || minutes <= 0) {
    throw new Error(`--older-than must be a positive number of minutes, got ${args["older-than"]}`);
  }
  await pruneStale(minutes);
  Deno.exit(0);
}

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
