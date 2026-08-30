// Type definitions and shared validation constants for the settings schema,
// authoritative for both the client renderer and core's setting reads.

// Inline so this file has zero non-type dependencies. Mirrors the client-side
// command builder's accepted command kinds.
export type CommandType = "llm";

import type { CoreCapabilities } from "../../api/http.ts";

export type OptionsSource = "monitors" | "fonts" | "tts_voices";

export type ActivationMode = "manual" | "sticky" | "push-to-talk";

export interface SettingOption {
  value: string | number;
  label: string;
}

export interface FieldCondition {
  /** Field id to compare against. Optional only when `allOf` is set;
   *  in that case the parent acts as a pure AND wrapper around its children. */
  field?: string;
  eq?: string | number | boolean;
  neq?: string | number | boolean;
  in?: (string | number | boolean)[];
  nin?: (string | number | boolean)[];
  /** When set, all listed conditions must pass. Combines with the field-level
   *  comparison if both are present. */
  allOf?: FieldCondition[];
}

export interface PresetBadge {
  icon: string;
  label: string;
}

export interface PresetOption {
  id: string;
  label: string;
  title?: string;
  badges?: PresetBadge[];
  description?: string;
  icon?: string;
  defaults?: Record<string, string | number | boolean>;
}

export interface PresetConfig {
  options: PresetOption[];
  secondaryOptions?: PresetOption[];
  /** Setting ids this preset controls beyond an option's `defaults` (for
   *  runtime-computed presets). Editing one flips the preset to "custom". */
  managedKeys?: string[];
}

export interface RegexValidationRule {
  regex: string;
  errorMessage: string;
}

export type RegexValidation = string | RegexValidationRule[];

// External-API URL validator: HTTPS for any host, or HTTP only for loopback.
// Used by llm.external.baseUrl, dualModel.external.baseUrl, and stt.external.baseUrl.
export const SECURE_URL_VALIDATION: RegexValidationRule[] = [
  {
    regex: "^(https://|http://(localhost|127\\.0\\.0\\.1)(:|/|$))",
    errorMessage: "URL must use HTTPS, or HTTP with localhost / 127.0.0.1 only.",
  },
];

/** Common props every field carries. */
interface BaseField {
  id: string;
  name: string;
  description: string;
  visibleWhen?: FieldCondition;
  editableWhen?: FieldCondition;
  optionalWhen?: FieldCondition;
  optional?: boolean;
  /** Hidden on mobile (no touch-shell equivalent). Persisted state is
   *  untouched; only the control is hidden. */
  desktopOnly?: boolean;
  /** `none` never shows the description, `ondemand` puts it behind an info
   *  button, `always` renders it inline. Unset falls back to `ondemand` when
   *  description is non-empty, else `none`. */
  descriptionTier?: "none" | "ondemand" | "always";
}

/** Shared shape for free-text inputs (string / password / multiline). */
interface TextLikeFieldProps {
  defaultValue: string;
  placeholder?: string;
  regex?: RegexValidation;
  suffix?: string;
}

/** Shared shape for numeric inputs (number / float). `""` is the sentinel
 *  for "unset" on `optional` numeric fields. */
interface NumberLikeFieldProps {
  defaultValue: number | string;
  placeholder?: string;
  regex?: RegexValidation;
  suffix?: string;
  // Optional inclusive bounds: validation-only guards enforced by
  // validateSettingsPatch, unlike number_slider's required, rendered track.
  min?: number;
  max?: number;
}

export type StringField = BaseField & TextLikeFieldProps & { type: "string" };
export type PasswordField = BaseField &
  TextLikeFieldProps & {
    type: "password";
  };
export type MultilineField = BaseField &
  TextLikeFieldProps & {
    type: "multiline";
    mono?: boolean;
  };
export type NumberField = BaseField & NumberLikeFieldProps & { type: "number" };
export type FloatField = BaseField & NumberLikeFieldProps & { type: "float" };

export type BooleanField = BaseField & {
  type: "boolean";
  defaultValue: boolean;
};
export type ColorField = BaseField & {
  type: "color";
  defaultValue: string;
  /** When set, only this color's hue + chroma seed a derived light/dark scale,
   *  so the picker hides the lightness slider and pins OKLCH L here. Use the
   *  light-mode lightness of the shade where the color is seen most (bg-surface
   *  is ~0.985). Omit for colors rendered as-is (the bubble shadow). */
  lockedLightness?: number;
};
export type ShortcutField = BaseField & {
  type: "shortcut";
  defaultValue: string;
};

/** Render-only fields whose value is ignored; defaultValue exists only so the
 *  defaults loader has a stable entry. `scope` selects whose resources the
 *  field reflects. */
