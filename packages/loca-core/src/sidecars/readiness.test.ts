// Critical-path tests for the sidecar readiness helpers. validateHealthCheckUrl
// is the gate that prevents a config-injected sidecar URL from pointing at an
// arbitrary host; pollHttpHealth is the boot orchestrator's "is it up yet"
// loop. sleep() has an abort path that's easy to regress.

import { assertEquals, assertRejects, assertThrows } from "@std/assert";
import { AppError } from "@loca/core-engine";
import { pollHttpHealth, sleep, validateHealthCheckUrl } from "./readiness.ts";

Deno.test("validateHealthCheckUrl: accepts loopback http URLs", () => {
  validateHealthCheckUrl("http://127.0.0.1:8080/health");
  validateHealthCheckUrl("http://localhost:1234/ping");
});

Deno.test("validateHealthCheckUrl: rejects non-http schemes", () => {
  assertThrows(() => validateHealthCheckUrl("https://127.0.0.1/health"), AppError, "must use http");
  assertThrows(() => validateHealthCheckUrl("file:///etc/passwd"), AppError, "must use http");
});

Deno.test("validateHealthCheckUrl: rejects non-loopback hostnames", () => {
  assertThrows(
    () => validateHealthCheckUrl("http://evil.example/health"),
    AppError,
    "127.0.0.1 or localhost",
  );
  assertThrows(
    () => validateHealthCheckUrl("http://10.0.0.1/health"),
    AppError,
    "127.0.0.1 or localhost",
  );
});

Deno.test("validateHealthCheckUrl: rejects malformed URLs", () => {
  assertThrows(() => validateHealthCheckUrl("not a url"), AppError, "invalid health-check");
});

Deno.test("pollHttpHealth: returns true when the endpoint is immediately 200", async () => {
  const abort = new AbortController();
  // Spin up a one-shot 200 server on an ephemeral port.
  const server = Deno.serve(
    { port: 0, hostname: "127.0.0.1", signal: abort.signal },
    () => new Response("ok", { status: 200 }),
  );
  try {
    const port = (server.addr as Deno.NetAddr).port;
    const ok = await pollHttpHealth(`http://127.0.0.1:${port}/health`, {
      attempts: 1,
      intervalMs: 1,
    });
    assertEquals(ok, true);
  } finally {
    abort.abort();
    await server.finished.catch(() => {});
  }
});

Deno.test("pollHttpHealth: returns false when the endpoint is unreachable for the whole budget", async () => {
  // Loopback port 1 is almost guaranteed to be closed; 3 attempts at 5ms = 15ms.
  const ok = await pollHttpHealth("http://127.0.0.1:1/health", {
    attempts: 3,
    intervalMs: 5,
  });
  assertEquals(ok, false);
});

Deno.test("pollHttpHealth: short-circuits when the signal is already aborted", async () => {
  const controller = new AbortController();
  controller.abort();
  const ok = await pollHttpHealth("http://127.0.0.1:1/health", {
    signal: controller.signal,
  });
  assertEquals(ok, false);
});

Deno.test("sleep: rejects with 'aborted' when the signal fires mid-wait", async () => {
  const controller = new AbortController();
  setTimeout(() => controller.abort(), 5);
  await assertRejects(() => sleep(1_000, controller.signal), Error, "aborted");
});

Deno.test("sleep: rejects immediately when the signal is already aborted", async () => {
  const controller = new AbortController();
  controller.abort();
  await assertRejects(() => sleep(100, controller.signal), Error, "aborted");
});
