import type { ComponentProps } from "svelte";
import type { OmitSnippetProps } from "./types.ts";
import type PromptButtonsView from "../components/chat/userinput/PromptButtonsView.svelte";

const noop = (): void => {};

export const promptButtonsSamples = {
  permission: {
    buttons: [
      {
        icon: "i-material-symbols-close-rounded",
        label: "Deny",
        title: "Reject this permission request",
        onClick: noop,
      },
      {
        icon: "i-material-symbols-check-rounded",
        label: "Allow",
        title: "Allow for this tool call",
        onClick: noop,
      },
    ],
  },
  scheduleConfirm: {
    buttons: [
      {
        icon: "i-material-symbols-close-rounded",
        label: "Decline",
        title: "Decline this scheduled prompt",
        onClick: noop,
      },
      {
        icon: "i-material-symbols-check-rounded",
        label: "Save",
        title: "Save the scheduled prompt",
        disabled: true,
        onClick: noop,
      },
    ],
  },
} satisfies Record<string, OmitSnippetProps<ComponentProps<typeof PromptButtonsView>>>;
