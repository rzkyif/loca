// Qwen 3.6 family.
//
// Sources (gathered 2026-06-08): unsloth GGUF repos for variants/sizes; GGUF
// headers for architecture; the Artificial Analysis API for the Intelligence
// Index (reasoning-variant `artificial_analysis_intelligence_index`).
//
// Includes the dense 27B and the 35B-A3B mixture-of-experts (3B active params).
// The fit engine estimates footprint from total weight size; for MoE that is a
// conservative over-estimate of resident memory (inactive experts can offload),
// so a fitting MoE model will run comfortably.

import type { ModelFamily } from "@loca/shared";
import { LOOP_RESISTANT_SAMPLER_ORDER } from "@loca/shared";

const spec = (repo: string, file: string) => `@unsloth/${repo}/main/${file}`;

const aa = (value: number) => [
  { source: "artificial-analysis", metric: "intelligence-index", value },
];

// Qwen's recommended thinking-mode sampling, at the precise/reasoning end
// (temperature 0.6 rather than the higher "general" 1.0) for agentic
// reliability. The 27B and 35B-A3B differ only in presence_penalty; both are left
// at the default here (loops are handled by the on-by-default DRY multiplier), so
// one set covers both. Users can adjust presence penalty in Model Behavior.
const SAMPLING = {
  temperature: 0.6,
  topP: 0.95,
  topK: 20,
  minP: 0,
  repeatPenalty: 1.0,
  // Defer the repetition guards until after temperature; the default order loops
  // on some quants of these reasoning models.
  samplers: [...LOOP_RESISTANT_SAMPLER_ORDER],
};

export const qwen36: ModelFamily = {
  family: "Qwen 3.6",
  publisher: "Qwen",
  models: [
    {
      id: "Qwen/Qwen3.6-27B",
      name: "Qwen3.6 27B",
      scores: aa(45.8),
      paramsB: 27,
      contextMax: 262144,
      arch: {
        blockCount: 64,
        embeddingLength: 5120,
        headCount: 24,
        headCountKv: 4,
        headDim: 256,
      },
      capabilities: { tools: true, vision: true, reasoning: true },
      sampling: SAMPLING,
      variants: [
        {
          label: "standard",
          tags: [],
          provider: "unsloth",
          mmprojSpec: spec("Qwen3.6-27B-GGUF", "mmproj-F16.gguf"),
          mmprojSizeBytes: 927607360,
          quants: [
            {
              quant: "Q8_0",
              modelSpec: spec("Qwen3.6-27B-GGUF", "Qwen3.6-27B-Q8_0.gguf"),
              fileSizeBytes: 28595763424,
            },
            {
              quant: "Q4_K_M",
              modelSpec: spec("Qwen3.6-27B-GGUF", "Qwen3.6-27B-Q4_K_M.gguf"),
              fileSizeBytes: 16817244384,
            },
            {
              quant: "Q3_K_M",
              modelSpec: spec("Qwen3.6-27B-GGUF", "Qwen3.6-27B-Q3_K_M.gguf"),
              fileSizeBytes: 13586217184,
            },
            {
              quant: "UD-Q2_K_XL",
              modelSpec: spec("Qwen3.6-27B-GGUF", "Qwen3.6-27B-UD-Q2_K_XL.gguf"),
              fileSizeBytes: 11849779424,
            },
          ],
        },
      ],
    },
    {
      id: "Qwen/Qwen3.6-35B-A3B",
      name: "Qwen3.6 35B-A3B",
      scores: aa(43.5),
      paramsB: 35,
      contextMax: 262144,
      arch: {
        blockCount: 40,
        embeddingLength: 2048,
        headCount: 16,
        headCountKv: 2,
        headDim: 256,
      },
      capabilities: { tools: true, vision: true, reasoning: true },
      sampling: SAMPLING,
      variants: [
        {
          label: "standard",
          tags: ["moe"],
          provider: "unsloth",
          mmprojSpec: spec("Qwen3.6-35B-A3B-GGUF", "mmproj-F16.gguf"),
          mmprojSizeBytes: 899283680,
          quants: [
            {
              quant: "Q8_0",
              modelSpec: spec("Qwen3.6-35B-A3B-GGUF", "Qwen3.6-35B-A3B-Q8_0.gguf"),
              fileSizeBytes: 36903140320,
            },
            {
              quant: "UD-Q4_K_M",
              modelSpec: spec("Qwen3.6-35B-A3B-GGUF", "Qwen3.6-35B-A3B-UD-Q4_K_M.gguf"),
              fileSizeBytes: 22134528992,
            },
            {
              quant: "UD-Q3_K_M",
              modelSpec: spec("Qwen3.6-35B-A3B-GGUF", "Qwen3.6-35B-A3B-UD-Q3_K_M.gguf"),
              fileSizeBytes: 16600710112,
            },
            {
              quant: "UD-Q2_K_XL",
              modelSpec: spec("Qwen3.6-35B-A3B-GGUF", "Qwen3.6-35B-A3B-UD-Q2_K_XL.gguf"),
              fileSizeBytes: 12290628576,
            },
          ],
        },
      ],
    },
  ],
};
