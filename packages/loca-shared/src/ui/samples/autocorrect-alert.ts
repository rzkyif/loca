import type { ComponentProps } from "svelte";
import type { OmitSnippetProps } from "./types.ts";
import type AutocorrectAlertView from "../components/chat/userinput/AutocorrectAlertView.svelte";

export const autocorrectAlertSamples = {
  short: {
    original: "whats the wether like",
  },
  multiline: {
    original: "remind me to:\n- buy milk\n- call mom\nthen go for a run",
  },
} satisfies Record<string, OmitSnippetProps<ComponentProps<typeof AutocorrectAlertView>>>;
