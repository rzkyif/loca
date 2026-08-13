import { assertEquals } from "@std/assert";
import { CallTimeout } from "./worker-call-timeout.ts";

const sleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));

Deno.test("CallTimeout: fires onExpire once after the budget elapses", async () => {
  let fired = 0;
  const t = new CallTimeout(25, () => fired++);
  t.arm();
  await sleep(150);
  assertEquals(fired, 1);
  assertEquals(t.armed, false);
});

Deno.test("CallTimeout: disarm cancels the pending fire", async () => {
  let fired = 0;
  const t = new CallTimeout(25, () => fired++);
  t.arm();
  t.disarm();
  await sleep(120);
  assertEquals(fired, 0);
});

Deno.test("CallTimeout: pause preserves the remaining budget across a later arm", async () => {
  let fired = 0;
  const t = new CallTimeout(60, () => fired++);
  t.arm();
  await sleep(20);
  t.pause(); // ~40ms remaining
  await sleep(150); // paused: must not fire
  assertEquals(fired, 0);
  t.arm(); // resume with the remaining budget
  await sleep(150);
  assertEquals(fired, 1);
});

Deno.test("CallTimeout: a non-positive budget makes arm a no-op", async () => {
  let fired = 0;
  const t = new CallTimeout(0, () => fired++);
  t.arm();
  assertEquals(t.armed, false);
  await sleep(60);
  assertEquals(fired, 0);
});
