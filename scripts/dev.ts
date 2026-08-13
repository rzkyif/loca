// Boots tomat-core (deno watch) and tomat-client (tauri dev) together on the
// "dev" install channel, so all state lives under ~/.tomat/dev/ and never
// touches a stable install. Seeds a dev admin token (the from-source core
// never mints one; only the installer does), sets a fresh dev admin password
// each run (for the password-gated remote flows), and prints a ready-to-use
// pairing code so connecting the client is a one-paste step.
// Stops both children on SIGINT/SIGTERM or when either exits.

import { fromFileUrl, join } from "@std/path";
import { ensureDir } from "@std/fs/ensure-dir";
import { SEEDED_EXTENSIONS } from "@tomat/shared";
import { computeDevManifest } from "../packages/tomat-core/src/extensions/seeded-manifest.ts";

const ROOT = fromFileUrl(new URL("..", import.meta.url));

// Dev runs on the "dev" install channel (~/.tomat/dev/{core,client}). Both
// children inherit TOMAT_CHANNEL so core and the Tauri client agree on the
// isolated location + keychain namespace.
const CHANNEL_ENV = { TOMAT_CHANNEL: "dev" };
// The dev core binds the dev-channel port (stable base 7800 + dev offset 20,
// see packages/tomat-core/src/paths.ts) and serves the API over TLS with a
// self-signed cert. The dev task trusts that cert for loopback via
// --unsafely-ignore-certificate-errors=127.0.0.1 (set in deno.json), so these
// HTTPS calls to the local core succeed without a public CA.
const CORE_URL = "https://127.0.0.1:7820";

function homeDir(): string {
  const home = Deno.env.get("HOME") ?? Deno.env.get("USERPROFILE");
  if (!home) {
    throw new Error("could not determine home directory (no HOME or USERPROFILE)");
  }
  return home;
}

const DEV_CORE_DIR = join(homeDir(), ".tomat", "dev", "core");
const ADMIN_TOKEN_FILE = join(DEV_CORE_DIR, ".admin-token");

// Dev seeded-extension manifests. There is no published manifests/dev/extension.json,
// so dev.ts generates one per seeded extension (the built-in plus the dev-only
// samples) from the in-repo source and regenerates it on edits, at the cache path
// core reads. The version carries a content hash so any edit reads as "Update
// available" after a check; "Update" then reinstalls the codebase copy. The
// version formula is reused from computeDevManifest() in
// packages/tomat-core/src/extensions/seeded-manifest.ts (no duplication).
const SEEDED_SRC_DIRS = SEEDED_EXTENSIONS.map((e) => join(ROOT, "packages", e.dir));

function devManifestPath(id: string): string {
  return join(DEV_CORE_DIR, "cache", `${id}-manifest.json`);
}

async function writeDevExtensionManifests(): Promise<void> {
  await ensureDir(join(DEV_CORE_DIR, "cache"));
  for (const ext of SEEDED_EXTENSIONS) {
    const manifest = await computeDevManifest(ext);
    const path = devManifestPath(ext.id);
    const tmp = path + ".tmp";
    await Deno.writeTextFile(tmp, JSON.stringify(manifest, null, 2));
    await Deno.rename(tmp, path);
  }
}

/** Regenerate the dev manifests now, then on every change under any seeded
 *  extension's source dir (debounced). Fire-and-forget: the watcher lives for the
 *  dev session and is torn down when the orchestrator exits. */
async function startDevExtensionManifest(): Promise<void> {
  await writeDevExtensionManifests();
  void (async () => {
    let timer: ReturnType<typeof setTimeout> | null = null;
    try {
      for await (const _event of Deno.watchFs(SEEDED_SRC_DIRS)) {
        if (timer) clearTimeout(timer);
        timer = setTimeout(() => void writeDevExtensionManifests().catch(() => {}), 200);
      }
    } catch {
      // watch failures are non-fatal in dev
    }
  })();
}
// The dev client's data root. --reset wipes it whole (see resetClientSettings):
// cores.json (the paired-cores registry, whose absence sends the next boot back
// through core management, see the boot gate in
// packages/tomat-client/src/ui/routes/+page.svelte), settings.json, snippets/,
// keychain.json (the dev file-backed bearer-token store), and logs. None of it
// needs downloading, so nothing here is preserved.
const DEV_CLIENT_DIR = join(homeDir(), ".tomat", "dev", "client");

// --reset is a denylist, not an allowlist: it deletes EVERYTHING under the dev
// core dir EXCEPT these names, so the next boot starts from a clean slate
// (db, settings, sessions, memories, secrets, admin token/password, the
// seed markers, installed extensions, logs, caches - all gone and all
// regenerated on boot). An allowlist silently let new state survive (this is
// exactly how tools.enabled lingered across resets). The two preserved entries
// are pure download/build artifacts that are expensive to re-acquire and hold
// no user state:
//   - bin/        the sidecar binaries (llama-server, tomat-core-speech + its
//                 fetched espeak-ng-data, the cargo-built helpers) and deno.
//   - deno-cache/ the downloaded deno/npm dependency cache.
// ~/.tomat/models lives outside the channel root and so is never touched.
// A future download-only cache added under the core dir belongs in this set.
const CORE_RESET_PRESERVE = new Set(["bin", "deno-cache"]);

