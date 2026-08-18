// Spawn, address, and tear down a Core under test.
//
// One interface, three targets. A case never learns which target it is talking
// to; that is the whole premise of the suite. The seam is the environment
// contract below, which both cores read identically.

import { join } from "@std/path";
import { DENO_CORE_ENTRY, findRustCore, REPO_ROOT, WORKERS_DIR } from "./repo.ts";
import { ADMIN_TOKEN, seedHome, type SeedOptions, type Target } from "./home.ts";
import { pinFromHome } from "./tls.ts";
import { type MockUpstream, startUpstream } from "./upstream.ts";

export type { Target };

export interface CoreOptions extends Omit<SeedOptions, "upstreamBaseUrl"> {
  /** Reuse an already-seeded home instead of creating one. Used by the parity
   *  cases, which hand one core's home to the other. */
  home?: string;
  /** The target `swap()` hands over to. Set only for cutover cases, which start
   *  on one core and finish on the other. */
  swapTo?: Target;
  /** `LOCA_CHANNEL` for this boot. Defaults to `dev`, which is what a core run
   *  from source is. A case sets it to reach channel-conditional behaviour;
   *  see `Case.channel`. */
  channel?: string;
}

export interface CoreUnderTest {
  /** Who is answering RIGHT NOW, not who answered first. A cutover case changes
   *  this mid-run, and a failure report that named the wrong binary would send
   *  the reader to the wrong codebase. */
  target: Target;
  home: string;
  port: number;
  baseUrl: string;
  wsUrl: string;
  adminToken: string;
  /** base64(SHA-256(SPKI)), derived from the core's cert, not asked for. */
  tlsPin: string;
  upstream: MockUpstream;
  recentLogs(): string[];
  /** Stop the process, keep the home, port, and upstream so it can come back. */
  kill(): Promise<void>;
  restart(): Promise<void>;
  /** Stop this core and bring the OTHER one up on the same home, port, and
   *  upstream. The cutover hand-off; needs `swapTo` at start. */
  swap(): Promise<void>;
  /** Kill and tear down the home and upstream for good. */
  stop(): Promise<void>;
}

// A bounded line ring, so a long-lived core cannot grow the buffer without
// bound. Attached to every failure: a spawn timeout with no captured stderr is
// the most expensive failure mode a process-spawning suite has.
class LogRing {
  private lines: string[] = [];
  private partial = "";
  constructor(private readonly cap = 400) {}
  push(chunk: string): void {
    this.partial += chunk;
    const parts = this.partial.split("\n");
    this.partial = parts.pop() ?? "";
    for (const l of parts) {
      this.lines.push(l);
      if (this.lines.length > this.cap) this.lines.shift();
    }
  }
  recent(): string[] {
    return this.partial ? [...this.lines, this.partial] : [...this.lines];
  }
}

/** An ephemeral port, released immediately. Racy in principle; the window is
 *  microseconds and this is what every other lane in the repo does. */
function freePort(): number {
  const l = Deno.listen({ hostname: "127.0.0.1", port: 0 });
  const port = (l.addr as Deno.NetAddr).port;
  l.close();
  return port;
}

/** How to start a target, and the string the OS will report for it. `marker` is
 *  what makes a hand-off checkable: see `assertServedBy`. */
function commandFor(target: Target): { cmd: string; args: string[]; marker: string } {
  if (target === "deno") {
    return { cmd: Deno.execPath(), args: ["run", "-A", DENO_CORE_ENTRY], marker: DENO_CORE_ENTRY };
  }
  const bin = findRustCore();
  if (!bin) {
    throw new Error(
      "the Rust core binary was not found at target/release/loca-core or " +
        "target/debug/loca-core. Build it with `deno task build:core-rs`. " +
        "Refusing to skip: a conformance run that passes because nothing ran " +
        "is the failure this suite exists to prevent.",
    );
  }
  return { cmd: bin.path, args: [], marker: bin.path };
}

