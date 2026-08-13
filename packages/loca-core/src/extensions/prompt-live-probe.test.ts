// Live probe: drive REAL Deno permission prompts through the real
// loca-core-ptyhost binary and the prompt parser. This is the drift
// tripwire: a deno release that changes the prompt wording or the answer
// handshake fails here before it can ship (the bundled deno is pinned on
// every channel, so a bump is always a deliberate, CI-gated change).
//
// Skipped when the ptyhost binary is absent (e.g. no cargo build yet) or on
// Windows: the probe FIXTURE below is unix-specific (it reads /etc/hosts and
// dlopens libSystem, and expects PTY-shaped prompt output), so it validates the
// unix PTY path only. The Windows ConPTY path has its own (local, Windows-only)
// live cover in worker-handle-live.test.ts (real prompts through the real
// ptyhost + ConPTY + control socket, at the WorkerHandle level), plus the
// ConPTY-shaped parser fixture in prompt-parser.test.ts and the
// platform-independent socket transport test in control-socket.test.ts. CI
// builds the debug binary before `deno task test`.

import { assertEquals } from "@std/assert";
import { decodeBase64, encodeBase64 } from "@std/encoding/base64";
import { fromFileUrl } from "@std/path";
import { PromptParser, type PromptParserEvent } from "./prompt-parser.ts";

function findPtyhost(): string | null {
  // Unix only: the probe fixture below is unix-specific (see the header).
  if (Deno.build.os === "windows") return null;
  const repoRoot = fromFileUrl(new URL("../../../../", import.meta.url));
  for (const candidate of [
    `${repoRoot}target/debug/loca-core-ptyhost`,
    `${repoRoot}target/release/loca-core-ptyhost`,
  ]) {
    try {
      if (Deno.statSync(candidate).isFile) return candidate;
    } catch {
      /* try next */
    }
  }
  return null;
}

const ptyhost = findPtyhost();

// CI sets LOCA_REQUIRE_PTYHOST=1 after building the helper: the probe is
// the drift tripwire for deno bumps, so silently skipping there would
// defeat its purpose. Local runs without a cargo build still skip.
Deno.test({
  name: "live probe: ptyhost binary present when required",
  ignore: Deno.env.get("LOCA_REQUIRE_PTYHOST") !== "1",
  fn() {
    assertEquals(
      ptyhost !== null,
      true,
      "LOCA_REQUIRE_PTYHOST is set but no ptyhost binary was found; run cargo build -p loca-core-ptyhost",
    );
  },
});

// One access per permission kind. The y/n column drives the answer; granted
// accesses must RESUME and produce data (proving pause-and-resume), denied
// ones must throw NotCapable into the still-running script.
const PROBE_SCRIPT = `
const out = (o) => console.log(JSON.stringify(o));
async function probe(label, fn) {
  try {
    const v = await fn();
    out({ label, ok: true, value: v });
  } catch (e) {
    out({ label, ok: false, name: e.name });
  }
}
await probe("read", () => Deno.readTextFile("/etc/hosts").then((t) => t.length));
await probe("env", () => Deno.env.get("LANG") ?? "");
await probe("sys", () => Deno.hostname());
await probe("write", () => Deno.writeTextFile("/tmp/loca-live-probe.txt", "x"));
await probe("run", () => new Deno.Command("ls", { args: ["/"] }).output().then((r) => r.code));
await probe("net", () => Deno.connect({ hostname: "127.0.0.1", port: 1 }));
await probe("ffi", () => Deno.dlopen("/usr/lib/libSystem.B.dylib", {}));
out({ label: "end" });
`;

// kind -> answer. Allowed kinds verify resume; denied kinds verify NotCapable.
const ANSWERS: Record<string, boolean> = {
  read: true,
  env: true,
  sys: true,
  write: false,
  run: false,
  net: false,
  ffi: false,
};

Deno.test({
  name: "live probe: real deno prompts parse, pause, and resume through ptyhost",
  ignore: ptyhost === null,
  // The subprocess tree (ptyhost -> deno) is torn down by the exit event;
  // sanitizers misread the piped readers that race it.
  sanitizeResources: false,
  sanitizeOps: false,
  async fn() {
    const scriptPath = await Deno.makeTempFile({ suffix: ".ts" });
    await Deno.writeTextFile(scriptPath, PROBE_SCRIPT);
    try {
      await runProbe(scriptPath);
    } finally {
      await Deno.remove(scriptPath).catch(() => {});
      await Deno.remove("/tmp/loca-live-probe.txt").catch(() => {});
    }
  },
});

