// Shared builders for repeated settings structures, so the group data files
// stay declarative without copy-pasting near-identical field blocks.

import type { FieldCondition, SettingField, SettingSection } from "../types.ts";
import { SECURE_URL_VALIDATION } from "../types.ts";

export interface ExternalModelSectionOpts {
  /** Prefix for the field ids; produces `${idPrefix}.baseUrl`, `.apiKey`,
   *  `.model`, and (when opted in) `.contextSize` / `.voice`. */
  idPrefix: string;
  label: string;
  visibleWhen: FieldCondition;
  baseUrlDescription: string;
  apiKeyDescription: string;
  model: { description: string; placeholder: string };
  /** Add a "Context Window" number field (the LLM-style providers track usage
   *  against it). */
  contextSize?: boolean;
  /** Add an optional "Relevance Model" field: a second model this provider uses
   *  to rank which Memories and Tools fit each message. Reuses this section's
   *  Base URL + API Key; blank turns the ranking off. */
  embedModel?: { description: string; placeholder: string };
  /** Add a "Voice" field (text-to-speech providers name a voice). */
  voice?: boolean;
  /** Add a "Language" field (speech-to-text providers take an optional default
   *  language hint; blank lets the provider auto-detect). */
  language?: boolean;
}

/** The GPU-acceleration override for the local speech engine. STT and TTS share
 *  one engine, so both groups render this SAME field (one `speech.binaryBackend`
 *  id, one persisted value) with their own `visibleWhen`. */
export function speechBackendField(visibleWhen: FieldCondition): SettingField {
  return {
    id: "speech.binaryBackend",
    name: "Acceleration",
    visibleWhen,
    description:
      "Which speech build to download and run for on-device voice. Auto picks the best for your GPU; choose CUDA to force the NVIDIA build, or CPU to stay off the GPU. Shared by Speech-to-Text and Text-to-Speech.",
    type: "select",
    defaultValue: "auto",
    // CUDA is the only GPU speech build, so other GPUs run speech on CPU.
    options: [
      { value: "auto", label: "Auto" },
      { value: "cpu", label: "CPU" },
      { value: "cuda", label: "CUDA (NVIDIA)" },
    ],
    descriptionTier: "ondemand",
  };
}

/** The "External Provider" section every external-model group shares. Always
 *  persisted to the core. */
export function externalModelSection(opts: ExternalModelSectionOpts): SettingSection {
  const fields: SettingField[] = [
    {
      id: `${opts.idPrefix}.baseUrl`,
      name: "Base URL",
      description: opts.baseUrlDescription,
      type: "string",
      defaultValue: "",
      placeholder: "https://api.example.com/v1",
      regex: SECURE_URL_VALIDATION,
      descriptionTier: "ondemand",
    },
    {
      id: `${opts.idPrefix}.apiKey`,
      name: "API Key",
      description: opts.apiKeyDescription,
      type: "password",
      defaultValue: "",
      placeholder: "sk-...",
      descriptionTier: "ondemand",
    },
    {
      id: `${opts.idPrefix}.model`,
      name: "Model",
      description: opts.model.description,
      type: "string",
      defaultValue: "",
      placeholder: opts.model.placeholder,
      descriptionTier: "ondemand",
    },
  ];
  if (opts.embedModel) {
    fields.push({
      id: `${opts.idPrefix}.embedModel`,
      name: "Relevance Model",
      description: opts.embedModel.description,
      type: "string",
      defaultValue: "",
      optional: true,
      placeholder: opts.embedModel.placeholder,
      descriptionTier: "ondemand",
    });
  }
  if (opts.contextSize) {
    fields.push({
      id: `${opts.idPrefix}.contextSize`,
      name: "Context Window",
      description: "The model's context window, in tokens. Used to track usage.",
      type: "number",
      defaultValue: 128000,
      min: 1, // a context window is a positive token count
      descriptionTier: "ondemand",
    });
  }
  if (opts.voice) {
    fields.push({
      id: `${opts.idPrefix}.voice`,
      name: "Voice",
      description: "The voice name the external model should use, e.g. alloy.",
      type: "string",
      defaultValue: "alloy",
      placeholder: "alloy",
      descriptionTier: "ondemand",
    });
  }
  if (opts.language) {
    fields.push({
      id: `${opts.idPrefix}.language`,
      name: "Language",
      description:
        "An optional default language hint, as an ISO code like en. Leave blank to let the provider detect it.",
      type: "string",
      defaultValue: "",
      placeholder: "en",
      descriptionTier: "ondemand",
    });
  }
  return {
    label: opts.label,
    destination: "core",
    visibleWhen: opts.visibleWhen,
    fields,
  };
}