/** Ask the OS what is running under this pid.
 *
 *  Deliberately not remembered from the spawn. A cutover case asserts that state
 *  written by one core is readable by the other, and every one of those
 *  assertions is ALSO satisfied by the first core simply restarting: "the
 *  bearer still works, the pin has not moved, the session is still there" is
 *  what a restart looks like too. So the only thing separating a real hand-off
 *  from a hand-off that quietly did nothing is WHICH BINARY is answering, and a
 *  field this harness assigns and then reads back would prove nothing about
 *  that (D-048's shape: the assertion is fine and its lens is not). */
async function runningCommand(pid: number): Promise<string> {
  const out = await new Deno.Command("ps", {
    args: ["-ww", "-o", "args=", "-p", String(pid)],
    stdout: "piped",
    stderr: "null",
  }).output();
  return new TextDecoder().decode(out.stdout).trim();
}

/** Fail loudly unless the process now serving really is `target`'s binary.
 *
 *  The marker is derived HERE, from the target that was asked for, and never
 *  taken from the spawn that answered. Taking it from the spawn is the version
 *  of this check that does not work, and that is measured rather than assumed:
 *  the first cut read the marker off the `Running` record, so making `swap()`
 *  re-spawn the Deno core left every cutover case green. The record agreed with
 *  the mistake because it was made by the mistake. Same family as D-048: the
 *  assertion was fine and its lens was not. */
async function assertServedBy(pid: number, target: Target): Promise<void> {
  const marker = commandFor(target).marker;
  const cmd = await runningCommand(pid);
  if (!cmd) {
    throw new Error(
      `the core answering on this port has pid ${pid}, which the OS does not ` +
        `report as running. The hand-off cannot be verified, so it is not trusted.`,
    );
  }
  if (!cmd.includes(marker)) {
    throw new Error(
      `the hand-off did not happen: expected the ${target} core (${marker}) to be ` +
        `serving, but pid ${pid} is running:\n  ${cmd}\n` +
        `Every state assertion in a cutover case also passes on a plain restart, ` +
        `so an unverified hand-off is a case that proves nothing.`,
    );
  }
}

function envFor(
  home: string,
  port: number,
  upstream: string,
  channel: string,
): Record<string, string> {
  return {
    LOCA_CORE_HOME: home,
    // The home is pinned above regardless, so this changes what the core
    // BELIEVES it is rather than where it keeps anything: the channel-gated
    // branches, and nothing else.
    LOCA_CHANNEL: channel,
    LOCA_CORE_HOST: "127.0.0.1",
    LOCA_CORE_PORT: String(port),
    // Catalog, manifests, and weight downloads all resolve to the loopback mock.
    LOCA_STORAGE_BASE_URL: upstream,
    LOCA_HF_BASE_URL: upstream,
    // The tool-worker entry lives in source when the core runs from source.
    LOCA_WORKERS_DIR: WORKERS_DIR,
  };
}

async function healthy(baseUrl: string): Promise<boolean> {
  try {
    const res = await fetch(`${baseUrl}/api/v1/health`, {
      signal: AbortSignal.timeout(2_000),
    });
    await res.body?.cancel();
    return res.status === 200;
  } catch {
    return false;
  }
}

async function waitHealthy(baseUrl: string, logs: LogRing, timeoutMs = 30_000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (await healthy(baseUrl)) return;
    await new Promise((r) => setTimeout(r, 150));
  }
  throw new Error(
    `core did not become healthy at ${baseUrl} within ${timeoutMs}ms\n` +
      `--- core stderr ---\n${logs.recent().join("\n")}`,
  );
}

interface Running {
  child: Deno.ChildProcess;
  done: Promise<unknown>;
}

