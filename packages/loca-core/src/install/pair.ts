// Mint a first pairing code from the freshly-started core.
//
// Runs on the core host after install-service: it waits for the daemon to bind
// its port, then mints a pairing code authenticated by the on-disk admin token.
// Trust for the self-signed loopback API comes from the cert core writes to
// paths().tlsCertFile at boot (a CA-of-one), so TLS verification stays on even
// against loopback. The result is one JSON line on stdout so a headless setup
// script can parse it (a human just reads the code).

import { corePort, paths } from "../paths.ts";
import { emitJson, progress } from "./io.ts";

const HEALTH_POLL_ATTEMPTS = 30;
const MINT_ATTEMPTS = 5;
// Per-probe deadline. The core answers over loopback in milliseconds, so a
// probe that hasn't returned in a few seconds is a stalled connection, not a
// slow one; abort it so a single hung request can't wedge the (otherwise
// bounded) retry loop forever.
const PROBE_TIMEOUT_MS = 5000;

export interface MintResult {
  code: string;
  url: string;
  port: number;
}

export async function mintCode(): Promise<MintResult> {
  const port = corePort();
  const base = `https://127.0.0.1:${port}`;
  // core writes its public cert to tlsCertFile asynchronously at boot, so a
  // freshly-started core can lose the race with this short-lived CLI. Resolve
  // the CA-of-one INSIDE the health wait, retrying until the cert lands, rather
  // than reading it once up front (which would leave `client` undefined and make
  // every TLS health probe fail verification).
  const client = await waitForHealth(base);

  const admin = (await Deno.readTextFile(paths().adminTokenFile)).trim();
  if (!admin) throw new Error(`admin token missing or empty at ${paths().adminTokenFile}`);

  for (let i = 0; i < MINT_ATTEMPTS; i++) {
    try {
      const res = await fetch(`${base}/api/v1/pairing/codes`, {
        method: "POST",
        headers: { "X-Admin-Token": admin, "Content-Type": "application/json" },
        body: "{}",
        client,
        signal: AbortSignal.timeout(PROBE_TIMEOUT_MS),
      });
      if (res.ok) {
        const body = (await res.json()) as { code?: string };
        if (body.code) {
          const result: MintResult = { code: body.code, url: base, port };
          emitJson(result);
          return result;
        }
      } else {
        await res.body?.cancel();
      }
    } catch {
      // The port is bound (health passed) but the pairing route can lag the
      // bind by a moment on a cold start; retry.
    }
    await sleep(1000);
  }
  throw new Error("could not mint a pairing code (core did not respond in time)");
}

// Build an HTTP client that trusts core's self-signed cert via the PEM core
// wrote at boot. Returns undefined until the cert lands on disk (core writes it
// asynchronously early in boot), so callers retry rather than falling back to an
// unverified request.
async function loopbackClient(): Promise<Deno.HttpClient | undefined> {
  try {
    const pem = await Deno.readTextFile(paths().tlsCertFile);
    if (!pem.trim()) return undefined;
    return Deno.createHttpClient({ caCerts: [pem] });
  } catch {
    return undefined;
  }
}

// Wait for the daemon to answer /health over TLS, resolving the CA-of-one on the
// way. The cert read is retried INSIDE the loop because core writes tlsCertFile
// asynchronously at boot: reading it once up front would race a cold start and
// leave every probe unverifiable. Returns the trusting client so the mint reuses it.
async function waitForHealth(base: string): Promise<Deno.HttpClient> {
  let client: Deno.HttpClient | undefined;
  let warnedNoCert = false;
  for (let i = 0; i < HEALTH_POLL_ATTEMPTS; i++) {
    if (!client) {
      client = await loopbackClient();
      if (!client) {
        if (!warnedNoCert && i > 3) {
          progress("waiting for core's TLS cert to appear on disk...");
          warnedNoCert = true;
        }
        await sleep(1000);
        continue;
      }
    }
    try {
      const res = await fetch(`${base}/api/v1/health`, {
        client,
        signal: AbortSignal.timeout(PROBE_TIMEOUT_MS),
      });
      await res.body?.cancel();
      if (res.ok) return client;
    } catch {
      // not up yet
    }
    await sleep(1000);
  }
  throw new Error(`core did not become healthy at ${base} within ${HEALTH_POLL_ATTEMPTS}s`);
}

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}
