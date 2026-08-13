import type { ComponentProps } from "svelte";
import type { OmitSnippetProps } from "./types.ts";
import type SttPresetFieldView from "../components/settings/SttPresetFieldView.svelte";

export const sttPresetFieldSamples = {
  selected: {
    presets: [
      {
        id: "fast",
        title: "Fast",
        description: "A small model for quick, low-latency transcription.",
        selected: true,
        selectable: true,
        badges: [
          { icon: "i-material-symbols-graphic-eq-rounded", text: "Whisper Base" },
          { icon: "i-material-symbols-memory-rounded", text: "142 MB" },
          { icon: "i-material-symbols-language", text: "Multilingual" },
        ],
      },
      {
        id: "accurate",
        title: "Accurate",
        description: "A larger model that transcribes more reliably.",
        selected: false,
        selectable: true,
        badges: [
          { icon: "i-material-symbols-graphic-eq-rounded", text: "Whisper Large v3" },
          { icon: "i-material-symbols-memory-rounded", text: "3.1 GB" },
          { icon: "i-material-symbols-language", text: "Multilingual" },
        ],
      },
    ],
    custom: {
      title: "Custom",
      description: "Pick any catalog model and quantization yourself.",
      selected: false,
      model: {
        value: "whisper-base",
        options: [
          { value: "whisper-base", label: "Whisper Base · Multilingual · 142 MB" },
          { value: "whisper-large-v3", label: "Whisper Large v3 · Multilingual · 1.5-3.1 GB" },
          { value: "__manual__", label: "Manual Configuration" },
        ],
      },
      quant: {
        value: "whisper-base-q5",
        options: [
          { value: "whisper-base-q5", label: "Q5 · 57 MB" },
          { value: "whisper-base-f16", label: "F16 · 142 MB" },
        ],
      },
    },
  },
  loading: {
    presets: [
      {
        id: "fast",
        title: "Fast",
        description: "A small model for quick, low-latency transcription.",
        selected: false,
        selectable: false,
        badges: null,
        placeholder: "Loading catalog…",
      },
    ],
  },
  customManual: {
    presets: [
      {
        id: "fast",
        title: "Fast",
        description: "A small model for quick, low-latency transcription.",
        selected: false,
        selectable: true,
        badges: [
          { icon: "i-material-symbols-graphic-eq-rounded", text: "Whisper Base" },
          { icon: "i-material-symbols-memory-rounded", text: "142 MB" },
          { icon: "i-material-symbols-language", text: "Multilingual" },
        ],
      },
    ],
    custom: {
      title: "Custom",
      selected: true,
      model: {
        value: "__manual__",
        options: [
          { value: "whisper-base", label: "Whisper Base · Multilingual · 142 MB" },
          { value: "__manual__", label: "Manual Configuration" },
        ],
      },
      quant: null,
    },
  },
  error: {
    error: "Could not load the transcription catalog",
    presets: [],
  },
  noCustom: {
    presets: [
      {
        id: "fast",
        title: "Fast",
        description: "A small model for quick, low-latency transcription.",
        selected: true,
        selectable: true,
        badges: [
          { icon: "i-material-symbols-graphic-eq-rounded", text: "Whisper Base" },
          { icon: "i-material-symbols-memory-rounded", text: "142 MB" },
          { icon: "i-material-symbols-language", text: "Multilingual" },
        ],
      },
    ],
    custom: null,
  },
} satisfies Record<string, OmitSnippetProps<ComponentProps<typeof SttPresetFieldView>>>;
