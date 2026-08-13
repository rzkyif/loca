// Gemma 4 family.
//
// Sources (gathered 2026-06-08): unsloth GGUF repos for variants/sizes; GGUF
// headers for architecture; the Artificial Analysis API for the Intelligence
// Index (reasoning-variant `artificial_analysis_intelligence_index`).
//
// Each model ships a standard build and a Quantization-Aware Training (QAT)
// build. QAT matches the standard model's quality at lower memory, so both carry
// the SAME score and the QAT variant is tagged "qat"; the fit engine's
// tie-breaker (preferTag:qat) picks it when scores tie. E2B/E4B are Matryoshka
// "effective" sizes. All are tool-, vision-, and reasoning-capable. (Gemma 4 12B
// is intentionally absent: it has no Unsloth-paired Artificial Analysis score;
// 31B is the next AA-scored size up.)

import type { CatalogVariant, ModelFamily } from "@loca/shared";
import { LOOP_RESISTANT_SAMPLER_ORDER } from "@loca/shared";

const spec = (repo: string, file: string) => `@unsloth/${repo}/main/${file}`;

const aa = (value: number) => [
  { source: "artificial-analysis", metric: "intelligence-index", value },
];

// Google's recommended Gemma sampling. Uniform across the family; users can
// adjust in Model Behavior.
const SAMPLING = {
  temperature: 1.0,
  topP: 0.95,
  topK: 64,
  minP: 0,
  repeatPenalty: 1.0,
  // Defer the repetition guards until after temperature; the default order loops
  // on some quants of these models.
  samplers: [...LOOP_RESISTANT_SAMPLER_ORDER],
};

const E2B_MMPROJ = {
  mmprojSpec: spec("gemma-4-E2B-it-GGUF", "mmproj-F16.gguf"),
  mmprojSizeBytes: 985654080,
};
const E4B_MMPROJ = {
  mmprojSpec: spec("gemma-4-E4B-it-GGUF", "mmproj-F16.gguf"),
  mmprojSizeBytes: 990372672,
};
const G31B_MMPROJ = {
  mmprojSpec: spec("gemma-4-31B-it-GGUF", "mmproj-F16.gguf"),
  mmprojSizeBytes: 1198957024,
};

const e2bStandard: CatalogVariant = {
  label: "standard",
  tags: [],
  provider: "unsloth",
  ...E2B_MMPROJ,
  quants: [
    {
      quant: "Q8_0",
      modelSpec: spec("gemma-4-E2B-it-GGUF", "gemma-4-E2B-it-Q8_0.gguf"),
      fileSizeBytes: 5048350848,
    },
    {
      quant: "Q4_K_M",
      modelSpec: spec("gemma-4-E2B-it-GGUF", "gemma-4-E2B-it-Q4_K_M.gguf"),
      fileSizeBytes: 3106736256,
    },
    {
      quant: "Q3_K_M",
      modelSpec: spec("gemma-4-E2B-it-GGUF", "gemma-4-E2B-it-Q3_K_M.gguf"),
      fileSizeBytes: 2536784000,
    },
    {
      quant: "UD-Q2_K_XL",
      modelSpec: spec("gemma-4-E2B-it-GGUF", "gemma-4-E2B-it-UD-Q2_K_XL.gguf"),
      fileSizeBytes: 2403612800,
    },
  ],
};

const e2bQat: CatalogVariant = {
  label: "QAT",
  tags: ["qat"],
  provider: "unsloth",
  ...E2B_MMPROJ,
  quants: [
    {
      quant: "UD-Q4_K_XL",
      modelSpec: spec("gemma-4-E2B-it-qat-GGUF", "gemma-4-E2B-it-qat-UD-Q4_K_XL.gguf"),
      fileSizeBytes: 2620368960,
    },
    {
      quant: "UD-Q2_K_XL",
      modelSpec: spec("gemma-4-E2B-it-qat-GGUF", "gemma-4-E2B-it-qat-UD-Q2_K_XL.gguf"),
      fileSizeBytes: 2186184768,
    },
  ],
};

