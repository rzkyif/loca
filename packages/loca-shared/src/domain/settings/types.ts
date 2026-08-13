/**
 * Type definitions and shared validation constants for the settings schema.
 * Used by every group file under `./groups/` and by the engine in
 * `./engine.ts`. The full schema is built from these primitives and is
 * authoritative for both the client renderer and core's setting reads.
 */

// Pulled inline so this file has zero non-type dependencies. Mirrors the
// client-side command builder's accepted command kinds.
export type CommandType = "llm";

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
  /** Setting ids this preset controls, beyond any in an option's `defaults`.
   *  Editing one of these flips the preset to "custom". Used when the preset's
   *  values are computed at runtime (e.g. the adaptive LLM presets) rather than
   *  baked into static `defaults`. */
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

/**
 * Common props every field carries. Anything that influences layout, search,
 * or conditional visibility but not how the field is rendered or what it
 * stores lives here.
 */
interface BaseField {
  id: string;
  name: string;
  description: string;
  visibleWhen?: FieldCondition;
  editableWhen?: FieldCondition;
  optionalWhen?: FieldCondition;
  optional?: boolean;
  /** When true, the field is hidden on mobile (no equivalent on the touch shell,
   *  e.g. window geometry). Persisted state is untouched; only the control is
   *  hidden. The section-level flag covers a whole group of such fields. */
  desktopOnly?: boolean;
  /** Controls how the description is presented:
   *  - `none`: never shown (no info button).
   *  - `ondemand`: hidden behind an info-button toggle (default for non-empty descriptions).
   *  - `always`: rendered inline, always visible.
   *  When unset, falls back to `ondemand` if description is non-empty, else `none`. */
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
  // Optional inclusive bounds. Unlike number_slider (min/max REQUIRED, rendered
  // as a track), these are validation-only guards for a plain number/float input.
  // Enforced by validateSettingsPatch so an out-of-range value can't reach the
  // core (and a sidecar argv) - declare them for any numeric knob with a valid
  // range instead of leaving it unbounded.
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
  /** When set, this color only seeds a derived light/dark scale (its hue and
   *  chroma are used; the theme sets each shade's lightness). The picker hides
   *  the lightness slider and pins OKLCH L to this value, since editing
   *  lightness would have no rendered effect. Choose the light-mode lightness
   *  of the shade where the color is seen most (e.g. the default color and the
   *  bubble fills both use the bg-surface step, ~0.985); the picker still
   *  theme-inverts it, so dark mode shows the matching dark shade. Omit for
   *  colors rendered as-is (the bubble shadow), which keep a free lightness
   *  slider. */
  lockedLightness?: number;
};
export type ShortcutField = BaseField & {
  type: "shortcut";
  defaultValue: string;
};

/** Render-only fields whose value is ignored; defaultValue exists only so the
 *  defaults loader has a stable initial entry. `scope` selects whose resources
 *  the field reflects: the local client (process + on-device files) or the
 *  paired core (service + sidecars + its persistence tree). */
export type RenderOnlyField = BaseField & {
  type: "services" | "storage";
  scope: "client" | "core";
  defaultValue: string | number | boolean;
};

/** Discriminator selecting which client-side manager renders an
 *  object_management field. The schema names WHICH manager, never HOW: data
 *  loading, actions, and detail rendering live in the per-type field component
 *  (`components/settings/fields/{Snippets,Extensions,Cores}Field.svelte`), which
 *  `SettingsField.svelte` dispatches to on this value. */
export type ObjectManagementType =
  | "snippets"
  | "tools"
  | "extensions"
  | "mcp"
  | "cores"
  | "memories"
  | "scheduled_prompts";

/** A scrollable, searchable manager for a list of objects (snippets, extensions,
 *  paired cores, memories, scheduled prompts). Render-only as far as the
 *  settings store is concerned: the managed objects live in their own stores
 *  (client settings, core REST, keychain), never in this field's value. The
 *  `objectType` discriminator picks the client provider from the registry. */
