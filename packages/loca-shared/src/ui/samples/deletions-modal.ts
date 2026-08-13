import type { ComponentProps } from "svelte";
import type { OmitSnippetProps } from "./types.ts";
import type DeletionsModalView from "../components/settings/DeletionsModalView.svelte";

export const deletionsModalSamples = {
  default: {
    title: "Delete model?",
    items: [{ label: "Qwen2.5-7B-Instruct-Q4_K_M.gguf", sizeText: "4.7 GB" }],
    totalText: "Frees 4.7 GB",
  },
  withSkipped: {
    title: "Delete unused binaries?",
    items: [
      { label: "llama-server", sizeText: "12 MB" },
      { label: "whisper-server", sizeText: "8 MB" },
    ],
    skipped: [{ label: "tts-server", reason: "running" }],
    totalText: "Frees 20 MB",
  },
  many: {
    title: "Clear download cache?",
    items: [
      { label: "Qwen2.5-7B-Instruct-Q4_K_M.gguf", sizeText: "4.7 GB" },
      { label: "Qwen2.5-3B-Instruct-Q4_K_M.gguf", sizeText: "2.1 GB" },
      { label: "nomic-embed-text-v1.5.gguf", sizeText: "138 MB" },
      { label: "llama-server", sizeText: "12 MB" },
      { label: "whisper-server", sizeText: "8 MB" },
      { label: "ggml-base.en.bin", sizeText: "142 MB" },
    ],
    totalText: "Frees 7.1 GB",
    confirmLabel: "Clear",
  },
  noticeOnly: {
    title: "Reset all settings?",
    notice:
      "This restores every setting to its default. Your sessions and downloaded models are kept.",
    confirmLabel: "Reset",
  },
  empty: {
    title: "Nothing to delete",
    notice: "There is nothing to free right now.",
    confirmDisabled: true,
  },
} satisfies Record<string, OmitSnippetProps<ComponentProps<typeof DeletionsModalView>>>;
