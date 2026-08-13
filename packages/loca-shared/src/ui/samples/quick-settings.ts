import type { ComponentProps } from "svelte";
import type { OmitSnippetProps } from "./types.ts";
import { getDefaultSettings } from "../../domain/settings/engine.ts";
import type QuickSettingsView from "../components/quick-settings/QuickSettingsView.svelte";

// The Quick Settings panel. The sections come from the shared manifest and the
// fields render statically from the schema defaults (no `field` snippet), so a
// gallery card shows the exact accordion a fresh app shows. These stand-ins
// only vary the panel-level state: the General section open, and the
// pending-downloads variant where the exits route to Settings instead of chat.

const D = getDefaultSettings();

export const quickSettingsSamples = {
  // The common case: nothing pending, so the exits go straight to chat.
  default: {
    values: D,
    openId: "general",
    exitLabel: "Continue to Chat",
    exitTitle: "Back to Chat",
    exitIcon: "i-material-symbols-arrow-forward-rounded",
  },

  // A field here picked something that still needs downloading, so the exits
  // route to Settings (which pops its pending-downloads modal on arrival).
  pendingDownloads: {
    values: D,
    openId: "general",
    exitLabel: "Review Pending Downloads",
    exitTitle: "Review Pending Downloads",
    exitIcon: "i-material-symbols-download-rounded",
  },
} satisfies Record<string, OmitSnippetProps<ComponentProps<typeof QuickSettingsView>>>;
