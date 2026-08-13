import type { ComponentProps } from "svelte";
import type { OmitSnippetProps } from "./types.ts";
import type ObjectDetailHeaderView from "../components/objects/ObjectDetailHeaderView.svelte";

// The standardized object detail header: title + badges and an optional
// subtitle. (Header actions carry callbacks, so they belong to the renderer, not
// a pure-data sample.) Covers title-only, with-subtitle, and with badges.
export const objectDetailHeaderSamples = {
  titleOnly: { title: "My Extension" },
  withSubtitle: { title: "filesystem", subtitle: "Local (stdio)" },
  withBadges: {
    title: "Code Search",
    subtitle: "Built-in extension",
    badges: [
      { label: "Enabled", accent: "green" },
      { label: "Updated", accent: "blue" },
    ],
  },
  withActions: {
    title: "Code Search",
    subtitle: "Built-in extension",
    actions: [
      {
        label: "Update",
        icon: "i-material-symbols-download-rounded",
        loading: true,
        onSelect: () => {},
      },
      {
        label: "Reinstall",
        icon: "i-material-symbols-refresh-rounded",
        disabled: true,
        onSelect: () => {},
      },
      {
        label: "Remove",
        icon: "i-material-symbols-delete-outline-rounded",
        variant: "danger",
        onSelect: () => {},
      },
    ],
  },
} satisfies Record<string, OmitSnippetProps<ComponentProps<typeof ObjectDetailHeaderView>>>;
