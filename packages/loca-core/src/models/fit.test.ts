import { assert, assertEquals } from "@std/assert";
import type { HardwareInfo } from "@loca/shared";
import { buildCatalogPayload } from "../../../loca-model-catalog/src/index.ts";
import { buildCatalogViews, computeRecommendations } from "./fit.ts";

const catalog = buildCatalogPayload("2026-06-08T00:00:00Z");
const GB = 1024 * 1024 * 1024;

function unified(totalGb: number): HardwareInfo {
  const total = totalGb * GB;
  return {
    totalRamBytes: total,
    availableRamBytes: total * 0.6,
    cpuCoresPhysical: 10,
    gpu: { backend: "metal", name: "Apple", vramBytes: total },
    unifiedMemory: true,
  };
}

Deno.test("fit: every bucket resolves on a typical 16GB device", () => {
  const rec = computeRecommendations(catalog, unified(16), {});
  assert(rec.buckets.smallest, "smallest should resolve");
  assert(rec.buckets.half, "half should resolve");
  assert(rec.buckets.full, "full should resolve");
});

Deno.test("fit: smallest clears the quality floor (Qwen 3.5 2B)", () => {
  const rec = computeRecommendations(catalog, unified(16), {});
  // Floor ref is Qwen/Qwen3.5-2B (AA score 16.3); the 0.8B (below floor) must
  // never be Smallest.
  assert(rec.buckets.smallest!.quality >= 16.3);
  assert(rec.buckets.smallest!.modelId !== "Qwen/Qwen3.5-0.8B");
});

Deno.test("fit: more memory never lowers the Full model's score", () => {
  const scores = [8, 16, 32, 64].map(
    (gb) => computeRecommendations(catalog, unified(gb), {}).buckets.full?.quality ?? 0,
  );
  for (let i = 1; i < scores.length; i++) {
    assert(scores[i] >= scores[i - 1], `full score regressed at index ${i}: ${scores}`);
  }
});

Deno.test("fit: Full stays under the safety ceiling", () => {
  const hw = unified(32);
  const rec = computeRecommendations(catalog, hw, {});
  const limit = hw.totalRamBytes * catalog.fit.budgetFraction.full - catalog.fit.safetyMarginBytes;
  assert(rec.buckets.full!.footprintBytes <= limit);
});

Deno.test("fit: QAT variant wins on quality-per-byte (Gemma 31B at 32GB)", () => {
  // At 32GB the standard Q8 does not fit but the QAT UD-Q4 build does, and its
  // QAT quality bonus outranks the standard low-bpw quants that also fit, so the
  // browser row resolves to the QAT variant and reports it fits.
  const view = buildCatalogViews(catalog, unified(32)).find(
    (m) => m.id === "google/gemma-4-31b-it",
  );
  assert(view, "gemma 31b should be in the catalog");
  assertEquals(view!.variantLabel, "QAT");
  assert(view!.fits);
});

Deno.test("fit: recommended quant is the sweet spot (not Q8), and is applied by default", () => {
  const view = buildCatalogViews(catalog, unified(16)).find((m) => m.id === "Qwen/Qwen3.5-4B");
  assert(view, "Qwen 3.5 4B should be in the catalog");
  const q8 = view!.quants.find((q) => q.quant === "Q8_0");
  const rec = view!.quants.find((q) => q.recommended);
  assert(rec, "a quant should be flagged recommended");
  // Q8_0 fits on 16GB, so the cap is what keeps it from being recommended.
  assert(q8?.fits, "Q8_0 should fit on 16GB (otherwise this proves nothing)");
  assert(
    rec!.quant !== "Q8_0",
    "Q8 is diminishing returns; never the recommendation when smaller fits",
  );
  assertEquals(rec!.quant, "Q4_K_M");
  // Picking the model (no explicit quant) applies the recommended one.
  assertEquals(view!.apply.modelPath, rec!.modelSpec);
});

Deno.test("fit: Full maxes the quant; Smallest/Balanced cap at the sweet spot", () => {
  const rec = computeRecommendations(catalog, unified(16), {});
  // Full takes the biggest quant that fits (Q8_0 fits the 9B at 16GB).
  assertEquals(rec.buckets.full!.quant, "Q8_0");
  // Smallest uses the recommended sweet spot, never Q8.
  assertEquals(rec.buckets.smallest!.quant, "Q4_K_M");
  // Balanced caps at the sweet spot too: never above Q4_K_M, even though the
  // chosen model's Q8 would technically fit the half budget here it steps down
  // to a lower quant (Q3_K_M) because the sweet-spot Q4 didn't fit.
  assert(rec.buckets.half!.quant !== "Q8_0", "Balanced never uses Q8");
  assertEquals(rec.buckets.half!.quant, "Q3_K_M");
});

Deno.test("fit: Full enables idle-unload, others do not", () => {
  const rec = computeRecommendations(catalog, unified(16), {});
  assert(rec.buckets.full!.apply.idleUnloadSeconds > 0);
  assertEquals(rec.buckets.half!.apply.idleUnloadSeconds, 0);
  assertEquals(rec.buckets.smallest!.apply.idleUnloadSeconds, 0);
});

Deno.test("fit: CPU-only backend offloads no layers", () => {
  const hw: HardwareInfo = {
    totalRamBytes: 16 * GB,
    availableRamBytes: 10 * GB,
    cpuCoresPhysical: 8,
    gpu: { backend: "cpu", name: "CPU", vramBytes: 0 },
    unifiedMemory: false,
  };
  const rec = computeRecommendations(catalog, hw, {});
  assertEquals(rec.buckets.smallest!.apply.gpuLayers, 0);
});

Deno.test("fit: applies each model's catalog sampling", () => {
  const views = buildCatalogViews(catalog, unified(32));
  const qwen = views.find((m) => m.id === "Qwen/Qwen3.5-4B");
  const gemma = views.find((m) => m.id === "google/gemma-4-31b-it");
  assert(qwen && gemma, "both models should be in the catalog");
  // Qwen ships temperature 0.6 / top_k 20; Gemma ships 1.0 / 64.
  assertEquals(qwen!.apply.temperature, 0.6);
  assertEquals(qwen!.apply.topK, 20);
  assertEquals(qwen!.apply.repeatPenalty, 1.0);
  assertEquals(gemma!.apply.temperature, 1.0);
  assertEquals(gemma!.apply.topK, 64);
});
