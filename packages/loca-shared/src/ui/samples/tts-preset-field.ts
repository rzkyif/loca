import type { ComponentProps } from "svelte";
import type { OmitSnippetProps } from "./types.ts";
import type TtsPresetFieldView from "../components/settings/TtsPresetFieldView.svelte";

export const ttsPresetFieldSamples = {
  selected: {
    presets: [
      {
        id: "natural",
        title: "Natural",
        description: "A warm, expressive voice for everyday speech.",
        selected: true,
        selectable: true,
        badges: [
          { icon: "i-material-symbols-graphic-eq-rounded", text: "Kokoro" },
          { icon: "i-material-symbols-memory-rounded", text: "327 MB" },
          { icon: "i-material-symbols-record-voice-over-rounded", text: "10 voices" },
        ],
      },
      {
        id: "crisp",
        title: "Crisp",
        description: "A clear, neutral voice optimized for narration.",
        selected: false,
        selectable: true,
        badges: [
          { icon: "i-material-symbols-graphic-eq-rounded", text: "Piper" },
          { icon: "i-material-symbols-memory-rounded", text: "63 MB" },
          { icon: "i-material-symbols-record-voice-over-rounded", text: "1 voice" },
        ],
      },
    ],
    custom: {
      title: "Custom",
      description: "Pick any catalog model and quantization yourself.",
      selected: false,
      model: {
        value: "kokoro",
        options: [
          { value: "kokoro", label: "Kokoro · 327 MB" },
          { value: "piper", label: "Piper · 63-118 MB" },
          { value: "__manual__", label: "Manual Configuration" },
        ],
      },
      quant: {
        value: "kokoro-fp32",
        options: [
          { value: "kokoro-fp32", label: "FP32 · 327 MB" },
          { value: "kokoro-q8", label: "Q8 · 88 MB" },
        ],
      },
    },
  },
  loading: {
    presets: [
      {
        id: "natural",
        title: "Natural",
        description: "A warm, expressive voice for everyday speech.",
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
        id: "natural",
        title: "Natural",
        description: "A warm, expressive voice for everyday speech.",
        selected: false,
        selectable: true,
        badges: [
          { icon: "i-material-symbols-graphic-eq-rounded", text: "Kokoro" },
          { icon: "i-material-symbols-memory-rounded", text: "327 MB" },
          { icon: "i-material-symbols-record-voice-over-rounded", text: "10 voices" },
        ],
      },
    ],
    custom: {
      title: "Custom",
      selected: true,
      model: {
        value: "__manual__",
        options: [
          { value: "kokoro", label: "Kokoro · 327 MB" },
          { value: "__manual__", label: "Manual Configuration" },
        ],
      },
      quant: null,
    },
  },
  error: {
    error: "Could not load the voice catalog",
    presets: [],
  },
  noCustom: {
    presets: [
      {
        id: "natural",
        title: "Natural",
        description: "A warm, expressive voice for everyday speech.",
        selected: true,
        selectable: true,
        badges: [
          { icon: "i-material-symbols-graphic-eq-rounded", text: "Kokoro" },
          { icon: "i-material-symbols-memory-rounded", text: "327 MB" },
          { icon: "i-material-symbols-record-voice-over-rounded", text: "10 voices" },
        ],
      },
    ],
    custom: null,
  },
} satisfies Record<string, OmitSnippetProps<ComponentProps<typeof TtsPresetFieldView>>>;
