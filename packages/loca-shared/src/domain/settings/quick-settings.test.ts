// Schema-drift guard for the Quick Settings manifest: every referenced field
// id must still exist in SETTINGS_SCHEMA (a rename there would otherwise
// silently drop the field from Quick Settings at runtime).

import { assert, assertEquals } from "@std/assert";
import type { FieldCondition } from "./types.ts";
import { findField, isValidSettingKey } from "./engine.ts";
import { QUICK_SETTINGS_SECTIONS } from "./quick-settings.ts";

function conditionFields(cond: FieldCondition | undefined): string[] {
  if (!cond) return [];
  const out: string[] = [];
  if (cond.field !== undefined) out.push(cond.field);
  for (const sub of cond.allOf ?? []) out.push(...conditionFields(sub));
  return out;
}

Deno.test("quick settings manifest: resolves every field id against the schema", () => {
  for (const section of QUICK_SETTINGS_SECTIONS) {
    for (const ref of section.fields) {
      assert(findField(ref.id), `${section.id}: ${ref.id}`);
    }
  }
});

Deno.test("quick settings manifest: every header toggle is a boolean field", () => {
  for (const section of QUICK_SETTINGS_SECTIONS) {
    if (!section.enabledField) continue;
    const field = findField(section.enabledField);
    assert(field, `${section.id}: ${section.enabledField}`);
    assertEquals(field?.type, "boolean", `${section.id}: ${section.enabledField}`);
  }
});

Deno.test("quick settings manifest: visibility conditions reference known keys", () => {
  for (const section of QUICK_SETTINGS_SECTIONS) {
    for (const ref of section.fields) {
      for (const key of conditionFields(ref.visibleWhen)) {
        assert(isValidSettingKey(key), `${section.id}: ${ref.id} -> ${key}`);
      }
    }
  }
});

Deno.test("quick settings manifest: no duplicate field ids within a section", () => {
  for (const section of QUICK_SETTINGS_SECTIONS) {
    const ids = section.fields.map((ref) => ref.id);
    assertEquals(new Set(ids).size, ids.length, section.id);
  }
});

Deno.test("quick settings manifest: at least one unconditional field per section", () => {
  // The renderer has no empty-body state: a section whose every field is
  // hidden by a manifest condition would open onto nothing.
  for (const section of QUICK_SETTINGS_SECTIONS) {
    assert(
      section.fields.some((ref) => !ref.visibleWhen),
      section.id,
    );
  }
});
