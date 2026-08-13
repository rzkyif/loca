/**
 * The condensation maps behind the chat input's quick model controls. The quick
 * bar surfaces a friendly dropdown ("Thinking": Off/Low/Medium/High,
 * "Creativity": Precise/Balanced/Creative) over the granular `llm.*` settings,
 * for a non-technical audience. Keeping the maps here (rather than buried in a
 * Svelte component) means the level <-> setting translation is visible and
 * shared.
 *
 * Each level reads/writes the same `llm.*` keys the full Settings UI edits, so
 * nothing here is a parallel source of truth. When the underlying value doesn't
 * line up with any level (a preset's tuning, or a hand-set value in Settings),
 * the selection reports a `__custom__` value plus a label that shows the raw
 * number, so the bar stays honest instead of silently snapping.
 */

import { DEFAULT_SAMPLING } from "./recommend.ts";

export type LlmProvider = "local" | "external";

/** The value a quick control reports as selected. `value` is an option value,
 *  or `CUSTOM_VALUE` when the underlying setting matches no option (then
 *  `customLabel` carries the raw "Temperature: X" / "Thinking Budget: X" text
 *  to render, shown disabled in the dropdown). */
export interface QuickSelection {
  value: string;
  customLabel?: string;
}

export const CUSTOM_VALUE = "__custom__";

/** One dropdown entry. `label` is shown in the open list (with the raw value in
 *  a suffix, e.g. "Low (Thinking Budget: 2048)"); `display` is the short text
 *  shown collapsed in the bar (e.g. "Low"), defaulting to `label`. */
export interface QuickOption {
  value: string;
  label: string;
  display?: string;
  disabled?: boolean;
}

// --- Thinking effort ------------------------------------------------------

export type ThinkingLevel = "off" | "minimal" | "low" | "medium" | "high";

/** Local thinking levels map to a `llm.reasoningBudget` token cap. */
export const LOCAL_THINKING_OPTIONS = [
  { value: "off", label: "Off" },
  { value: "low", label: "Low" },
  { value: "medium", label: "Medium" },
  { value: "high", label: "High" },
] as const;

/** External thinking levels map to OpenAI-style `reasoning_effort`. The effort
 *  values mirror exactly what the OpenAI API accepts (no token budget). */
export const EXTERNAL_THINKING_OPTIONS = [
  { value: "off", label: "Off" },
  { value: "minimal", label: "Minimal" },
  { value: "low", label: "Low" },
  { value: "medium", label: "Medium" },
  { value: "high", label: "High" },
] as const;

/** The default local context window (mirrors the schema default), used when the
 *  configured size is missing or unreadable. */
export const DEFAULT_CONTEXT_SIZE = 4096;

/** Each local level's thinking budget as a fraction of the context window. A
 *  fixed token budget is meaningless without the window it sits in; these scale
 *  with it and stay conservative enough to leave room for the prompt + answer
 *  (Low matches the model's own default budget around an 8k window). */
const LOCAL_THINKING_FRACTIONS: Record<"low" | "medium" | "high", number> = {
  low: 1 / 16,
  medium: 1 / 8,
  high: 1 / 4,
};

/** The local thinking budget (in tokens) for a level at a given context size. */
export function localThinkingBudget(level: ThinkingLevel, contextSize: number): number {
  if (level === "off") return 0;
  const ctx = contextSize > 0 ? contextSize : DEFAULT_CONTEXT_SIZE;
  const frac = level === "minimal" ? LOCAL_THINKING_FRACTIONS.low : LOCAL_THINKING_FRACTIONS[level];
  return Math.round(ctx * frac);
}

/** The dropdown entries for the active provider. Local levels carry their token
 *  budget (scaled to the context size) in the label suffix; external effort
 *  levels have no underlying number, so no suffix. */
export function thinkingDropdownOptions(provider: LlmProvider, contextSize: number): QuickOption[] {
  if (provider === "external") {
    return EXTERNAL_THINKING_OPTIONS.map((o) => ({
      value: o.value,
      label: o.label,
    }));
  }
  return LOCAL_THINKING_OPTIONS.map((o) =>
    o.value === "off"
      ? { value: o.value, label: o.label }
      : {
          value: o.value,
          label: `${o.label} (${localThinkingBudget(o.value, contextSize)} tokens)`,
          display: o.label,
        },
  );
}

/** The `llm.*` updates that realize a chosen thinking level for the active
 *  provider. Apply each through the same change path the Settings UI uses. */
