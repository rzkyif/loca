/**
 * The composed settings schema and the constants derived directly from it:
 * the secret-key set, the full field-id list, default values, and field
 * lookup. The schema is assembled from the per-group modules under `./groups/`;
 * everything here is the single source of truth for what setting keys exist and
 * what they default to.
 */

import type { SettingField, SettingGroup } from "./types.ts";

import { generalGroup } from "./groups/general.ts";
import { shortcutsGroup } from "./groups/shortcuts.ts";
import { appearanceGroup } from "./groups/appearance.ts";
import { llmGroup } from "./groups/llm.ts";
import { promptsGroup } from "./groups/prompts.ts";
import { snippetsGroup } from "./groups/snippets.ts";
import { memoriesGroup } from "./groups/memories.ts";
import { scheduledPromptsGroup } from "./groups/scheduled-prompts.ts";
import { greetingsGroup } from "./groups/greetings.ts";
import { toolsGroup } from "./groups/tools.ts";
import { extensionsGroup } from "./groups/extensions.ts";
import { mcpGroup } from "./groups/mcp.ts";
import { dualModelGroup } from "./groups/dual-model.ts";
import { sttGroup } from "./groups/stt.ts";
import { ttsGroup } from "./groups/tts.ts";
import { usageGroup } from "./groups/usage.ts";
import { coresGroup } from "./groups/cores.ts";

export const SETTINGS_SCHEMA: SettingGroup[] = [
  generalGroup,
  shortcutsGroup,
  appearanceGroup,
  llmGroup,
  promptsGroup,
  snippetsGroup,
  memoriesGroup,
  scheduledPromptsGroup,
  greetingsGroup,
  toolsGroup,
  extensionsGroup,
  mcpGroup,
  dualModelGroup,
  sttGroup,
  ttsGroup,
  usageGroup,
  coresGroup,
];

// Settings whose values are routed to the OS keychain rather than to
// settings.json. Derived from the schema so adding a `type: "password"`
// field automatically opts it in, with no manifest update needed.
export const SECRET_KEYS: readonly string[] = (() => {
  const out: string[] = [];
  for (const group of SETTINGS_SCHEMA) {
    for (const section of group.sections) {
      for (const field of section.fields) {
        if (field.type === "password") out.push(field.id);
      }
    }
  }
  return out;
})();

/**
 * Every setting field id known to the schema. Derived at import time so a
 * rename here doesn't require updating a hand-maintained list elsewhere.
 * Useful for: runtime validation of `currentSettings` lookups in dev, and
 * dependency-graph traversal.
 */
export const SETTING_IDS: readonly string[] = (() => {
  const out: string[] = [];
  for (const group of SETTINGS_SCHEMA) {
    for (const section of group.sections) {
      for (const field of section.fields) {
        out.push(field.id);
      }
    }
  }
  return out;
})();

const SETTING_ID_SET: ReadonlySet<string> = new Set(SETTING_IDS);

/** True if `key` matches a field id in `SETTINGS_SCHEMA`. */
export function isValidSettingKey(key: string): boolean {
  return SETTING_ID_SET.has(key);
}

export function getDefaultSettings(): Record<string, unknown> {
  const defaults: Record<string, unknown> = {};
  for (const group of SETTINGS_SCHEMA) {
    for (const section of group.sections) {
      for (const field of section.fields) {
        if (
          field.type !== "command_preview" &&
          field.type !== "services" &&
          field.type !== "storage" &&
          field.type !== "object_management"
        ) {
          defaults[field.id] = field.defaultValue;
        }
      }
    }
  }
  return defaults;
}

export function findField(fieldId: string): SettingField | undefined {
  for (const group of SETTINGS_SCHEMA) {
    for (const section of group.sections) {
      for (const field of section.fields) {
        if (field.id === fieldId) return field;
      }
    }
  }
  return undefined;
}

/** Returns the set of field IDs that are controlled by any preset in a given group. */
export function getPresetFieldIds(groupId: string): Set<string> {
  const ids = new Set<string>();
  const group = SETTINGS_SCHEMA.find((g) => g.id === groupId);
  if (!group) return ids;
  for (const section of group.sections) {
    for (const field of section.fields) {
      if (
        (field.type !== "preset" &&
          field.type !== "model_preset" &&
          field.type !== "stt_preset" &&
          field.type !== "tts_preset") ||
        !field.presetConfig
      ) {
        continue;
      }
      const allOptions = [
        ...field.presetConfig.options,
        ...(field.presetConfig.secondaryOptions || []),
      ];
      for (const opt of allOptions) {
        if (opt.defaults) {
          for (const key of Object.keys(opt.defaults)) {
            ids.add(key);
          }
        }
      }
      for (const key of field.presetConfig.managedKeys ?? []) {
        ids.add(key);
      }
    }
  }
  return ids;
}
