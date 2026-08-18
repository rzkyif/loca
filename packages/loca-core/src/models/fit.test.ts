import { assert, assertEquals } from "@std/assert";
import {
  fitConfigSchema,
  fitProfileForOs,
  type HardwareInfo,
  resolveFitProfile,
} from "@loca/shared";
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

// ---------------------------------------------------------------------------
// The desktop byte-identical guard.
//
// M4 moves COMPUTE_OVERHEAD_BYTES, CTX_CEILING and CTX_LADDER out of this file
// and into the signed catalog's FitConfig, so a mobile profile can override them
// as data (D-158). The whole refactor is only safe if desktop output does not
// move, and "does not move" is not something the other tests in this file can
// see: they assert properties (a bucket resolves, Full maxes the quant, more
// memory never lowers the score) that survive a retuned constant unchanged.
//
// So this pins the OUTPUT, exactly, at five device shapes. Every value below was
// MEASURED from the engine before any of the refactor existed and pasted in as a
// literal. It is deliberately not derived from the constants: a fixture computed
// from the thing under test scales with it and the mutation survives, which is
// the trap D-050 #5 and D12 phase 1 both recorded.
//
// If this test fails during the refactor, the refactor changed desktop
// behaviour. That is the finding, not the test being stale. Update these numbers
// only with a deliberate, explained change to desktop tuning.

/** [modelId, quant, contextSize, footprintBytes, gpuLayers, threads,
 *  idleUnloadSeconds, supportImages, reasoningBudget] */
type Pinned = [string, string, number, number, number, number, number, boolean, number];

function discrete(totalGb: number, backend: string, vramGb: number): HardwareInfo {
  const total = totalGb * GB;
  return {
    totalRamBytes: total,
    availableRamBytes: total * 0.6,
    cpuCoresPhysical: 16,
    gpu: { backend, name: "GPU", vramBytes: vramGb * GB },
    unifiedMemory: false,
  } as HardwareInfo;
}

function cpuOnly(totalGb: number): HardwareInfo {
  const total = totalGb * GB;
  return {
    totalRamBytes: total,
    availableRamBytes: total * 0.6,
    cpuCoresPhysical: 8,
    gpu: { backend: "cpu", name: "CPU", vramBytes: 0 },
    unifiedMemory: false,
  } as HardwareInfo;
}

const DESKTOP_BASELINE: Array<[string, HardwareInfo, Record<string, Pinned>]> = [
  [
    "unified 16GB",
    unified(16),
    {
      smallest: ["Qwen/Qwen3.5-2B", "Q4_K_M", 8192, 2951716288, 999, 8, 0, true, 512],
      half: ["Qwen/Qwen3.5-9B", "Q3_K_M", 4096, 6728680736, 999, 8, 0, true, 512],
      full: ["Qwen/Qwen3.5-9B", "Q8_0", 16384, 13193151776, 999, 8, 300, true, 1024],
    },
  ],
  [
    "unified 32GB",
    unified(32),
    {
      smallest: ["Qwen/Qwen3.5-2B", "Q4_K_M", 8192, 2951716288, 999, 8, 0, true, 512],
      half: ["Qwen/Qwen3.6-27B", "UD-Q2_K_XL", 8192, 15524870432, 999, 8, 0, true, 512],
      full: ["Qwen/Qwen3.6-27B", "Q4_K_M", 32768, 26934786336, 999, 8, 300, true, 2048],
    },
  ],
  [
    "unified 64GB",
    unified(64),
    {
      smallest: ["Qwen/Qwen3.5-2B", "Q4_K_M", 8192, 2951716288, 999, 8, 0, true, 512],
      half: ["Qwen/Qwen3.6-27B", "Q4_K_M", 16384, 22639819040, 999, 8, 0, true, 1024],
      full: ["Qwen/Qwen3.6-27B", "Q8_0", 32768, 38713305376, 999, 8, 300, true, 2048],
    },
  ],
  // CPU-only: same picks, gpuLayers 0. Pinned separately because gpuLayers and
  // threads are the two apply fields a hardware change moves independently of
  // the fit maths.
  [
    "cpu-only 16GB",
    cpuOnly(16),
    {
      smallest: ["Qwen/Qwen3.5-2B", "Q4_K_M", 8192, 2951716288, 0, 8, 0, true, 512],
      half: ["Qwen/Qwen3.5-9B", "Q3_K_M", 4096, 6728680736, 0, 8, 0, true, 512],
      full: ["Qwen/Qwen3.5-9B", "Q8_0", 16384, 13193151776, 0, 8, 300, true, 1024],
    },
  ],
  // Discrete GPU: the budget is VRAM, not the 64GB of system RAM, so this row
  // is the one that catches a change to memoryBudgetBytes specifically.
  [
    "cuda 24GB VRAM in a 64GB box",
    discrete(64, "cuda", 24),
    {
      smallest: ["Qwen/Qwen3.5-2B", "Q4_K_M", 8192, 2951716288, 999, 8, 0, true, 512],
      half: ["Qwen/Qwen3.5-9B", "Q4_K_M", 16384, 9346172192, 999, 8, 0, true, 1024],
      full: ["Qwen/Qwen3.6-27B", "Q4_K_M", 8192, 20492335392, 999, 8, 300, true, 512],
    },
  ],
];

