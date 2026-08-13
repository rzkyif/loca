// Embedding service: produces L2-normalized vectors for tool-relevance + memory
// RAG (compared with cosine in services/relevance.ts). Two sources, chosen from
// settings:
//   - LOCAL (default): the host's local embed endpoint (host.localEmbed), a
//     llama-embed sidecar hosting the MiniLM GGUF over an OpenAI-style
//     /v1/embeddings. Present only on a host that can run local inference.
//   - EXTERNAL: when llm.provider = "external" AND an optional Relevance Model
//     (llm.external.embedModel) is set, embeddings reuse that provider's Base URL
//     + API Key. No separate provider block; blank embedModel means "off".
//
// Embeddings are OPTIONAL: when no source is configured (external with no
// Relevance Model, or a local model not downloaded / no local host), `embed()`
// throws and callers degrade (tools sent unfiltered, memory RAG skipped). The
// vector dimension is whatever the active model returns (MiniLM 384, OpenAI
// 1536, ...); the model identity is folded into the relevance staleness hash
// (see activeEmbedModelId) so switching models re-embeds instead of comparing
// incompatible vectors.

import { EMBED_REPO, errMessage } from "@loca/shared";
import { host } from "../platform/runtime.ts";
import { AppError } from "../platform/errors.ts";
import { loadCoreSettings, subscribeCoreSettings } from "./core-settings.ts";
import { resolveExternalApiKey } from "./external-endpoint.ts";
import { strSetting } from "./settings-access.ts";

// Upper bound on a single embeddings call. Embedding a handful of short strings
// is fast; this only guards against a wedged-but-listening endpoint pinning the
// caller forever (mirrors stt-transcribe's timeout).
const EMBED_TIMEOUT_MS = 30_000;

// The resolved place to send an /embeddings request. `apiKey` is set only for an
// external provider (the local sidecar needs none).
interface EmbedEndpoint {
  url: string;
  model: string;
  apiKey?: string;
}

function trimTrailingSlash(s: string): string {
  return s.replace(/\/+$/, "");
}

// True when llm is in external mode with a non-empty Relevance Model set.
function externalEmbedModel(settings: Record<string, unknown>): string | null {
  if (strSetting(settings, "llm.provider", "local") !== "external") return null;
  const model = strSetting(settings, "llm.external.embedModel", "").trim();
  return model || null;
}

// The identity of the currently-active embedding model, folded into the
// relevance staleness hash so a model change (local MiniLM <-> an external model,
// or one external model to another) reads every stored vector as stale and
// re-embeds it instead of comparing incompatible dimensions. Kept as cached
// module state (refreshed at boot + on every settings change via
// initEmbeddingService) so embedSourceHash stays synchronous on the hot path.
let _activeEmbedModelId = EMBED_REPO;

export function activeEmbedModelId(): string {
  return _activeEmbedModelId;
}

function refreshEmbedModelIdentity(settings: Record<string, unknown>): void {
  _activeEmbedModelId = externalEmbedModel(settings) ?? EMBED_REPO;
}

// Wire the active-model identity to settings: resolve it now and keep it current.
// Called once at core boot (main.ts).
export async function initEmbeddingService(): Promise<void> {
  refreshEmbedModelIdentity(await loadCoreSettings());
  subscribeCoreSettings((settings) => refreshEmbedModelIdentity(settings));
}

// Resolve where to embed, or null when embeddings aren't configured/available.
async function resolveEmbedEndpoint(
  settings: Record<string, unknown>,
): Promise<EmbedEndpoint | null> {
  const external = externalEmbedModel(settings);
  if (external) {
    const baseUrl = strSetting(settings, "llm.external.baseUrl", "").trim();
    if (!baseUrl) return null;
    const apiKey = await resolveExternalApiKey(settings, "llm.external.apiKey", baseUrl);
    if (!apiKey) return null;
    return { url: `${trimTrailingSlash(baseUrl)}/embeddings`, model: external, apiKey };
  }
  // Local sidecar: only when the host provides local endpoints (it can run local
  // inference) AND the model is downloaded (embedEndpoint returns null
  // otherwise). A host without local inference omits localEndpoints entirely.
  return (await host().localEndpoints?.embedEndpoint()) ?? null;
}

// Returns L2-normalized vectors, one per input text, in input order (dimension
// is whatever the active model emits). Empty input returns []. Rejects
// (server_unavailable / provider_error) when no source is configured or the
// endpoint errors, so callers degrade as they already do.
export async function embed(texts: string[]): Promise<Float32Array[]> {
  if (texts.length === 0) return [];
  const settings = await loadCoreSettings();
  const endpoint = await resolveEmbedEndpoint(settings);
  if (!endpoint) {
    throw new AppError(
      "server_unavailable",
      "embeddings are not available (no local model, and no external Relevance Model configured)",
    );
  }
  const headers: Record<string, string> = { "content-type": "application/json" };
  if (endpoint.apiKey) headers["authorization"] = `Bearer ${endpoint.apiKey}`;
  let res: Response;
  try {
    res = await fetch(endpoint.url, {
      method: "POST",
      headers,
      body: JSON.stringify({ model: endpoint.model, input: texts }),
      signal: AbortSignal.timeout(EMBED_TIMEOUT_MS),
    });
  } catch (err) {
    throw new AppError(
      "server_unavailable",
      `embeddings endpoint not reachable at ${endpoint.url}: ${errMessage(err)}`,
    );
  }
  if (!res.ok) {
    throw new AppError("provider_error", `embeddings HTTP ${res.status} from ${endpoint.url}`);
  }
  const body = (await res.json()) as {
    data?: Array<{ index?: number; embedding?: number[] }>;
  };
  // Reassemble in input order (the endpoint returns one entry per input with an
  // `index`; sort defensively in case order isn't guaranteed).
  const data = (body.data ?? []).slice().sort((a, b) => (a.index ?? 0) - (b.index ?? 0));
  if (data.length !== texts.length) {
    throw new AppError(
      "provider_error",
      `embeddings returned ${data.length} vectors for ${texts.length} inputs`,
    );
  }
  // The dimension is the model's own; require only that the batch is internally
  // consistent (same non-zero width) so a malformed response is caught. Cross-
  // model comparisons are prevented upstream by the staleness hash, not here.
  let dim = -1;
  return data.map((d) => {
    const vec = Float32Array.from(d.embedding ?? []);
    if (dim === -1) dim = vec.length;
    if (vec.length === 0 || vec.length !== dim) {
      throw new AppError(
        "provider_error",
        `embeddings returned an inconsistent vector width (${vec.length} vs ${dim})`,
      );
    }
    return l2normalize(vec);
  });
}

/** True when embeddings can be produced right now: an external Relevance Model
 *  is configured (Base URL + API Key present), or the local model is downloaded
 *  on a host that can run it. Used to gate /embed, /reindex, and RAG so callers
 *  skip cleanly instead of hitting an opaque error. */
export async function isEmbeddingModelReady(): Promise<boolean> {
  return (await resolveEmbedEndpoint(await loadCoreSettings())) !== null;
}

/** L2-normalize in place. A local llama-server already normalizes with
 *  `--pooling mean` and most external providers return unit vectors, but
 *  normalizing defensively keeps cosineNormalized (a bare dot product) correct
 *  regardless of the source's normalization. */
function l2normalize(v: Float32Array): Float32Array {
  let sum = 0;
  for (const x of v) sum += x * x;
  const norm = Math.sqrt(sum);
  if (norm > 0) {
    for (let i = 0; i < v.length; i++) v[i] /= norm;
  }
  return v;
}
