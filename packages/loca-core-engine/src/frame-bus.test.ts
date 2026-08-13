import { assertEquals } from "@std/assert";
import { FrameBus } from "./frame-bus.ts";
import type { ServerToClientFrame } from "@loca/shared";

// A minimal frame shape is enough here; the bus only serializes and routes.
const frame = (id: string): ServerToClientFrame =>
  ({ kind: "core.status", status: id }) as unknown as ServerToClientFrame;

Deno.test("broadcastToClient reaches only that client's sinks", () => {
  const bus = new FrameBus();
  const a: string[] = [];
  const b: string[] = [];
  const connA = bus.registerConnection("a");
  const connB = bus.registerConnection("b");
  connA.subscribe((f) => a.push(f));
  connB.subscribe((f) => b.push(f));

  bus.broadcastToClient("a", frame("x"));

  assertEquals(a.length, 1);
  assertEquals(b.length, 0);
  assertEquals(JSON.parse(a[0]).status, "x");
});

Deno.test("broadcastAll reaches every connection", () => {
  const bus = new FrameBus();
  let count = 0;
  bus.registerConnection("a").subscribe(() => count++);
  bus.registerConnection("b").subscribe(() => count++);

  bus.broadcastAll(frame("y"));

  assertEquals(count, 2);
});

Deno.test("inbound frames dispatch to the wired handler", () => {
  const bus = new FrameBus();
  const seen: Array<[string, string]> = [];
  bus.onInbound((clientId, f) => seen.push([clientId, f]));
  const conn = bus.registerConnection("a");

  conn.send('{"kind":"ping"}');

  assertEquals(seen, [["a", '{"kind":"ping"}']]);
});

Deno.test("close detaches a connection so it stops receiving", () => {
  const bus = new FrameBus();
  const got: string[] = [];
  const conn = bus.registerConnection("a");
  conn.subscribe((f) => got.push(f));

  conn.close();
  bus.broadcastToClient("a", frame("z"));

  assertEquals(got.length, 0);
});
