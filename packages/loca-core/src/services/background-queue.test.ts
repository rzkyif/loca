// BackgroundQueue: key dedupe, FIFO single-active draining, the idle gate
// (busy -> poll, quiet period after busy), and dispose dropping the queue.
// Uses short real delays via the constructor's test seams.

import { assertEquals } from "@std/assert";
import { BackgroundQueue } from "./background-queue.ts";

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}

Deno.test("queue: dedupes by key and drains FIFO one at a time", async () => {
  const q = new BackgroundQueue({
    isBusy: () => false,
    quietPeriodMs: 0,
    busyPollMs: 5,
  });
  const ran: string[] = [];
  let concurrent = 0;
  let maxConcurrent = 0;
  const job = (key: string) => ({
    key,
    run: async () => {
      concurrent++;
      maxConcurrent = Math.max(maxConcurrent, concurrent);
      await sleep(5);
      ran.push(key);
      concurrent--;
    },
  });
  q.enqueue(job("a"));
  q.enqueue(job("b"));
  q.enqueue(job("a")); // duplicate while waiting: dropped
  assertEquals(q.size(), 2);
  await sleep(60);
  assertEquals(ran, ["a", "b"]);
  assertEquals(maxConcurrent, 1);
  q.dispose();
});

Deno.test("queue: waits out busy streams plus the quiet period", async () => {
  let busy = true;
  // Margins sized well above the ~16ms Windows timer granularity: the
  // inside-the-quiet-period probe fires 70ms before the earliest drain.
  const q = new BackgroundQueue({
    isBusy: () => busy,
    quietPeriodMs: 100,
    busyPollMs: 5,
  });
  const ran: string[] = [];
  q.enqueue({
    key: "job",
    run: () => {
      ran.push("job");
      return Promise.resolve();
    },
  });
  await sleep(40);
  assertEquals(ran, []); // still busy
  busy = false;
  await sleep(30);
  assertEquals(ran, []); // idle, but inside the quiet period
  await sleep(250);
  assertEquals(ran, ["job"]);
  q.dispose();
});

Deno.test("queue: a failing job does not stop the drain", async () => {
  const q = new BackgroundQueue({
    isBusy: () => false,
    quietPeriodMs: 0,
    busyPollMs: 5,
  });
  const ran: string[] = [];
  q.enqueue({ key: "boom", run: () => Promise.reject(new Error("boom")) });
  q.enqueue({
    key: "after",
    run: () => {
      ran.push("after");
      return Promise.resolve();
    },
  });
  await sleep(40);
  assertEquals(ran, ["after"]);
  q.dispose();
});

Deno.test("queue: dispose drops queued jobs and blocks new ones", async () => {
  const q = new BackgroundQueue({
    isBusy: () => false,
    quietPeriodMs: 0,
    busyPollMs: 5,
  });
  const ran: string[] = [];
  q.enqueue({
    key: "dropped",
    run: () => {
      ran.push("dropped");
      return Promise.resolve();
    },
  });
  q.dispose();
  q.enqueue({
    key: "late",
    run: () => {
      ran.push("late");
      return Promise.resolve();
    },
  });
  assertEquals(q.size(), 0);
  await sleep(20);
  assertEquals(ran, []);
});