export function thinkingLevelUpdates(
  level: ThinkingLevel,
  provider: LlmProvider,
  contextSize: number,
): Record<string, string | number> {
  if (level === "off") return { "llm.reasoning": "off" };
  if (provider === "external") {
    return { "llm.reasoning": "on", "llm.reasoningEffort": level };
  }
  return {
    "llm.reasoning": "on",
    "llm.reasoningBudget": localThinkingBudget(level, contextSize),
  };
}

/** The current thinking selection. External effort is always a known level;
 *  local snaps to a level only when the budget lines up exactly with that
 *  level's value for the current context, else reports a "N tokens" custom
 *  label (or "Unlimited" for an unset/zero budget). */
export function thinkingSelection(
  settings: Record<string, unknown>,
  provider: LlmProvider,
): QuickSelection {
  if (str(settings["llm.reasoning"], "off") === "off") return { value: "off" };
  if (provider === "external") {
    const effort = str(settings["llm.reasoningEffort"], "high");
    const known = EXTERNAL_THINKING_OPTIONS.some((o) => o.value === effort);
    return { value: known ? effort : "high" };
  }
  const budget = num(settings["llm.reasoningBudget"], 0);
  if (budget <= 0) return { value: CUSTOM_VALUE, customLabel: "Unlimited" };
  const ctx = num(settings["llm.contextSize"], DEFAULT_CONTEXT_SIZE);
  for (const level of ["low", "medium", "high"] as const) {
    if (budget === localThinkingBudget(level, ctx)) return { value: level };
  }
  return { value: CUSTOM_VALUE, customLabel: `${budget} tokens` };
}

/** The `llm.reasoningBudget` a fresh local config should start at so the default
 *  "Low" thinking budget tracks the ACTUAL context window rather than the static
 *  4k the schema default is computed against. Called once at startup for a budget
 *  that was never explicitly set (the caller checks the store); the running
 *  Settings form and preset apply already realign on later context/preset
 *  changes, this fills the missed initial case. Returns null when no change is
 *  needed (external provider, thinking off, or already aligned). */
export function initialThinkingBudgetAlignment(settings: Record<string, unknown>): number | null {
  if (str(settings["llm.provider"], "local") === "external") return null;
  if (str(settings["llm.reasoning"], "off") === "off") return null;
  const ctx = num(settings["llm.contextSize"], DEFAULT_CONTEXT_SIZE);
  const current = num(settings["llm.reasoningBudget"], 0);
  const aligned = localThinkingBudget("low", ctx);
  return aligned > 0 && aligned !== current ? aligned : null;
}

// --- Creativity (temperature) ---------------------------------------------

export type CreativityLevel = "precise" | "balanced" | "creative";

export const CREATIVITY_OPTIONS = [
  { value: "precise", label: "Precise" },
  { value: "balanced", label: "Balanced" },
  { value: "creative", label: "Creative" },
] as const;

/** Representative temperature for each creativity level. Balanced mirrors the
 *  shared sampling default so the two stay in step. */
const CREATIVITY_TEMPERATURES: Record<CreativityLevel, number> = {
  precise: 0.3,
  balanced: DEFAULT_SAMPLING.temperature,
  creative: 1.0,
};

/** The `llm.temperature` value for a chosen creativity level. */
export function creativityTemperature(level: CreativityLevel): number {
  return CREATIVITY_TEMPERATURES[level];
}

/** Dropdown entries carrying each level's temperature in the label suffix
 *  (matching the custom "X°" format; temperature is dimensionless, so the degree
 *  glyph is just a compact, intuitive stand-in). */
export function creativityDropdownOptions(): QuickOption[] {
  return CREATIVITY_OPTIONS.map((o) => ({
    value: o.value,
    label: `${o.label} (${CREATIVITY_TEMPERATURES[o.value]}°)`,
    display: o.label,
  }));
}

/** The current creativity selection: a level when the temperature matches one
 *  exactly, else a "X°" custom label showing the raw value. */
export function creativitySelection(settings: Record<string, unknown>): QuickSelection {
  const t = num(settings["llm.temperature"], DEFAULT_SAMPLING.temperature);
  for (const level of ["precise", "balanced", "creative"] as const) {
    if (t === CREATIVITY_TEMPERATURES[level]) return { value: level };
  }
  return { value: CUSTOM_VALUE, customLabel: `${t}°` };
}

// --- helpers --------------------------------------------------------------

function str(v: unknown, def: string): string {
  return typeof v === "string" ? v : def;
}

function num(v: unknown, def: number): number {
  if (typeof v === "number" && Number.isFinite(v)) return v;
  if (typeof v === "string" && v !== "") {
    const n = Number(v);
    if (Number.isFinite(n)) return n;
  }
  return def;
}