async function runProbe(scriptPath: string): Promise<void> {
  const proc = new Deno.Command(ptyhost as string, {
    args: [],
    stdin: "piped",
    stdout: "piped",
    stderr: "piped",
  }).spawn();
  const writer = proc.stdin.getWriter();
  const enc = new TextEncoder();
  const writeControl = (frame: unknown) =>
    writer.write(enc.encode(JSON.stringify(frame) + "\n")).catch(() => {});

  const prompts: Array<{ permission: string; resource: string; apiName?: string }> = [];
  const settles: boolean[] = [];
  let answerTimer: ReturnType<typeof setTimeout> | undefined;
  let pendingAnswer: Uint8Array | null = null;
  let promptSeenAt = 0;

  const scheduleAnswer = () => {
    // Deno flushes stdin until quiescent (~100 ms) before reading the
    // answer; write after 300 ms and retry until the confirmation settles.
    const tick = () => {
      if (pendingAnswer === null) return;
      void writeControl({
        kind: "answer",
        dataB64: encodeBase64(pendingAnswer),
      });
      answerTimer = setTimeout(tick, 600);
    };
    answerTimer = setTimeout(tick, Math.max(0, 300 - (Date.now() - promptSeenAt)));
  };

  const parser = new PromptParser((event: PromptParserEvent) => {
    if (event.kind === "prompt") {
      prompts.push(event);
      promptSeenAt = Date.now();
      const allow = ANSWERS[event.permission] ?? false;
      pendingAnswer = enc.encode(allow ? "y\n" : "n\n");
      scheduleAnswer();
    } else if (event.kind === "settled") {
      settles.push(event.granted);
      pendingAnswer = null;
      if (answerTimer !== undefined) clearTimeout(answerTimer);
    }
  });

  await writeControl({
    kind: "spawn",
    cmd: Deno.execPath(),
    args: ["run", "--quiet", scriptPath],
    env: {
      PATH: Deno.env.get("PATH") ?? "",
      HOME: Deno.env.get("HOME") ?? "",
      LANG: Deno.env.get("LANG") ?? "en_US.UTF-8",
    },
  });

  // Pump ptyhost events into the parser until the child exits.
  const stderrDone = (async () => {
    const decoder = new TextDecoder();
    let buf = "";
    for await (const chunk of proc.stderr) {
      buf += decoder.decode(chunk, { stream: true });
      const lines = buf.split("\n");
      buf = lines.pop() ?? "";
      for (const line of lines) {
        if (!line.trim()) continue;
        const ev = JSON.parse(line) as { kind: string; dataB64?: string };
        if (ev.kind === "pty" && ev.dataB64) {
          parser.feed(new TextDecoder().decode(decodeBase64(ev.dataB64)));
        }
      }
    }
  })();

  // Collect the probe script's stdout verdicts.
  const results = new Map<string, { ok: boolean; name?: string }>();
  const stdoutDone = (async () => {
    const decoder = new TextDecoder();
    let buf = "";
    for await (const chunk of proc.stdout) {
      buf += decoder.decode(chunk, { stream: true });
      const lines = buf.split("\n");
      buf = lines.pop() ?? "";
      for (const line of lines) {
        if (!line.trim()) continue;
        const o = JSON.parse(line) as {
          label: string;
          ok?: boolean;
          name?: string;
        };
        if (o.label !== "end") {
          results.set(o.label, { ok: o.ok === true, name: o.name });
        }
      }
    }
  })();

  const timeout = setTimeout(() => {
    void writeControl({ kind: "kill" });
  }, 60_000);
  await proc.status;
  clearTimeout(timeout);
  if (answerTimer !== undefined) clearTimeout(answerTimer);
  await Promise.all([stderrDone, stdoutDone]);
  await writer.close().catch(() => {});

  // Every kind prompted, with the kind string and a non-empty resource
  // parsed out of the live prompt text (env/sys resources are key/flag
  // names; net is host:port).
  const promptedKinds = prompts.map((p) => p.permission);
  assertEquals(
    promptedKinds.sort(),
    Object.keys(ANSWERS).sort(),
    `prompt kinds drifted; raw prompts: ${JSON.stringify(prompts)}`,
  );
  for (const p of prompts) {
    if (p.permission !== "env" || p.resource !== "") {
      assertEquals(p.resource.length > 0, true, `empty resource for ${p.permission}`);
    }
  }
  // Settles arrived for every prompt with the expected verdicts.
  assertEquals(settles.length, prompts.length, "every prompt must settle");
  for (let i = 0; i < prompts.length; i++) {
    assertEquals(
      settles[i],
      ANSWERS[prompts[i].permission] ?? false,
      `verdict mismatch for ${prompts[i].permission}`,
    );
  }
  // Granted ops RESUMED with data; denied ops threw NotCapable into the
  // still-running script (it reached "end" and exited 0).
  assertEquals(results.get("read")?.ok, true, "granted read must resume");
  assertEquals(results.get("env")?.ok, true, "granted env must resume");
  assertEquals(results.get("sys")?.ok, true, "granted sys must resume");
  for (const denied of ["write", "run", "net", "ffi"]) {
    assertEquals(results.get(denied)?.ok, false, `denied ${denied} must fail`);
    assertEquals(results.get(denied)?.name, "NotCapable", `denied ${denied} error name`);
  }
}
