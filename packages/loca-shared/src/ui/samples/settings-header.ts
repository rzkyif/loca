import type { ComponentProps } from "svelte";
import type { OmitSnippetProps } from "./types.ts";
import type SettingsHeaderView from "../components/settings/SettingsHeaderView.svelte";

export const settingsHeaderSamples = {
  default: { searchValue: "" },
  searching: { searchValue: "color" },
} satisfies Record<string, OmitSnippetProps<ComponentProps<typeof SettingsHeaderView>>>;
