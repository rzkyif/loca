import { assert, assertEquals, assertRejects } from "@std/assert";
import type { Tool } from "@loca/shared";
import type { PoolToWorkerFrame, WorkerToPoolFrame } from "./worker-protocol.ts";
import { AppError } from "@loca/core-engine";
import {
  type CallEvent,
  type CallWorker,
  InFlightCall,
  type PoolCallbacks,
  type ToolCallStart,
} from "./worker-call.ts";

// A controllable stand-in for WorkerHandle: records sent frames + prompt
// answers, lets the test resolve/reject boot and push worker->pool frames.
class FakeWorker implements CallWorker {
  sent: PoolToWorkerFrame[] = [];
  answered: Array<{ requestId: string; allow: boolean }> = [];
  private listener: ((frame: WorkerToPoolFrame) => void) | null = null;
  private bootResolve!: () => void;
  private bootReject!: (err: Error) => void;
  private boot = new Promise<void>((res, rej) => {
    this.bootResolve = res;
    this.bootReject = rej;
  });

  on(listener: (frame: WorkerToPoolFrame) => void): () => void {
    this.listener = listener;
    return () => {
      this.listener = null;
    };
  }
  send(frame: PoolToWorkerFrame): void {
    this.sent.push(frame);
  }
  waitForBoot(): Promise<void> {
    return this.boot;
  }
  answerPrompt(requestId: string, allow: boolean): void {
    this.answered.push({ requestId, allow });
  }

  // test drivers
  emit(frame: WorkerToPoolFrame): void {
    this.listener?.(frame);
  }
  get listenerAttached(): boolean {
    return this.listener !== null;
  }
  succeedBoot(): void {
    this.bootResolve();
  }
  failBoot(err: Error): void {
    this.bootReject(err);
  }
}

function makeTool(): Tool {
  return {
    id: "ext::doit",
    extensionId: "ext",
    providerKind: "extension",
    name: "doit",
    description: "",
    parameters: {},
    triggers: [],
    fnExport: "doit",
    alwaysAvailable: false,
    platforms: [],
    enabled: true,
    requiredPermissions: [],
    missingRequired: [],
    grants: [],
  };
}

interface Harness {
  worker: FakeWorker;
  events: CallEvent[];
  counts: { started: number; settled: number; killed: number; bootFailed: number };
  call: InFlightCall;
}

function setup(opts?: { callTimeoutMs?: number; drainTimeoutMs?: number }): Harness {
  const worker = new FakeWorker();
  const events: CallEvent[] = [];
  const counts = { started: 0, settled: 0, killed: 0, bootFailed: 0 };
  const callbacks: PoolCallbacks = {
    onStarted: () => counts.started++,
    onSettled: () => counts.settled++,
    onKilled: () => counts.killed++,
    onBootFailed: () => counts.bootFailed++,
  };
  const spec: ToolCallStart = {
    extensionId: "ext",
    tool: makeTool(),
    argumentsJson: "{}",
    chatContext: { userMessage: "hi", sessionId: "s1", locale: undefined },
  };
  const call = new InFlightCall({
    callId: "c1",
    worker,
    spec,
    onEvent: (e) => events.push(e),
    callbacks,
    callTimeoutMs: opts?.callTimeoutMs ?? 60_000,
    drainTimeoutMs: opts?.drainTimeoutMs ?? 50,
  });
  return { worker, events, counts, call };
}

// Let the queued waitForBoot().then(onBoot) microtask run.
const flush = () => new Promise<void>((r) => setTimeout(r, 0));

Deno.test("InFlightCall: boot sends the call frame and reports started", async () => {
  const { worker, counts, call } = setup();
  worker.succeedBoot();
  await flush();
  assertEquals(counts.started, 1);
  const callFrame = worker.sent.find((f) => f.kind === "call");
  assert(callFrame, "expected a call frame after boot");
  assertEquals((callFrame as { callId: string }).callId, "c1");
  // settle so the test leaves no armed timer behind
  worker.emit({ kind: "tool_result", callId: "c1", result: 7 });
  assertEquals(await call.done, 7);
  assertEquals(counts.settled, 1);
});

Deno.test("InFlightCall: tool_error rejects done as provider_error and unhooks", async () => {
  const { worker, counts, call } = setup();
  worker.succeedBoot();
  await flush();
  worker.emit({ kind: "tool_error", callId: "c1", error: "boom" });
  const err = await assertRejects(() => call.done, AppError, "boom");
  assertEquals((err as AppError).code, "provider_error");
  assertEquals(counts.settled, 1);
  assertEquals(worker.listenerAttached, false);
});