Deno.test("fit: desktop recommendations are byte-identical to the pinned baseline", () => {
  for (const [label, hw, expected] of DESKTOP_BASELINE) {
    const set = computeRecommendations(catalog, hw, {});
    for (const bucket of ["smallest", "half", "full"] as const) {
      const got = set.buckets[bucket];
      assert(got, `${label}/${bucket}: expected a recommendation, got null`);
      const actual: Pinned = [
        got.modelId,
        got.quant,
        got.apply.contextSize,
        got.footprintBytes,
        got.apply.gpuLayers,
        got.apply.threads,
        got.apply.idleUnloadSeconds,
        got.apply.supportImages,
        got.apply.reasoningBudget,
      ];
      assertEquals(actual, expected[bucket], `${label}/${bucket} moved`);
    }
  }
});

Deno.test("fit: the browser view is byte-identical to the pinned baseline", () => {
  // buildCatalogViews shares bestCtx / recommendedConfigForModel / footprintBytes
  // with computeRecommendations but has its own budget line, so a refactor can
  // move it while leaving the three buckets alone.
  const view = buildCatalogViews(catalog, unified(16)).find((v) => v.id === "Qwen/Qwen3.5-4B");
  assert(view, "Qwen3.5-4B is in the catalog");
  assertEquals(
    [view.quant, view.footprintBytes, view.fits, view.quants.length],
    ["Q4_K_M", 8308328800, true, 4],
    "the 4B browser row moved",
  );
});

// ---------------------------------------------------------------------------
// The mobile profile.
//
// Every device row is a fixture chosen to produce a named outcome, not a hope.
// The project plan asked for "6, 8 and 12 GB including the nothing fits
// outcome"; measured, 6 GiB resolves all three buckets once vision is off, so
// pinning the empty case on 6 GiB would have written down a wrong expectation.
// 2 GiB carries the whole-device empty case and 4 GiB carries the empty-Balanced
// case, which is the one a real phone can hit.

function phone(totalGb: number): HardwareInfo {
  const total = totalGb * GB;
  // No `os` field: `HardwareInfo` has none and deliberately should not grow one
  // (catalog.ts says so), because whether to apply a phone's memory policy is a
  // fact about the Core doing the fitting rather than something a client sends.
  // An earlier version of this fixture set `os: "android"` through an
  // `as unknown as` cast and explained why in a comment that was already stale.
  // Nothing read it. That double cast is how a test asserts against a world that
  // cannot exist, so it is gone and so is the cast.
  return {
    totalRamBytes: total,
    availableRamBytes: total * 0.5,
    cpuCoresPhysical: 8,
    gpu: { backend: "cpu", name: "CPU", vramBytes: 0 },
    unifiedMemory: true,
  };
}

/** [modelId, quant, contextSize, footprintBytes, supportImages] */
type MobilePin = [string, string, number, number, boolean];