// CLI flags, wired to the deno tasks in deno.json:
//  --reset          delete the dev client's settings.json before launch so the
//                   client boots into core management instead of straight to
//                   chat, AND drop the dev core's SQLite db + built-in seed
//                   marker so it boots onto a fresh schema and re-stages the
//                   built-in extension (dev:reset / dev:reset:install).
//  --fresh-install  make the client report no local core, so first boot shows
//                   the welcome remote-connect page instead of silently pairing
//                   with the dev core. Sets TOMAT_DEV_FRESH_INSTALL for the
//                   client (read by the Tauri pairing commands) and skips the
//                   remote prefill so that page starts blank
//                   (dev:reset:install).
const RESET = Deno.args.includes("--reset");
const FRESH_INSTALL = Deno.args.includes("--fresh-install");
// --android / --ios run the mobile client (`tauri android|ios dev`) against this
// dev core instead of the desktop client. The mobile WebView runs on an
// emulator/simulator/device, so the core must be reachable off-loopback
// (TOMAT_CORE_HOST=0.0.0.0 below) and the onboarding prefill is passed through
// Vite env (mobile has no launch argv).
const ANDROID = Deno.args.includes("--android");
const IOS = Deno.args.includes("--ios");
// The parts that are identical for either mobile target: bind the core to every
// interface and route the prefill through Vite env rather than argv.
const MOBILE = ANDROID || IOS;
// --client-only spawns just the client (no dev core, no helper/sidecar setup),
// the orchestrated equivalent of running the bare client task. It exists so the
// mobile client-only loop still gets this script's clean teardown (the detached
// Gradle/Xcode build sweep below) instead of leaking on Ctrl+C. A core is
// assumed to be running separately; onboarding derives its address from the dev
// host.
const CLIENT_ONLY = Deno.args.includes("--client-only");

// All child labels ("core", "client", "dev") are left-aligned to this width so
// the message columns line up regardless of label length.
const LABEL_WIDTH = 6;

// Color only when stdout is a real terminal and NO_COLOR is unset, so piping the
// dev output to a file (or NO_COLOR=1) yields clean, escape-free text.
const useColor = !Deno.env.get("NO_COLOR") && Deno.stdout.isTerminal();

function color(code: string, s: string): string {
  return useColor ? `\x1b[${code}m${s}\x1b[0m` : s;
}

// Compact wall-clock HH:MM:SS.mmm. dev.ts is the single timestamp authority for
// the multiplexed console: children emit bare messages (core skips its own
// timestamp via TOMAT_LOG_NO_TIME) and this stamp is prepended to every line.
function stamp(): string {
  const d = new Date();
  const p = (n: number, w = 2) => String(n).padStart(w, "0");
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}.${p(
    d.getMilliseconds(),
    3,
  )}`;
}

// `<time> <badge> ` prefix, built per line at write time so the timestamp marks
// when the line arrived. The badge color is the child's SGR code.
function linePrefix(name: string, code: string): string {
  return `${color("2", stamp())} ${color(code, name.padEnd(LABEL_WIDTH))} `;
}

// Strip ANSI escape sequences so the line matchers see plain text (tauri runs
// cargo with `--color always`, and Deno bolds its "Watcher" prefix). Matches the
// full CSI family, not just SGR color (`m`): cargo prefixes its final "Finished"
// status with a clear-line `\x1b[2K`, so an `m`-only strip would leave a leading
// ESC and miss the match.
// oxlint-disable-next-line no-control-regex
const ANSI_RE = /\x1b\[[0-9;?]*[A-Za-z]/g;

// Our own logs (core's @std/log + the client's fern) start every line with a
// lowercase, padded level word. That positive signal is how we recognize a line
// as ours and pass it through untouched; anything else is build noise (dropped)
// or a leak (wrapped, below).
const LEVEL_RE = /^(?:error|warn|info|debug|trace)\s/;

// The tauri-cli / cargo / deno-task orchestration chatter that shares the child's
// stream with the app's own logs. Pure noise, dropped outright. Build
// ERRORS/warnings don't match these, so they fall through to the leak path and
// still surface (clearly marked as not-ours).
const BUILD_NOISE = [
  /^Running BeforeDevCommand\b/,
  /^Running DevCommand\b/,
  /^Running `/,
  /^Compiling\b/,
  /^Finished\b/,
  /^Building\b/,
  /^Blocking\b/,
  /^Updating\b/,
  /^Locking\b/,
  /^Info\b/, // tauri-cli "Info Watching ... for changes..." (our logs are lowercase)
  /^Task\b/, // deno "Task <name> deno run ..."
];

// Level SGR codes, matching core's @std/log + the client's fern coloring (see
// each package's log module): debug dim, info green, warn yellow, error red.
const LEVEL_CODE = { debug: "2", info: "32", warn: "33", error: "31" } as const;

// Render a line dev.ts synthesizes itself in the same `<level> <scope> <message>`
// shape the children's own logs use, so it groups with them visually. The level
// word is padded to 5 like @std/log / fern; the scope is dim.
function levelLine(level: keyof typeof LEVEL_CODE, scope: string, message: string): string {
  return `${color(LEVEL_CODE[level], level.padEnd(5))} ${color("2", scope)} ${message}`;
}

// Wrap a line that did NOT come from our logging system in a dim "leak" marker
// so it's obvious it bypassed our format. We rely on a denylist for build noise,
// so new leaks are inevitable; marking (not hiding) them keeps them visible.
function leakLine(content: string): string {
  return `${color("2", "leak".padEnd(5))} ${content}`;
}

