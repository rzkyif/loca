import type { ComponentProps } from "svelte";
import type { OmitSnippetProps } from "./types.ts";
import type ObjectBadgeView from "../components/objects/ObjectBadgeView.svelte";

// The small status chip shown on cards and detail headers. Covers the neutral
// (no accent) variant and each accent hue, plus an icon-bearing chip.
export const objectBadgeSamples = {
  neutral: { label: "Default" },
  enabled: { label: "Enabled", accent: "green" },
  error: { label: "Error", accent: "red", title: "Connection failed" },
  current: { label: "Current", accent: "blue" },
  pending: { label: "Pending", accent: "yellow" },
  withIcon: { label: "Local", icon: "i-material-symbols-computer", accent: "purple" },
} satisfies Record<string, OmitSnippetProps<ComponentProps<typeof ObjectBadgeView>>>;