const MOBILE_BASELINE: Array<[string, HardwareInfo, Record<string, MobilePin | null>]> = [
  // Nothing fits at all. The whole-device empty case: even the smallest quant
  // of the smallest model at the lowest context overflows a 0.35 budget.
  ["2 GiB phone", phone(2), { smallest: null, half: null, full: null }],
  // Balanced is empty while the other two resolve. Reachable on a real budget
  // phone, and the case a naive implementation would report as "no models".
  [
    "4 GiB phone",
    phone(4),
    {
      smallest: ["Qwen/Qwen3.5-0.8B", "Q4_K_M", 2048, 1033180416, false],
      half: null,
      full: ["Qwen/Qwen3.5-2B", "UD-Q2_K_XL", 2048, 1467196672, false],
    },
  ],
  // The mainstream phone, and the row the whole profile is aimed at. Before the
  // mmproj came out of the footprint this was `half: null`.
  [
    "6 GiB phone",
    phone(6),
    {
      smallest: ["Qwen/Qwen3.5-0.8B", "Q4_K_M", 2048, 1033180416, false],
      half: ["Qwen/Qwen3.5-0.8B", "Q4_K_M", 4096, 1133843712, false],
      full: ["Qwen/Qwen3.5-2B", "Q4_K_M", 4096, 1882162432, false],
    },
  ],
  [
    "8 GiB phone",
    phone(8),
    {
      smallest: ["Qwen/Qwen3.5-0.8B", "Q4_K_M", 2048, 1033180416, false],
      half: ["Qwen/Qwen3.5-2B", "Q3_K_M", 4096, 1708475648, false],
      full: ["Qwen/Qwen3.5-4B", "Q3_K_M", 2048, 2961823904, false],
    },
  ],
  [
    "12 GiB phone",
    phone(12),
    {
      smallest: ["Qwen/Qwen3.5-0.8B", "Q4_K_M", 2048, 1033180416, false],
      half: ["Qwen/Qwen3.5-2B", "Q4_K_M", 4096, 1882162432, false],
      full: ["Qwen/Qwen3.5-4B", "Q4_K_M", 4096, 3677808800, false],
    },
  ],
];

Deno.test("fit: the mobile profile at 2, 4, 6, 8 and 12 GiB, empty buckets included", () => {
  for (const [label, hw, expected] of MOBILE_BASELINE) {
    const set = computeRecommendations(catalog, hw, {}, "mobile");
    for (const bucket of ["smallest", "half", "full"] as const) {
      const want = expected[bucket];
      const got = set.buckets[bucket];
      if (want === null) {
        assertEquals(got, null, `${label}/${bucket}: expected nothing to fit`);
        continue;
      }
      assert(got, `${label}/${bucket}: expected a recommendation, got null`);
      assertEquals(
        [
          got.modelId,
          got.quant,
          got.apply.contextSize,
          got.footprintBytes,
          got.apply.supportImages,
        ],
        want,
        `${label}/${bucket} moved`,
      );
    }
  }
});

Deno.test("fit: fitProfileForOs maps the two phone platforms and nothing else", () => {
  // Found by mutation, not by design: making `fitProfileForOs` return "desktop"
  // unconditionally left every other test in this file green. It has exactly one
  // production caller, and on a desktop test machine that caller passes a
  // desktop OS, so the mobile branch was never observed anywhere. D-145's
  // dominant-caller shape, in the function written to avoid it.
  assertEquals(fitProfileForOs("android"), "mobile");
  assertEquals(fitProfileForOs("ios"), "mobile");
  for (const os of ["darwin", "linux", "windows", "freebsd", ""]) {
    assertEquals(fitProfileForOs(os), "desktop", `${os} is not a phone`);
  }
});

Deno.test("fit: an out-of-order ctxLadder is refused by the schema", () => {
  // F-A. `bestCtx` takes the FIRST rung that fits and two other sites read the
  // LAST element as "the lowest rung", so order is load-bearing and was
  // unenforced. The Rust resolver normalised its copy and this one did not, so
  // one signed catalog answered ctx 2048 here and 4096 there, with the Rust
  // suite entirely green. The bound lives in the schema both cores generate
  // from, because a bound only one side enforces is the same divergence.
  const base = {
    budgetFraction: { half: 0.5, full: 0.9 },
    safetyMarginBytes: 0,
    primaryScore: { source: "s", metric: "m" },
    smallestQualityFloorRef: "x",
  };
  expect_ok(fitConfigSchema.safeParse({ ...base, ctxLadder: [8192, 4096, 2048] }).success, true);
  expect_ok(fitConfigSchema.safeParse({ ...base, ctxLadder: [2048, 4096, 8192] }).success, false);
  expect_ok(fitConfigSchema.safeParse({ ...base, ctxLadder: [4096, 4096] }).success, false);
  expect_ok(
    fitConfigSchema.safeParse({
      ...base,
      profiles: { mobile: { ctxLadder: [2048, 8192] } },
    }).success,
    false,
  );
});

Deno.test("fit: both resolvers normalise a ladder that never met the schema", () => {
  // The schema refuses the bad order, so this covers the hand-built config that
  // bypasses it. Rust does exactly the same thing in `resolve_fit_profile`; two
  // implementations normalising differently IS the finding.
  const parsed = fitConfigSchema.parse({
    budgetFraction: { half: 0.5, full: 0.9 },
    safetyMarginBytes: 0,
    primaryScore: { source: "s", metric: "m" },
    smallestQualityFloorRef: "x",
  });
  const hand = { ...parsed, ctxLadder: [2048, 8192, 4096, 4096] };
  assertEquals(resolveFitProfile(hand, "desktop").ctxLadder, [8192, 4096, 2048]);
  assertEquals(
    resolveFitProfile({ ...hand, ctxLadder: [] }, "desktop").ctxLadder,
    [32768, 16384, 8192, 4096],
  );
});

