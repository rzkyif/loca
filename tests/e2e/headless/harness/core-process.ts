// Spawn + supervise a real loca-core subprocess for one E2E scenario (Node).
//
// Each scenario gets a throwaway LOCA_CORE_HOME tempdir, the dev channel (so
// the model catalog is built in-memory with no signed fetch), the native
// helper binary symlinked from target/debug (so core's boot-time presence
// check passes without a source skip), and settings that point every outbound
// dependency at the mock services server. External LLM/STT + TTS-off means core
// spawns no native llama/speech sidecars.

import { spawn, type ChildProcess } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, dirname } from "node:path";
import net from "node:net";
import tls from "node:tls";
import https from "node:https";
import { createHash } from "node:crypto";
import {
  DEV_CORE_BIN,
  findRustCore,
  REPO_ROOT,
  REQUIRED_HELPERS,
  SHARED_MODELS_DIR,
  SIDECAR_BINARIES,
  WORKERS_DIR,
} from "./repo.ts";
import { startMockServices, type LlmScript, type MockServices } from "./mock-services.ts";

export interface ScenarioOptions {
  /** Extra core settings.json entries, merged over the mock-wired defaults. */
  settings?: Record<string, unknown>;
  /** Initial LLM behaviour (default: echo the user message). */
  llm?: LlmScript;
  /** Model files: "present" (default) points the test models dir at the shared
   *  real cache so the requirements snapshot is satisfied; "absent" leaves it
   *  empty so the downloader flow shows pending models. */
  models?: "present" | "absent";
  /** Sidecar binaries (deno, llama-server): "present" (default) symlinks them
   *  from the dev install so requirements are satisfied; "absent" omits them. */
  binaries?: "present" | "absent";
}

export interface CoreInstance {
  id: string;
  home: string;
  baseUrl: string; // https://127.0.0.1:<port>
  adminToken: string;
  /** base64(SHA-256(SPKI)) of the core's self-signed cert; the value the PAKE
   *  channel-binds. Computed Node-side since Chromium hides the cert. The TLS
   *  keypair is persisted in the home's secrets vault, so the pin is stable
   *  across restart(). */
  tlsPin: string;
  mock: MockServices;
  /** The most recent stderr lines from the core subprocess, for failure
   *  diagnostics (the bare suite otherwise yields a timeout with no cause). */
  recentLogs(): string[];
  /** Kill the running core subprocess but keep its home + mock + port, so it can
   *  be brought back with restart(). Models a network/process blip. */
  kill(): Promise<void>;
  /** Respawn the core on the SAME home + port (pin unchanged), then wait until
   *  healthy. Idempotent if already running. */
  restart(): Promise<void>;
  /** Kill the core and tear down its home + mock for good. */
  stop(): Promise<void>;
}

// A bounded line ring so a long-lived core can't grow the buffer without bound.
class LogRing {
  private lines: string[] = [];
  private partial = "";
  constructor(private readonly cap = 500) {}
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

// Compute the cert's SPKI pin the same way core does (services/tls.ts):
// base64(SHA-256(SubjectPublicKeyInfo DER)).
function spkiPin(host: string, port: number): Promise<string> {
  return new Promise((resolve, reject) => {
    const socket = tls.connect({ host, port, rejectUnauthorized: false }, () => {
      try {
        const cert = socket.getPeerX509Certificate();
        if (!cert) throw new Error("no peer certificate");
        const spki = cert.publicKey.export({ type: "spki", format: "der" }) as Buffer;
        const pin = createHash("sha256").update(spki).digest("base64");
        socket.end();
        resolve(pin);
      } catch (err) {
        socket.destroy();
        reject(err);
      }
    });
    // Health already gated the connect, so this is belt-and-suspenders: a stalled
    // handshake fails fast and frees the socket instead of hanging to test timeout.
    socket.setTimeout(5_000, () => {
      socket.destroy();
      reject(new Error(`spki handshake to ${host}:${port} timed out`));
    });
    socket.on("error", (err) => {
      socket.destroy();
      reject(err);
    });
  });
}

function freePort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const s = net.createServer();
    s.on("error", reject);
    s.listen(0, "127.0.0.1", () => {
      const p = (s.address() as net.AddressInfo).port;
      s.close(() => resolve(p));
    });
  });
}

function seedHome(home: string, settings: Record<string, unknown>, opts: ScenarioOptions): void {
  const bin = join(home, "bin");
  mkdirSync(bin, { recursive: true });
  // Required helper binaries (dev channel suffix "-dev"); their boot-time
  // presence check refuses to start without them.
  const profileDir = dirname(findRustCore());
  for (const base of REQUIRED_HELPERS) {
    const src = join(profileDir, base);
    if (!existsSync(src))
      throw new Error(`missing helper ${src}; run \`cargo build -p loca-core-updater\``);
    symlinkSync(src, join(bin, `${base}-dev`));
  }
  // Sidecar binaries (deno, llama-server): present by default so the
  // requirements snapshot is satisfied and the chat composer is enabled.
  if ((opts.binaries ?? "present") === "present") {
    for (const base of SIDECAR_BINARIES) {
      const src = join(DEV_CORE_BIN, base);
      if (existsSync(src)) symlinkSync(src, join(bin, base));
    }
  }
  // Models: point the test models dir at the shared real cache so required
  // weights (the embed model) report present. "absent" leaves it empty so the
  // downloader flow can show pending models.
  if ((opts.models ?? "present") === "present" && existsSync(SHARED_MODELS_DIR)) {
    symlinkSync(SHARED_MODELS_DIR, join(home, "models"));
  }
  writeFileSync(join(home, "settings.json"), JSON.stringify(settings, null, 2));
  writeFileSync(join(home, ".admin-token"), "e2e-admin-token\n");
}