function spawn(
  target: Target,
  home: string,
  port: number,
  upstream: string,
  channel: string,
  logs: LogRing,
): Running {
  const { cmd, args } = commandFor(target);
  const child = new Deno.Command(cmd, {
    args,
    cwd: REPO_ROOT,
    env: envFor(home, port, upstream, channel),
    stdin: "null",
    stdout: "null",
    stderr: "piped",
  }).spawn();

  const echo = Deno.env.get("LOCA_CONFORMANCE_CORE_LOGS") === "1";
  const pump = child.stderr
    .pipeTo(
      new WritableStream({
        write(chunk) {
          const text = new TextDecoder().decode(chunk);
          logs.push(text);
          if (echo) Deno.stderr.writeSync(chunk);
        },
      }),
    )
    .catch(() => {
      /* closed on kill */
    });

  return { child, done: pump };
}

async function killProcess(run: Running): Promise<void> {
  try {
    run.child.kill("SIGTERM");
  } catch {
    // already gone
  }
  const exited = run.child.status.catch(() => null);
  await Promise.race([exited, new Promise((r) => setTimeout(r, 3_000))]);
  try {
    run.child.kill("SIGKILL");
  } catch {
    // already gone
  }
  await exited;
  await run.done;
}

export async function startCore(opts: CoreOptions): Promise<CoreUnderTest> {
  const upstream = await startUpstream();
  const home = opts.home ?? (await seedHome({ ...opts, upstreamBaseUrl: upstream.baseUrl }));
  const port = freePort();
  const baseUrl = `https://127.0.0.1:${port}`;
  const logs = new LogRing();

  let current: Target = opts.target;
  const channel = opts.channel ?? "dev";
  let run = spawn(current, home, port, upstream.baseUrl, channel, logs);
  let running = true;

  let tlsPin: string;
  try {
    await waitHealthy(baseUrl, logs);
    // Checked on the first boot too, not only on a hand-off: a cutover case
    // claims state was written by the Deno core and read by the Rust one, and
    // that claim is only as good as the identity of BOTH halves.
    await assertServedBy(run.child.pid, current);
    tlsPin = await pinFromHome(home);
  } catch (err) {
    await killProcess(run);
    await upstream.close();
    if (!opts.home) await Deno.remove(home, { recursive: true }).catch(() => {});
    throw err;
  }

  const kill = async (): Promise<void> => {
    if (!running) return;
    running = false;
    await killProcess(run);
  };

  return {
    get target() {
      return current;
    },
    home,
    port,
    baseUrl,
    wsUrl: `wss://127.0.0.1:${port}/ws/v1`,
    adminToken: ADMIN_TOKEN,
    tlsPin,
    upstream,
    recentLogs: () => logs.recent(),
    kill,
    restart: async () => {
      if (running) return;
      run = spawn(current, home, port, upstream.baseUrl, channel, logs);
      running = true;
      await waitHealthy(baseUrl, logs);
    },
    swap: async () => {
      const to = opts.swapTo;
      if (!to) {
        throw new Error(
          'this case uses handoff: "swap" but the core was started without a ' +
            "swap target. A cutover case is grouped and booted separately by the " +
            "runner; it cannot share a scenario.",
        );
      }
      if (to === current) {
        // A swap that lands on the core it started from passes every state
        // assertion a cutover case makes, because those assertions describe a
        // restart just as well. Refuse rather than report a green nothing.
        throw new Error(
          `refusing a hand-off from ${current} to ${to}: swapping a core for ` +
            `itself is a restart, and a cutover case that restarts proves nothing.`,
        );
      }
      await kill();
      current = to;
      run = spawn(current, home, port, upstream.baseUrl, channel, logs);
      running = true;
      await waitHealthy(baseUrl, logs);
      await assertServedBy(run.child.pid, current);
    },
    stop: async () => {
      await kill();
      await upstream.close();
      if (Deno.env.get("LOCA_CONFORMANCE_KEEP_HOME") === "1") return;
      await Deno.remove(home, { recursive: true }).catch(() => {});
    },
  };
}

/** The path a case uses to reach into a core's home (storage-tree assertions,
 *  parity diffs). Reading the home is fair game; writing it is not. */
export function homePath(core: CoreUnderTest, ...parts: string[]): string {
  return join(core.home, ...parts);
}
