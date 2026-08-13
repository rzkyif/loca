/**
 * Settings visibility helpers (section/group visible, default-expanded
 * sections) and the search index that powers the settings search box.
 */

import type { SearchResultGroup, SettingField, SettingGroup, SettingSection } from "./types.ts";
import { SETTINGS_SCHEMA } from "./schema.ts";
import { evalCondition } from "./conditions.ts";

/** True when a section should be rendered in non-search mode. A section only
 *  disappears when it has no fields; per-field `visibleWhen` is applied by the
 *  renderer. (Collapse is a separate, purely-UI concern; a collapsed section is
 *  still "visible" here, just rendered header-only.) */
export function isSectionVisible(section: SettingSection): boolean {
  return section.fields.length > 0;
}

/** True when a group should appear in the settings UI. `platform` defaults to
 *  desktop, so existing desktop callers are unaffected; passing `"mobile"` also
 *  drops `desktopOnly` groups (e.g. global shortcuts) that have no mobile
 *  equivalent. */
export function isGroupVisible(
  group: SettingGroup,
  platform: "desktop" | "mobile" = "desktop",
): boolean {
  if (group.hidden) return false;
  if (group.desktopOnly && platform === "mobile") return false;
  return true;
}

/** The set of section keys (`${groupId}-${sectionIndex}`) that are expanded by
 *  default: every labeled section in a non-hidden group that isn't flagged
 *  `defaultCollapsed`. Used to seed the Settings panel on mount and to restore
 *  a group's default expand/collapse state. Unlabeled sections render their
 *  fields inline (no collapse), so they're omitted. */
export function defaultExpandedSections(): Set<string> {
  const expanded = new Set<string>();
  for (const group of SETTINGS_SCHEMA) {
    if (group.hidden) continue;
    group.sections.forEach((section, si) => {
      if (section.label && !section.defaultCollapsed) {
        expanded.add(`${group.id}-${si}`);
      }
    });
  }
  return expanded;
}

/** Search all settings fields by query string, grouped by section. Skips
 *  sections/fields whose visibility conditions fail against the current
 *  settings. */
export function searchFields(
  query: string,
  currentSettings: Record<string, unknown>,
  platform: "desktop" | "mobile" = "desktop",
): SearchResultGroup[] {
  if (!query.trim()) return [];
  const q = query.toLowerCase();
  const results: SearchResultGroup[] = [];

  for (const group of SETTINGS_SCHEMA) {
    // Skip groups the current platform hides (e.g. desktop-only Shortcuts on
    // mobile) so search never surfaces a field the sidebar won't navigate to.
    if (!isGroupVisible(group, platform)) continue;
    for (let si = 0; si < group.sections.length; si++) {
      const section = group.sections[si];
      if (!evalCondition(section.visibleWhen, currentSettings)) continue;
      if (section.desktopOnly && platform === "mobile") continue;

      const matched: SettingField[] = [];
      for (const field of section.fields) {
        if (field.desktopOnly && platform === "mobile") continue;
        // command_preview is a derived display, not user-targetable; the
        // services/storage display panels and object_management managers
        // (snippets/extensions/cores) have no atomic field-level state to surface
        // in search results, and a manager is a full scrolling surface that
        // doesn't render sensibly inline, so they're excluded.
        if (
          field.type === "command_preview" ||
          field.type === "services" ||
          field.type === "storage" ||
          field.type === "object_management"
        ) {
          continue;
        }
        if (!evalCondition(field.visibleWhen, currentSettings)) continue;
        if (fieldMatchesQuery(field, q)) {
          matched.push(field);
        }
      }

      if (matched.length > 0) {
        results.push({
          groupId: group.id,
          groupName: group.name,
          sectionKey: `${group.id}-${si}`,
          sectionLabel: section.label,
          fields: matched,
        });
      }
    }
  }

  return results;
}

function fieldMatchesQuery(field: SettingField, q: string): boolean {
  if (field.name.toLowerCase().includes(q)) return true;
  // description is optional on object_management fields (it lives on the group).
  if (field.description?.toLowerCase().includes(q)) return true;

  if (field.type === "select" && field.options) {
    for (const opt of field.options) {
      if (opt.label.toLowerCase().includes(q)) return true;
    }
  }

  if (
    field.type === "preset" ||
    field.type === "model_preset" ||
    field.type === "stt_preset" ||
    field.type === "tts_preset"
  ) {
    for (const opt of field.presetConfig.options) {
      if (opt.label.toLowerCase().includes(q)) return true;
    }
    if (field.presetConfig.secondaryOptions) {
      for (const opt of field.presetConfig.secondaryOptions) {
        if (opt.label.toLowerCase().includes(q)) return true;
      }
    }
  }

  return false;
}