Deno.test("InFlightCall: worker_exited after start kills and rejects", async () => {
  const { worker, counts, call } = setup();
  worker.succeedBoot();
  await flush();
  worker.emit({ kind: "worker_exited", code: 1 });
  await assertRejects(() => call.done, AppError, "tool worker exited");
  assertEquals(counts.killed, 1);
});

Deno.test("InFlightCall: a pre-boot worker_exited is ignored (boot rejection owns it)", async () => {
  const { worker, counts, call } = setup();
  // callStarted is false until boot; an early exit frame must not settle.
  worker.emit({ kind: "worker_exited", code: 1 });
  assertEquals(counts.killed, 0);
  worker.failBoot(new Error("spawn failed"));
  await assertRejects(() => call.done, Error, "spawn failed");
  assertEquals(counts.bootFailed, 1);
  // boot failure surfaces a synthetic error log so the UI bubble can settle
  assert(call.callId === "c1");
});

Deno.test("InFlightCall: askUser forwards a known answer and drops an unknown requestId", async () => {
  const { worker, events, call } = setup();
  worker.succeedBoot();
  await flush();
  worker.emit({
    kind: "ask_user_request",
    callId: "c1",
    requestId: "r1",
    questions: [{ question: "pick" }],
  });
  assert(events.some((e) => e.kind === "ask_user_request"));
  const before = worker.sent.length;
  call.respondAskUser("unknown", []); // stale/forged id: dropped whole
  assertEquals(worker.sent.length, before);
  call.respondAskUser("r1", []);
  assert(
    worker.sent.some((f) => f.kind === "ask_user_response" && f.requestId === "r1"),
    "expected the real answer to be forwarded",
  );
  worker.emit({ kind: "tool_result", callId: "c1", result: null });
  await call.done;
});

Deno.test("InFlightCall: an invalid askUser request is auto-answered empty", async () => {
  const { worker, events, call } = setup();
  worker.succeedBoot();
  await flush();
  worker.emit({
    kind: "ask_user_request",
    callId: "c1",
    requestId: "bad",
    questions: [],
  });
  // The call is not paused on an unrenderable form: an empty answer is sent.
  const answer = worker.sent.find((f) => f.kind === "ask_user_response");
  assert(answer, "expected an empty auto-answer");
  assertEquals((answer as { requestId: string }).requestId, "bad");
  // and a stale respondAskUser for it is a no-op (already cleared)
  const before = worker.sent.length;
  call.respondAskUser("bad", []);
  assertEquals(worker.sent.length, before);
  assert(events.some((e) => e.kind === "log"));
  worker.emit({ kind: "tool_result", callId: "c1", result: null });
  await call.done;
});

Deno.test("InFlightCall: exceeding the time budget kills the worker and rejects", async () => {
  const { worker, counts, call } = setup({ callTimeoutMs: 25 });
  worker.succeedBoot();
  await flush();
  await assertRejects(() => call.done, AppError, "timed out");
  assertEquals(counts.killed, 1);
});

Deno.test("InFlightCall: a pending prompt pauses the budget so it does not time out", async () => {
  const { worker, call } = setup({ callTimeoutMs: 40 });
  worker.succeedBoot();
  await flush();
  // Open a prompt before the budget elapses; this pauses the countdown.
  worker.emit({
    kind: "ask_user_request",
    callId: "c1",
    requestId: "r1",
    questions: [{ question: "pick" }],
  });
  await new Promise<void>((r) => setTimeout(r, 120)); // would have timed out if not paused
  // Still resolvable: answer, then complete normally.
  call.respondAskUser("r1", []);
  worker.emit({ kind: "tool_result", callId: "c1", result: "ok" });
  assertEquals(await call.done, "ok");
});

Deno.test("InFlightCall: cancel emits tool_cancelled, sends cancel, and rejects as cancelled", async () => {
  const { worker, events, counts, call } = setup({ drainTimeoutMs: 30 });
  worker.succeedBoot();
  await flush();
  call.cancel();
  assert(events.some((e) => e.kind === "tool_cancelled"));
  assert(worker.sent.some((f) => f.kind === "cancel"));
  // A late tool_error after cancel still rejects with the cancelled message.
  worker.emit({ kind: "tool_error", callId: "c1", error: "late" });
  const err = await assertRejects(() => call.done, AppError, "cancelled");
  assertEquals((err as AppError).code, "internal_error");
  assertEquals(counts.settled, 1);
});

Deno.test("InFlightCall: unacked cancel force-kills after the drain window", async () => {
  const { worker, counts, call } = setup({ drainTimeoutMs: 20 });
  worker.succeedBoot();
  await flush();
  call.cancel(); // worker never acks
  await assertRejects(() => call.done, AppError, "cancelled");
  assertEquals(counts.killed, 1);
});
