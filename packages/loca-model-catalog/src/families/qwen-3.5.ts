// Qwen 3.5 family.
//
// Sources (gathered via scripts/catalog/probe.ts on 2026-06-08):
//  - GGUF variants + file sizes: unsloth/Qwen3.5-*-GGUF repos on HuggingFace.
//  - Architecture (block/embedding/head counts, head_dim, context): read from
//    the GGUF headers of those repos.
//  - Intelligence Index: Artificial Analysis API (the reasoning-variant
//    `artificial_analysis_intelligence_index`), fetched 2026-06-08. Re-fetch on
//    each curation pass.
//
// All Qwen 3.5 models are tool-capable, vision-capable (ship an mmproj), and
// support a reasoning trace. 0.8B is included for the browser but sits below the
// Smallest floor (Qwen/Qwen3.5-2B).

import type { CatalogQuant, ModelFamily } from "@loca/shared";
import { LOOP_RESISTANT_SAMPLER_ORDER } from "@loca/shared";

const PROVIDER = "unsloth";

/** Build the standard quant ladder (best quality first) for a repo. */
function quants(repo: string, base: string, sizes: Record<string, number>): CatalogQuant[] {
  const spec = (file: string) => `@${PROVIDER}/${repo}/main/${file}`;
  return [
    {
      quant: "Q8_0",
      modelSpec: spec(`${base}-Q8_0.gguf`),
      fileSizeBytes: sizes.q8,
    },
    {
      quant: "Q4_K_M",
      modelSpec: spec(`${base}-Q4_K_M.gguf`),
      fileSizeBytes: sizes.q4,
    },
    {
      quant: "Q3_K_M",
      modelSpec: spec(`${base}-Q3_K_M.gguf`),
      fileSizeBytes: sizes.q3,
    },
    {
      quant: "UD-Q2_K_XL",
      modelSpec: spec(`${base}-UD-Q2_K_XL.gguf`),
      fileSizeBytes: sizes.q2,
    },
  ];
}

const aa = (value: number) => [
  { source: "artificial-analysis", metric: "intelligence-index", value },
];

// Qwen's recommended thinking-mode sampling, at the precise/reasoning end
// (temperature 0.6 rather than the higher "general" 1.0) for agentic
// reliability. Uniform across the family; users can adjust in Model Behavior.
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

export const qwen35: ModelFamily = {
  family: "Qwen 3.5",
  publisher: "Qwen",
  models: [
    {
      id: "Qwen/Qwen3.5-0.8B",
      name: "Qwen3.5 0.8B",
      scores: aa(10.5),
      paramsB: 0.8,
      contextMax: 262144,
      arch: {
        blockCount: 24,
        embeddingLength: 1024,
        headCount: 8,
        headCountKv: 2,
        headDim: 256,
      },
      capabilities: { tools: true, vision: true, reasoning: true },
      sampling: SAMPLING,
      variants: [
        {
          label: "standard",
          tags: [],
          provider: PROVIDER,
          mmprojSpec: "@unsloth/Qwen3.5-0.8B-GGUF/main/mmproj-F16.gguf",
          mmprojSizeBytes: 204987232,
          quants: quants("Qwen3.5-0.8B-GGUF", "Qwen3.5-0.8B", {
            q8: 811843840,
            q4: 532517120,
            q3: 470167808,
            q2: 417718528,
          }),
        },
      ],
    },
    {
      id: "Qwen/Qwen3.5-2B",
      name: "Qwen3.5 2B",
      scores: aa(16.3),
      paramsB: 2,
      contextMax: 262144,
      arch: {
        blockCount: 24,
        embeddingLength: 2048,
        headCount: 8,
        headCountKv: 2,
        headDim: 256,
      },
      capabilities: { tools: true, vision: true, reasoning: true },
      sampling: SAMPLING,
      variants: [
        {
          label: "standard",
          tags: [],
          provider: PROVIDER,
          mmprojSpec: "@unsloth/Qwen3.5-2B-GGUF/main/mmproj-F16.gguf",
          mmprojSizeBytes: 668227264,
          quants: quants("Qwen3.5-2B-GGUF", "Qwen3.5-2B", {
            q8: 2012012800,
            q4: 1280835840,
            q3: 1107149056,
            q2: 966533376,
          }),
        },
      ],
    },
    {
      id: "Qwen/Qwen3.5-4B",
      name: "Qwen3.5 4B",
      scores: aa(27.1),
      paramsB: 4,
      contextMax: 262144,
      arch: {
        blockCount: 32,
        embeddingLength: 2560,
        headCount: 16,
        headCountKv: 4,
        headDim: 256,
      },
      capabilities: { tools: true, vision: true, reasoning: true },
      sampling: SAMPLING,
      variants: [
        {
          label: "standard",
          tags: [],
          provider: PROVIDER,
          mmprojSpec: "@unsloth/Qwen3.5-4B-GGUF/main/mmproj-F16.gguf",
          mmprojSizeBytes: 672423616,
          quants: quants("Qwen3.5-4B-GGUF", "Qwen3.5-4B", {
            q8: 4482403488,
            q4: 2740937888,
            q3: 2293388448,
            q2: 1940825248,
          }),
        },
      ],
    },
    {
      id: "Qwen/Qwen3.5-9B",
      name: "Qwen3.5 9B",
      scores: aa(32.4),
      paramsB: 9,
      contextMax: 262144,
      arch: {
        blockCount: 32,
        embeddingLength: 4096,
        headCount: 16,
        headCountKv: 4,
        headDim: 256,
      },
      capabilities: { tools: true, vision: true, reasoning: true },
      sampling: SAMPLING,
      variants: [
        {
          label: "standard",
          tags: [],
          provider: PROVIDER,
          mmprojSpec: "@unsloth/Qwen3.5-9B-GGUF/main/mmproj-F16.gguf",
          mmprojSizeBytes: 918166080,
          quants: quants("Qwen3.5-9B-GGUF", "Qwen3.5-9B", {
            q8: 9527502048,
            q4: 5680522464,
            q3: 4673643744,
            q2: 4121781472,
          }),
        },
      ],
    },
  ],
};
