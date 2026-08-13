import type { ComponentProps } from "svelte";
import type { OmitSnippetProps } from "./types.ts";
import type QuickModelBarView from "../components/chat/userinput/QuickModelBarView.svelte";
import { findField, getDefaultSettings } from "../../domain/settings/engine.ts";
import type { ModelPresetField } from "../../domain/settings/types.ts";
import {
  creativityDropdownOptions,
  creativitySelection,
  CUSTOM_VALUE,
  type QuickOption,
  type QuickSelection,
  thinkingDropdownOptions,
  thinkingSelection,
} from "../../domain/quick-controls.ts";

// Builds the quick-access selections from the schema defaults via the same
// shared helpers the client's quick bar uses, so a fresh-app preview matches
// the real bar exactly. Controls are inert here (the renderer overrides
// onchange as needed).
const D = getDefaultSettings();
const noop = (): void => {};
const contextSize = Number(D["llm.contextSize"]) || 4096;
const presetField = findField("llm.preset") as ModelPresetField | undefined;
const presetOptions = (presetField?.presetConfig.options ?? []).map((o) => ({
  value: o.id,
  label: o.title ?? o.label,
}));

// A selection matching no preset level shows its raw "custom" label as a
// disabled lead option, exactly as the client's bar does.
function withCustom(sel: QuickSelection, options: QuickOption[]): QuickOption[] {
  if (sel.value !== CUSTOM_VALUE) return options;
  const label = sel.customLabel ?? "";
  return [{ value: CUSTOM_VALUE, label, display: label, disabled: true }, ...options];
}

const thinkingSel = thinkingSelection(D, "local");
const thinkingOpts = withCustom(thinkingSel, thinkingDropdownOptions("local", contextSize));
const creativitySel = creativitySelection(D);
const creativityOpts = withCustom(creativitySel, creativityDropdownOptions());

export const quickModelBarSamples = {
  local: {
    model: {
      value: D["llm.preset"] as string,
      options: presetOptions,
      onchange: noop,
      ariaLabel: "Smart preset",
    },
    thinking: {
      value: thinkingSel.value,
      options: thinkingOpts,
      onchange: noop,
    },
    creativity: {
      value: creativitySel.value,
      options: creativityOpts,
      onchange: noop,
    },
  },
} satisfies Record<string, OmitSnippetProps<ComponentProps<typeof QuickModelBarView>>>;
