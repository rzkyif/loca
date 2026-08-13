// protocol round-trip against a real toolWorker subprocess.
//
// Spawns the worker entry script with a tempdir-based "extension" that exports
// a couple of test tools, then drives the NDJSON protocol from the
// pool side and asserts the worker's frame sequence.

import { assertEquals } from "@std/assert";
import { fromFileUrl, join } from "@std/path";
import type { PoolToWorkerFrame, WorkerToPoolFrame } from "../extensions/worker-protocol.ts";

const WORKER_ENTRY = fromFileUrl(new URL("./tool-worker.ts", import.meta.url));

const EXTENSION_SOURCE = `
export async function echo(args, ctx) {
  ctx.setProgress(0.5, "halfway");
  ctx.log("info", "echoing");
  return { args };
}

export async function boom() {
  throw new Error("intentional failure");
}

export async function slow(_args, ctx) {
  ctx.setProgress(0);
  // Sleep until aborted so cancel() has something to interrupt.
  await new Promise((_resolve, reject) => {
    ctx.signal.addEventListener("abort", () => reject(new Error("aborted")), { once: true });
  });
  return null;
}

export async function ask(_args, ctx) {
  const answer = await ctx.askUser([{ question: "pick one", options: [{ label: "a", value: "a" }] }]);
  return { picked: answer[0] };
}

export async function useDb(_args, ctx) {
  const rows = await ctx.db.query("SELECT 1 AS n", []);
  return { rows };
}

export async function showThing(_args, ctx) {
  ctx.display.markdown("# hi");
  return { shown: true };
}

export async function proposeSchedule(_args, ctx) {
  return await ctx.schedulePrompt({
    title: "t",
    instruction: "do it",
    schedule: { kind: "interval", everyMinutes: 60 },
    runMissed: false,
  });
}
`.trim();

interface WorkerSession {
  proc: Deno.ChildProcess;
  writer: WritableStreamDefaultWriter<Uint8Array>;
  frames: AsyncIterator<WorkerToPoolFrame>;
  shutdown(): Promise<void>;
}

function startWorker(entry: string): WorkerSession {
  const proc = new Deno.Command(Deno.execPath(), {
    args: [
      "run",
      "--no-check",
      "--quiet",
      "--allow-read",
      WORKER_ENTRY,
      `--extension-id=test-extension`,
      `--entry=${entry}`,
    ],
    stdin: "piped",
    stdout: "piped",
    stderr: "null",
  }).spawn();
  const writer = proc.stdin.getWriter();
  const frames = pumpFrames(proc.stdout).getReader();

  // Convert ReadableStreamDefaultReader -> AsyncIterator surface
  const iter: AsyncIterator<WorkerToPoolFrame> = {
    async next() {
      const { value, done } = await frames.read();
      if (done) return { value: undefined as never, done: true };
      return { value, done: false };
    },
  };
  return {
    proc,
    writer,
    frames: iter,
    async shutdown() {
      try {
        await writer.write(encode({ kind: "shutdown" }));
      } catch {
        /* ignore */
      }
      try {
        writer.releaseLock();
      } catch {
        /* ignore */
      }
      try {
        await proc.stdin.close();
      } catch {
        /* ignore */
      }
      try {
        frames.releaseLock();
      } catch {
        /* ignore */
      }
      await proc.status;
    },
  };
}

function encode(frame: PoolToWorkerFrame): Uint8Array {
  return new TextEncoder().encode(JSON.stringify(frame) + "\n");
}

function pumpFrames(stream: ReadableStream<Uint8Array>): ReadableStream<WorkerToPoolFrame> {
  const decoder = new TextDecoder();
  let buf = "";
  return new ReadableStream<WorkerToPoolFrame>({
    async start(controller) {
      for await (const chunk of stream) {
        buf += decoder.decode(chunk, { stream: true });
        const lines = buf.split("\n");
        buf = lines.pop() ?? "";
        for (const line of lines) {
          if (!line.trim()) continue;
          try {
            controller.enqueue(JSON.parse(line) as WorkerToPoolFrame);
          } catch {
            // ignore: stderr lines are pumped via a separate pipe in prod;
            // here we just skip non-JSON noise.
          }
        }
      }
      controller.close();
    },
  });
}

