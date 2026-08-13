import type { ComponentProps } from "svelte";
import type { OmitSnippetProps } from "./types.ts";
import type PasswordPromptModalView from "../components/settings/PasswordPromptModalView.svelte";

export const passwordPromptModalSamples = {
  default: {
    title: "Admin access required",
    message: "Enter your admin password to install the Core service.",
  },
  error: {
    title: "Admin access required",
    password: "wrong",
    error: "Incorrect password, try again",
  },
  submitting: {
    title: "Admin access required",
    password: "hunter2",
    submitting: true,
  },
} satisfies Record<string, OmitSnippetProps<ComponentProps<typeof PasswordPromptModalView>>>;