function expect_ok(actual: boolean, want: boolean) {
  assertEquals(actual, want);
}

Deno.test("fit: the profile argument is observed, not decoration", () => {
  // D-145: a function with a dominant caller has an unobserved parameter, and
  // every other test in this file passes the default. Mutating
  // `resolveFitProfile` to ignore its argument has to go red somewhere, and
  // this is the somewhere.
  const hw = phone(8);
  const desktop = computeRecommendations(catalog, hw, {}, "desktop");
  const mobile = computeRecommendations(catalog, hw, {}, "mobile");
  assert(
    desktop.buckets.full?.modelId !== mobile.buckets.full?.modelId ||
      desktop.buckets.full?.apply.contextSize !== mobile.buckets.full?.apply.contextSize,
    "the same hardware produced the same answer under both profiles",
  );
});

Deno.test("fit: a context below the desktop ladder floor is reachable on mobile", () => {
  // The direct regression test for D-158. The desktop ladder bottoms out at
  // 4096, so before the ladder became data no phone-sized context existed at
  // any budget and every bucket came back empty. `smallest` at 2048 is only
  // expressible because the mobile ladder carries that rung.
  const smallest = computeRecommendations(catalog, phone(6), {}, "mobile").buckets.smallest;
  assert(smallest, "the smallest bucket resolves on a 6 GiB phone");
  assertEquals(smallest.apply.contextSize, 2048);
});

Deno.test("fit: mobile defaults vision off without taking it away", () => {
  // Two halves, asserted separately, because one passes against a half-done
  // implementation: clearing `supportImages` while still budgeting for the
  // mmproj shrinks what fits for a file the profile does not preselect, and
  // budgeting for it while preselecting vision overcommits memory.
  const hw = phone(6);
  const half = computeRecommendations(catalog, hw, {}, "mobile").buckets.half;
  assert(half, "the Balanced bucket resolves on a 6 GiB phone");

  // Half 1: the flag.
  assertEquals(half.apply.supportImages, false, "mobile must not preselect vision");
  assertEquals(half.vision, false, "the recommendation must not report vision");

  // Half 2: the budget. Asserted as an identity against catalog data rather
  // than by subtracting one profile from the other, because the two profiles
  // legitimately pick different contexts and a subtraction across them is not
  // a fact about vision at all. Qwen3.5-0.8B Q4_K_M at 4096:
  //   weights 532_517_120 + kv 4096*49_152 + overhead 400_000_000
  // The mmproj is 204_987_232, so a footprint that still carried it could not
  // equal this sum.
  const weights = 532_517_120;
  const kvPerToken = 2 * 24 * 2 * 256 * 2; // blocks * headCountKv * headDim * 2B
  assertEquals(half.modelId, "Qwen/Qwen3.5-0.8B");
  assertEquals(half.apply.contextSize, 4096);
  assertEquals(
    half.footprintBytes,
    weights + 4096 * kvPerToken + 400_000_000,
    "the mobile footprint still carries the vision add-on",
  );

  // And the browser row, which has its own budget line.
  const row = buildCatalogViews(catalog, hw, "mobile").find((v) => v.id === "Qwen/Qwen3.5-2B");
  assert(row);
  assertEquals(row.vision, false, "the browser row must not advertise vision");
});

Deno.test("fit: mobile still writes the projector path, so the setting can be turned on", () => {
  // The other half of the ruling above, and the one with teeth. A phone that
  // starts with images off must still SELECT a projector, because `applyToPatch`
  // writes `llm.mmprojPath` and that path is the only thing standing between
  // "Image Attachments is off by default" and "Image Attachments is a switch
  // wired to nothing". Gating the path on the profile passes every other test in
  // this file, because the only projector-path assertion is a desktop one.
  const row = buildCatalogViews(catalog, phone(12), "mobile").find(
    (v) => v.id === "Qwen/Qwen3.5-4B",
  );
  assert(row);
  assertEquals(row.vision, false, "the mobile row must not preselect vision");
  assertEquals(row.apply.supportImages, false, "nor turn the setting on");
  assertEquals(
    row.apply.mmprojPath,
    "@unsloth/Qwen3.5-4B-GGUF/main/mmproj-F16.gguf",
    "but it must still point at the projector it did not preselect",
  );
});