// Decide how a raw child line is presented: pass our own logs through verbatim,
// drop build noise, render the known watcher lines as standard leveled logs, and
// wrap everything else as a leak. Returns null to drop.
function formatChildLine(raw: string): string | null {
  const plain = raw.replace(ANSI_RE, "").trimStart();
  if (plain.trim() === "") return null; // blank spacing from build tools
  if (BUILD_NOISE.some((re) => re.test(plain))) return null;
  if (LEVEL_RE.test(plain)) return raw; // our log: keep it (and its colors) verbatim
  // Deno's `--watch` supervisor emits a small, known family of lines on core's
  // stream: "Restarting! File change detected: ..." (only on an actual reload),
  // "Waiting for graceful termination..." (on every reload and on Ctrl-C), and
  // "Process started/finished.". They're fully expected, so render them as
  // standard leveled `watcher`-scoped logs instead of leaks: the reload event as
  // info, the rest as debug chatter.
  const watcher = plain.match(/^Watcher\s+(.*)$/);
  if (watcher) {
    const body = watcher[1];
    const level = body.startsWith("Restarting!") ? "info" : "debug";
    return levelLine(level, "watcher", body);
  }
  return leakLine(plain);
}

type Child = {
  name: string;
  proc: Deno.ChildProcess;
};

const children: Child[] = [];

function spawn(
  name: string,
  code: string,
  cmd: string[],
  cwd?: string,
  env?: Record<string, string>,
): Child {
  const proc = new Deno.Command(cmd[0], {
    args: cmd.slice(1),
    cwd: cwd ?? ROOT,
    // Merge atop the parent env so PATH / HOME / etc. carry through.
    env: env ? { ...Deno.env.toObject(), ...env } : undefined,
    stdout: "piped",
    stderr: "piped",
    stdin: "null",
  }).spawn();

  void pipe(proc.stdout, stdoutWriter, name, code);
  void pipe(proc.stderr, stderrWriter, name, code);

  void proc.status.then((status) => {
    console.log(`${linePrefix(name, code)}exited with code ${status.code}`);
    void shutdown();
  });

  return { name, proc };
}

// A WritableStream permits only one active writer, so every child's stdout/
// stderr pipe shares one long-lived writer per std stream. Acquiring a second
// writer on Deno.stdout/stderr (e.g. once per child) throws "The stream is
// already locked". The writers are never released; they live for the whole
// dev session.
const stdoutWriter = Deno.stdout.writable.getWriter();
const stderrWriter = Deno.stderr.writable.getWriter();

async function pipe(
  source: ReadableStream<Uint8Array>,
  writer: WritableStreamDefaultWriter<Uint8Array>,
  name: string,
  code: string,
): Promise<void> {
  const decoder = new TextDecoder();
  const encoder = new TextEncoder();
  // Collapse a line to the content after its last carriage return: progress
  // redraws (cargo's "\r    Finished …") would otherwise survive and let the
  // terminal overwrite our badge. The settled tail is what a TTY would show.
  const settle = (s: string) => (s.includes("\r") ? s.slice(s.lastIndexOf("\r") + 1) : s);
  let buf = "";
  try {
    for await (const chunk of source) {
      buf += decoder.decode(chunk, { stream: true });
      const lines = buf.split("\n");
      buf = lines.pop() ?? "";
      for (const line of lines) {
        const out = formatChildLine(settle(line));
        if (out === null) continue;
        await writer.write(encoder.encode(`${linePrefix(name, code)}${out}\n`));
      }
    }
    if (buf.length > 0) {
      const out = formatChildLine(settle(buf));
      if (out !== null) {
        await writer.write(encoder.encode(`${linePrefix(name, code)}${out}\n`));
      }
    }
  } catch {
    // Stream ended; ignore.
  }
}

function devLog(msg: string): void {
  console.log(`${linePrefix("dev", "33")}${msg}`);
}

// Generate (once) and return the dev core's admin token. The from-source core
// never writes ~/.tomat/dev/core/.admin-token itself, so without this the
// client could not mint a pairing code against it. Written only when absent,
// so an existing pairing keeps working across dev restarts.
async function ensureAdminToken(): Promise<string> {
  await Deno.mkdir(DEV_CORE_DIR, { recursive: true });
  try {
    const existing = (await Deno.readTextFile(ADMIN_TOKEN_FILE)).trim();
    if (existing) return existing;
  } catch (err) {
    if (!(err instanceof Deno.errors.NotFound)) throw err;
  }
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  const token = Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
  await Deno.writeTextFile(ADMIN_TOKEN_FILE, token);
  if (Deno.build.os !== "windows") {
    try {
      await Deno.chmod(ADMIN_TOKEN_FILE, 0o600);
    } catch {
      /* best-effort */
    }
  }
  return token;
}

// Dev convenience: set a known, randomly-generated admin password on the dev
// core and return it (or null on failure). The password gates the remote flows
// (generate a pairing code, remove a device) that an already-paired client
// drives, so without one those can't be exercised in dev. The stored hash is
// argon2id (a prior plaintext can't be read back), so we overwrite each run and
// print the fresh value. Skipped in --fresh-install, which exercises the
// no-password first-boot state (the password is then set in Settings > Cores).
async function setDevAdminPassword(token: string): Promise<string | null> {
  const suffix = Array.from(crypto.getRandomValues(new Uint8Array(4)), (b) =>
    b.toString(16).padStart(2, "0"),
  ).join("");
  const password = `dev-${suffix}`; // 12 chars, clears the 8-char floor.
  try {
    const r = await fetch(`${CORE_URL}/api/v1/admin/password`, {
      method: "POST",
      headers: { "X-Admin-Token": token, "Content-Type": "application/json" },
      body: JSON.stringify({ password }),
    });
    await r.body?.cancel();
    if (!r.ok) throw new Error(`HTTP ${r.status}`);
    return password;
  } catch (err) {
    devLog(`could not set dev admin password (${err instanceof Error ? err.message : err})`);
    return null;
  }
}

