// Per-use-case thinking budget for single-shot utility LLM calls.
//
// Each single-shot use case (title generation, transcription cleanup/merge,
// memory summary, complexity check, tool-filter refinement) has its own
// `*ThinkingBudget` number setting. 0 turns thinking off for that call; N>0
// enables it and caps the `<think>` block at N tokens. This reads and
// normalizes one such setting; callers pass it through `overrides.reasoningBudget`
// and add it to their `maxTokens` so the answer still has room after thinking.

export function thinkingBudget(settings: Record<string, unknown>, key: string): number {
  const v = settings[key];
  const n = typeof v === "number" ? v : typeof v === "string" && v !== "" ? Number(v) : NaN;
  return Number.isFinite(n) && n > 0 ? Math.floor(n) : 0;
}
