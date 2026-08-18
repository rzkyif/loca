// Emits the core's projection of the settings schema, for
// `packages/loca-core-rs/src/settings/schema.json`.
//
// Run via `deno task contract`, not directly.
//
// The core does not render settings, so it does not need most of SETTINGS_SCHEMA.
// It needs, per field: the id, the type, the persistence destination, the
// default value, the numeric bounds, the static select options, and the regex
// rules. That is exactly what `validateSettingsPatch`, `getDefaultSettings`,
// `settingKeyDestination` and `SECRET_KEYS` read. Everything else (names,
// descriptions, icons, presets, tabs, visibleWhen, descriptionTier) is Client
// rendering, and 32 KB of it is product copy under COPY.md that no core code
// path reads.
//
// Secret keys are not a field here: they are exactly the `type: "password"`
// fields, derived on the Rust side the same way `SECRET_KEYS` derives them, so a
// new password field opts in with no manifest to update.

import { SETTINGS_SCHEMA } from "../../packages/loca-shared/src/domain/settings/schema.ts";
import { settingKeyDestination } from "../../packages/loca-shared/src/domain/settings/routing.ts";
import type { SettingField } from "../../packages/loca-shared/src/domain/settings/types.ts";

// Render-only field types hold no persisted scalar: `getDefaultSettings` skips
// them and `validateSettingsPatch` rejects them. They still appear here, since
// rejecting one requires knowing it exists.
const RENDER_ONLY = new Set(["command_preview", "services", "storage", "object_management"]);

interface ProjectedField {
  id: string;
  type: string;
  destination: string;
  defaultValue?: unknown;
  min?: number;
  max?: number;
  options?: (string | number)[];
  regex?: unknown;
}

function project(field: SettingField): ProjectedField {
  const f = field as unknown as Record<string, unknown>;
  const destination = settingKeyDestination(field.id);
  if (!destination) throw new Error(`${field.id} has no destination`);
  const out: ProjectedField = { id: field.id, type: field.type, destination };
  if (!RENDER_ONLY.has(field.type)) out.defaultValue = f.defaultValue;
  if (typeof f.min === "number") out.min = f.min;
  if (typeof f.max === "number") out.max = f.max;
  if (Array.isArray(f.options)) {
    out.options = (f.options as { value: string | number }[]).map((o) => o.value);
  }
  if (f.regex !== undefined) out.regex = f.regex;
  return out;
}

const fields: ProjectedField[] = [];
const byId = new Map<string, ProjectedField>();

for (const group of SETTINGS_SCHEMA) {
  for (const section of group.sections) {
    for (const field of section.fields) {
      const projected = project(field);
      const existing = byId.get(field.id);
      // One id may be declared twice: `speech.binaryBackend` appears in both the
      // stt and tts groups, differing only in `visibleWhen`, which is not
      // projected. Collapsing them is safe only while they agree on everything
      // the core reads, and the TypeScript resolves a disagreement by iteration
      // order alone (findField takes the first, getDefaultSettings and
      // keyDestinations take the last). Throwing is what stops that being latent.
      if (existing) {
        if (JSON.stringify(existing) !== JSON.stringify(projected)) {
          throw new Error(
            `"${field.id}" is declared more than once with different core-visible ` +
              `properties:\n  ${JSON.stringify(existing)}\n  ${JSON.stringify(projected)}\n` +
              "The core reads one entry per id. Give them distinct ids, or make the " +
              "declarations agree.",
          );
        }
        continue;
      }
      byId.set(field.id, projected);
      fields.push(projected);
    }
  }
}

const out = Deno.args[0];
if (!out) throw new Error("usage: emit-settings-schema.ts <out.json>");
await Deno.writeTextFile(
  out,
  JSON.stringify(
    {
      generatedBy: "deno task contract, via scripts/contract/emit-settings-schema.ts. Do not edit.",
      fields,
    },
    null,
    2,
  ) + "\n",
);
console.log(`settings: ${fields.length} fields projected from ${SETTINGS_SCHEMA.length} groups`);
