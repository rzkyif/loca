import type { ComponentProps } from "svelte";
import type { OmitSnippetProps } from "./types.ts";
import type SessionBarView from "../components/chat/SessionBarView.svelte";

export const sessionBarSamples = {
  default: {
    tokenUsage: { used: 980, max: 8192 },
    showTitle: true,
    defaultTitle: "Installing Loca",
    titleText: "Installing Loca",
  },
  nearFull: {
    tokenUsage: { used: 7800, max: 8192 },
    showTitle: true,
    titleText: "Long debugging session",
  },
  newSession: {
    tokenUsage: null,
    showTitle: true,
    titleText: "",
    defaultTitle: "New session",
    isNewSession: true,
  },
} satisfies Record<string, OmitSnippetProps<ComponentProps<typeof SessionBarView>>>;
