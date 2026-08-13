/**
 * Field/section visibility conditions: evaluating a `FieldCondition` against
 * the current settings, and the reverse dependency map (which fields a setting
 * key's value affects) so a single change re-evaluates only what depends on it.
 */

import type { ConditionDep, FieldCondition } from "./types.ts";
import { SETTINGS_SCHEMA } from "./schema.ts";

export function evalCondition(
  cond: FieldCondition | undefined,
  currentSettings: Record<string, unknown>,
): boolean {
  if (!cond) return true;
  if (cond.field !== undefined) {
    const val = currentSettings[cond.field];
    if (cond.eq !== undefined && val !== cond.eq) return false;
    if (cond.neq !== undefined && val === cond.neq) return false;
    if (cond.in !== undefined && !cond.in.includes(val as string | number | boolean)) {
      return false;
    }
    if (cond.nin !== undefined && cond.nin.includes(val as string | number | boolean)) {
      return false;
    }
  }
  if (cond.allOf) {
    for (const sub of cond.allOf) {
      if (!evalCondition(sub, currentSettings)) return false;
    }
  }
  return true;
}

// --- Condition dependency map ---
// Maps a setting key to the field/section IDs whose conditions depend on it.
// Built once from the schema so that handleChange can efficiently find what
// to re-evaluate without scanning the entire schema on every change.

let _conditionDeps: Map<string, ConditionDep[]> | null = null;

export function getConditionDeps(): Map<string, ConditionDep[]> {
  if (_conditionDeps) return _conditionDeps;

  const deps = new Map<string, ConditionDep[]>();

  function add(key: string, dep: ConditionDep) {
    let list = deps.get(key);
    if (!list) {
      list = [];
      deps.set(key, list);
    }
    list.push(dep);
  }

  // Walk a condition (including any nested `allOf` children) and yield each
  // field id it depends on. Used so a single setting change can re-evaluate
  // every dependent field/section regardless of how deeply nested.
  function fieldsOf(cond: FieldCondition | undefined): string[] {
    if (!cond) return [];
    const out: string[] = [];
    if (cond.field !== undefined) out.push(cond.field);
    if (cond.allOf) {
      for (const sub of cond.allOf) out.push(...fieldsOf(sub));
    }
    return out;
  }

  for (const group of SETTINGS_SCHEMA) {
    for (let si = 0; si < group.sections.length; si++) {
      const section = group.sections[si];
      for (const field of fieldsOf(section.visibleWhen)) {
        add(field, {
          kind: "section",
          groupId: group.id,
          sectionIndex: si,
          condition: "visibleWhen",
        });
      }
      for (const field of fieldsOf(section.expandWhen)) {
        add(field, {
          kind: "section",
          groupId: group.id,
          sectionIndex: si,
          condition: "expandWhen",
        });
      }
      for (const f of section.fields) {
        for (const field of fieldsOf(f.editableWhen)) {
          add(field, {
            kind: "field",
            fieldId: f.id,
            condition: "editableWhen",
          });
        }
        for (const field of fieldsOf(f.optionalWhen)) {
          add(field, {
            kind: "field",
            fieldId: f.id,
            condition: "optionalWhen",
          });
        }
      }
    }
  }

  _conditionDeps = deps;
  return deps;
}