async function nextOfKind<K extends WorkerToPoolFrame["kind"]>(
  iter: AsyncIterator<WorkerToPoolFrame>,
  kind: K,
): Promise<Extract<WorkerToPoolFrame, { kind: K }>> {
  while (true) {
    const { value, done } = await iter.next();
    if (done) throw new Error(`stream ended before frame kind=${kind}`);
    if (value.kind === kind) {
      return value as Extract<WorkerToPoolFrame, { kind: K }>;
    }
    // Allow ready/booted/progress/log/stderr_log to flow through silently.
  }
}

async function setupExtension(): Promise<{ dir: string; entry: string }> {
  const dir = await Deno.makeTempDir({ prefix: "loca-tool-worker-" });
  const entry = join(dir, "index.ts");
  await Deno.writeTextFile(entry, EXTENSION_SOURCE);
  return { dir, entry };
}

Deno.test("toolWorker: ready -> booted -> call returns tool_result + progress + log", async () => {
  const { dir, entry } = await setupExtension();
  const session = startWorker(entry);
  try {
    await nextOfKind(session.frames, "ready");
    await nextOfKind(session.frames, "booted");

    await session.writer.write(
      encode({
        kind: "call",
        callId: "c1",
        toolName: "echo",
        fnExport: "echo",
        arguments: JSON.stringify({ x: 42 }),
        chatContext: { userMessage: "hi", sessionId: null },
      }),
    );

    const progress = await nextOfKind(session.frames, "progress");
    assertEquals(progress.callId, "c1");
    assertEquals(progress.progress, 0.5);

    const log = await nextOfKind(session.frames, "log");
    assertEquals(log.message, "echoing");

    const result = await nextOfKind(session.frames, "tool_result");
    assertEquals(result.callId, "c1");
    assertEquals(result.result, { args: { x: 42 } });
  } finally {
    await session.shutdown();
    await Deno.remove(dir, { recursive: true }).catch(() => {});
  }
});

Deno.test("toolWorker: a throwing tool emits tool_error with the message", async () => {
  const { dir, entry } = await setupExtension();
  const session = startWorker(entry);
  try {
    await nextOfKind(session.frames, "ready");
    await nextOfKind(session.frames, "booted");

    await session.writer.write(
      encode({
        kind: "call",
        callId: "c2",
        toolName: "boom",
        fnExport: "boom",
        arguments: "{}",
        chatContext: { userMessage: "", sessionId: null },
      }),
    );

    const err = await nextOfKind(session.frames, "tool_error");
    assertEquals(err.callId, "c2");
    assertEquals(err.error, "intentional failure");
  } finally {
    await session.shutdown();
    await Deno.remove(dir, { recursive: true }).catch(() => {});
  }
});

Deno.test("toolWorker: cancel aborts an in-flight call", async () => {
  const { dir, entry } = await setupExtension();
  const session = startWorker(entry);
  try {
    await nextOfKind(session.frames, "ready");
    await nextOfKind(session.frames, "booted");

    await session.writer.write(
      encode({
        kind: "call",
        callId: "c3",
        toolName: "slow",
        fnExport: "slow",
        arguments: "{}",
        chatContext: { userMessage: "", sessionId: null },
      }),
    );
    // Wait for the first progress so the call is definitely running.
    await nextOfKind(session.frames, "progress");

    await session.writer.write(encode({ kind: "cancel", callId: "c3" }));

    const err = await nextOfKind(session.frames, "tool_error");
    assertEquals(err.callId, "c3");
    assertEquals(err.error, "aborted");
  } finally {
    await session.shutdown();
    await Deno.remove(dir, { recursive: true }).catch(() => {});
  }
});

