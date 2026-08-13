import type { ComponentProps } from "svelte";
import type { OmitSnippetProps } from "./types.ts";
import type UpdateButtonView from "../components/settings/UpdateButtonView.svelte";

// The Settings sidebar update button. The client owns the live update state
// machine (client + core + sidecar checks, then the install/restart handoff),
// mapping all of it down to a phase. These scripted stand-ins cover one
// rendition per visible phase, plus a collapsed variant. The View resolves each
// phase's label itself, so every sample reads exactly as the app does (the
// "available" label swaps to "Install Updates" on hover, here too).
export const updateButtonSamples = {
  idle: {
    phase: "idle",
    collapsed: false,
  },
  checking: {
    phase: "checking",
    collapsed: false,
  },
  available: {
    phase: "available",
    blink: true,
    collapsed: false,
  },
  updating: {
    phase: "updating",
    collapsed: false,
  },
  ready: {
    phase: "clientRestartPending",
    collapsed: false,
  },
  collapsed: {
    phase: "available",
    blink: true,
    collapsed: true,
  },
} satisfies Record<string, OmitSnippetProps<ComponentProps<typeof UpdateButtonView>>>;