const e4bStandard: CatalogVariant = {
  label: "standard",
  tags: [],
  provider: "unsloth",
  ...E4B_MMPROJ,
  quants: [
    {
      quant: "Q8_0",
      modelSpec: spec("gemma-4-E4B-it-GGUF", "gemma-4-E4B-it-Q8_0.gguf"),
      fileSizeBytes: 8192951456,
    },
    {
      quant: "Q4_K_M",
      modelSpec: spec("gemma-4-E4B-it-GGUF", "gemma-4-E4B-it-Q4_K_M.gguf"),
      fileSizeBytes: 4977169568,
    },
    {
      quant: "Q3_K_M",
      modelSpec: spec("gemma-4-E4B-it-GGUF", "gemma-4-E4B-it-Q3_K_M.gguf"),
      fileSizeBytes: 4058135712,
    },
    {
      quant: "UD-Q2_K_XL",
      modelSpec: spec("gemma-4-E4B-it-GGUF", "gemma-4-E4B-it-UD-Q2_K_XL.gguf"),
      fileSizeBytes: 3757417632,
    },
  ],
};

const e4bQat: CatalogVariant = {
  label: "QAT",
  tags: ["qat"],
  provider: "unsloth",
  ...E4B_MMPROJ,
  quants: [
    {
      quant: "UD-Q4_K_XL",
      modelSpec: spec("gemma-4-E4B-it-qat-GGUF", "gemma-4-E4B-it-qat-UD-Q4_K_XL.gguf"),
      fileSizeBytes: 4215693760,
    },
    {
      quant: "UD-Q2_K_XL",
      modelSpec: spec("gemma-4-E4B-it-qat-GGUF", "gemma-4-E4B-it-qat-UD-Q2_K_XL.gguf"),
      fileSizeBytes: 3219530176,
    },
  ],
};

const g31bStandard: CatalogVariant = {
  label: "standard",
  tags: [],
  provider: "unsloth",
  ...G31B_MMPROJ,
  quants: [
    {
      quant: "Q8_0",
      modelSpec: spec("gemma-4-31B-it-GGUF", "gemma-4-31B-it-Q8_0.gguf"),
      fileSizeBytes: 32635675648,
    },
    {
      quant: "Q4_K_M",
      modelSpec: spec("gemma-4-31B-it-GGUF", "gemma-4-31B-it-Q4_K_M.gguf"),
      fileSizeBytes: 18323731456,
    },
    {
      quant: "Q3_K_M",
      modelSpec: spec("gemma-4-31B-it-GGUF", "gemma-4-31B-it-Q3_K_M.gguf"),
      fileSizeBytes: 14736606208,
    },
    {
      quant: "UD-Q2_K_XL",
      modelSpec: spec("gemma-4-31B-it-GGUF", "gemma-4-31B-it-UD-Q2_K_XL.gguf"),
      fileSizeBytes: 11774989312,
    },
  ],
};

const g31bQat: CatalogVariant = {
  label: "QAT",
  tags: ["qat"],
  provider: "unsloth",
  ...G31B_MMPROJ,
  quants: [
    {
      quant: "UD-Q4_K_XL",
      modelSpec: spec("gemma-4-31B-it-qat-GGUF", "gemma-4-31B-it-qat-UD-Q4_K_XL.gguf"),
      fileSizeBytes: 17287668064,
    },
  ],
};

export const gemma4: ModelFamily = {
  family: "Gemma 4",
  publisher: "Google",
  models: [
    {
      id: "google/gemma-4-E2B-it",
      name: "Gemma 4 E2B",
      scores: aa(15.2),
      paramsB: 2,
      contextMax: 131072,
      arch: {
        blockCount: 35,
        embeddingLength: 1536,
        headCount: 8,
        headCountKv: 1,
        headDim: 512,
      },
      capabilities: { tools: true, vision: true, reasoning: true },
      sampling: SAMPLING,
      variants: [e2bStandard, e2bQat],
    },
    {
      id: "google/gemma-4-E4B-it",
      name: "Gemma 4 E4B",
      scores: aa(18.8),
      paramsB: 4,
      contextMax: 131072,
      arch: {
        blockCount: 42,
        embeddingLength: 2560,
        headCount: 8,
        headCountKv: 2,
        headDim: 512,
      },
      capabilities: { tools: true, vision: true, reasoning: true },
      sampling: SAMPLING,
      variants: [e4bStandard, e4bQat],
    },
    {
      id: "google/gemma-4-31b-it",
      name: "Gemma 4 31B",
      scores: aa(39.2),
      paramsB: 31,
      contextMax: 262144,
      arch: {
        blockCount: 60,
        embeddingLength: 5376,
        headCount: 32,
        headCountKv: 16,
        headDim: 512,
      },
      capabilities: { tools: true, vision: true, reasoning: true },
      sampling: SAMPLING,
      variants: [g31bStandard, g31bQat],
    },
  ],
};
