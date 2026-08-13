// Text-to-Speech: one hybrid group spanning client and core. The enable toggle
// and the synthesis model (type, model path, bundle files) live on the core
// because the synthesis engine does: core computes the model files into its
// required-downloads snapshot, so a client-side value would leave requirements
// (and the pending-downloads gate) blind to it. Playback and batching settings
// stay on the client. A provider select switches the model section between the
// local synthesis engine and an external OpenAI-compatible endpoint, mirroring
// the Speech-to-Text structure.

import type { RegexValidationRule, SettingGroup } from "../types.ts";
import { externalModelSection, speechBackendField } from "./factories.ts";

// Shared validator for the 0.25x-3x speed fields (synthesis + playback).
const SPEED_RANGE_VALIDATION: RegexValidationRule[] = [
  {
    regex: "^(0\\.(2[5-9]|[3-9]\\d?)|[12](\\.\\d+)?|3(\\.0+)?)$",
    errorMessage: "Must be between 0.25 and 3",
  },
];

export const ttsGroup: SettingGroup = {
  id: "tts",
  destination: ["client-on-client", "core"],
  name: "Text-to-Speech",
  description:
    "Have the agent read its replies aloud. Playback runs on this device; the speech model runs on the core.",
  descriptionTier: "ondemand",
  icon: "i-material-symbols-volume-up-rounded",
  iconInactive: "i-material-symbols-volume-up-outline-rounded",
  sections: [
    {
      label: "General",
      destination: "core",
      fields: [
        {
          id: "tts.enabled",
          name: "Enable Text-to-Speech",
          description: "Read the agent's replies aloud as they arrive.",
          type: "boolean",
          defaultValue: false,
          descriptionTier: "ondemand",
        },
        // Same key + control as the Speech-to-Text group: one speech binary
        // serves both, so its acceleration is a single shared setting.
        speechBackendField({ field: "tts.enabled", eq: true }),
      ],
    },
    {
      label: "Model",
      destination: "core",
      visibleWhen: { field: "tts.enabled", eq: true },
      fields: [
        {
          id: "tts.provider",
          name: "Provider",
          description:
            "Run speech synthesis on the core's device (private) or send text to an external API.",
          type: "select",
          defaultValue: "local",
          options: [
            { value: "local", label: "Local" },
            { value: "external", label: "External" },
          ],
          descriptionTier: "ondemand",
        },
        {
          id: "tts.preset",
          name: "Preset",
          visibleWhen: { field: "tts.provider", eq: "local" },
          description:
            "Curated text-to-speech models. Editing the settings below switches this to Custom.",
          type: "tts_preset",
          defaultValue: "kokoro",
          presetConfig: {
            // The model resolves from the signed catalog at runtime, so list the
            // keys each card manages: editing any flips the preset to Custom.
            // The chosen voice is not managed here (it is independent of which
            // model is selected), though picking a model resets it to that
            // model's default voice.
            managedKeys: ["tts.modelType", "tts.modelPath", "tts.modelFiles"],
            options: [
              {
                id: "kokoro",
                label: "Kokoro",
                title: "Kokoro",
                description:
                  "The highest-quality voices across eight languages, with 53 voices to choose from. Larger download.",
              },
              {
                id: "kitten",
                label: "Light (English)",
                title: "Light (English)",
                description:
                  "A tiny English model with eight voices and a very small download, for low RAM use.",
              },
              {
                id: "piper",
                label: "Piper",
                title: "Piper",
                description:
                  "Fast, natural English voices. Pick other Piper voices from the Custom model browser.",
              },
              {
                id: "matcha",
                label: "Matcha",
                title: "Matcha",
                description:
                  "A fast English voice with a separate vocoder, a lighter alternative to Kokoro.",
              },
            ],
            secondaryOptions: [
              {
                id: "custom",
                label: "Custom",
                title: "Custom",
                description:
                  "Pick any model from the catalog, or configure the model type, path, and files yourself.",
              },
            ],
          },
        },
      ],
    },
    {
      label: "Model Server Configuration",
      destination: "core",
      visibleWhen: {
        allOf: [
          { field: "tts.enabled", eq: true },
          { field: "tts.provider", eq: "local" },
        ],
      },
      fields: [
        {
          id: "tts.modelType",
          name: "Model Type",
          description:
            "Which sherpa-onnx synthesizer the model runs as. Picking a model in the catalog sets this for you; change it only when configuring files by hand.",
          type: "select",
          defaultValue: "kokoro",
          options: [
            { value: "kokoro", label: "Kokoro" },
            { value: "kitten", label: "Kitten" },
            { value: "vits", label: "VITS / Piper" },
            { value: "matcha", label: "Matcha" },
          ],
          descriptionTier: "ondemand",
        },
        {
          id: "tts.modelPath",
          name: "Model File",
          description:
            "The model's main file, e.g. @user/repo/branch/model.int8.onnx. Picking a model in the catalog fills this and the bundle files below.",
          type: "string",
          defaultValue: "@csukuangfj/kokoro-int8-multi-lang-v1_0/main/model.int8.onnx",
          descriptionTier: "ondemand",
        },
        {
          id: "tts.modelFiles",
          name: "Bundle Files",
          description:
            "Advanced. The full set of files the model needs, as a JSON object mapping each sherpa role to an @user/repo/branch/file spec. Set automatically when you pick a model.",
          type: "multiline",
          defaultValue: JSON.stringify(
            {
              model: "@csukuangfj/kokoro-int8-multi-lang-v1_0/main/model.int8.onnx",
              voices: "@csukuangfj/kokoro-int8-multi-lang-v1_0/main/voices.bin",
              tokens: "@csukuangfj/kokoro-int8-multi-lang-v1_0/main/tokens.txt",
            },
            null,
            2,
          ),
          descriptionTier: "ondemand",
        },
      ],
    },
    {
      label: "Voice",
      destination: "client-on-client",
      visibleWhen: { field: "tts.enabled", eq: true },
      fields: [
        {
          id: "tts.spellOutEmojis",
          name: "Read Emojis Aloud",
          description: "Pronounce emojis instead of skipping them.",
          type: "boolean",
          defaultValue: false,
          descriptionTier: "ondemand",
        },
        {
          id: "tts.voice",
          name: "Voice",
          description:
            "The voice used to read replies. The available voices depend on the selected model. Downloads the first time you use it.",
          type: "select",
          optionsSource: "tts_voices",
          defaultValue: "af_bella",
          descriptionTier: "ondemand",
        },
        {
          id: "tts.minChunkWords",
          name: "Words Per Chunk",
          description:
            "How many words to gather before speaking. Higher sounds smoother but starts later.",
          type: "number",
          defaultValue: 8,
          suffix: "words",
          regex: [
            {
              regex: "^[1-9][0-9]*$",
              errorMessage: "Must be a positive integer",
            },
          ],
          descriptionTier: "ondemand",
        },
        {
          id: "tts.synthesisSpeed",
          name: "Synthesis Speed",
          description: "How fast the voice is generated. Affects pacing.",
          type: "float",
          defaultValue: 1,
          suffix: "x",
          regex: SPEED_RANGE_VALIDATION,
          descriptionTier: "ondemand",
        },
        {
          id: "tts.playbackSpeed",
          name: "Playback Speed",
          description: "How fast the audio plays back. Also shifts pitch.",
          type: "float",
          defaultValue: 1,
          suffix: "x",
          regex: SPEED_RANGE_VALIDATION,
          descriptionTier: "ondemand",
        },
        {
          id: "tts.volume",
          name: "Volume",
          description: "Playback volume.",
          type: "number_slider",
          defaultValue: 70,
          min: 0,
          max: 100,
          step: 1,
          suffix: "%",
          descriptionTier: "ondemand",
        },
      ],
    },
    externalModelSection({
      idPrefix: "tts.external",
      label: "External Provider",
      visibleWhen: {
        allOf: [
          { field: "tts.enabled", eq: true },
          { field: "tts.provider", eq: "external" },
        ],
      },
      baseUrlDescription:
        "The speech synthesis API endpoint. Must be HTTPS (HTTP allowed only for localhost).",
      apiKeyDescription: "API key for speech synthesis. Stored securely in your keychain.",
      model: { description: "The speech synthesis model name, e.g. tts-1.", placeholder: "tts-1" },
      voice: true,
    }),
  ],
};