function defaultSettings(mockBaseUrl: string): Record<string, unknown> {
  return {
    // External providers => no local llama / speech sidecars spawn.
    "llm.provider": "external",
    "llm.external.baseUrl": `${mockBaseUrl}/v1`,
    "llm.external.model": "mock-model",
    "stt.provider": "external",
    "stt.external.baseUrl": `${mockBaseUrl}/v1`,
    "stt.external.model": "whisper-1",
    // TTS is off by default (no native sidecar); a TTS test flips tts.enabled +
    // tts.provider=external and these point synthesis at the mock /v1.
    "tts.enabled": false,
    "tts.external.baseUrl": `${mockBaseUrl}/v1`,
    "tts.external.model": "tts-1",
    // Secondary (dual-model) endpoint points at the mock with a distinct model
    // name, so a dual-model test can assert routing by the recorded model.
    "dualModel.external.baseUrl": `${mockBaseUrl}/v1`,
    "dualModel.external.model": "secondary-model",
  };
}

// A single health probe over the core's self-signed TLS. `rejectUnauthorized:
// false` is scoped to this request (and to spkiPin's connect) so the harness
// never disables TLS validation process-wide.
function healthProbe(baseUrl: string): Promise<boolean> {
  return new Promise((resolve, reject) => {
    const req = https.get(
      `${baseUrl}/api/v1/health`,
      { rejectUnauthorized: false, timeout: 2_000 },
      (res) => {
        const ok = res.statusCode === 200;
        res.resume(); // drain so the socket frees
        resolve(ok);
      },
    );
    req.on("timeout", () => req.destroy(new Error("health probe timed out")));
    req.on("error", reject);
  });
}

async function waitHealthy(baseUrl: string, timeoutMs = 30_000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  let lastErr: unknown;
  while (Date.now() < deadline) {
    try {
      if (await healthProbe(baseUrl)) return;
    } catch (err) {
      lastErr = err;
    }
    await new Promise((r) => setTimeout(r, 300));
  }
  throw new Error(`core did not become healthy at ${baseUrl}: ${lastErr ?? "timeout"}`);
}

// Spawn the core subprocess on a fixed home + port, piping stderr into `logs`
// (and echoing it when LOCA_E2E_CORE_LOGS is set). Shared by startCore and
// restart so a brought-back core captures logs the same way.
function spawnCore(home: string, port: number, mockBaseUrl: string, logs: LogRing): ChildProcess {
  const child = spawn(findRustCore(), [], {
    cwd: REPO_ROOT,
    env: {
      ...process.env,
      LOCA_CORE_HOME: home,
      LOCA_CHANNEL: "dev",
      LOCA_CORE_PORT: String(port),
      LOCA_CORE_HOST: "127.0.0.1",
      // The tool-worker entry lives in source (no self-update stages it here).
      LOCA_WORKERS_DIR: WORKERS_DIR,
      // Route catalog / manifest / HF downloads at the mock (see config.ts).
      LOCA_STORAGE_BASE_URL: mockBaseUrl,
      LOCA_HF_BASE_URL: mockBaseUrl,
    },
    stdio: ["ignore", "ignore", "pipe"],
  });
  const echo = !!process.env.LOCA_E2E_CORE_LOGS;
  child.stderr?.setEncoding("utf8");
  child.stderr?.on("data", (chunk: string) => {
    logs.push(chunk);
    if (echo) process.stderr.write(chunk);
  });
  return child;
}

async function killChild(child: ChildProcess): Promise<void> {
  try {
    child.kill("SIGTERM");
  } catch {
    /* already gone */
  }
  await new Promise((r) => setTimeout(r, 300));
  try {
    child.kill("SIGKILL");
  } catch {
    /* */
  }
}

let counter = 0;

export async function startCore(opts: ScenarioOptions = {}): Promise<CoreInstance> {
  const id = `core-${++counter}`;
  const mock = await startMockServices();
  if (opts.llm) mock.setLlmScript(opts.llm);

  const home = mkdtempSync(join(tmpdir(), "loca-e2e-"));
  const settings = { ...defaultSettings(mock.baseUrl), ...opts.settings };
  seedHome(home, settings, opts);

  const port = await freePort();
  const baseUrl = `https://127.0.0.1:${port}`;
  const logs = new LogRing();

  let child = spawnCore(home, port, mock.baseUrl, logs);
  let running = true;

  let tlsPin: string;
  try {
    await waitHealthy(baseUrl);
    tlsPin = await spkiPin("127.0.0.1", port);
  } catch (err) {
    await killChild(child);
    rmSync(home, { recursive: true, force: true });
    await mock.close();
    throw new Error(`${err}\n--- core logs ---\n${logs.recent().join("\n")}`);
  }

  const kill = async (): Promise<void> => {
    if (!running) return;
    running = false;
    await killChild(child);
  };

  const restart = async (): Promise<void> => {
    if (running) return;
    child = spawnCore(home, port, mock.baseUrl, logs);
    running = true;
    await waitHealthy(baseUrl);
  };

  const stop = async (): Promise<void> => {
    await kill();
    await mock.close();
    rmSync(home, { recursive: true, force: true });
  };

  return {
    id,
    home,
    baseUrl,
    adminToken: "e2e-admin-token",
    tlsPin,
    mock,
    recentLogs: () => logs.recent(),
    kill,
    restart,
    stop,
  };
}