// Wait for the dev core to answer, mint a pairing code, print how to connect,
// and return the code (or null on failure). Runs before the client spawns so
// the code can be forwarded as a launch argument that prefills onboarding.
// Best-effort: on any failure, print the manual command and return null.
async function mintPairingCode(token: string): Promise<string | null> {
  let up = false;
  for (let i = 0; i < 60; i++) {
    if (shuttingDown) return null;
    try {
      const r = await fetch(`${CORE_URL}/api/v1/health`);
      await r.body?.cancel();
      if (r.ok) {
        up = true;
        break;
      }
    } catch {
      /* core not bound yet */
    }
    await new Promise((res) => setTimeout(res, 250));
  }
  // -k: the dev core's TLS cert is self-signed (see CORE_URL), so curl needs
  // --insecure to reach it when run by hand.
  const manual = `  curl -sk -X POST -H "X-Admin-Token: ${token}" ${CORE_URL}/api/v1/pairing/codes`;
  if (!up) {
    devLog("core did not come up in time; mint a pairing code manually:");
    devLog(manual);
    return null;
  }
  try {
    const r = await fetch(`${CORE_URL}/api/v1/pairing/codes`, {
      method: "POST",
      headers: { "X-Admin-Token": token, "Content-Type": "application/json" },
      body: "{}",
    });
    if (!r.ok) throw new Error(`HTTP ${r.status}`);
    const data = (await r.json()) as { code?: string };
    if (!data.code) throw new Error("response had no code");
    // Set a known admin password too (non-fresh-install), so the password-gated
    // flows are testable. Printed in the box below.
    const devPassword = FRESH_INSTALL ? null : await setDevAdminPassword(token);
    const bar = "─".repeat(58);
    devLog(bar);
    if (FRESH_INSTALL) {
      devLog(`Fresh-install test: the welcome remote-connect page shows.`);
      devLog(`Pair it with this dev core by entering:`);
    } else {
      devLog(`First boot silently pairs with this dev core; to pair by hand:`);
    }
    devLog(`  URL : ${CORE_URL}`);
    devLog(`  Code: ${data.code}`);
    if (devPassword) {
      devLog(`  Admin password: ${devPassword}  (for "Generate pairing code" / removing devices)`);
    }
    devLog(bar);
    return data.code;
  } catch (err) {
    devLog(
      `could not mint a pairing code (${err instanceof Error ? err.message : err}); mint manually:`,
    );
    devLog(manual);
    return null;
  }
}

// Remove every entry directly under `dir` whose name is not in `preserve`.
// Best-effort: a missing dir is already "reset", and a single stubborn entry is
// logged without aborting the rest.
async function wipeDirExcept(dir: string, preserve: Set<string>): Promise<void> {
  let entries: Deno.DirEntry[];
  try {
    entries = await Array.fromAsync(Deno.readDir(dir));
  } catch (err) {
    if (err instanceof Deno.errors.NotFound) return;
    throw err;
  }
  for (const entry of entries) {
    if (preserve.has(entry.name)) continue;
    const target = join(dir, entry.name);
    try {
      await Deno.remove(target, { recursive: true });
      devLog(`--reset: deleted ${target}`);
    } catch (err) {
      if (err instanceof Deno.errors.NotFound) continue;
      devLog(`--reset: could not delete ${target}: ${err}`);
    }
  }
}

// Wipe the dev client's data root whole (settings, paired cores, snippets, the
// dev keychain, logs) so the next boot lands on core management with no carried
// state. Nothing here needs downloading, so nothing is preserved.
async function resetClientSettings(): Promise<void> {
  await wipeDirExcept(DEV_CLIENT_DIR, new Set());
}

// Wipe the dev core dir to a clean slate, preserving only the download/build
// artifacts in CORE_RESET_PRESERVE. Everything else (db, settings, sessions,
// memories, secrets, admin credentials, the seed markers, installed
// extensions, caches, logs) is regenerated on the next boot: the db is rebuilt
// from the current schema (editing schema.sql in place only reaches a fresh db,
// since the migration runner skips a version the db already has and CREATE TABLE
// IF NOT EXISTS never adds a column to an existing table; see
// packages/tomat-core/src/db/migrate.ts), and dropping the seed markers
// re-stages the seeded extensions against that fresh db. Must run before core
// spawns and opens the db.
async function resetCoreState(): Promise<void> {
  await wipeDirExcept(DEV_CORE_DIR, CORE_RESET_PRESERVE);
}

// Collect a pid plus every descendant (children, grandchildren, …), gathered
// up front before any are killed: once a parent dies its children reparent to
// init and can no longer be found via `pgrep -P`. Deno's proc.kill() signals
// only the direct child, so without walking the tree the Tauri CLI's vite dev
// server (port 1420) and the built app binary survive as orphans that block the
// next `deno task dev` run. Unix-only; on Windows just returns the pid itself.
async function processTree(pid: number): Promise<number[]> {
  if (Deno.build.os === "windows") return [pid];
  const tree = [pid];
  try {
    const out = await new Deno.Command("pgrep", {
      args: ["-P", String(pid)],
      stdout: "piped",
      stderr: "null",
    }).output();
    const kids = new TextDecoder()
      .decode(out.stdout)
      .split("\n")
      .map((s) => s.trim())
      .filter(Boolean)
      .map(Number);
    for (const kid of kids) tree.push(...(await processTree(kid)));
  } catch {
    // pgrep unavailable; fall back to just this pid.
  }
  return tree;
}

