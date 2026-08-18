// Bootstrap config + URLs.
// All hardcoded URLs live here so changing the hostnames later is a one-file
// edit.

import { channel, corePort } from "./paths.ts";

export const CORE_VERSION = "0.1.15";

// Two-host layout:
//
//   WEBSITE_BASE_URL  is the landing page (Cloudflare Worker / Astro static
//                       assets). Nothing release-related lives here; the core
//                       runtime does not consume this URL.
//   STORAGE_BASE_URL  is the public R2 bucket holding every release artifact:
//                       /<version>/<triple>/<file>, /manifests/*.json,
//                       /install/*, /schemas/*.
export const WEBSITE_BASE_URL = "https://loca.rzkyif.com";
// The release-artifact host. Overridable via LOCA_STORAGE_BASE_URL so the E2E
// harness can point catalog / manifest / binary fetches at a local mock without
// touching the real R2 bucket. Production leaves the env unset.
export const STORAGE_BASE_URL =
  Deno.env.get("LOCA_STORAGE_BASE_URL") ?? "https://get.loca.rzkyif.com";
// The HuggingFace host model-file downloads resolve against. Overridable via
// LOCA_HF_BASE_URL for the same test-isolation reason (see downloads/sources.ts).
export const HF_BASE_URL = Deno.env.get("LOCA_HF_BASE_URL") ?? "https://huggingface.co";
// The GitHub API host upstream sidecar releases are resolved against.
// Overridable for the same reason as the two above: without it
// GET /api/v1/binaries/check and POST /api/v1/binaries/probe reach the real
// api.github.com during a conformance run, which contradicts the harness's
// stated guarantee that every base URL points at 127.0.0.1 so a core that
// escapes fails its cases instead of quietly succeeding.
export const GITHUB_API_BASE_URL =
  (Deno.env.get("LOCA_GITHUB_API_BASE_URL") ?? "").replace(/\/+$/, "") || "https://api.github.com";

// Manifests live under a per-channel path segment so latest/dev publish + fetch
// their own signed manifests without touching stable's. Stable stays bare
// (manifests/core.json) for back-compat; dev/latest nest (manifests/latest/...).
function manifestDir(): string {
  const ch = channel();
  return ch === "stable" ? "manifests" : `manifests/${ch}`;
}

export function coreManifestUrl(): string {
  return `${STORAGE_BASE_URL}/${manifestDir()}/core.json`;
}

export function binaryManifestUrl(): string {
  return `${STORAGE_BASE_URL}/${manifestDir()}/binaries.json`;
}

// Signed manifest for the CDN-distributed built-in extension, nested per channel
// alongside core.json / binaries.json.
export function builtinExtensionManifestUrl(): string {
  return `${STORAGE_BASE_URL}/${manifestDir()}/extension.json`;
}

// Signed model catalog (hand-authored in @loca/model-catalog, compiled +
// published by `deno task release`). The core fetches + verifies it and
// runs the on-device fit engine against it. Nested per channel like the others.
export function modelsCatalogUrl(): string {
  return `${STORAGE_BASE_URL}/${manifestDir()}/catalog.json`;
}

// Schemas (loca-v1.json) are channel-independent: one published copy.
export const SCHEMAS_BASE_URL = `${STORAGE_BASE_URL}/schemas`;

// Default HTTP bind. Override via LOCA_CORE_HOST / LOCA_CORE_PORT env.
// DEFAULT_PORT is the stable base; loadBootConfig() applies the per-channel
// offset via corePort() so latest/dev cores bind a distinct port by default.
export const DEFAULT_HOST = "127.0.0.1";
export const DEFAULT_PORT = 7800;

export interface BootConfig {
  host: string;
  port: number;
  version: string;
}

export function loadBootConfig(): BootConfig {
  const portStr = Deno.env.get("LOCA_CORE_PORT");
  const port = portStr ? Number(portStr) : corePort();
  if (!Number.isInteger(port) || port < 1 || port > 65535) {
    throw new Error(`invalid LOCA_CORE_PORT: ${portStr}`);
  }
  const host = Deno.env.get("LOCA_CORE_HOST") ?? DEFAULT_HOST;
  return { host, port, version: CORE_VERSION };
}
