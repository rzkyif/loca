/**
 * The settings field-change engine shared by the full Settings panel and the
 * Quick Settings accordion: validation (required / optionalWhen / regex),
 * optimistic apply through `settingsState`, the preset-to-Custom flip for
 * managed keys, preset card application, and condition-dependency
 * re-evaluation. Extracted so both views get identical semantics.
 *
 * Reacting to a section's `expandWhen` is view-specific (only the full
 * Settings panel tracks expanded sections), so the constructor takes an
 * optional `onSectionExpand` hook instead of mutating view state directly.
 */

import type { AppliedModelSettings, PresetBucket, PresetOption } from "@loca/shared";
import {
  detectLlmPreset,
  errMessage,
  evalCondition,
  findField,
  getConditionDeps,
  getPresetFieldIds,
  getValidationError,
  localThinkingBudget,
  PRESET_BUCKETS,
  SETTINGS_SCHEMA,
  thinkingSelection,
} from "@loca/shared";
import { settingsState } from "$stores/settings.svelte";
import { modelRecommendState } from "$stores/model-recommend.svelte";

export class SettingsForm {
  /** Field id -> human-readable error, rendered by the field's FormField. */
  validationErrors = $state<Record<string, string>>({});

  /** `onSectionExpand` is called with a section key
   *  (`${groupId}-${sectionIndex}`) whenever a change re-evaluates that
   *  section's `expandWhen` condition. */
  constructor(private onSectionExpand?: (sectionKey: string, expand: boolean) => void) {}

  validateAllFields = (): void => {
    for (const group of SETTINGS_SCHEMA) {
      for (const section of group.sections) {
        for (const field of section.fields) {
          if (
            field.type === "command_preview" ||
            field.type === "services" ||
            field.type === "storage" ||
            field.type === "object_management"
          ) {
            continue;
          }
          const value = settingsState.currentSettings[field.id];
          this.validateField(field.id, value);
        }
      }
    }
  };

  // Apply optimistically. The core recomputes the required-files snapshot and
  // re-broadcasts it; the pending-downloads popup reacts with the full updated
  // list. No pre-download probe / revert here.
  handleChange = async (key: string, value: any): Promise<void> => {
    this.validateField(key, value);
    if (this.validationErrors[key]) return;
    await this.tryApply(key, value);
  };

  validateField = (fieldId: string, value: any): void => {
    const field = findField(fieldId);
    if (!field) return;

    const isOptional = field.optionalWhen
      ? evalCondition(field.optionalWhen, settingsState.currentSettings)
      : !!field.optional;

    if (!isOptional && (value === undefined || value === null || value === "")) {
      this.validationErrors = {
        ...this.validationErrors,
        [fieldId]: "This field is required",
      };
      return;
    }

    if (
      this.validationErrors[field.id] === "This field is required" &&
      (isOptional || (value !== "" && value !== undefined && value !== null))
    ) {
      const { [fieldId]: _, ...rest } = this.validationErrors;
      this.validationErrors = rest;
    }

    const regex = "regex" in field ? field.regex : undefined;
    if (!regex) return;

    const error = getValidationError(regex, value);
    if (error) {
      this.validationErrors = { ...this.validationErrors, [fieldId]: error };
    } else {
      const { [fieldId]: _, ...rest } = this.validationErrors;
      this.validationErrors = rest;
    }
  };

  resetToDefault = (fieldId: string): void => {
    const field = findField(fieldId);
    if (field) {
      void this.handleChange(fieldId, field.defaultValue);
    }
  };

  // Apply optimistically (like handleChange); the requirements popup reacts to
  // whatever the core then reports as missing.
  handlePresetSelect = async (fieldId: string, option: PresetOption): Promise<void> => {
    const updates: Record<string, any> = { [fieldId]: option.id };
    if (option.defaults) Object.assign(updates, option.defaults);
    await this.applyPresetUpdates(updates);
  };