// pids holding a TCP listen socket on `port` (Unix-only; empty if lsof is
// missing). Used to reclaim port 1420 from a stale vite server orphaned by a
// previous dev session that didn't shut down cleanly.
async function listenersOn(port: number): Promise<number[]> {
  if (Deno.build.os === "windows") return [];
  try {
    const out = await new Deno.Command("lsof", {
      args: ["-ti", `tcp:${port}`, "-sTCP:LISTEN"],
      stdout: "piped",
      stderr: "null",
    }).output();
    return new TextDecoder()
      .decode(out.stdout)
      .split("\n")
      .map((s) => s.trim())
      .filter(Boolean)
      .map(Number);
  } catch {
    return [];
  }
}

const VITE_PORT = 1420;

// Free the Vite dev port. Called at startup (in case an earlier dev session was
// SIGKILLed and left an orphan behind) and again at shutdown (in case this
// session's Vite server outlived the descendant-tree sweep).
async function reclaimVitePort(): Promise<void> {
  for (const pid of await listenersOn(VITE_PORT)) {
    devLog(`port ${VITE_PORT} held by stale pid ${pid}; terminating it`);
    try {
      Deno.kill(pid, "SIGKILL");
    } catch {
      // Already gone.
    }
  }
}

const VITE_CACHE_DIRS = [
  join(ROOT, "packages", "tomat-client", ".vite"),
  join(ROOT, "packages", "tomat-client", "node_modules", ".vite"),
];

// A `deps_temp_*` dir inside a Vite cache marks a dependency optimization that
// was interrupted (Vite hard-killed mid-flight). Left in place it crashes the
// next startup deep in rolldown ("Failed to unwrap exclusive reference of
// `BindingBundler`"). A healthy cache holds only a `deps` dir, so we wipe the
// cache only when that corruption marker is present, never slowing a clean
// start. Vite re-optimizes from scratch on the next run (~0.5 s).
async function clearStaleViteCache(): Promise<void> {
  for (const dir of VITE_CACHE_DIRS) {
    let interrupted = false;
    try {
      for await (const entry of Deno.readDir(dir)) {
        if (entry.isDirectory && entry.name.startsWith("deps_temp_")) {
          interrupted = true;
          break;
        }
      }
    } catch {
      continue; // cache dir absent
    }
    if (interrupted) {
      devLog(`clearing interrupted Vite cache at ${dir}`);
      try {
        await Deno.remove(dir, { recursive: true });
      } catch (err) {
        devLog(`could not clear ${dir}: ${err}`);
      }
    }
  }
}

// The android NDK cross-compile (cargo/rustc/clang) runs as a child of the
// persistent Gradle daemon, which double-forks into its own session and so sits
// OUTSIDE this process's descendant tree: `pgrep -P` can't reach it, and a Ctrl+C
// would otherwise leave it grinding in the background. Match it by the NDK target
// triples that appear in its command line and reap it. (The Gradle daemon itself
// and the adb server are persistent by design and intentionally left running;
// re-spawning the daemon's warm JVM on every run would slow rebuilds.)
const ANDROID_TARGET_TRIPLES = [
  "aarch64-linux-android",
  "armv7-linux-androideabi",
  "i686-linux-android",
  "x86_64-linux-android",
];

// The iOS analogue: `tauri ios dev` drives xcodebuild, which runs the cargo/rustc/
// clang cross-compile for the apple-ios triples in its own process group, outside
// this orchestrator's descendant tree. Match those by triple and reap them on
// Ctrl+C. The Simulator app and CoreSimulator services are persistent by design
// (like adb + the Gradle daemon) and are intentionally left running.
const IOS_TARGET_TRIPLES = ["aarch64-apple-ios", "aarch64-apple-ios-sim", "x86_64-apple-ios"];

// SIGKILL every process whose command line still references one of these NDK/SDK
// target triples: the detached mobile cross-compile jobs a descendant-tree walk
// (pgrep -P) can't reach. Shared by the android + ios shutdown sweeps.
async function sweepBuildTriples(triples: string[]): Promise<void> {
  if (Deno.build.os === "windows") return;
  for (const triple of triples) {
    let pids: number[] = [];
    try {
      const out = await new Deno.Command("pgrep", {
        args: ["-f", triple],
        stdout: "piped",
        stderr: "null",
      }).output();
      pids = new TextDecoder()
        .decode(out.stdout)
        .split("\n")
        .map((s) => s.trim())
        .filter(Boolean)
        .map(Number);
    } catch {
      continue; // pgrep unavailable
    }
    for (const pid of pids) {
      if (pid === Deno.pid) continue; // never signal the orchestrator itself
      try {
        Deno.kill(pid, "SIGKILL");
      } catch {
        // Gone.
      }
    }
  }
}