// description is optional here (unlike other fields): an object_management
// field has no label or field-level description of its own; any descriptive
// copy lives on the owning group (SettingGroup.description), rendered under the
// group header.
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

/** A select either supplies a static `options` list or names an `optionsSource`
 *  the UI should resolve at runtime (monitors, system fonts, etc.). The two
 *  alternatives are mutually exclusive. */
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
 *  runtime (the adaptive LLM presets) rather than applying static `defaults`.
 *  Rendered by ModelPresetField; otherwise handled identically to `preset`. */
export type ModelPresetField = BaseField & {
  type: "model_preset";
  defaultValue: string;
  presetConfig: PresetConfig;
};

/** Like `model_preset` for Speech-to-Text: the cards bind to whisper models in
 *  the signed catalog (no per-device fit). Rendered by SttPresetField;
 *  otherwise handled identically to `preset`. */
export type SttPresetField = BaseField & {
  type: "stt_preset";
  defaultValue: string;
  presetConfig: PresetConfig;
};

/** Like `stt_preset` for Text-to-Speech: the cards bind to TTS models in the
 *  signed catalog. Rendered by TtsPresetField; otherwise handled identically to
 *  `preset`. */
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
  /** A labeled section is collapsible: the label row is the collapse toggle.
   *  An unlabeled section renders its fields inline (always visible, no
   *  toggle). Inline sections are allowed, but a group's unlabeled sections
   *  must come first, above any labeled (collapsible) ones; never place an
   *  inline section below a collapsible one. */
  label?: string;
  /** Which of the owning group's `tabs` this section belongs to. Required when
   *  the group sets `tabs`; ignored otherwise. The section's index in
   *  `group.sections` is still its identity (section keys, expand state), so
   *  `sections` stays one flat list and only the renderer partitions by tab. */
  tab?: string;
  /** Persistence destination for this section's fields, overriding the group's.
   *  In a hybrid group (one whose `destination` is an array spanning client and
   *  core), every section MUST be labeled and MUST set this, so each field
   *  routes correctly and the section header can show a Client/Core badge.
   *  Single-destination groups normally leave this unset and inherit the group
   *  destination; the one sanctioned exception is an unlabeled section holding
   *  only hidden persisted flags (visibleWhen never matches) that must live on
   *  the other side, e.g. `extensions.skipRiskyGrantWarning`. */
  destination?: SettingDestination;
  visibleWhen?: FieldCondition;
  expandWhen?: FieldCondition;
  /** When true, the whole section is hidden on mobile (no touch-shell
   *  equivalent, e.g. desktop startup/window options). Persisted state is
   *  untouched; only the rendering is suppressed. */
  desktopOnly?: boolean;
  fields: SettingField[];
  /** When true, the section starts collapsed (its fields hidden behind the
   *  section header) on a fresh load and on "reset group to defaults". Only
   *  meaningful for labeled sections, since the label is the collapse toggle.
   *  Unlabeled sections always render their fields inline. */
  defaultCollapsed?: boolean;
}

/** Where a setting is persisted. Three storage locations, two user-visible
 *  labels (the chip collapses any `client-on-*` to "Client", `core` to "Core"):
 *   - "client-on-client": this device's `~/.loca/client/settings.json` (Tauri).
 *     Pure device preferences the core never reads.
 *   - "client-on-core": this client's row in the core's `client_settings` table,
 *     keyed by `clients.id`. Per-user preferences the core must apply
 *     server-side (inference knobs), including for scheduled/automated sessions
 *     that have no client frame. Reached via PATCH /api/v1/settings, partitioned
 *     by destination; layered over `core` as `loadEffective(clientId)`.
 *   - "core": the shared `~/.loca/core/settings.json` on the selected paired
 *     core via PATCH /api/v1/settings. Shared physical resources. */
