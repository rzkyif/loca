import { assertEquals } from "@std/assert";
import { selectVariant, variantPreference } from "./variant.ts";
import { assetVariants, UPSTREAM_BINARIES } from "./model.ts";

// The offered-variant sets for the two triples that carry GPU builds, derived
// straight from the real UPSTREAM_BINARIES map so these stay honest if it moves.
const winX64 = assetVariants(UPSTREAM_BINARIES["llama-server"]!.assets["x86_64-pc-windows-msvc"]);
const linuxX64 = assetVariants(
  UPSTREAM_BINARIES["llama-server"]!.assets["x86_64-unknown-linux-gnu"],
);
const macArm = assetVariants(UPSTREAM_BINARIES["llama-server"]!.assets["aarch64-apple-darwin"]);

Deno.test("variantPreference always terminates at cpu", () => {
  for (const b of ["metal", "cuda", "rocm", "vulkan", "cpu"] as const) {
    assertEquals(variantPreference(b).at(-1), "cpu");
  }
});

Deno.test("Windows NVIDIA (cuda) picks the cuda build", () => {
  assertEquals(selectVariant(winX64, "cuda"), "cuda");
});

Deno.test("Linux NVIDIA (cuda) falls back to vulkan (no Linux cuda build)", () => {
  // llama.cpp ships no Linux CUDA prebuilt, so cuda hardware must degrade to the
  // universal vulkan build there.
  assertEquals(selectVariant(linuxX64, "cuda"), "vulkan");
});

Deno.test("Windows AMD (rocm backend) picks hip; Linux AMD picks rocm", () => {
  // llama.cpp ships `hip` (not `rocm`) on Windows and `rocm` on Linux.
  assertEquals(selectVariant(winX64, "rocm"), "hip");
  assertEquals(selectVariant(linuxX64, "rocm"), "rocm");
});

Deno.test("generic vulkan hardware picks vulkan where offered, else cpu", () => {
  assertEquals(selectVariant(winX64, "vulkan"), "vulkan");
  assertEquals(selectVariant(linuxX64, "vulkan"), "vulkan");
  // mac ships a single (cpu-keyed) build; vulkan hardware can't happen there,
  // but the fallback must still be safe.
  assertEquals(selectVariant(macArm, "vulkan"), "cpu");
});

Deno.test("cpu backend always picks cpu", () => {
  assertEquals(selectVariant(winX64, "cpu"), "cpu");
  assertEquals(selectVariant(linuxX64, "cpu"), "cpu");
  assertEquals(selectVariant(macArm, "cpu"), "cpu");
});

Deno.test("mac (single build) always resolves to cpu regardless of backend", () => {
  // Metal is baked into the one mac build, expressed as the cpu variant.
  assertEquals(selectVariant(macArm, "metal"), "cpu");
  assertEquals(selectVariant(macArm, "cuda"), "cpu");
});
