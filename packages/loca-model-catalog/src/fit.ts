// Fit-engine tuning, shipped inside the catalog so policy changes need no core
// release. See @loca/shared catalog.ts (FitConfig) for field docs.

import type { FitConfigInput } from "@loca/shared";

export const FIT_CONFIG: FitConfigInput = {
  // Half targets ~50% of the memory budget; Full ~90% with a safety margin so a
  // model that fits on paper does not OOM the machine in practice.
  budgetFraction: { half: 0.5, full: 0.9 },
  // Headroom subtracted from the budget before fitting (OS + Loca + slack).
  safetyMarginBytes: 1_500_000_000,
  // "Smartness" ranking source.
  primaryScore: { source: "artificial-analysis", metric: "intelligence-index" },
  // Smallest must be at least as capable as Qwen 3.5 2B (0.8B is too weak; it is
  // still in the catalog and browsable, just below this floor).
  smallestQualityFloorRef: "Qwen/Qwen3.5-2B",
  // Applied in order when primary scores tie: prefer the smaller footprint, then
  // the QAT variant. Add more "preferTag:<tag>" rules as data, never code.
  tieBreakers: ["lowerFootprint", "preferTag:qat"],

  // The desktop values for computeOverheadBytes, ctxCeiling, ctxLadder and
  // vision are the schema's defaults and are deliberately not restated here: a
  // catalog that omits them gets exactly what the engine used to hardcode.

  profiles: {
    // Phones. A phone is not a small desktop, so this is a different policy
    // rather than a scaled one.
    //
    // EVERY NUMBER MARKED "provisional" BELOW IS UNMEASURED: they were chosen so
    // the shape is right, and living here as catalog data means retuning one is a
    // one-line change with no core release.
    mobile: {
      // PROVISIONAL. Fractions of TOTAL RAM with no absolute margin: on iOS the
      // real ceiling is the per-app jetsam limit, which scales with the device,
      // so a fraction already expresses headroom and the desktop 1.5 GB margin
      // would leave nothing that fits on an 8 GB phone.
      budgetFraction: { half: 0.2, full: 0.35 },
      safetyMarginBytes: 0,

      // PROVISIONAL, and the one most likely to be wrong: the desktop 600 MB is
      // sized for a llama-server process with a desktop graph, and in-process on
      // a phone it should be smaller by an unknown amount. At 6 GB this is ~40%
      // of the half budget, so a 200 MB error changes which presets exist.
      computeOverheadBytes: 400_000_000,

      // Below the desktop floor on purpose: Qwen3.5-2B does not fit a 6 GB phone
      // at any useful context, and a Smallest preset that resolves to nothing is
      // worse than one that resolves to a small model.
      smallestQualityFloorRef: "Qwen/Qwen3.5-0.8B",

      // PROVISIONAL, and a LATENCY budget rather than a memory one: prompt
      // processing is seconds on a phone CPU at 8192. The 2048 rung is not
      // tuning, it is structural: the desktop ladder bottoms out at 4096, so
      // without it no phone-sized context is reachable at any budget and every
      // bucket comes back empty.
      ctxCeiling: { smallest: 2048, half: 4096, full: 4096 },
      ctxLadder: [8192, 4096, 2048],

      // Not preselected and not budgeted for: mmproj files run 205 MB to 1.2 GB,
      // and on a 6 GB phone that one file decides whether a Balanced preset
      // exists at all. A DEFAULT, not a ceiling: turning Image Attachments on
      // still works, the budget just stops matching what is loaded.
      vision: false,
    },
  },
};
