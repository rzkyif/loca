import type { ComponentProps } from "svelte";
import type { OmitSnippetProps } from "./types.ts";
import type SettingsSidebarView from "../components/settings/SettingsSidebarView.svelte";
import { SAMPLE_FIRST_GROUP, SAMPLE_GROUPS } from "./settings-groups.ts";

const noop = (): void => {};

export const settingsSidebarSamples = {
  expanded: {
    groups: SAMPLE_GROUPS,
    selectedGroupId: SAMPLE_FIRST_GROUP,
    collapsed: false,
    onToggleCollapse: noop,
    onSelectGroup: noop,
  },
  collapsed: {
    groups: SAMPLE_GROUPS,
    selectedGroupId: SAMPLE_FIRST_GROUP,
    collapsed: true,
    onToggleCollapse: noop,
    onSelectGroup: noop,
  },
} satisfies Record<string, OmitSnippetProps<ComponentProps<typeof SettingsSidebarView>>>;
