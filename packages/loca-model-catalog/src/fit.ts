// Fit-engine tuning, shipped inside the catalog so policy changes need no core
// release. See @loca/shared catalog.ts (FitConfig) for field docs.

import type { FitConfig } from "@loca/shared";

export const FIT_CONFIG: FitConfig = {
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
};
