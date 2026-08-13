import type { ComponentProps } from "svelte";
import type { OmitSnippetProps } from "./types.ts";
import type SettingsFieldView from "../components/settings/SettingsFieldView.svelte";
import { getDefaultSettings, SETTINGS_SCHEMA } from "../../domain/settings/engine.ts";
import type { SettingField } from "../../domain/settings/types.ts";

// Picks a real field of each type the View renders directly (not the delegated
// complexField stand-ins) straight from the schema, with its default value, so
// each field sample equals a fresh app and survives field-id churn.
const D = getDefaultSettings();
const allFields: SettingField[] = SETTINGS_SCHEMA.flatMap((g) =>
  g.sections.flatMap((s) => s.fields),
);

function sample(type: SettingField["type"]) {
  const field = allFields.find((f) => f.type === type);
  if (!field) {
    throw new Error(`no sample settings field of type "${type}" in the schema`);
  }
  return { field, value: D[field.id] };
}

export const settingsFieldSamples = {
  boolean: sample("boolean"),
  select: sample("select"),
  text: sample("string"),
  number: sample("number"),
  slider: sample("number_slider"),
  color: { ...sample("color"), isDark: false },
  float: sample("float"),
  colorDark: { ...sample("color"), isDark: true },
  withError: { ...sample("string"), error: "Must not be empty" },
  readOnly: { ...sample("string"), editable: false },
  horizontalReset: { ...sample("number"), horizontal: true, showReset: true },
} satisfies Record<string, OmitSnippetProps<ComponentProps<typeof SettingsFieldView>>>;
