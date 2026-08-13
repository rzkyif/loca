import type { ComponentProps } from "svelte";
import type { OmitSnippetProps } from "./types.ts";
import type SettingsContentView from "../components/settings/SettingsContentView.svelte";
import { SAMPLE_FIRST_GROUP, SAMPLE_VALUES } from "./settings-groups.ts";

export const settingsContentSamples = {
  group: { groupId: SAMPLE_FIRST_GROUP, values: SAMPLE_VALUES },
  searchResults: { searchQuery: "color", values: SAMPLE_VALUES },
} satisfies Record<string, OmitSnippetProps<ComponentProps<typeof SettingsContentView>>>;
