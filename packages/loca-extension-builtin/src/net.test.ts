// Network-safety guard: assertSafePublicUrl must reject loopback /
// link-local / private targets (incl. the cloud-metadata IP) and accept
// ordinary public hosts, and safeFetch must re-check a redirect hop.

import { assertEquals, assertRejects, assertThrows } from "@std/assert";
import { assertSafePublicUrl, safeFetch } from "./net.ts";

Deno.test("assertSafePublicUrl: rejects non-http(s) schemes", () => {
  for (const url of ["ftp://example.com", "file:///etc/passwd", "javascript:alert(1)"]) {
    assertThrows(() => assertSafePublicUrl(url), Error, "only http(s)");
  }
});

Deno.test("assertSafePublicUrl: rejects loopback / private / link-local hosts", () => {
  const blocked = [
    "http://localhost/x",
    "http://127.0.0.1/x",
    "http://127.5.5.5/x",
    "http://10.0.0.1/x",
    "http://172.16.0.1/x",
    "http://172.31.255.255/x",
    "http://192.168.1.1/x",
    "http://169.254.169.254/latest/meta-data/", // cloud metadata
    "http://100.64.0.1/x", // CGNAT
    "http://[::1]/x",
    "http://[fe80::1]/x",
    "http://[fd00::1]/x",
    "http://service.local/x",
    "http://api.internal/x",
  ];
  for (const url of blocked) {
    assertThrows(() => assertSafePublicUrl(url), Error, undefined, `should block ${url}`);
  }
});

Deno.test("assertSafePublicUrl: allows ordinary public hosts and public IPs", () => {
  for (const url of ["https://example.com/x", "http://93.184.216.34/x", "https://1.1.1.1/"]) {
    assertEquals(assertSafePublicUrl(url).protocol.startsWith("http"), true);
  }
});

Deno.test("assertSafePublicUrl: 172.x outside 16-31 is public", () => {
  assertEquals(assertSafePublicUrl("http://172.15.0.1/x").hostname, "172.15.0.1");
  assertEquals(assertSafePublicUrl("http://172.32.0.1/x").hostname, "172.32.0.1");
  assertThrows(() => assertSafePublicUrl("http://172.20.0.1/x"), Error);
});

Deno.test("safeFetch: re-checks a redirect hop and blocks an internal target", async () => {
  const original = globalThis.fetch;
  globalThis.fetch = ((input: RequestInfo | URL) => {
    const url = typeof input === "string" ? input : input.toString();
    if (url.startsWith("https://example.com")) {
      return Promise.resolve(
        new Response(null, {
          status: 302,
          headers: { location: "http://169.254.169.254/" },
        }),
      );
    }
    return Promise.resolve(new Response("should not reach here", { status: 200 }));
  }) as typeof fetch;
  try {
    await assertRejects(() => safeFetch("https://example.com/start"), Error, "private");
  } finally {
    globalThis.fetch = original;
  }
});

Deno.test("safeFetch: follows a redirect to another public host", async () => {
  const original = globalThis.fetch;
  globalThis.fetch = ((input: RequestInfo | URL) => {
    const url = typeof input === "string" ? input : input.toString();
    if (url === "https://example.com/start") {
      return Promise.resolve(
        new Response(null, {
          status: 301,
          headers: { location: "https://example.org/dest" },
        }),
      );
    }
    return Promise.resolve(new Response("ok", { status: 200 }));
  }) as typeof fetch;
  try {
    const res = await safeFetch("https://example.com/start");
    assertEquals(res.status, 200);
    assertEquals(await res.text(), "ok");
  } finally {
    globalThis.fetch = original;
  }
});
