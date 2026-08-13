// Shared fetch helper for the web tools: read a URL's text with a byte cap
// so a huge or endless response can't blow up the worker. Reading stops at
// the cap and reports `truncated` instead of erroring; an HTML parser copes
// fine with a cut-off document. All requests go through safeFetch, which
// blocks loopback/private targets and re-checks every redirect hop.

import { safeFetch } from "./net.ts";

// A few sites (DuckDuckGo's HTML endpoint included) reject requests that
// carry no User-Agent at all.
export const BROWSER_UA = "Mozilla/5.0 (compatible; loca/1.0; +https://loca.rzkyif.com)";

const FETCH_MAX_BYTES = 5 * 1024 * 1024;

export async function fetchTextCapped(
  url: string,
  init: RequestInit,
  maxBytes = FETCH_MAX_BYTES,
): Promise<{ text: string; finalUrl: string; contentType: string; truncated: boolean }> {
  const res = await safeFetch(url, init);
  if (!res.ok) throw new Error(`server returned ${res.status}`);
  const rawContentType = res.headers.get("content-type") ?? "";
  const contentType = rawContentType.split(";")[0]?.trim().toLowerCase() ?? "";
  const finalUrl = res.url || url;
  if (!res.body) return { text: "", finalUrl, contentType, truncated: false };

  const reader = res.body.getReader();
  const chunks: Uint8Array[] = [];
  let received = 0;
  let truncated = false;
  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    chunks.push(value);
    received += value.byteLength;
    if (received >= maxBytes) {
      truncated = true;
      await reader.cancel().catch(() => {});
      break;
    }
  }
  const all = new Uint8Array(received);
  let offset = 0;
  for (const c of chunks) {
    all.set(c, offset);
    offset += c.byteLength;
  }
  return {
    text: decodeBody(all, rawContentType),
    finalUrl,
    contentType,
    truncated,
  };
}

// Decode response bytes with the declared charset rather than assuming UTF-8,
// so a Latin-1 / Shift_JIS / etc. page comes through as real text instead of
// mojibake. Charset resolution order: the Content-Type header, then an HTML
// `<meta charset>` (or `<meta http-equiv>`) sniff, then UTF-8. An unknown
// label falls back to UTF-8 (TextDecoder throws on labels it can't honor).
function decodeBody(bytes: Uint8Array, rawContentType: string): string {
  const label = charsetFromContentType(rawContentType) ?? sniffHtmlCharset(bytes) ?? "utf-8";
  try {
    return new TextDecoder(label).decode(bytes);
  } catch {
    return new TextDecoder("utf-8").decode(bytes);
  }
}

function charsetFromContentType(rawContentType: string): string | null {
  const m = rawContentType.match(/charset\s*=\s*"?([^";]+)"?/i);
  return m ? m[1].trim().toLowerCase() : null;
}

// Scan the first slice for a charset declaration. The meta tag is ASCII in
// the byte range we care about, so decoding that slice as Latin-1 (every byte
// maps to a code point) is a safe way to read it without knowing the charset
// yet.
function sniffHtmlCharset(bytes: Uint8Array): string | null {
  const head = new TextDecoder("latin1").decode(bytes.subarray(0, 2048));
  const metaCharset = head.match(/<meta[^>]+charset\s*=\s*["']?([\w-]+)/i);
  if (metaCharset) return metaCharset[1].toLowerCase();
  const httpEquiv = head.match(/<meta[^>]+content\s*=\s*["'][^"']*charset=([\w-]+)/i);
  return httpEquiv ? httpEquiv[1].toLowerCase() : null;
}
