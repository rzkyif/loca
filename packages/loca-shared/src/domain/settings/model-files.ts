// Model-file helpers for settings that reference downloadable weights: the
// `*.modelFiles` bundle maps (STT/TTS), the embedding model, and the
// requirement set the current settings imply.

import type { RequiredModelRef } from "../model.ts";
import { sttUsesLocal, ttsUsesLocal } from "../model.ts";

// A sherpa speech model is a bundle of role-tagged files whose roles differ by
// family, so rather than guess sibling filenames, selection bakes the catalog's
// explicit file list into `stt.modelFiles` / `tts.modelFiles` as a JSON map of
// sherpa role to HF spec. (espeak-ng phonemizer data is NOT listed: it ships
// inside the speech binary's archive.)
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

/** The HF file specs a `*.modelFiles` setting references, [] when malformed.
 *  `modelFilesError` is the loud-failure guard for manual edits. */
export function parseModelFiles(v: unknown): string[] {
  const map = modelFilesMap(v);
  return map ? Object.values(map).filter(isHfSpec) : [];
}

/** Human-readable error if a `*.modelFiles` value is present but not a JSON
 *  object mapping each role to an `@user/repo/branch/file` spec, so a malformed
 *  manual edit is rejected at PATCH time. null when valid or empty. */
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

// The local embedding model for tool-relevance + memory RAG, served by the
// llama-embed sidecar. EMBED_REPO is folded into the stored-vector staleness
// hash, so changing the model forces a re-embed of every cached vector.
export const EMBED_REPO = "@second-state/All-MiniLM-L6-v2-Embedding-GGUF/main";
export const EMBED_MODEL_FILE = `${EMBED_REPO}/all-MiniLM-L6-v2-Q8_0.gguf`;

function isHfSpec(v: unknown): v is string {
  return typeof v === "string" && v.startsWith("@") && v.length > 1;
}

/** Model files (HF specs) the current settings require, tagged with their
 *  requirement group. The single source of truth for both core and client. */
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
  // With llm external and a Relevance Model set, embeddings reuse that provider
  // and the local GGUF isn't needed.
  const externalEmbed =
    s["llm.provider"] === "external" &&
    typeof s["llm.external.embedModel"] === "string" &&
    (s["llm.external.embedModel"] as string).trim() !== "";
  if (!externalEmbed) {
    out.push({ source: EMBED_MODEL_FILE, group: "embed" });
  }
  return out;
}
