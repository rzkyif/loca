// End-to-end test of the loopback control-socket transport: a real tool-worker
// subprocess connects back over the socket (as it does in the Windows ConPTY
// path) and runs the full boot + call + shutdown protocol through it. Runs on
// every platform (the socket transport itself is platform-independent; only the
// decision to USE it is Windows-only), so this is the primary automated cover
// for the socket path since the ConPTY prompt path can only run on Windows.

import { assertEquals } from "@std/assert";
import { fromFileUrl } from "@std/path";
import { ControlListener } from "./control-socket.ts";
import type { PoolToWorkerFrame, WorkerToPoolFrame } from "./worker-protocol.ts";

const workerPath = fromFileUrl(new URL("../workers/tool-worker.ts", import.meta.url));

// Minimal extension entry: one tool export that echoes its args back.
const FIXTURE = `
export function echo(args) {
  return { echoed: args };
}
`;

async function withWorker(
  fn: (io: {
    send: (f: PoolToWorkerFrame) => void;
    next: (kind: WorkerToPoolFrame["kind"]) => Promise<WorkerToPoolFrame>;
    proc: Deno.ChildProcess;
  }) => Promise<void>,
): Promise<void> {
  const listener = ControlListener.create();
  const entry = await Deno.makeTempFile({ suffix: ".ts" });
  await Deno.writeTextFile(entry, FIXTURE);
  const proc = new Deno.Command(Deno.execPath(), {
    args: [
      "run",
      "--no-check",
      "--quiet",
      "--allow-all",
      workerPath,
      "--extension-id=test-ext",
      `--entry=${entry}`,
      `--control-addr=${listener.addr}`,
      `--control-token=${listener.token}`,
    ],
    stdin: "null",
    stdout: "null",
    stderr: "null",
  }).spawn();

  try {
    const channel = await listener.accept();
    const iter = channel.readLines()[Symbol.asyncIterator]();
    // Pull frames on demand, skipping blanks; buffer any that arrive out of the
    // requested order so a slow test consumer never drops one.
    const pending: WorkerToPoolFrame[] = [];
    async function nextFrame(): Promise<WorkerToPoolFrame> {
      if (pending.length) return pending.shift()!;
      while (true) {
        const { value, done } = await iter.next();
        if (done) throw new Error("worker channel closed unexpectedly");
        if (!value.trim()) continue;
        return JSON.parse(value) as WorkerToPoolFrame;
      }
    }
    await fn({
      send: (f) => channel.writeLine(JSON.stringify(f) + "\n"),
      next: async (kind) => {
        while (true) {
          const f = await nextFrame();
          if (f.kind === kind) return f;
          pending.push(f); // not the awaited kind; hold it (shouldn't happen here)
        }
      },
      proc,
    });
    channel.close();
  } finally {
    try {
      proc.kill();
    } catch {
      /* already exited */
    }
    await proc.status;
    await Deno.remove(entry).catch(() => {});
  }
}

Deno.test("control socket: worker boots, runs a call, and shuts down over the socket", async () => {
  await withWorker(async ({ send, next }) => {
    // Worker announces readiness then a successful boot, both over the socket.
    assertEquals((await next("ready")).kind, "ready");
    const booted = await next("booted");
    assertEquals(booted.kind === "booted" && booted.extensionId, "test-ext");

    // A call round-trips through the socket and returns the tool result.
    send({
      kind: "call",
      callId: "c1",
      toolName: "echo",
      fnExport: "echo",
      arguments: JSON.stringify({ hello: "world" }),
      chatContext: { userMessage: "hi", sessionId: null },
    });
    const result = await next("tool_result");
    assertEquals(result.kind === "tool_result" && result.callId, "c1");
    assertEquals(result.kind === "tool_result" && result.result, { echoed: { hello: "world" } });

    send({ kind: "shutdown" });
  });
});

Deno.test("control socket: rejects a connection presenting the wrong token", async () => {
  const listener = ControlListener.create();
  const colon = listener.addr.lastIndexOf(":");
  const port = Number(listener.addr.slice(colon + 1));

  // An impostor connects first with a bad token; it must be dropped without
  // locking out the real worker, which then connects with the right token.
  const bad = await Deno.connect({ hostname: "127.0.0.1", port, transport: "tcp" });
  await bad.write(new TextEncoder().encode("not-the-token\n"));

  const good = await Deno.connect({ hostname: "127.0.0.1", port, transport: "tcp" });
  await good.write(new TextEncoder().encode(listener.token + "\n"));

  const channel = await listener.accept();
  channel.writeLine(JSON.stringify({ kind: "shutdown" }) + "\n");
  // Give the write a tick to flush, then tear everything down.
  await new Promise((r) => setTimeout(r, 50));
  channel.close();
  try {
    bad.close();
  } catch {
    /* dropped by the listener */
  }
  try {
    good.close();
  } catch {
    /* closed via channel */
  }
});