export type SettingDestination = "client-on-client" | "client-on-core" | "core";

/** The user-visible label for a destination: the two-value collapse the chip
 *  renders. Both `client-on-*` values are "this client"; `core` is shared. */
export function destinationLabel(dest: SettingDestination): "Client" | "Core" {
  return dest === "core" ? "Core" : "Client";
}

/** True when persisting this destination requires a live core connection: both
 *  `core` (shared store) and `client-on-core` (per-client overlay) live on the
 *  core, while `client-on-client` is a local file write. Drives the client's
 *  save-layer routing and the reconnect-dim of core-backed groups. */
export function destinationNeedsCore(dest: SettingDestination): boolean {
  return dest !== "client-on-client";
}

/** One mode of a tabbed group. A tab partitions the group's sections (by
 *  `section.tab`) behind a selector rendered below the group description. A tab
 *  may hold a single object_management section (a full-height manager) or a
 *  stack of config sections, never forcing the manager into its own group. The
 *  group's single description still owns the explanatory copy; a tab is just a
 *  label. */
export interface SettingTab {
  id: string;
  label: string;
}

export interface SettingGroup {
  id: string;
  name: string;
  /** Optional group-level description rendered under the group header. Same
   *  presentation tiers as a field's description: `always` shows it
   *  persistently, `ondemand` keeps it behind the header's info-button toggle,
   *  `none` (or unset, when description is empty) shows nothing. Primarily for
   *  object_management groups whose single field carries no label of its own. */
  description?: string;
  descriptionTier?: "none" | "ondemand" | "always";
  /** When true, the group is omitted from the settings UI entirely (no sidebar
   *  entry, not rendered). Used for groups that exist only to register setting
   *  ids / a persistence destination (e.g. `cores`). */
  hidden?: boolean;
  /** When true, the group is hidden on mobile (the platform has no equivalent of
   *  the feature it configures, e.g. global keyboard shortcuts). Honored by
   *  `isGroupVisible(group, platform)` so the desktop UI is unaffected. */
  desktopOnly?: boolean;
  /** Where this group's values are persisted. Usually a single destination. A
   *  group may declare BOTH (an array) when it spans client and core - currently
   *  only the render-only `usage` group, which shows client + core resources
   *  side by side. A group with real (persistable) fields should stay single-
   *  destination, since persistable fields route to the first listed
   *  destination; only render-only groups should span both. The header renders
   *  one chip per destination. */
  destination: SettingDestination | SettingDestination[];
  /** Filled-variant icon class. Used when the group is the active sidebar entry. */
  icon: string;
  /** Outline-variant icon class. Falls back to `icon` when the icon set has no
   *  outline equivalent (e.g. tune, call-split). */
  iconInactive?: string;
  /** When set, the group renders a tab selector below its description and shows
   *  only the sections whose `tab` matches the active tab. The first tab is the
   *  default. Lets one group host both a config view and an object_management
   *  manager, instead of splitting them across two groups. */
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
// The full set of group ids known to the schema, classified by destination.
// Used by the client persistence layer to decide which settings go where.
// Hand-maintained here so they carry literal types (ClientGroupId/CoreGroupId);
// a derive-check in engine.test.ts fails CI if these ever drift from each
// group's `destination` in SETTINGS_SCHEMA.

// A group id appears in CLIENT_GROUP_IDS if any of its destinations carry the
// "Client" label (either `client-on-client` or `client-on-core`) and in
// CORE_GROUP_IDS if any carry the "Core" label. A group spanning both (e.g.
// `usage`, or a hybrid llm/tools group) is listed in BOTH.
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

/** The chips a group header shows: its destinations deduped by visible label
 *  (so a group spanning `client-on-client` and `client-on-core` shows one
 *  "Client" chip, not two), preserving declared order. Each returned value is a
 *  representative destination for its label, rendered as a group-header chip. */
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