let shuttingDown = false;
async function shutdown(): Promise<void> {
  if (shuttingDown) return;
  shuttingDown = true;
  // Hard failsafe: whatever happens (a wedged sweep, a process that ignores
  // every signal), the orchestrator exits. The clean path below exits first.
  setTimeout(() => Deno.exit(0), 6000);
  // Gather every child's full descendant tree first, then signal them all, so
  // tauri's vite server + app binary die with the session instead of leaking.
  // (Reparented children keep their pid, so SIGKILLing this captured set still
  // reaches them even after their parent has exited.)
  const pids = new Set<number>();
  for (const { proc } of children) {
    for (const pid of await processTree(proc.pid)) pids.add(pid);
  }
  for (const pid of pids) {
    try {
      Deno.kill(pid, "SIGTERM");
    } catch {
      // Already exited.
    }
  }
  // Anything still alive after a 3 s grace period gets SIGKILL; then sweep the
  // leftovers a descendant-tree walk can't reach and exit.
  setTimeout(() => {
    for (const pid of pids) {
      try {
        Deno.kill(pid, "SIGKILL");
      } catch {
        // Gone.
      }
    }
    void finishShutdown();
  }, 3000);
}

// Final cleanup after the tree is killed: free the Vite port in case its server
// outlived the tree (orphaned past pgrep -P reach), and on mobile reap the
// detached Gradle-daemon / xcodebuild cross-compile. Best-effort, then exit
// unconditionally so a hung sweep can never wedge the shutdown.
async function finishShutdown(): Promise<void> {
  try {
    await reclaimVitePort();
  } catch {
    // best-effort
  }
  if (ANDROID) {
    try {
      await sweepBuildTriples(ANDROID_TARGET_TRIPLES);
    } catch {
      // best-effort
    }
  }
  if (IOS) {
    try {
      await sweepBuildTriples(IOS_TARGET_TRIPLES);
    } catch {
      // best-effort
    }
  }
  Deno.exit(0);
}

for (const sig of ["SIGINT", "SIGTERM"] as const) {
  try {
    Deno.addSignalListener(sig, shutdown);
  } catch {
    // SIGTERM unsupported on Windows; ignore.
  }
}

// Build the native helper binaries from source and link them into the dev
// core's bin dir. Production downloads these via the signed manifest at install
// time; dev has no such download, so without this they are absent and core
// fails its boot-time helper check (see ensureHelperBinaries in main.ts). The
// crate name is also the on-disk base name; core looks them up channel-suffixed
// ("-dev"). All four build + run on every platform (ptyhost uses a ConPTY on
// Windows, a PTY on unix).
const HELPER_CRATES = [
  "tomat-core-keychain",
  "tomat-core-updater",
  "tomat-core-hwinfo",
  "tomat-core-ptyhost",
];

async function provisionHelpers(): Promise<void> {
  const exe = Deno.build.os === "windows" ? ".exe" : "";
  const crates = HELPER_CRATES;
  try {
    const out = await new Deno.Command("cargo", {
      args: ["build", ...crates.flatMap((c) => ["-p", c])],
      stdout: "piped",
      stderr: "piped",
    }).output();
    if (!out.success) {
      const tail = new TextDecoder().decode(out.stderr).trim().split("\n").at(-1) ?? "";
      devLog(`helper build failed (${tail}); core will refuse to boot until it succeeds`);
      return;
    }
    await ensureDir(join(DEV_CORE_DIR, "bin"));
    for (const crate of crates) {
      const dest = join(DEV_CORE_DIR, "bin", `${crate}-dev${exe}`);
      const built = join(ROOT, "target", "debug", `${crate}${exe}`);
      try {
        await Deno.remove(dest);
      } catch {
        /* not present yet */
      }
      // Symlink on unix so a later `cargo build` is picked up with no relink;
      // copy on Windows where symlinks need elevation.
      if (Deno.build.os === "windows") await Deno.copyFile(built, dest);
      else await Deno.symlink(built, dest);
    }
    devLog(`linked ${crates.length} helper binaries into the dev bin dir`);
  } catch (err) {
    devLog(`could not provision helper binaries (${err instanceof Error ? err.message : err})`);
  }
}

// The Kokoro phonemizer data. Production bakes it into the speech binary's
// archive (consent-gated with the binary download); dev fetches the standalone
// bundle once. bzip2, so shell out to system tar - Deno has gzip but not bzip2.
const ESPEAK_DATA_URL =
  "https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/espeak-ng-data.tar.bz2";

// Build + stage the tomat-core-speech sidecar (Whisper STT + Kokoro TTS) into
// the dev bin dir. Production downloads it from the signed binaries manifest and
// extracts espeak-ng-data alongside; dev has no such download (it is self-hosted,
// so devManifest skips it). Unlike the small helper crates, this one statically
// links the sherpa-onnx ONNX runtime (a heavy build), so we build it ONCE rather
// than on every dev start - on unix the symlink follows a later manual
// `cargo build -p tomat-core-speech`; delete the staged binary to force a rebuild
// here. Sidecar binaries are NOT channel-suffixed (binaryName() = the bare name).
async function provisionSpeechSidecar(): Promise<void> {
  const exe = Deno.build.os === "windows" ? ".exe" : "";
  const binDir = join(DEV_CORE_DIR, "bin");
  const dest = join(binDir, `tomat-core-speech${exe}`);
  try {
    let staged = false;
    try {
      // Deno.stat follows the symlink, so a dangling link (its target removed by
      // a `cargo clean` or an interleaved mobile cross-build) reads as NOT staged
      // and forces a rebuild - lstat would see the dead link and wrongly skip it,
      // leaving the dev core with a missing binary the speech manifest can't
      // resolve (STT/TTS then show a stuck "tomat-core-speech" pending download).
      await Deno.stat(dest);
      staged = true;
    } catch {
      /* not staged (or a dangling symlink) */
    }
    if (!staged) {
      const out = await new Deno.Command("cargo", {
        args: ["build", "-p", "tomat-core-speech"],
        stdout: "piped",
        stderr: "piped",
      }).output();
      if (!out.success) {
        const tail = new TextDecoder().decode(out.stderr).trim().split("\n").at(-1) ?? "";
        devLog(`speech sidecar build failed (${tail}); STT/TTS will stay Disabled in dev`);
        return;
      }
      await ensureDir(binDir);
      const built = join(ROOT, "target", "debug", `tomat-core-speech${exe}`);
      // Clear a stale (possibly dangling) link first: symlink/copy fail if dest
      // already exists, which it does when we're re-staging over a dead link.
      await Deno.remove(dest).catch(() => {});
      if (Deno.build.os === "windows") await Deno.copyFile(built, dest);
      else await Deno.symlink(built, dest);
      devLog("built + linked tomat-core-speech sidecar into the dev bin dir");
    }
    await ensureEspeakData(join(binDir, "lib", "tomat-core-speech"));
  } catch (err) {
    devLog(`could not provision speech sidecar (${err instanceof Error ? err.message : err})`);
  }
}

