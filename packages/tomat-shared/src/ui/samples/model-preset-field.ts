import type { ComponentProps } from "svelte";
import type { OmitSnippetProps } from "./types.ts";
import type ModelPresetFieldView from "../components/settings/ModelPresetFieldView.svelte";

// The device-tier buckets and the catalog models mirror what Tomat actually
// ships (the recommendation tiers Smallest / Balanced / Smartest, and the
// real Gemma 4 + Qwen3.5 families), so the gallery and the website demos show
// the genuine picker rather than placeholder names.
const MODEL_OPTIONS = [
  { value: "google/gemma-4-E2B-it", label: "Gemma 4 E2B · 1.8-5.1 GB" },
  { value: "google/gemma-4-E4B-it", label: "Gemma 4 E4B · 2.9-8.0 GB" },
  { value: "Qwen/Qwen3.5-4B", label: "Qwen3.5 4B · 2.3-4.5 GB" },
  { value: "Qwen/Qwen3.5-9B", label: "Qwen3.5 9B · 5.0-9.5 GB" },
  { value: "__manual__", label: "Manual Configuration" },
];

const QUANT_OPTIONS = [
  { value: "Q8_0", label: "Q8_0 · 4.5 GB · highest quality" },
  { value: "Q4_K_M", label: "Q4_K_M · 2.7 GB · recommended" },
  { value: "Q3_K_M", label: "Q3_K_M · 2.3 GB" },
];

export const modelPresetFieldSamples = {
  recommended: {
    checkLabel: "Check for Newer Models",
    checkIcon: "i-material-symbols-refresh-rounded",
    buckets: [
      {
        id: "smallest",
        title: "Smallest",
        description: "The lightest model that still runs well, for low-RAM devices.",
        selected: false,
        selectable: true,
        badges: [
          { icon: "i-material-symbols-psychology-alt-rounded", text: "Gemma 4 E2B" },
          { icon: "i-material-symbols-memory-rounded", text: "2.4 GB" },
          { icon: "i-material-symbols-bolt-rounded", text: "Q4_K_M" },
        ],
      },
      {
        id: "balanced",
        title: "Balanced",
        description: "A capable model tuned to fit comfortably on this device.",
        selected: true,
        selectable: true,
        badges: [
          { icon: "i-material-symbols-psychology-alt-rounded", text: "Qwen3.5 4B" },
          { icon: "i-material-symbols-memory-rounded", text: "2.7 GB" },
          { icon: "i-material-symbols-bolt-rounded", text: "Q4_K_M" },
        ],
      },
      {
        id: "smartest",
        title: "Smartest",
        description: "The strongest model that still fits this device.",
        selected: false,
        selectable: true,
        badges: [
          { icon: "i-material-symbols-psychology-alt-rounded", text: "Qwen3.5 9B" },
          { icon: "i-material-symbols-memory-rounded", text: "5.8 GB" },
          { icon: "i-material-symbols-bolt-rounded", text: "Q4_K_M" },
        ],
      },
    ],
    custom: {
      title: "Custom",
      description: "Pick any catalog model and quantization yourself.",
      selected: false,
      model: {
        value: "Qwen/Qwen3.5-4B",
        options: MODEL_OPTIONS,
      },
      quant: {
        value: "Q4_K_M",
        options: QUANT_OPTIONS,
      },
    },
  },
  checking: {
    checkLabel: "Checking…",
    checkIcon: "i-line-md:loading-loop",
    checkDisabled: true,
    buckets: [
      {
        id: "balanced",
        title: "Balanced",
        description: "A capable model tuned to fit comfortably on this device.",
        selected: false,
        selectable: false,
        badges: null,
        placeholder: "Computing for your device…",
      },
    ],
  },
  betterAvailable: {
    checkLabel: "Newer Models Found",
    checkIcon: "i-material-symbols-auto-awesome-rounded",
    buckets: [
      {
        id: "balanced",
        title: "Balanced",
        description: "A capable model tuned to fit comfortably on this device.",
        selected: true,
        selectable: true,
        badges: [
          { icon: "i-material-symbols-psychology-alt-rounded", text: "Qwen3.5 4B" },
          { icon: "i-material-symbols-memory-rounded", text: "2.7 GB" },
          { icon: "i-material-symbols-bolt-rounded", text: "Q4_K_M" },
        ],
        better: {
          message: "A better model is available for Balanced: Qwen3.6 27B",
          applying: false,
          onApply: () => {},
          onDismiss: () => {},
        },
      },
    ],
    custom: {
      title: "Custom",
      selected: false,
      model: {
        value: "__manual__",
        options: [
          { value: "Qwen/Qwen3.5-4B", label: "Qwen3.5 4B · 2.3-4.5 GB" },
          { value: "Qwen/Qwen3.6-27B", label: "Qwen3.6 27B · 16.4 GB · won't fit", disabled: true },
          { value: "__manual__", label: "Manual Configuration" },
        ],
      },
      quant: null,
    },
  },
  error: {
    error: "Could not reach the Core",
    checkLabel: "Check for Newer Models",
    checkIcon: "i-material-symbols-refresh-rounded",
    buckets: [],
  },
  noCustom: {
    checkLabel: "Check for Newer Models",
    checkIcon: "i-material-symbols-refresh-rounded",
    buckets: [
      {
        id: "balanced",
        title: "Balanced",
        description: "A capable model tuned to fit comfortably on this device.",
        selected: true,
        selectable: true,
        badges: [
          { icon: "i-material-symbols-psychology-alt-rounded", text: "Qwen3.5 4B" },
          { icon: "i-material-symbols-memory-rounded", text: "2.7 GB" },
          { icon: "i-material-symbols-bolt-rounded", text: "Q4_K_M" },
        ],
      },
    ],
    custom: null,
  },
} satisfies Record<string, OmitSnippetProps<ComponentProps<typeof ModelPresetFieldView>>>;
