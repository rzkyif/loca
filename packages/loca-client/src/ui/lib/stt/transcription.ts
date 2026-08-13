/**
 * The speech-to-text post-processing chain: transcribe captured audio, then
 * optionally run LLM autocorrect and chain-merge it into the existing input.
 *
 * The chain is pure and dependency-injected (the transcribe/autocorrect/merge
 * calls are supplied by the caller) so it is fully unit-testable. The default
 * deps wired to the paired core live in `defaultTranscriptionDeps`. The
 * stateful coordinator (timing, the processing flag, applying the result to the
 * input) lives in the use-stt-input composable.
 */

import { cores } from "$lib/core";
import { errMessage } from "@loca/shared";

export type TranscriptionDeps = {
  transcribe: (audioBase64: string) => Promise<{ text: string; error?: string }>;
  autocorrect: (text: string) => Promise<string>;
  merge: (prior: string, next: string) => Promise<string>;
};

export type TranscriptionResult =
  | { kind: "empty" }
  | { kind: "error"; message?: string }
  // `original` carries the pre-autocorrect text only when autocorrect actually
  // changed it (so the UI can offer a one-click revert); null otherwise.
  | { kind: "ok"; text: string; original: string | null };

/**
 * Run transcribe -> (autocorrect) -> (chain-merge). Autocorrect and merge are
 * best-effort: a failure in either is reported via `onWarn` and falls back to
 * the un-processed text rather than aborting the whole turn.
 */
export async function runTranscriptionChain(
  audioBase64: string,
  existingText: string,
  opts: { autocorrect: boolean; chain: boolean },
  deps: TranscriptionDeps,
  onWarn?: (stage: "autocorrect" | "merge", err: unknown) => void,
): Promise<TranscriptionResult> {
  const result = await deps.transcribe(audioBase64);
  if (result.error) return { kind: "error", message: result.error };
  const transcription = result.text.trim();
  if (!transcription) return { kind: "empty" };

  let raw = transcription;
  let corrected: string | null = null;

  if (opts.autocorrect) {
    try {
      const c = await deps.autocorrect(raw);
      if (c) corrected = c;
    } catch (e) {
      onWarn?.("autocorrect", e);
    }
  }

  // Only merge when there is prior text to chain onto.
  if (opts.chain && existingText.trim().length > 0) {
    try {
      const mergedRaw = await deps.merge(existingText, raw);
      if (mergedRaw) raw = mergedRaw;
      if (corrected) {
        const mergedCorrected = await deps.merge(existingText, corrected);
        if (mergedCorrected) corrected = mergedCorrected;
      }
    } catch (e) {
      onWarn?.("merge", e);
    }
  }

  if (corrected) {
    const changed = corrected.trim() !== raw.trim();
    return { kind: "ok", text: corrected, original: changed ? raw : null };
  }
  return { kind: "ok", text: raw, original: null };
}

/** Transcribe/autocorrect/merge bound to the currently-paired core. */
export function defaultTranscriptionDeps(language?: string): TranscriptionDeps {
  return {
    transcribe: async (audioBase64) => {
      try {
        const blob = new Blob([Uint8Array.from(atob(audioBase64), (c) => c.charCodeAt(0))], {
          type: "audio/wav",
        });
        const res = await cores().api().stt.transcribe(blob, language);
        return { text: res.text };
      } catch (e) {
        return { text: "", error: errMessage(e) };
      }
    },
    autocorrect: (text) => cores().api().llm.autocorrect(text),
    merge: (prior, next) => cores().api().llm.merge(prior, next),
  };
}
