// SpeechScheduler: the single-slot semaphore, per-client round-robin fairness,
// and the queue-depth `server_busy` guard.

import { assertEquals, assertRejects } from "@std/assert";
import { SpeechScheduler } from "./speech-scheduler.ts";
import { AppError } from "@loca/core-engine";

// A controllable task: resolves only when its release fn is called, so the test
// can hold a slot and inspect queue behavior deterministically.
function gate() {
  let release!: () => void;
  const promise = new Promise<void>((r) => (release = r));
  return { promise, release };
}

Deno.test("speech-scheduler: serializes to one slot", async () => {
  const s = new SpeechScheduler();
  const order: string[] = [];
  const g1 = gate();

  const p1 = s.schedule("a", async () => {
    order.push("start1");
    await g1.promise;
    order.push("end1");
  });
  const p2 = s.schedule("a", () => {
    order.push("start2");
    return Promise.resolve();
  });

  // p2 must not start until p1 releases the only slot.
  await Promise.resolve();
  assertEquals(order, ["start1"]);
  g1.release();
  await Promise.all([p1, p2]);
  assertEquals(order, ["start1", "end1", "start2"]);
});

Deno.test("speech-scheduler: round-robin across clients", async () => {
  const s = new SpeechScheduler();
  const ran: string[] = [];
  const g0 = gate();

  // Hold the slot with a first task for client a.
  const held = s.schedule("a", async () => {
    await g0.promise;
  });
  // Queue: a, a, b. Round-robin should dispatch a, then b, then a (fairness),
  // i.e. not drain all of a before b.
  const q: Array<Promise<void>> = [];
  q.push(s.schedule("a", () => void ran.push("a2") as void));
  q.push(s.schedule("a", () => void ran.push("a3") as void));
  q.push(s.schedule("b", () => void ran.push("b1") as void));

  g0.release();
  await Promise.all([held, ...q]);
  // After the held 'a' task, the next dispatched client rotates to b before
  // the remaining a's: first queued a, then b, then last a.
  assertEquals(ran, ["a2", "b1", "a3"]);
});

Deno.test("speech-scheduler: server_busy when queue saturated", async () => {
  const s = new SpeechScheduler();
  const g = gate();
  // 1 slot, maxQueueDepth = 4. Hold the slot, then fill the queue to depth 4.
  const held = s.schedule("a", async () => {
    await g.promise;
  });
  const queued: Array<Promise<unknown>> = [];
  for (let i = 0; i < 4; i++) {
    queued.push(s.schedule("a", () => Promise.resolve()).catch(() => {}));
  }
  // The 5th exceeds the depth cap and rejects immediately.
  await assertRejects(
    () => s.schedule("a", () => Promise.resolve()),
    AppError,
    "speech queue full",
  );
  g.release();
  await Promise.all([held, ...queued]);
});
