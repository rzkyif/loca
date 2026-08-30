// Boots loca-core (deno watch) and loca-client (tauri dev) together on the
// "dev" install channel, so all state lives under ~/.loca/dev/ and never
// touches a stable install. Seeds a dev admin token (the from-source core
// never mints one; only the installer does), sets a fresh dev admin password
// each run (for the password-gated remote flows), and prints a ready-to-use
// pairing code so connecting the client is a one-paste step.
// Stops both children on SIGINT/SIGTERM or when either exits.

import { fromFileUrl, join } from "@std/path";
import { ensureDir } from "@std/fs/ensure-dir";

const ROOT = fromFileUrl(new URL("..", import.meta.url));

// Dev runs on the "dev" install channel (~/.loca/dev/{core,client}); both
// children inherit LOCA_CHANNEL so they agree on the isolated location and
// keychain namespace.
const CHANNEL_ENV = { LOCA_CHANNEL: "dev" };
// The dev core binds the dev-channel port (stable base 7800 + dev offset 20) and
// serves TLS with a self-signed cert, which deno.json's dev task trusts for
// loopback via --unsafely-ignore-certificate-errors=127.0.0.1.
const CORE_URL = "https://127.0.0.1:7820";

function homeDir(): string {
  const home = Deno.env.get("HOME") ?? Deno.env.get("USERPROFILE");
  if (!home) {
    throw new Error("could not determine home directory (no HOME or USERPROFILE)");
  }
  return home;
}

const DEV_CORE_DIR = join(homeDir(), ".loca", "dev", "core");
const ADMIN_TOKEN_FILE = join(DEV_CORE_DIR, ".admin-token");

// The dev client's data root. --reset wipes it whole: cores.json (whose absence
// sends the next boot back through core management), settings.json, snippets/,
// keychain.json, and logs. None of it needs downloading, so nothing is kept.
const DEV_CLIENT_DIR = join(homeDir(), ".loca", "dev", "client");

// --reset is a denylist, not an allowlist: it deletes EVERYTHING under the dev
// core dir EXCEPT these names, so no new kind of state can silently survive.
// The two preserved entries hold no user state, only expensive-to-reacquire
// downloads: `bin/` (the sidecar binaries plus deno) and `deno-cache/`. A future
// download-only cache under the core dir belongs in this set. ~/.loca/models
// lives outside the channel root and is never touched.
const CORE_RESET_PRESERVE = new Set(["bin", "deno-cache"]);

// CLI flags, wired to the deno tasks in deno.json:
//  --reset          wipe the dev client and core state so the client boots into
//                   core management on a fresh schema with the seeded
//                   extensions re-staged.
//  --fresh-install  make the client report no local core, so first boot shows
//                   the welcome remote-connect page instead of silently pairing.
//                   Sets LOCA_DEV_FRESH_INSTALL and skips the remote prefill.
const RESET = Deno.args.includes("--reset");
const FRESH_INSTALL = Deno.args.includes("--fresh-install");
// --android / --ios run the mobile client against this dev core. The WebView
// runs on an emulator/simulator/device, so the core must be reachable
// off-loopback (LOCA_CORE_HOST=0.0.0.0 below) and the onboarding prefill goes
// through Vite env (mobile has no launch argv).
const ANDROID = Deno.args.includes("--android");
const IOS = Deno.args.includes("--ios");
const MOBILE = ANDROID || IOS;
// --client-only spawns just the client (no dev core, no helper/sidecar setup),
// so the mobile client-only loop still gets this script's clean teardown instead
// of leaking on Ctrl+C. A core is assumed to be running separately.
const CLIENT_ONLY = Deno.args.includes("--client-only");

// Child labels ("core", "client", "dev") are padded to this width so the message
// columns line up.
const LABEL_WIDTH = 6;

// Color only on a real terminal with NO_COLOR unset, so piped output is clean.
const useColor = !Deno.env.get("NO_COLOR") && Deno.stdout.isTerminal();

function color(code: string, s: string): string {
  return useColor ? `\x1b[${code}m${s}\x1b[0m` : s;
}

