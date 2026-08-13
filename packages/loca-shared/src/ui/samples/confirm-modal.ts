import type { ComponentProps } from "svelte";
import type { OmitSnippetProps } from "./types.ts";
import type ConfirmModalView from "../components/settings/ConfirmModalView.svelte";

export const confirmModalSamples = {
  default: {
    title: "Restart Core?",
    message: "The Core will restart to apply the update.",
  },
  destructive: {
    title: "Delete session?",
    message: "This permanently removes the conversation and its messages.",
    destructive: true,
    confirmLabel: "Delete",
  },
  withDownloads: {
    title: "Download models?",
    message: "These files are required before the model can run.",
    downloads: [
      {
        source: "binary:llama-server",
        title: "llama-server",
        subtitle: "v0.3.2",
        sizeText: "12 MB",
      },
      {
        source: "Qwen/Qwen2.5-7B-Instruct-GGUF",
        title: "Qwen2.5-7B-Instruct-Q4_K_M.gguf",
        subtitle: "Qwen/Qwen2.5-7B-Instruct-GGUF",
        sizeText: "4.7 GB",
      },
    ],
    totalText: "Total: 4.7 GB",
    confirmLabel: "Download",
  },
  dontShowAgain: {
    title: "Run this tool?",
    message: "The assistant wants to run a shell command on this device.",
    dontShowAgainLabel: "Always allow for this session",
    confirmLabel: "Run",
  },
  alert: {
    title: "Update available",
    message: "A new version of the Client is ready to install.",
    hideCancel: true,
    confirmLabel: "OK",
  },
} satisfies Record<string, OmitSnippetProps<ComponentProps<typeof ConfirmModalView>>>;
