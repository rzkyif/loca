import { assertEquals } from "@std/assert";
import { Semaphore } from "./semaphore.ts";

const microtask = () => Promise.resolve();

Deno.test("Semaphore: a single permit serializes acquirers (one active at a time)", async () => {
  const s = new Semaphore(1);
  await s.acquire();
  let secondGranted = false;
  const second = s.acquire().then(() => {
    secondGranted = true;
  });
  await microtask();
  assertEquals(secondGranted, false); // blocked while the first holds the permit
  s.release();
  await second;
  assertEquals(secondGranted, true);
});

Deno.test("Semaphore: grants up to `permits` immediately, queues the rest FIFO", async () => {
  const s = new Semaphore(2);
  const order: string[] = [];
  await s.acquire();
  order.push("a");
  await s.acquire();
  order.push("b");

  let cDone = false;
  let dDone = false;
  const c = s.acquire().then(() => {
    cDone = true;
    order.push("c");
  });
  const d = s.acquire().then(() => {
    dDone = true;
    order.push("d");
  });
  await microtask();
  assertEquals(cDone, false);
  assertEquals(dDone, false);

  s.release(); // hands the permit to the first waiter
  await c;
  assertEquals(order, ["a", "b", "c"]);
  assertEquals(dDone, false);

  s.release(); // then the second
  await d;
  assertEquals(order, ["a", "b", "c", "d"]);
});

Deno.test("Semaphore: release without a waiter frees a permit for the next acquire", async () => {
  const s = new Semaphore(1);
  await s.acquire();
  s.release();
  // No queued waiter; a fresh acquire resolves immediately.
  let granted = false;
  await s.acquire().then(() => {
    granted = true;
  });
  assertEquals(granted, true);
});