export type RenderOnlyField = BaseField & {
  type: "services" | "storage";
  scope: "client" | "core";
  defaultValue: string | number | boolean;
};

/** Which client-side manager renders an object_management field. The schema
 *  names WHICH manager, never HOW: loading, actions, and detail rendering live
 *  in the per-type field component `SettingsField.svelte` dispatches to. */
export type ObjectManagementType =
  | "snippets"
  | "tools"
  | "extensions"
  | "mcp"
  | "cores"
  | "memories"
  | "scheduled_prompts";

/** A scrollable, searchable manager for a list of objects. Render-only as far
 *  as the settings store is concerned: the managed objects live in their own
 *  stores, never in this field's value. `description` is optional (unlike other
 *  fields) because the copy lives on the owning group. */
export type ObjectManagementField = Omit<BaseField, "description"> & {
  type: "object_management";
  objectType: ObjectManagementType;
  description?: string;
  /** Present only so the defaults loader has a stable entry; ignored. */
  defaultValue: string | number | boolean;
};

export type NumberSliderField = BaseField & {
  type: "number_slider";
  defaultValue: number;
  min: number;
  max: number;
  step?: number;
  suffix?: string;
};

/** Either a static `options` list or an `optionsSource` the UI resolves at
 *  runtime, never both. */
export type SelectField = BaseField & {
  type: "select";
  defaultValue: string | number;
} & (
    | { options: SettingOption[]; optionsSource?: never }
    | { optionsSource: OptionsSource; options?: never }
  );

export type PresetField = BaseField & {
  type: "preset";
  defaultValue: string;
  presetConfig: PresetConfig;
};

/** Like `preset`, but the cards' models + tuning are computed per-device at
 *  runtime. Rendered by ModelPresetField. */
export type ModelPresetField = BaseField & {
  type: "model_preset";
  defaultValue: string;
  presetConfig: PresetConfig;
};

/** Like `model_preset` for Speech-to-Text: the cards bind to whisper models in
 *  the signed catalog (no per-device fit). Rendered by SttPresetField. */
export type SttPresetField = BaseField & {
  type: "stt_preset";
  defaultValue: string;
  presetConfig: PresetConfig;
};

/** Like `stt_preset` for Text-to-Speech. Rendered by TtsPresetField. */
export type TtsPresetField = BaseField & {
  type: "tts_preset";
  defaultValue: string;
  presetConfig: PresetConfig;
};

export type CommandPreviewField = BaseField & {
  type: "command_preview";
  defaultValue: string | boolean;
  commandType: CommandType;
};

export type SettingField =
  | StringField
  | PasswordField
  | MultilineField
  | NumberField
  | FloatField
  | BooleanField
  | ColorField
  | ShortcutField
  | RenderOnlyField
  | ObjectManagementField
  | NumberSliderField
  | SelectField
  | PresetField
  | ModelPresetField
  | SttPresetField
  | TtsPresetField
  | CommandPreviewField;

export type SettingType = SettingField["type"];

export interface SettingSection {
  /** A labeled section is collapsible (the label row is the toggle); an
   *  unlabeled one renders inline. A group's unlabeled sections must all come
   *  above its labeled ones. */
  label?: string;
  /** Which of the owning group's `tabs` this section belongs to. Required when
   *  the group sets `tabs`. `sections` stays one flat list (index is identity);
   *  only the renderer partitions by tab. */
  tab?: string;
  /** Persistence destination overriding the group's. In a hybrid group (an
   *  array destination spanning client and core) every section MUST be labeled
   *  and MUST set this. The one other sanctioned use is an unlabeled section of
   *  hidden persisted flags that must live on the other side, e.g.
   *  `extensions.skipRiskyGrantWarning`. */
  destination?: SettingDestination;
  visibleWhen?: FieldCondition;
  expandWhen?: FieldCondition;
  /** Hidden on mobile. Persisted state is untouched. */
  desktopOnly?: boolean;
  fields: SettingField[];
  /** Start collapsed on a fresh load and on "reset group to defaults". Only
   *  meaningful for labeled sections. */
  defaultCollapsed?: boolean;
}

/** Where a setting is persisted. Three storage locations, two user-visible
 *  labels (any `client-on-*` chips as "Client", `core` as "Core"):
 *   - "client-on-client": this device's `~/.loca/client/settings.json`; pure
 *     device preferences the core never reads.
 *   - "client-on-core": this client's row in the core's `client_settings`
 *     table, layered over `core` as `loadEffective(clientId)`, so the core can
 *     apply them for sessions with no client frame.
 *   - "core": the shared `~/.loca/core/settings.json`; shared resources. */
export type SettingDestination = "client-on-client" | "client-on-core" | "core";