Deno.test("toolWorker: ask_user round-trip resolves the tool with the user's answer", async () => {
  const { dir, entry } = await setupExtension();
  const session = startWorker(entry);
  try {
    await nextOfKind(session.frames, "ready");
    await nextOfKind(session.frames, "booted");

    await session.writer.write(
      encode({
        kind: "call",
        callId: "c4",
        toolName: "ask",
        fnExport: "ask",
        arguments: "{}",
        chatContext: { userMessage: "", sessionId: null },
      }),
    );

    const ask = await nextOfKind(session.frames, "ask_user_request");
    assertEquals(ask.callId, "c4");

    await session.writer.write(
      encode({
        kind: "ask_user_response",
        callId: "c4",
        requestId: ask.requestId,
        answers: ["picked-value"],
      }),
    );

    const result = await nextOfKind(session.frames, "tool_result");
    assertEquals(result.result, { picked: "picked-value" });
  } finally {
    await session.shutdown();
    await Deno.remove(dir, { recursive: true }).catch(() => {});
  }
});

Deno.test("toolWorker: module_request round-trips a db query through the pool", async () => {
  const { dir, entry } = await setupExtension();
  const session = startWorker(entry);
  try {
    await nextOfKind(session.frames, "ready");
    await nextOfKind(session.frames, "booted");

    await session.writer.write(
      encode({
        kind: "call",
        callId: "c5",
        toolName: "useDb",
        fnExport: "useDb",
        arguments: "{}",
        chatContext: { userMessage: "", sessionId: null },
      }),
    );

    const req = await nextOfKind(session.frames, "module_request");
    assertEquals(req.callId, "c5");
    assertEquals(req.module, "db");
    assertEquals(req.op, "query");
    assertEquals(req.args, { sql: "SELECT 1 AS n", params: [] });

    await session.writer.write(
      encode({
        kind: "module_response",
        callId: "c5",
        requestId: req.requestId,
        ok: true,
        result: [{ n: 1 }],
      }),
    );

    const result = await nextOfKind(session.frames, "tool_result");
    assertEquals(result.result, { rows: [{ n: 1 }] });
  } finally {
    await session.shutdown();
    await Deno.remove(dir, { recursive: true }).catch(() => {});
  }
});

Deno.test("toolWorker: display is a one-way frame that precedes the result", async () => {
  const { dir, entry } = await setupExtension();
  const session = startWorker(entry);
  try {
    await nextOfKind(session.frames, "ready");
    await nextOfKind(session.frames, "booted");

    await session.writer.write(
      encode({
        kind: "call",
        callId: "c6",
        toolName: "showThing",
        fnExport: "showThing",
        arguments: "{}",
        chatContext: { userMessage: "", sessionId: null },
      }),
    );

    // No response is sent: display is fire-and-forget, so the tool returns
    // without us answering anything.
    const display = await nextOfKind(session.frames, "display");
    assertEquals(display.callId, "c6");
    assertEquals(display.content, { type: "markdown", markdown: "# hi" });

    const result = await nextOfKind(session.frames, "tool_result");
    assertEquals(result.result, { shown: true });
  } finally {
    await session.shutdown();
    await Deno.remove(dir, { recursive: true }).catch(() => {});
  }
});

Deno.test("toolWorker: schedule_request round-trips and resolves with the confirmation", async () => {
  const { dir, entry } = await setupExtension();
  const session = startWorker(entry);
  try {
    await nextOfKind(session.frames, "ready");
    await nextOfKind(session.frames, "booted");

    await session.writer.write(
      encode({
        kind: "call",
        callId: "c7",
        toolName: "proposeSchedule",
        fnExport: "proposeSchedule",
        arguments: "{}",
        chatContext: { userMessage: "", sessionId: null },
      }),
    );

    const req = await nextOfKind(session.frames, "schedule_request");
    assertEquals(req.callId, "c7");
    assertEquals(req.draft.title, "t");

    // The pool replies with the user's (possibly edited) draft.
    const editedDraft = {
      title: "edited",
      instruction: "do it",
      schedule: { kind: "interval", everyMinutes: 30 },
      runMissed: true,
    } as const;
    await session.writer.write(
      encode({
        kind: "schedule_confirm_response",
        callId: "c7",
        requestId: req.requestId,
        accepted: true,
        draft: editedDraft,
      }),
    );

    const result = await nextOfKind(session.frames, "tool_result");
    assertEquals(result.result, { accepted: true, draft: editedDraft });
  } finally {
    await session.shutdown();
    await Deno.remove(dir, { recursive: true }).catch(() => {});
  }
});
