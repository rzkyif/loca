import type { ComponentProps } from "svelte";
import type { OmitSnippetProps } from "./types.ts";
import type SettingsShellView from "../components/settings/SettingsShellView.svelte";
import { SAMPLE_FIRST_GROUP, SAMPLE_GROUPS } from "./settings-groups.ts";

// `groupContent` / `searchContent` / `sidebarFooter` snippets are supplied by
// the renderer (the gallery and showcase wrap a SettingsContentView).
export const settingsShellSamples = {
  default: {
    groups: SAMPLE_GROUPS,
    selectedGroupId: SAMPLE_FIRST_GROUP,
    sidebarCollapsed: false,
  },
  collapsedSidebar: {
    groups: SAMPLE_GROUPS,
    selectedGroupId: SAMPLE_FIRST_GROUP,
    sidebarCollapsed: true,
  },
} satisfies Record<string, OmitSnippetProps<ComponentProps<typeof SettingsShellView>>>;