/** The user-visible label for a destination. */
export function destinationLabel(dest: SettingDestination): "Client" | "Core" {
  return dest === "core" ? "Core" : "Client";
}

/** True when persisting this destination requires a live core connection. */
export function destinationNeedsCore(dest: SettingDestination): boolean {
  return dest !== "client-on-client";
}

/** One mode of a tabbed group, partitioning its sections behind a selector
 *  below the group description. The group's description still owns the copy. */
export interface SettingTab {
  id: string;
  label: string;
}

export interface SettingGroup {
  id: string;
  name: string;
  /** Rendered under the group header, with the same tiers as a field's. */
  description?: string;
  descriptionTier?: "none" | "ondemand" | "always";
  /** Omitted from the settings UI entirely. For groups that exist only to
   *  register setting ids / a persistence destination (e.g. `cores`). */
  hidden?: boolean;
  /** Hidden on mobile, honored by `isGroupVisible(group, platform)`.
   *
   *  About the DEVICE, where `requiresCapability` is about the CORE: a phone
   *  paired to a desktop Core still gets that Core's extensions. */
  desktopOnly?: boolean;
  /** Hidden unless the connected Core reports that capability on
   *  `GET /api/v1/health`. Use it for something the connected Core cannot do at
   *  all, never for something the screen cannot show. Unknown counts as
   *  capable. */
  requiresCapability?: keyof CoreCapabilities;
  /** Where this group's values are persisted, usually one destination. Only a
   *  render-only group should declare both (persistable fields route to the
   *  first listed). The header renders one chip per destination. */
  destination: SettingDestination | SettingDestination[];
  /** Filled-variant icon class. Used when the group is the active sidebar entry. */
  icon: string;
  /** Outline-variant icon class. Falls back to `icon` when the icon set has no
   *  outline equivalent. */
  iconInactive?: string;
  /** Renders a tab selector below the description, showing only the sections
   *  whose `tab` matches. The first tab is the default. */
  tabs?: SettingTab[];
  sections: SettingSection[];
}

export type ConditionDep =
  | {
      kind: "field";
      fieldId: string;
      condition: "editableWhen" | "optionalWhen";
    }
  | {
      kind: "section";
      groupId: string;
      sectionIndex: number;
      condition: "visibleWhen" | "expandWhen";
    };

export interface SearchResultGroup {
  groupId: string;
  groupName: string;
  sectionKey: string;
  sectionLabel?: string;
  fields: SettingField[];
}

/** Sparse on-disk format: only non-defaults are serialized. Defaults come
 *  from the schema at runtime. Used by both client and core's settings.json. */
export type SettingsRecord = Record<string, unknown>;

// --- Group id registry -----------------------------------------------------
// Group ids classified by the label of their destinations, so a group spanning
// both is listed twice. Hand-maintained so they carry literal types; a
// derive-check in engine.test.ts fails CI if they drift from SETTINGS_SCHEMA.
export const CLIENT_GROUP_IDS = [
  "appearance",
  "shortcuts",
  "snippets",
  "cores",
  "usage",
  "stt",
  "tts",
  "general",
  "greetings",
  "prompts",
  "llm",
  "memories",
  "dualModel",
  "tools",
] as const;
export type ClientGroupId = (typeof CLIENT_GROUP_IDS)[number];

export const CORE_GROUP_IDS = [
  "llm",
  "memories",
  "scheduledPrompts",
  "dualModel",
  "tools",
  "extensions",
  "mcp",
  "stt",
  "tts",
  "usage",
] as const;
export type CoreGroupId = (typeof CORE_GROUP_IDS)[number];

export type SettingGroupId = ClientGroupId | CoreGroupId;

export function isClientGroup(id: string): id is ClientGroupId {
  return (CLIENT_GROUP_IDS as readonly string[]).includes(id);
}

export function isCoreGroup(id: string): id is CoreGroupId {
  return (CORE_GROUP_IDS as readonly string[]).includes(id);
}

/** Normalize a group's destination(s) to an array. */
export function groupDestinations(group: {
  destination: SettingDestination | SettingDestination[];
}): SettingDestination[] {
  return Array.isArray(group.destination) ? group.destination : [group.destination];
}

/** The chips a group header shows: destinations deduped by visible label,
 *  preserving declared order. */
export function groupDestinationChips(group: {
  destination: SettingDestination | SettingDestination[];
}): SettingDestination[] {
  const out: SettingDestination[] = [];
  const seen = new Set<string>();
  for (const dest of groupDestinations(group)) {
    const label = destinationLabel(dest);
    if (seen.has(label)) continue;
    seen.add(label);
    out.push(dest);
  }
  return out;
}
