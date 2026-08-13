// Network-safety helpers shared by the web/download tools. Tomat is a
// local-first app, so a tool that fetches an arbitrary URL on the model's
// behalf must not be steerable into reaching loopback, link-local (incl. the
// 169.254.169.254 cloud-metadata endpoint), or private-range hosts. The
// guard runs on the initial URL AND on every redirect hop, since a public
// URL can 30x into an internal one.

const LOOPBACK_HOSTNAMES = new Set([
  "localhost",
  "localhost.localdomain",
  "ip6-localhost",
  "ip6-loopback",
]);

/** Parse + validate a URL for outbound fetching. Throws on a non-http(s)
 *  scheme or a host that names a loopback / link-local / private target.
 *  Returns the parsed URL on success. */
export function assertSafePublicUrl(raw: string): URL {
  let u: URL;
  try {
    u = new URL(raw);
  } catch {
    throw new Error("invalid URL");
  }
  if (u.protocol !== "http:" && u.protocol !== "https:") {
    throw new Error("only http(s) URLs are allowed");
  }
  // new URL keeps IPv6 hosts bracketed; strip for the literal check.
  const host = u.hostname.toLowerCase().replace(/^\[|\]$/g, "");
  if (LOOPBACK_HOSTNAMES.has(host) || host.endsWith(".local") || host.endsWith(".internal")) {
    throw new Error("refusing to fetch a loopback or internal host");
  }
  if (isPrivateIp(host)) {
    throw new Error("refusing to fetch a private, loopback, or link-local address");
  }
  return u;
}

function isPrivateIp(host: string): boolean {
  const v4 = host.match(/^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/);
  if (v4) {
    const a = Number(v4[1]);
    const b = Number(v4[2]);
    if ([a, b, Number(v4[3]), Number(v4[4])].some((n) => n > 255)) return true; // malformed, reject
    if (a === 0 || a === 127) return true; // "this network" + loopback
    if (a === 10) return true; // RFC1918
    if (a === 172 && b >= 16 && b <= 31) return true; // RFC1918
    if (a === 192 && b === 168) return true; // RFC1918
    if (a === 169 && b === 254) return true; // link-local incl. metadata
    if (a === 100 && b >= 64 && b <= 127) return true; // CGNAT (RFC6598)
    return false;
  }
  if (host.includes(":")) {
    if (host === "::1" || host === "::") return true; // loopback / unspecified
    if (host.startsWith("fe80")) return true; // link-local
    if (host.startsWith("fc") || host.startsWith("fd")) return true; // unique-local
    const mapped = host.match(/^::ffff:(\d+\.\d+\.\d+\.\d+)$/);
    if (mapped) return isPrivateIp(mapped[1]); // IPv4-mapped
    return false;
  }
  return false;
}

/** fetch() that validates the target and re-validates every redirect hop.
 *  Deno (unlike browsers) exposes the Location header on a manual-redirect
 *  response, so we follow hops ourselves and SSRF-check each one. A combined
 *  timeout + caller-signal aborts a stalled connection. */
export async function safeFetch(
  url: string,
  init: RequestInit = {},
  opts: { maxRedirects?: number; timeoutMs?: number } = {},
): Promise<Response> {
  const maxRedirects = opts.maxRedirects ?? 5;
  const timeoutMs = opts.timeoutMs ?? 30_000;
  let current = assertSafePublicUrl(url).toString();
  const signal = init.signal
    ? AbortSignal.any([init.signal, AbortSignal.timeout(timeoutMs)])
    : AbortSignal.timeout(timeoutMs);
  for (let hop = 0; hop <= maxRedirects; hop++) {
    const res = await fetch(current, { ...init, signal, redirect: "manual" });
    if (res.status >= 300 && res.status < 400 && res.headers.has("location")) {
      const loc = res.headers.get("location")!;
      await res.body?.cancel().catch(() => {});
      current = assertSafePublicUrl(new URL(loc, current).toString()).toString();
      continue;
    }
    return res;
  }
  throw new Error("too many redirects");
}