// Fetch + unpack espeak-ng-data into <libDir>/espeak-ng-data once, mirroring
// where the binaries manager extracts it in production.
async function ensureEspeakData(libDir: string): Promise<void> {
  const dataDir = join(libDir, "espeak-ng-data");
  try {
    if ((await Deno.stat(dataDir)).isDirectory) return; // already staged
  } catch {
    /* not staged yet */
  }
  await ensureDir(libDir);
  const tmp = join(libDir, "espeak-ng-data.tar.bz2");
  const res = await fetch(ESPEAK_DATA_URL);
  if (!res.ok) throw new Error(`espeak-ng-data fetch HTTP ${res.status}`);
  await Deno.writeFile(tmp, new Uint8Array(await res.arrayBuffer()));
  const out = await new Deno.Command("tar", {
    args: ["-xjf", tmp, "-C", libDir],
    stdout: "piped",
    stderr: "piped",
  }).output();
  await Deno.remove(tmp).catch(() => {});
  if (!out.success) throw new Error("tar -xjf espeak-ng-data failed");
  devLog("fetched espeak-ng-data for the dev speech sidecar");
}

// Core-side setup + the core process itself are skipped under --client-only,
// where a core is assumed to be running separately.
let adminToken = "";
if (!CLIENT_ONLY) {
  // --reset: wipe the core dir to a clean slate FIRST, before any setup below
  // writes into it (the admin token, the built-in manifest) or core boots and
  // opens the db. Running it last would delete those just-written files.
  if (RESET) await resetCoreState();

  adminToken = await ensureAdminToken();

  // Generate the dev built-in extension manifest before core boots so first-boot
  // seeding can resolve a version, and keep it fresh as the codebase extension is
  // edited.
  await startDevExtensionManifest();

  // Link the native helper binaries into the dev bin dir before core boots, so
  // its boot-time helper check passes and the first tool call can spawn in
  // prompt-capable mode.
  await provisionHelpers();
  await provisionSpeechSidecar();

  children.push(
    spawn(
      "core",
      "36",
      ["deno", "run", "--watch", "--allow-all", "packages/tomat-core/src/main.ts"],
      undefined,
      // Worker .ts files live in the source tree during dev so edits hot-
      // reload via --watch. Without this override, paths().workersDir would
      // point at ~/.tomat/dev/core/workers (an empty dir in dev).
      {
        ...CHANNEL_ENV,
        // For mobile dev the emulator/simulator/device reaches the host core
        // over the network, so bind every interface (loopback-only would be
        // unreachable). The self-signed cert is still pinned at pairing, so this
        // only widens reachability, not trust.
        ...(MOBILE ? { TOMAT_CORE_HOST: "0.0.0.0" } : {}),
        TOMAT_WORKERS_DIR: `${ROOT}packages/tomat-core/src/workers`,
        // Core runs under `deno run --watch`. On a file change the watcher sends
        // SIGTERM and waits for the program to wind down so it can re-run the
        // module in-process. This flag tells core's SIGTERM handler to skip its
        // Deno.exit(0) (which would hard-kill the watcher and end the dev session)
        // and instead let the event loop drain for the restart. See main.ts.
        TOMAT_DEV_WATCH: "1",
        // dev.ts owns the timestamp + badge column, so core emits bare console
        // lines (no ISO timestamp). Its stderr is piped here (not a TTY), so force
        // color on so level coloring survives.
        TOMAT_LOG_NO_TIME: "1",
        ...(useColor ? { TOMAT_LOG_COLOR: "1" } : {}),
      },
    ),
  );
}

await reclaimVitePort();
await clearStaleViteCache();

// Bake the dev channel for channel.rs (via the Tauri crate's build.rs). Desktop
// picks up TOMAT_CHANNEL at runtime, but a mobile build runs cargo inside
// Xcode's script phase / the Gradle daemon, neither of which forwards the env to
// the compile - so the on-disk `channel` file is the deterministic source. Write
// it before the client (and its cargo build) spawns.
await Deno.writeTextFile(join(ROOT, "packages/tomat-client/src/tauri/channel"), "dev");

// Mint the pairing code before spawning the client so it can be forwarded as a
// launch argument. The core boots in parallel during the health poll inside.
// Skipped under --client-only (no core here to mint against).
const pairingCode = CLIENT_ONLY ? null : await mintPairingCode(adminToken);

// --reset: clear the client's paired-cores state so it boots into onboarding.
if (RESET) await resetClientSettings();

