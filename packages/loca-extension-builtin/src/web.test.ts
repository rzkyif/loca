// fetchTextCapped charset handling: bytes are decoded with the declared
// charset (header, then HTML meta sniff, then UTF-8) instead of assuming
// UTF-8, so a non-UTF-8 page reads as real text rather than mojibake.

import { assertEquals } from "@std/assert";
import { fetchTextCapped } from "./web.ts";

// "café" in Latin-1: the é is a single 0xE9 byte, which UTF-8 would mangle.
const LATIN1_CAFE = new Uint8Array([0x63, 0x61, 0x66, 0xe9]);

function installFakeFetch(body: BodyInit, headers: Record<string, string>): () => void {
  const original = globalThis.fetch;
  globalThis.fetch = (() =>
    Promise.resolve(new Response(body, { status: 200, headers }))) as typeof fetch;
  return () => {
    globalThis.fetch = original;
  };
}

Deno.test("fetchTextCapped: decodes using the Content-Type charset", async () => {
  const restore = installFakeFetch(LATIN1_CAFE, {
    "content-type": "text/html; charset=iso-8859-1",
  });
  try {
    const { text } = await fetchTextCapped("https://example.com/p", {});
    assertEquals(text, "café");
  } finally {
    restore();
  }
});

Deno.test("fetchTextCapped: falls back to an HTML <meta charset> sniff", async () => {
  const head = new TextEncoder().encode('<meta charset="iso-8859-1">');
  const bytes = new Uint8Array([...head, ...LATIN1_CAFE]);
  // No charset in the header, so the meta tag is the only signal.
  const restore = installFakeFetch(bytes, { "content-type": "text/html" });
  try {
    const { text } = await fetchTextCapped("https://example.com/p", {});
    assertEquals(text.endsWith("café"), true);
  } finally {
    restore();
  }
});

Deno.test("fetchTextCapped: an unknown charset label falls back to UTF-8", async () => {
  const utf8 = new TextEncoder().encode("résumé");
  const restore = installFakeFetch(utf8, {
    "content-type": "text/plain; charset=totally-not-a-charset",
  });
  try {
    const { text } = await fetchTextCapped("https://example.com/p", {});
    assertEquals(text, "résumé");
  } finally {
    restore();
  }
});

Deno.test("fetchTextCapped: defaults to UTF-8 when nothing declares a charset", async () => {
  const utf8 = new TextEncoder().encode("plain café");
  const restore = installFakeFetch(utf8, { "content-type": "text/plain" });
  try {
    const { text } = await fetchTextCapped("https://example.com/p", {});
    assertEquals(text, "plain café");
  } finally {
    restore();
  }
});