  reEvaluateDeps = (...keys: string[]): void => {
    const deps = getConditionDeps();

    for (const key of keys) {
      const entries = deps.get(key);
      if (!entries) continue;

      for (const dep of entries) {
        if (dep.kind === "field" && dep.condition === "optionalWhen") {
          this.validateField(dep.fieldId, settingsState.currentSettings[dep.fieldId]);
        } else if (dep.kind === "section" && dep.condition === "expandWhen") {
          const sectionKey = `${dep.groupId}-${dep.sectionIndex}`;
          const group = SETTINGS_SCHEMA.find((g) => g.id === dep.groupId);
          const section = group?.sections[dep.sectionIndex];
          const expand =
            !!section && evalCondition(section.expandWhen, settingsState.currentSettings);
          this.onSectionExpand?.(sectionKey, expand);
        }
      }
    }
  };

  private async tryApply(key: string, value: any): Promise<void> {
    try {
      await this.applyFieldChange(key, value);
    } catch (e) {
      this.validationErrors = {
        ...this.validationErrors,
        [key]: errMessage(e),
      };
    }
  }

  private async applyFieldChange(key: string, value: any): Promise<void> {
    // Capture the active thinking level before a local context-size change: the
    // budget is a fraction of the context window, so re-scale it afterward to
    // keep the user on the same level (Low/Medium/High) as the window grows.
    const isLocal = settingsState.currentSettings["llm.provider"] !== "external";
    const prevThinkingLevel =
      key === "llm.contextSize" && isLocal
        ? thinkingSelection(settingsState.currentSettings, "local").value
        : null;

    if (
      key.startsWith("llm.") &&
      key !== "llm.preset" &&
      !key.startsWith("llm.external.") &&
      getPresetFieldIds("llm").has(key)
    ) {
      // Don't blindly flip to Custom: if the edited config still matches a smart
      // preset (e.g. the user set a value back to a preset's), restore that
      // preset. detectLlmPreset returns "custom" when nothing matches (the UI
      // then splits "custom preset" vs "custom -> manual" by the model path).
      const next = { ...settingsState.currentSettings, [key]: value };
      const detected = detectLlmPreset(next, bucketAppliedMap());
      if (settingsState.currentSettings["llm.preset"] !== detected) {
        await settingsState.updateSetting("llm.preset", detected);
      }
    }
    if (
      key.startsWith("stt.") &&
      key !== "stt.preset" &&
      !key.startsWith("stt.external.") &&
      getPresetFieldIds("stt").has(key)
    ) {
      if (settingsState.currentSettings["stt.preset"] !== "custom") {
        await settingsState.updateSetting("stt.preset", "custom");
      }
    }
    if (
      key.startsWith("prompts.") &&
      key !== "prompts.defaultSystemPrompt.preset" &&
      getPresetFieldIds("prompts").has(key)
    ) {
      if (settingsState.currentSettings["prompts.defaultSystemPrompt.preset"] !== "custom") {
        await settingsState.updateSetting("prompts.defaultSystemPrompt.preset", "custom");
      }
    }
    await settingsState.updateSetting(key, value);

    if (
      prevThinkingLevel === "low" ||
      prevThinkingLevel === "medium" ||
      prevThinkingLevel === "high"
    ) {
      const ctx = Number(value);
      if (Number.isFinite(ctx) && ctx > 0) {
        await settingsState.updateSetting(
          "llm.reasoningBudget",
          localThinkingBudget(prevThinkingLevel, ctx),
        );
      }
    }
    this.reEvaluateDeps(key);
  }

  private async applyPresetUpdates(updates: Record<string, any>): Promise<void> {
    await settingsState.updateSettings(updates);
    this.validateAllFields();
    this.reEvaluateDeps(...Object.keys(updates));
  }
}

/** The per-device applied config for each smart preset, from the loaded
 *  recommendations. Empty when recommendations haven't loaded (detectLlmPreset
 *  then falls back to "custom", preserving the old flip-to-Custom behavior). */
function bucketAppliedMap(): Partial<Record<PresetBucket, AppliedModelSettings | null>> {
  const rec = modelRecommendState.recommendations;
  if (!rec) return {};
  const out: Partial<Record<PresetBucket, AppliedModelSettings | null>> = {};
  for (const bucket of PRESET_BUCKETS) {
    out[bucket] = rec.buckets[bucket]?.apply ?? null;
  }
  return out;
}

export function useSettingsForm(
  onSectionExpand?: (sectionKey: string, expand: boolean) => void,
): SettingsForm {
  return new SettingsForm(onSectionExpand);
}