// Resolve a concrete iOS simulator NAME for `tauri ios dev` so it never drops
// into its interactive device picker, which spins forever under this script's
// piped (non-TTY) stdin ("...you need to pick a device"). Tauri's [DEVICE]
// positional matches by device name (not UDID). Prefers TAURI_IOS_DEVICE, then
// any already-booted simulator (the developer's current choice), then boots the
// first available iPhone and surfaces Simulator.app. Returns null only when
// simctl finds nothing. (Android needs no equivalent: `tauri android dev`
// auto-targets the running emulator.)
async function resolveIosSimulator(): Promise<string | null> {
  const override = Deno.env.get("TAURI_IOS_DEVICE");
  if (override) return override;

  type Sim = { udid: string; name: string; state: string };
  const simctlDevices = async (filter: string): Promise<Sim[]> => {
    try {
      const out = await new Deno.Command("xcrun", {
        args: ["simctl", "list", "devices", filter, "-j"],
        stdout: "piped",
        stderr: "null",
      }).output();
      const byRuntime = JSON.parse(new TextDecoder().decode(out.stdout)).devices ?? {};
      return Object.values(byRuntime).flat() as Sim[];
    } catch {
      return []; // simctl unavailable (not a mac / no Xcode)
    }
  };

  const booted = (await simctlDevices("booted")).find((d) => d.state === "Booted");
  if (booted) return booted.name;

  const iphone = (await simctlDevices("available")).find((d) => d.name.includes("iPhone"));
  if (!iphone) return null;
  await new Deno.Command("xcrun", {
    args: ["simctl", "boot", iphone.udid],
    stdout: "null",
    stderr: "null",
  })
    .output()
    .catch(() => {});
  await new Deno.Command("open", { args: ["-a", "Simulator"], stdout: "null", stderr: "null" })
    .output()
    .catch(() => {});
  return iphone.name;
}

// The onboarding prefill reaches the client differently per platform. Desktop
// forwards it as launch argv (read by the read_launch_prefill command); mobile
// has no argv path, so it is passed through Vite env vars that mobile.ts's
// launchPrefill reads at runtime. All connect to this same dev core.
const clientPrefillEnv: Record<string, string> = {};
const clientCmd = ["deno", "run", "-A", "npm:@tauri-apps/cli@^2"];
if (MOBILE) {
  clientCmd.push(ANDROID ? "android" : "ios", "dev");
  // Pass a concrete simulator so `tauri ios dev` stays non-interactive (see
  // resolveIosSimulator); without it the device picker spins under piped stdin.
  if (IOS) {
    const device = await resolveIosSimulator();
    if (device) clientCmd.push(device);
    else devLog("no iOS simulator found; boot one in Xcode or set TAURI_IOS_DEVICE");
  }
  // --client-only has no dev core here to point at, so it skips the address
  // prefill and lets mobile.ts derive it from the dev-server host at runtime.
  if (!CLIENT_ONLY) {
    // The mobile target reaches the host core over the network, not loopback: a
    // physical device uses TAURI_DEV_HOST (also the HMR host). Without it, the
    // Android emulator uses its host-loopback alias 10.0.2.2, while the iOS
    // Simulator shares the host network so plain 127.0.0.1 reaches it. Port = the
    // dev channel core port (see CORE_URL).
    const devHost = Deno.env.get("TAURI_DEV_HOST") ?? (ANDROID ? "10.0.2.2" : "127.0.0.1");
    clientPrefillEnv.VITE_DEV_CORE_URL = `https://${devHost}:7820`;
    if (pairingCode) clientPrefillEnv.VITE_DEV_PAIRING_CODE = pairingCode;
  }
} else {
  // `tauri dev` treats args after the first `--` as runner (cargo) args and args
  // after a SECOND `--` as app args, so the prefill flags go after `-- --` to
  // reach the binary's argv. Skipped under --fresh-install, where the welcome
  // remote-connect page should start blank.
  clientCmd.push("dev");
  if (!FRESH_INSTALL) {
    const prefill = [`--core-url=${CORE_URL}`];
    if (pairingCode) prefill.push(`--pairing-code=${pairingCode}`);
    clientCmd.push("--", "--", ...prefill);
  }
}

// The core may have exited during the health poll (which triggers shutdown);
// don't spawn the client into a tearing-down session.
if (!shuttingDown) {
  // The Tauri CLI builds vite + cargo before the app binary runs and emits its
  // first "Tomat Client starting" line, and that build chatter is filtered out
  // here. Emit a synthetic client line up front (mirroring the client's fern
  // format) so the console shows the client is building; it stays the latest
  // client message until the binary starts (or a build error leaks through
  // pipe()). Written directly like devLog, so it bypasses formatChildLine.
  console.log(`${linePrefix("client", "35")}${levelLine("info", "boot", "Tomat Client building")}`);
  children.push(
    spawn("client", "35", clientCmd, `${ROOT}packages/tomat-client`, {
      ...CHANNEL_ENV,
      // dev.ts owns the timestamp + badge column and the client's stdout is
      // piped here (not a TTY), so the client emits bare lines and forces color
      // on, exactly like the core spawn above.
      TOMAT_LOG_NO_TIME: "1",
      ...(useColor ? { TOMAT_LOG_COLOR: "1" } : {}),
      ...(FRESH_INSTALL ? { TOMAT_DEV_FRESH_INSTALL: "1" } : {}),
      ...clientPrefillEnv,
    }),
  );
}

await Promise.all(children.map((c) => c.proc.status));
