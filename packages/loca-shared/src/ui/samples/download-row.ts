import type { ComponentProps } from "svelte";
import type { OmitSnippetProps } from "./types.ts";
import type DownloadRowView from "../components/settings/DownloadRowView.svelte";

export const downloadRowSamples = {
  downloading: {
    status: "Downloading",
    icon: "i-material-symbols-psychology-rounded",
    filename: "Qwen2.5-7B-Instruct-Q4_K_M.gguf",
    title: "Qwen2.5-7B-Instruct-Q4_K_M.gguf (models/Qwen2.5-7B-Instruct-Q4_K_M.gguf)",
    sizeText: "2.1 GB / 4.7 GB",
    progress: 45,
  },
  paused: {
    status: "Cancelled",
    icon: "i-material-symbols-psychology-rounded",
    filename: "nomic-embed-text-v1.5.Q4_K_M.gguf",
    title: "nomic-embed-text-v1.5.Q4_K_M.gguf (models/nomic-embed-text-v1.5.Q4_K_M.gguf)",
    sizeText: "84 MB",
    progress: 0,
  },
  queued: {
    status: "Pending",
    icon: "i-material-symbols-download-rounded",
    filename: "llama-server",
    title: "llama-server (binaries/llama-server)",
    sizeText: "12 MB",
    progress: 0,
  },
  completed: {
    status: "Completed",
    icon: "i-material-symbols-psychology-rounded",
    filename: "Qwen2.5-0.5B-Instruct-Q8_0.gguf",
    title: "Qwen2.5-0.5B-Instruct-Q8_0.gguf (models/Qwen2.5-0.5B-Instruct-Q8_0.gguf)",
    sizeText: "531 MB",
    progress: 100,
    showReveal: true,
  },
  error: {
    status: "Error",
    icon: "i-material-symbols-download-rounded",
    filename: "kokoro-v1.0.onnx",
    title: "Network error: connection reset",
    sizeText: "38 MB",
    progress: 0,
  },
} satisfies Record<string, OmitSnippetProps<ComponentProps<typeof DownloadRowView>>>;
