// Shared helpers for the external (OpenAI-compatible) provider paths used by
// the LLM, STT, and TTS modules. Centralizes API-key resolution so the three
// modules treat keyless loopback gateways and missing keys identically.

import { getSecret } from "./secrets.ts";
import { strSetting } from "./settings-access.ts";

/** Request timeout for external STT/TTS calls, matching the local sidecar's
 *  120s bound. Never pass `timeout: 0` to the OpenAI SDK: it treats 0 as a
 *  literal 0ms deadline and aborts every request instantly. */
export const EXTERNAL_TIMEOUT_MS = 120_000;

/** True for a loopback base URL (the only host SECURE_URL_VALIDATION lets run
 *  over plain HTTP). Local gateways here often need no API key. */
export function isLocalhostUrl(url: string): boolean {
  return /^http:\/\/(localhost|127\.0\.0\.1)(:|\/|$)/.test(url);
}

/** Resolve the API key for an external endpoint. The vault is authoritative; a
 *  plaintext value in settings.json is a lower-precedence fallback (headless
 *  setups). Returns a harmless placeholder for a keyless loopback gateway (so
 *  the SDK builds and the "is it configured" checks pass), and "" when a
 *  non-loopback host has no key. The caller decides whether "" is fatal: the
 *  chat path reports it with a friendly message, STT/TTS reject it outright. */
export async function resolveExternalApiKey(
  settings: Record<string, unknown>,
  keyId: string,
  baseUrl: string,
): Promise<string> {
  const apiKey = (await getSecret(keyId)) || strSetting(settings, keyId, "") || "";
  if (apiKey) return apiKey;
  return isLocalhostUrl(baseUrl) ? "sk-noauth" : "";
}
