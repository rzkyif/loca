// GPU-variant selection: map a detected GPU backend to the best binary variant
// the manifest offers for the host triple. Pure and dependency-free so core, the
// release script, and tests can all import it.

import type { BinaryVariant } from "./model.ts";
import type { GpuBackend } from "./recommend.ts";

// For each detected backend, the ordered list of variants to try, best first.
// A variant not offered by the manifest is skipped, so one list serves every
// binary kind (llama.cpp and sherpa-onnx offer different subsets). Every list
// terminates at `cpu`, the guaranteed fallback.
//
// Notable cross-mappings: Linux NVIDIA reports `cuda` but llama.cpp ships no
// Linux CUDA build, so it falls to `vulkan`; Windows AMD reports `rocm` but
// llama.cpp ships `hip` (not `rocm`) on Windows and sherpa-onnx ships `directml`.
const PREFERENCE: Record<GpuBackend, BinaryVariant[]> = {
  metal: ["metal", "coreml", "cpu"],
  cuda: ["cuda", "directml", "vulkan", "cpu"],
  rocm: ["rocm", "hip", "directml", "vulkan", "cpu"],
  vulkan: ["vulkan", "directml", "cpu"],
  cpu: ["cpu"],
};

/** Ordered variant preference for a detected GPU backend, best first. */
export function variantPreference(backend: GpuBackend): BinaryVariant[] {
  return PREFERENCE[backend] ?? ["cpu"];
}

/** The best variant the manifest actually offers for the detected backend.
 *  `offered` is the set of variant keys available for the host triple (from
 *  `assetVariants` / `platformVariants`). Falls back to `cpu`, which every
 *  entry guarantees. */
export function selectVariant(
  offered: Partial<Record<BinaryVariant, unknown>>,
  backend: GpuBackend,
): BinaryVariant {
  for (const v of variantPreference(backend)) {
    if (offered[v] !== undefined) return v;
  }
  return "cpu";
}
