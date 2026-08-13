// Pure logic behind the settings Share (import / export) UI, kept out of the
// Svelte components so it can be unit tested. Everything here walks the static
// SETTINGS_SCHEMA and operates on plain records (the merged `currentSettings`
// view and the schema defaults), with no dependency on the reactive store.

import { isGroupVisible, SECRET_KEYS, type SettingField, SETTINGS_SCHEMA } from "@loca/shared";

// Field types that hold no persistable value, mirroring the filter in
// getDefaultSettings(). These never carry a value to share.
const RENDER_ONLY_TYPES = new Set<SettingField["type"]>([
  "command_preview",
  "services",
  "storage",
  "object_management",
]);

const SECRET_KEY_SET = new Set<string>(SECRET_KEYS);

// A field is shareable when it carries a real persisted value and is not a
// secret: secret values are never returned to the client, so they cannot be
// exported and must not travel as raw JSON.
function isShareableField(field: SettingField): boolean {
  return !RENDER_ONLY_TYPES.has(field.type) && !SECRET_KEY_SET.has(field.id);
}

/** Every field id eligible for import/export, in schema order. The single
 *  source of truth for "what can be shared": excludes render-only fields,
 *  secrets, and hidden groups. */
export const EXPORTABLE_FIELD_IDS: ReadonlySet<string> = (() => {
  const out = new Set<string>();
  for (const group of SETTINGS_SCHEMA) {
    if (!isGroupVisible(group)) continue;
    for (const section of group.sections) {
      for (const field of section.fields) {
        if (isShareableField(field)) out.add(field.id);
      }
    }
  }
  return out;
})();

export interface TreeField {
  id: string;
  name: string;
  /** Importing this field would overwrite a value the user already customized. */
  warn: boolean;
  /** Not applicable: importing it is a no-op (its value already matches). */
  disabled: boolean;
}

export interface TreeSection {
  /** `${groupId}-${sectionIndex}`, matching the settings panel's section keys. */
  key: string;
  label: string;
  fields: TreeField[];
}

export interface TreeGroup {
  id: string;
  name: string;
  /** Fields of the group's unlabeled (inline) sections, hoisted to sit directly
   *  under the group with no section row, matching how they render inline. */
  fields: TreeField[];
  sections: TreeSection[];
}

/** Build the group -> section -> field tree from the schema, keeping only
 *  fields in `allowedIds`. Empty sections and empty groups are dropped.
 *  `warnFor` flags fields whose import would overwrite a customized value;
 *  `disabledFor` flags fields whose import is a no-op (not applicable). */
export function buildTree(
  allowedIds: ReadonlySet<string>,
  warnFor?: (id: string) => boolean,
  disabledFor?: (id: string) => boolean,
): TreeGroup[] {
  const groups: TreeGroup[] = [];
  for (const group of SETTINGS_SCHEMA) {
    if (!isGroupVisible(group)) continue;
    const directFields: TreeField[] = [];
    const sections: TreeSection[] = [];
    group.sections.forEach((section, index) => {
      const fields: TreeField[] = [];
      for (const field of section.fields) {
        if (!allowedIds.has(field.id)) continue;
        fields.push({
          id: field.id,
          name: field.name,
          warn: warnFor?.(field.id) ?? false,
          disabled: disabledFor?.(field.id) ?? false,
        });
      }
      if (fields.length === 0) return;
      if (section.label) {
        sections.push({
          key: `${group.id}-${index}`,
          label: section.label,
          fields,
        });
      } else {
        directFields.push(...fields);
      }
    });
    if (directFields.length === 0 && sections.length === 0) continue;
    groups.push({
      id: group.id,
      name: group.name,
      fields: directFields,
      sections,
    });
  }
  return groups;
}

/** Every field id reachable in a tree group (direct + section fields). */
export function groupFieldIds(group: TreeGroup): string[] {
  return [
    ...group.fields.map((f) => f.id),
    ...group.sections.flatMap((s) => s.fields.map((f) => f.id)),
  ];
}

/** The export JSON: only the selected fields whose current value differs from
 *  the schema default, in schema order for a stable, diff-friendly output. */
export function computeExportJson(
  selectedIds: ReadonlySet<string>,
  current: Record<string, unknown>,
  defaults: Record<string, unknown>,
): string {
  const out: Record<string, unknown> = {};
  for (const id of EXPORTABLE_FIELD_IDS) {
    if (!selectedIds.has(id)) continue;
    if (!Object.is(current[id], defaults[id])) out[id] = current[id];
  }
  return JSON.stringify(out, null, 2);
}

export interface ParsedImport {
  /** Importable keys found in the pasted JSON, mapped to their incoming value. */
  values: Record<string, unknown>;
  /** Keys present in the JSON that aren't shareable settings (unknown, secret,
   *  or render-only). Surfaced to the user and excluded from import. */
  unknownKeys: string[];
  /** Set when the text is non-empty but not a JSON object of setting keys. */
  error?: string;
}

/** Parse pasted import text into shareable values plus a list of ignored keys. */
export function parseImport(text: string): ParsedImport {
  const trimmed = text.trim();
  if (!trimmed) return { values: {}, unknownKeys: [] };
  let parsed: unknown;
  try {
    parsed = JSON.parse(trimmed);
  } catch {
    return { values: {}, unknownKeys: [], error: "Not valid JSON" };
  }
  if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
    return {
      values: {},
      unknownKeys: [],
      error: "Expected a JSON object of setting keys.",
    };
  }
  const values: Record<string, unknown> = {};
  const unknownKeys: string[] = [];
  for (const [key, value] of Object.entries(parsed as Record<string, unknown>)) {
    if (EXPORTABLE_FIELD_IDS.has(key)) values[key] = value;
    else unknownKeys.push(key);
  }
  return { values, unknownKeys };
}

export type ImportKind = "apply" | "overwrite" | "noop";

/** How importing one key would land: `noop` if the value is unchanged,
 *  `overwrite` if it replaces a customized (non-default) value, else `apply`. */
export function classifyImport(
  incoming: unknown,
  current: unknown,
  defaultValue: unknown,
): ImportKind {
  if (Object.is(incoming, current)) return "noop";
  return Object.is(current, defaultValue) ? "apply" : "overwrite";
}
