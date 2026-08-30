// Fans a standardized verb (check, lint, fmt, fmt:check, test, test:js, build,
// ...) out across the workspace members that define it, reading the package list
// from the root `deno.json` `workspace` array (the single source of truth for
// packages).
//
// Each member runs in isolation via its own `deno task`, so the root's
// same-named aggregate is never re-entered (`deno task -r` would recurse).
//
// Packages run concurrently (bounded pool, capped by LOCA_PKG_CONCURRENCY);
// each one's output is buffered and always written in full to
// `.gate-logs/<verb>-<pkg>.log`. The console stays concise: `ok` on a pass, the
// last 20 lines plus log path and rerun command on a failure, or the full output
// when CI is set (the log file is unreadable there).
//
// Usage: deno run -A scripts/pkg.ts <verb>

import { dirname, fromFileUrl, join } from "@std/path";

const ROOT = dirname(dirname(fromFileUrl(import.meta.url)));

const verb = Deno.args[0];
if (!verb) {
  console.error("usage: deno run -A scripts/pkg.ts <verb>");
  Deno.exit(2);
}

type Config = {
  name?: string;
  workspace?: string[];
  tasks?: Record<string, string>;
};

async function readConfig(dir: string): Promise<Config | null> {
  try {
    return JSON.parse(await Deno.readTextFile(join(dir, "deno.json")));
  } catch {
    return null;
  }
}

const rootCfg = await readConfig(ROOT);
const members: string[] = rootCfg?.workspace ?? [];

// Resolve the members that actually define the verb, preserving workspace order
// so the failure summary is stable.
const targets: { dir: string; rel: string; label: string }[] = [];
for (const member of members) {
  const rel = member.replace(/^\.\//, "");
  const dir = join(ROOT, rel);
  const cfg = await readConfig(dir);
  if (!cfg?.tasks?.[verb]) continue;
  targets.push({ dir, rel, label: cfg.name ?? rel });
}

const concurrency = Math.max(
  1,
  Number(Deno.env.get("LOCA_PKG_CONCURRENCY")) || navigator.hardwareConcurrency || 4,
);

// Color only on a real terminal with NO_COLOR unset, so piped output is clean.
const useColor = !Deno.env.get("NO_COLOR") && Deno.stdout.isTerminal();

function color(code: string, s: string): string {
  return useColor ? `\x1b[${code}m${s}\x1b[0m` : s;
}

// In CI the log file is unreadable, so failure blocks print the full output.
const CI = Boolean(Deno.env.get("CI"));

const EXCERPT_LINES = 20;
const LOG_DIR = ".gate-logs";
await Deno.mkdir(join(ROOT, LOG_DIR), { recursive: true });

// Dot reporters emit one marker per line when piped; pack consecutive
// marker-only lines into 80-column rows so the output stays readable.
function packDotLines(out: string): string {
  const packed: string[] = [];
  let dots = "";
  const flush = () => {
    for (let i = 0; i < dots.length; i += 80) packed.push(dots.slice(i, i + 80));
    dots = "";
  };
  for (const line of out.split("\n")) {
    if (/^[.·,!]+$/.test(line)) {
      dots += line;
      continue;
    }
    flush();
    packed.push(line);
  }
  flush();
  return packed.join("\n");
}

const failedLabels: string[] = [];
let next = 0;
async function worker() {
  while (next < targets.length) {
    const target = targets[next++];
    const started = performance.now();
    const { code, stdout, stderr } = await new Deno.Command("deno", {
      args: ["task", verb],
      cwd: target.dir,
      // deno task colors its output even when piped; keep logs and excerpts
      // escape-free.
      env: { NO_COLOR: "1" },
      stdout: "piped",
      stderr: "piped",
    }).output();
    const secs = ((performance.now() - started) / 1000).toFixed(1);
    const decoder = new TextDecoder();
    const out = packDotLines((decoder.decode(stdout) + decoder.decode(stderr)).trimEnd());
    // Preserved pass or fail, so the concise console never hides a warning.
    const logRel = `${LOG_DIR}/${verb.replaceAll(":", "-")}-${target.rel.split("/").pop()}.log`;
    await Deno.writeTextFile(join(ROOT, logRel), out + "\n");
    if (code === 0) {
      console.log(`${color("32", "ok")} ${target.label} ${verb} ${color("2", `(${secs}s)`)}`);
      continue;
    }
    failedLabels.push(target.label);
    const lines = [
      `\n${color("31", "!!")} ${target.label} ${verb} failed (exit ${code}, ${secs}s)`,
    ];
    const outLines = out ? out.split("\n") : [];
    if (CI || outLines.length <= EXCERPT_LINES) {
      lines.push(...outLines);
    } else {
      lines.push(`... ${outLines.length - EXCERPT_LINES} earlier lines in ${logRel} ...`);
      lines.push(...outLines.slice(-EXCERPT_LINES));
    }
    if (!CI) lines.push(color("2", `full: ${logRel}`));
    lines.push(color("2", `rerun: deno task --cwd ${target.rel} ${verb}`));
    // console.log is synchronous, so buffered blocks never interleave.
    console.log(lines.join("\n") + "\n");
  }
}

await Promise.all(Array.from({ length: Math.min(concurrency, targets.length) }, () => worker()));

if (failedLabels.length > 0) {
  console.error(
    `${verb}: ${failedLabels.length} of ${targets.length} package(s) failed: ${failedLabels.join(", ")}`,
  );
  Deno.exit(1);
}
console.log(`${verb}: ${targets.length} package(s) passed.`);