// Compact wall-clock HH:MM:SS.mmm. dev.ts is the single timestamp authority for
// the multiplexed console; children emit bare messages (core skips its own via
// LOCA_LOG_NO_TIME).
function stamp(): string {
  const d = new Date();
  const p = (n: number, w = 2) => String(n).padStart(w, "0");
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}.${p(
    d.getMilliseconds(),
    3,
  )}`;
}

// `<time> <badge> ` prefix, built per line so the stamp marks its arrival.
function linePrefix(name: string, code: string): string {
  return `${color("2", stamp())} ${color(code, name.padEnd(LABEL_WIDTH))} `;
}

// Strip ANSI so the matchers below see plain text. Matches the full CSI family,
// not just SGR (`m`): cargo prefixes "Finished" with a clear-line `\x1b[2K`, so
// an `m`-only strip would leave a leading ESC and miss the match.
// oxlint-disable-next-line no-control-regex
const ANSI_RE = /\x1b\[[0-9;?]*[A-Za-z]/g;

// Our own logs start every line with a lowercase, padded level word. That
// positive signal is how a line is recognized as ours and passed through.
const LEVEL_RE = /^(?:error|warn|info|debug|trace)\s/;

// The tauri-cli / cargo / deno-task chatter sharing the child's stream. Dropped
// outright; build errors match none of these and still surface as leaks.
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

// Level SGR codes, matching core's @std/log and the client's fern coloring.
const LEVEL_CODE = { debug: "2", info: "32", warn: "33", error: "31" } as const;

// A dev.ts-synthesized line in the children's own `<level> <scope> <message>`
// shape, so it groups with them visually.
function levelLine(level: keyof typeof LEVEL_CODE, scope: string, message: string): string {
  return `${color(LEVEL_CODE[level], level.padEnd(5))} ${color("2", scope)} ${message}`;
}

// Mark (rather than hide) a line that bypassed our logging format: BUILD_NOISE
// is a denylist, so new leaks are inevitable and must stay visible.
function leakLine(content: string): string {
  return `${color("2", "leak".padEnd(5))} ${content}`;
}

// Present a raw child line: our own logs verbatim, build noise dropped (null),
// watcher lines as leveled logs, everything else as a leak.
function formatChildLine(raw: string): string | null {
  const plain = raw.replace(ANSI_RE, "").trimStart();
  if (plain.trim() === "") return null; // blank spacing from build tools
  if (BUILD_NOISE.some((re) => re.test(plain))) return null;
  if (LEVEL_RE.test(plain)) return raw; // our log: keep it (and its colors) verbatim
  // Deno's `--watch` supervisor emits a small, expected family of lines on
  // core's stream; render them as `watcher`-scoped logs rather than leaks, the
  // reload event as info and the rest as debug chatter.
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

// A WritableStream permits only one active writer, so every child's pipes share
// one long-lived writer per std stream; a second would throw "already locked".
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

// Generate (once) and return the dev core's admin token: the from-source core
// never writes .admin-token itself, so without this the client could not mint a
// pairing code. Written only when absent, so pairings survive dev restarts.
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

// Set a known, random admin password on the dev core and return it (null on
// failure). It gates the remote flows an already-paired client drives. The
// stored hash is argon2id, so a prior plaintext can't be read back: overwrite
// each run and print the fresh value. Skipped in --fresh-install, which
// exercises the no-password first-boot state.
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

// Wait for the dev core, mint a pairing code, print how to connect, and return
// the code. Runs before the client spawns so the code can prefill onboarding.
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

// Wipe the dev core dir to a clean slate, preserving only CORE_RESET_PRESERVE.
// Everything else is regenerated on the next boot: the db is rebuilt from the
// current schema (an in-place schema.sql edit only reaches a fresh db), and
// dropping the seed markers re-stages the seeded extensions against it. Must run
// before core spawns and opens the db.
async function resetCoreState(): Promise<void> {
  await wipeDirExcept(DEV_CORE_DIR, CORE_RESET_PRESERVE);
}

// Run a pid-listing command (Unix-only), returning the pids it printed. An
// absent or failing tool yields no pids rather than throwing.
async function pidsFrom(cmd: string, args: string[]): Promise<number[]> {
  if (Deno.build.os === "windows") return [];
  try {
    const out = await new Deno.Command(cmd, { args, stdout: "piped", stderr: "null" }).output();
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

// A pid plus every descendant, gathered BEFORE any are killed: once a parent
// dies its children reparent to init and `pgrep -P` can no longer find them.
// proc.kill() signals only the direct child, so without this the Tauri CLI's
// vite server and the built app binary survive as orphans that block the next
// run. On Windows, just the pid itself.
async function processTree(pid: number): Promise<number[]> {
  if (Deno.build.os === "windows") return [pid];
  const tree = [pid];
  for (const kid of await pidsFrom("pgrep", ["-P", String(pid)])) {
    tree.push(...(await processTree(kid)));
  }
  return tree;
}

// pids holding a TCP listen socket on `port`, used to reclaim the Vite port from
// a server orphaned by a previous session that didn't shut down cleanly.
function listenersOn(port: number): Promise<number[]> {
  return pidsFrom("lsof", ["-ti", `tcp:${port}`, "-sTCP:LISTEN"]);
}

const VITE_PORT = 1420;

// Free the Vite dev port, at startup (an earlier session left an orphan) and
// again at shutdown (this session's server outlived the tree sweep).
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
  join(ROOT, "packages", "loca-client", ".vite"),
  join(ROOT, "packages", "loca-client", "node_modules", ".vite"),
];

// A `deps_temp_*` dir marks a dependency optimization Vite was hard-killed
// mid-flight; left in place it crashes the next startup deep in rolldown
// ("Failed to unwrap exclusive reference of `BindingBundler`"). Wiping only on
// that marker never slows a clean start, and Vite re-optimizes in ~0.5s.
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

// The android NDK cross-compile runs under the persistent Gradle daemon, which
// double-forks into its own session and so sits OUTSIDE this process's
// descendant tree: `pgrep -P` can't reach it, and Ctrl+C would leave it
// grinding. Match it by NDK target triple and reap it. The Gradle daemon and adb
// server are persistent by design and left running.
const ANDROID_TARGET_TRIPLES = [
  "aarch64-linux-android",
  "armv7-linux-androideabi",
  "i686-linux-android",
  "x86_64-linux-android",
];

// The iOS analogue: xcodebuild runs the apple-ios cross-compile in its own
// process group, outside this orchestrator's descendant tree. Match by triple
// and reap on Ctrl+C; Simulator.app and CoreSimulator are left running.
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
// outlived the sweep, and on mobile reap the detached cross-compile.
// Best-effort, then exit unconditionally so a hung sweep cannot wedge shutdown.
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

// Build the native helper binaries from source and link them into the dev core's
// bin dir. Production downloads these via the signed manifest at install time;
// dev has no such download, so without this core fails its boot-time helper
// check. The crate name is also the on-disk base name, channel-suffixed
// ("-dev"). Only the updater is a separate crate, because a process cannot
// replace the executable it is running from on Windows.
const HELPER_CRATES = ["loca-core-updater"];

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

/// Build the Rust core for dev and return the path to the binary.
///
/// Debug rather than release, so an edit-rebuild cycle is bearable; both native
/// features on, so dev is shaped like production. The first build compiles the
/// vendored llama.cpp tree and is slow; later ones are incremental. There is no
/// `--watch`, so a core change needs a dev restart.
async function buildDevCore(): Promise<string> {
  // `vendor/` is gitignored, so a fresh clone has no llama.cpp and `build.rs`
  // fails in a way that reads as a compiler error. Idempotent and near-instant
  // once the tree is at the pin.
  const vendored = await new Deno.Command(Deno.execPath(), {
    args: ["run", "-A", join(ROOT, "scripts", "vendor-llama.ts")],
    stdout: "piped",
    stderr: "piped",
  }).output();
  if (!vendored.success) {
    const tail = new TextDecoder().decode(vendored.stderr).trim().split("\n").at(-1) ?? "";
    devLog(`could not vendor llama.cpp for the dev core: ${tail}`);
    Deno.exit(1);
  }

  devLog("building the Rust core (local-inference + local-speech)");
  const out = await new Deno.Command("cargo", {
    args: [
      "build",
      "--manifest-path",
      join(ROOT, "packages", "loca-core-rs", "Cargo.toml"),
      "--bin",
      "loca-core",
      "--features",
      "local-inference,local-speech",
    ],
    stdout: "inherit",
    stderr: "inherit",
  }).output();
  if (!out.success) {
    devLog("the dev core did not build");
    Deno.exit(1);
  }

  const exe = Deno.build.os === "windows" ? ".exe" : "";
  return join(ROOT, "target", "debug", `loca-core${exe}`);
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

  // Link the native helper binaries into the dev bin dir before core boots, so
  // its boot-time helper check passes and the first tool call can spawn in
  // prompt-capable mode.
  await provisionHelpers();

  // Build the Rust core with both native features, so what a developer runs is
  // shaped like what ships.
  const coreBin = await buildDevCore();

  children.push(
    spawn(
      "core",
      "36",
      [coreBin],
      undefined,
      // Worker .ts files live in the source tree during dev so edits hot-
      // reload via --watch. Without this override, paths().workersDir would
      // point at ~/.loca/dev/core/workers (an empty dir in dev).
      {
        ...CHANNEL_ENV,
        // For mobile dev the emulator/simulator/device reaches the host core
        // over the network, so bind every interface (loopback-only would be
        // unreachable). The self-signed cert is still pinned at pairing, so this
        // only widens reachability, not trust.
        ...(MOBILE ? { LOCA_CORE_HOST: "0.0.0.0" } : {}),
        // The tool worker is TypeScript run under the pinned `deno`; point at
        // the source tree so edits take effect on the next tool call.
        LOCA_WORKERS_DIR: `${ROOT}packages/loca-core-rs/workers`,
        // dev.ts owns the timestamp + badge column, so core emits bare console
        // lines (no ISO timestamp). Its stderr is piped here (not a TTY), so force
        // color on so level coloring survives.
        LOCA_LOG_NO_TIME: "1",
        ...(useColor ? { LOCA_LOG_COLOR: "1" } : {}),
      },
    ),
  );
}

await reclaimVitePort();
await clearStaleViteCache();

// Bake the dev channel for channel.rs. Desktop reads LOCA_CHANNEL at runtime,
// but a mobile build runs cargo inside Xcode's script phase / the Gradle daemon,
// neither of which forwards the env to the compile, so the on-disk `channel`
// file is the deterministic source. Write it before the client spawns.
await Deno.writeTextFile(join(ROOT, "packages/loca-client/src/tauri/channel"), "dev");

// Mint the pairing code before spawning the client so it can be forwarded as a
// launch argument. The core boots in parallel during the health poll inside.
// Skipped under --client-only (no core here to mint against).
const pairingCode = CLIENT_ONLY ? null : await mintPairingCode(adminToken);

// --reset: clear the client's paired-cores state so it boots into onboarding.
if (RESET) await resetClientSettings();

// Resolve a concrete iOS simulator NAME (Tauri's [DEVICE] positional matches by
// name, not UDID) so `tauri ios dev` never drops into its interactive picker,
// which spins forever under this script's piped stdin. Prefers
// TAURI_IOS_DEVICE, then an already-booted simulator, then boots the first
// available iPhone. Null only when simctl finds nothing. Android needs no
// equivalent: `tauri android dev` auto-targets the running emulator.
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

// The onboarding prefill reaches the client differently per platform: desktop as
// launch argv (read_launch_prefill), mobile through Vite env vars that
// mobile.ts's launchPrefill reads at runtime.
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
  // first "Loca Client starting" line, and that build chatter is filtered out
  // here. Emit a synthetic client line up front (mirroring the client's fern
  // format) so the console shows the client is building; it stays the latest
  // client message until the binary starts (or a build error leaks through
  // pipe()). Written directly like devLog, so it bypasses formatChildLine.
  console.log(`${linePrefix("client", "35")}${levelLine("info", "boot", "Loca Client building")}`);
  children.push(
    spawn("client", "35", clientCmd, `${ROOT}packages/loca-client`, {
      ...CHANNEL_ENV,
      // dev.ts owns the timestamp + badge column and the client's stdout is
      // piped here (not a TTY), so the client emits bare lines and forces color
      // on, exactly like the core spawn above.
      LOCA_LOG_NO_TIME: "1",
      ...(useColor ? { LOCA_LOG_COLOR: "1" } : {}),
      ...(FRESH_INSTALL ? { LOCA_DEV_FRESH_INSTALL: "1" } : {}),
      ...clientPrefillEnv,
    }),
  );
}

await Promise.all(children.map((c) => c.proc.status));
