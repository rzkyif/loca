// Model-catalog fetch + signature verification.
//
// Mirrors binaries/manifest.ts: fetch the signed catalog.json, verify its
// Ed25519 signature against the baked-in public key (whole payload minus
// `signature`, canonicalized, like core.json), and cache the verified copy at
// ~/.loca/<channel>/core/cache/model-catalog.json. The cache is re-verified on
// every read (a perf optimization, not a trust anchor) and lets re-check work
// offline against the last good fetch.

import { verifyAsync } from "@noble/ed25519";
import { canonicalize, decodeBase64, errMessage, modelCatalogSchema } from "@loca/shared";
import type { CatalogPayload, ModelCatalog } from "@loca/shared";
import { join } from "@std/path";
import { modelsCatalogUrl } from "../config.ts";
import { channel, paths } from "../paths.ts";
import { AppError } from "@loca/core-engine";
import { getLogger } from "../shared/log.ts";
import signingKeys from "../../data/signing-keys.json" with { type: "json" };

const log = getLogger("catalog");

export interface FetchCatalogOptions {
  force?: boolean;
  signal?: AbortSignal;
}

/** Load the catalog: cached-and-verified unless `force`, else fetch + verify +
 *  cache. Throws AppError on fetch/parse/signature failure. */
export async function loadModelCatalog(opts: FetchCatalogOptions = {}): Promise<ModelCatalog> {
  // Dev runs from source: there is no published manifests/dev/catalog.json, so
  // compile the in-repo @loca/model-catalog families directly. The from-source
  // build is the trust anchor, so no signature in dev (mirrors the dev binary +
  // extension manifests).
  if (channel() === "dev") return await devCatalog();
  if (!opts.force) {
    const cached = await readCachedCatalog();
    if (cached) return cached;
  }
  const fetched = await fetchAndVerify(opts.signal);
  await writeCachedCatalog(fetched);
  return fetched;
}

async function devCatalog(): Promise<ModelCatalog> {
  // Dynamic import so this only loads on the dev channel. The authoring package
  // is tiny (model metadata), so any bundling under `deno compile` is harmless.
  const { buildCatalogPayload } = await import("@loca/model-catalog");
  const payload = buildCatalogPayload(new Date().toISOString());
  return { ...payload, signature: "" };
}

async function fetchAndVerify(signal?: AbortSignal): Promise<ModelCatalog> {
  let res: Response;
  try {
    res = await fetch(modelsCatalogUrl(), { signal });
  } catch (err) {
    throw new AppError(
      "manifest_fetch_failed",
      `failed to fetch model catalog: ${errMessage(err)}`,
    );
  }
  if (!res.ok) {
    throw new AppError(
      "manifest_fetch_failed",
      `model catalog HTTP ${res.status} ${res.statusText}`,
    );
  }
  const raw = await res.text();
  return await parseAndVerify(raw);
}

async function parseAndVerify(raw: string): Promise<ModelCatalog> {
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch (err) {
    throw new AppError("manifest_fetch_failed", `invalid catalog JSON: ${errMessage(err)}`);
  }
  const result = modelCatalogSchema.safeParse(parsed);
  if (!result.success) {
    throw new AppError("manifest_fetch_failed", `catalog schema invalid: ${result.error.message}`);
  }
  const catalog = result.data;
  await verifyCatalogSignature(catalog);
  return catalog;
}

async function verifyCatalogSignature(catalog: ModelCatalog): Promise<void> {
  const { signature, ...payload } = catalog;
  const pk = decodeBase64(signingKeys.publicKey);
  const sig = decodeBase64(signature);
  const message = new TextEncoder().encode(canonicalize(payload));
  const ok = await verifyAsync(sig, message, pk);
  if (!ok) {
    throw new AppError("signature_invalid", "model catalog signature verification failed");
  }
}

async function readCachedCatalog(): Promise<ModelCatalog | null> {
  try {
    const text = await Deno.readTextFile(cachePath());
    return await parseAndVerify(text);
  } catch (err) {
    if (!(err instanceof Deno.errors.NotFound)) {
      log.warn(`ignoring unreadable/invalid catalog cache: ${errMessage(err)}`);
    }
    return null;
  }
}

async function writeCachedCatalog(c: ModelCatalog): Promise<void> {
  await Deno.mkdir(paths().cacheDir, { recursive: true });
  const tmp = cachePath() + ".tmp";
  await Deno.writeTextFile(tmp, JSON.stringify(c));
  await Deno.rename(tmp, cachePath());
}

function cachePath(): string {
  return join(paths().cacheDir, "model-catalog.json");
}

/** Index every downloadable file's modelSpec -> pinned sha256 from the catalog,
 *  so a model download can verify its bytes against the signed catalog. */
export function buildSha256Index(catalog: CatalogPayload): Map<string, string> {
  const out = new Map<string, string>();
  for (const fam of catalog.families) {
    for (const model of fam.models) {
      for (const v of model.variants) {
        if (v.mmprojSpec && v.mmprojSha256) {
          out.set(v.mmprojSpec, v.mmprojSha256);
        }
        for (const q of v.quants) if (q.sha256) out.set(q.modelSpec, q.sha256);
      }
    }
  }
  for (const cat of [catalog.stt, catalog.tts]) {
    for (const model of cat.models) {
      for (const quant of model.quants) {
        for (const f of quant.files) {
          if (f.sha256) out.set(f.modelSpec, f.sha256);
        }
      }
    }
  }
  return out;
}

/** Pinned sha256 for a model file's HF spec from the signed catalog, or
 *  undefined when the catalog can't be loaded (offline first run) or the spec
 *  isn't a catalog file (a custom user pick), so the caller falls back to HF's
 *  published hash. */
export async function modelCatalogSha256(modelSpec: string): Promise<string | undefined> {
  try {
    const catalog = await loadModelCatalog();
    return buildSha256Index(catalog).get(modelSpec);
  } catch {
    return undefined;
  }
}
