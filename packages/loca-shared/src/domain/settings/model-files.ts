/**
 * Model-file helpers for settings that reference downloadable weights: the
 * `*.modelFiles` bundle maps (STT/TTS), the always-present embedding model, and
 * the requirement set the current settings imply. Consumed by both the core
 * (`sourcesForKind` / requirements) and the client.
 */

import type { RequiredModelRef } from "../model.ts";
import { sttUsesLocal, ttsUsesLocal } from "../model.ts";

// A sherpa speech model (STT or TTS) is a bundle of role-tagged files, and the
// roles differ by family (whisper: encoder/decoder/tokens; sense-voice:
// model/tokens; kokoro: model/voices/tokens; ...). Rather than guess sibling
// filenames from one spec, the curated catalog lists every file explicitly and
// selection bakes the chosen bundle into `stt.modelFiles` / `tts.modelFiles` as
// a JSON object mapping each sherpa role to its HF spec. The download flow and
// the speech sidecar read that map; no naming convention is assumed. (espeak-ng
// phonemizer data is NOT listed here: it ships inside the speech binary's
// archive and is added by the sidecar for the kokoro/kitten families.)
export function modelFilesMap(v: unknown): Record<string, string> | null {
  if (typeof v !== "string" || v.trim() === "") return null;
  let parsed: unknown;
  try {
    parsed = JSON.parse(v);
  } catch {
    return null;
  }
  if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
    return null;
  }
  const out: Record<string, string> = {};
  for (const [role, spec] of Object.entries(parsed)) {
    if (typeof spec !== "string") return null;
    out[role] = spec;
  }
  return out;
}

/** The list of HF file specs a `*.modelFiles` setting references. Returns [] for
 *  a missing or malformed value; `modelFilesError` is the loud-failure guard for
 *  validating manual edits. */
export function parseModelFiles(v: unknown): string[] {
  const map = modelFilesMap(v);
  return map ? Object.values(map).filter(isHfSpec) : [];
}

/** Human-readable error if a `*.modelFiles` value is present but not a JSON
 *  object mapping each role to an `@user/repo/branch/file` spec. null when valid
 *  or empty, so a malformed manual edit is rejected at PATCH time rather than
 *  silently producing an empty bundle the sidecar can't load. */
export function modelFilesError(v: unknown): string | null {
  if (v === undefined || v === null || v === "") return null;
  const map = modelFilesMap(v);
  if (!map) {
    return "must be a JSON object mapping each file role to an @user/repo/branch/file spec";
  }
  for (const [role, spec] of Object.entries(map)) {
    if (!isHfSpec(spec)) {
      return `role "${role}" must be an @user/repo/branch/file spec`;
    }
  }
  return null;
}

// The local embedding model for tool-relevance + memory RAG: an all-MiniLM-L6-v2
// GGUF served by the llama-embed sidecar over /v1/embeddings. EMBED_REPO is the
// local model's identity; when embeddings run locally it is folded into the
// stored-vector staleness hash (services/relevance.ts, via activeEmbedModelId)
// so changing the model forces a re-embed of every cached vector. (In external
// mode with a Relevance Model set, embeddings reuse that provider instead and
// this file isn't downloaded.)
export const EMBED_REPO = "@second-state/All-MiniLM-L6-v2-Embedding-GGUF/main";
export const EMBED_MODEL_FILE = `${EMBED_REPO}/all-MiniLM-L6-v2-Q8_0.gguf`;

function isHfSpec(v: unknown): v is string {
  return typeof v === "string" && v.startsWith("@") && v.length > 1;
}

/** Model files (HF specs) the current settings require, tagged with their
 *  requirement group. The single source of truth consumed by both the core
 *  (`sourcesForKind` / requirements) and the client. llm local → modelPath
 *  (+ mmproj if image support); stt enabled+local → every file in its bundle;
 *  tts (when enabled) → every file in its bundle; embed → the MiniLM GGUF unless
 *  embeddings are served by an external Relevance Model. The stt/tts bundles are read from the
 *  `*.modelFiles` map that selection baked from the catalog, so any family's
 *  file set is covered without assuming filenames. */
export function requiredModelRefs(s: Record<string, unknown>): RequiredModelRef[] {
  const out: RequiredModelRef[] = [];
  const llmLocal = s["llm.provider"] !== "external";
  const sttActive = sttUsesLocal(s);
  const ttsActive = ttsUsesLocal(s);
  const imagesOn = !!s["llm.supportImages"];

  const llmModel = s["llm.modelPath"];
  if (llmLocal && isHfSpec(llmModel)) {
    out.push({ source: llmModel, group: "llm" });
  }
  const mmproj = s["llm.mmprojPath"];
  if (llmLocal && imagesOn && isHfSpec(mmproj)) {
    out.push({ source: mmproj, group: "llm" });
  }
  if (sttActive) {
    for (const spec of parseModelFiles(s["stt.modelFiles"])) {
      out.push({ source: spec, group: "stt" });
    }
  }
  if (ttsActive) {
    for (const spec of parseModelFiles(s["tts.modelFiles"])) {
      out.push({ source: spec, group: "tts" });
    }
  }
  // The local MiniLM embedding model is only required when embeddings run
  // locally. With llm in external mode and a Relevance Model (embedModel) set,
  // embeddings reuse that external provider, so the local GGUF isn't needed.
  const externalEmbed =
    s["llm.provider"] === "external" &&
    typeof s["llm.external.embedModel"] === "string" &&
    (s["llm.external.embedModel"] as string).trim() !== "";
  if (!externalEmbed) {
    out.push({ source: EMBED_MODEL_FILE, group: "embed" });
  }
  return out;
}
