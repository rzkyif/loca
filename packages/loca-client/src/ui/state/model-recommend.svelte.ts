/**
 * Reactive state for the adaptive LLM presets + model browser.
 *
 * Loads the three computed presets (Smallest / Half / Full) for the paired
 * core's hardware, supports a non-destructive "Check for better models" recheck
 * (which never changes settings on its own), and applies a chosen bucket or a
 * specific catalog model via the core, then reloads settings so the UI reflects
 * the new llm.* values.
 */

import {
  type CatalogModelView,
  errMessage,
  PRESET_BUCKETS,
  type PresetBucket,
  type RecommendationSet,
} from "@loca/shared";
import { cores } from "$lib/core";
import { settingsState } from "./settings.svelte";

/** Minimum time the recheck spinner stays up, so the loading state is legible
 *  rather than a flicker on a fast (dev/cached) recompute. */
const MIN_CHECK_MS = 1000;

/** How long the recheck result ("found"/"none") shows before reverting to idle. */
const RESULT_LINGER_MS = 3000;

const delay = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));

class ModelRecommendState {
  recommendations = $state<RecommendationSet | null>(null);
  catalog = $state<CatalogModelView[] | null>(null);
  loading = $state(false);
  checking = $state(false);
  // Outcome of the most recent recheck: a newer model is available, none is, or
  // no check has run yet. Drives the "Check for Newer Models" result state.
  checkResult = $state<"found" | "none" | null>(null);
  applying = $state<PresetBucket | "custom" | null>(null);
  error = $state<string | null>(null);
  lastCheckedMs = $state<number | null>(null);
  // Buckets whose "better available" badge the user has dismissed this session.
  private dismissed = $state<Set<PresetBucket>>(new Set());
  private resultTimer: ReturnType<typeof setTimeout> | null = null;

  async load(): Promise<void> {
    if (!cores().currentEntry()) return;
    this.loading = true;
    this.error = null;
    try {
      this.recommendations = await cores().api().models.recommend();
    } catch (e) {
      this.error = errMessage(e);
    } finally {
      this.loading = false;
    }
  }

  async recheck(): Promise<void> {
    if (!cores().currentEntry()) return;
    this.clearResultTimer();
    this.checking = true;
    this.checkResult = null;
    this.error = null;
    try {
      // Hold the spinner for at least MIN_CHECK_MS so the state is readable.
      const [set] = await Promise.all([cores().api().models.recheck(), delay(MIN_CHECK_MS)]);
      this.recommendations = set;
      this.lastCheckedMs = Date.now();
      this.dismissed = new Set();
      this.checkResult = this.anyBetterAvailable() ? "found" : "none";
      // Revert the button to its idle label after the result has been seen.
      this.resultTimer = setTimeout(() => {
        this.checkResult = null;
        this.resultTimer = null;
      }, RESULT_LINGER_MS);
    } catch (e) {
      this.error = errMessage(e);
    } finally {
      this.checking = false;
    }
  }

  private clearResultTimer(): void {
    if (this.resultTimer !== null) {
      clearTimeout(this.resultTimer);
      this.resultTimer = null;
    }
  }

  async loadCatalog(): Promise<void> {
    if (!cores().currentEntry()) return;
    try {
      this.catalog = (await cores().api().models.catalog()).models;
    } catch (e) {
      this.error = errMessage(e);
    }
  }

  async applyBucket(bucket: PresetBucket): Promise<void> {
    await this.applySelection({ bucket }, bucket);
  }

  async applyModel(modelId: string): Promise<void> {
    await this.applySelection({ modelId }, "custom");
  }

  /** Apply one specific quant (by its unique modelSpec) of a catalog model. */
  async applyQuant(modelSpec: string): Promise<void> {
    await this.applySelection({ modelSpec }, "custom");
  }

  private async applySelection(
    sel: { bucket: PresetBucket } | { modelId: string } | { modelSpec: string },
    tag: PresetBucket | "custom",
  ): Promise<void> {
    this.applying = tag;
    this.error = null;
    // Applying resolves any "newer models" notice from a prior recheck.
    this.clearResultTimer();
    this.checkResult = null;
    try {
      const res = await cores().api().models.select(sel);
      // Reflect the server-side llm.* writes (incl. llm.preset) in the UI.
      await settingsState.loadCoreSettings();
      // The recommendation buckets don't change on apply (same catalog +
      // hardware); only what's "applied" does. Update it locally instead of
      // re-fetching GET /recommend.
      if (this.recommendations) {
        this.recommendations = {
          ...this.recommendations,
          applied: {
            preset: res.applied.preset,
            modelPath: res.applied.settings.modelPath,
          },
        };
      }
    } catch (e) {
      this.error = errMessage(e);
    } finally {
      this.applying = null;
    }
  }

  /** Any bucket where a newer/better model is now available for the user's
   *  current preset. Drives the recheck result state. */
  anyBetterAvailable(): boolean {
    return PRESET_BUCKETS.some((b) => this.betterAvailable(b));
  }

  /** True when the user is on this preset but a different (better) model now
   *  fits it. Drives the non-destructive "better available" badge. */
  betterAvailable(bucket: PresetBucket): boolean {
    const set = this.recommendations;
    if (!set || this.dismissed.has(bucket)) return false;
    const rec = set.buckets[bucket];
    if (!rec) return false;
    return set.applied.preset === bucket && set.applied.modelPath !== rec.apply.modelPath;
  }

  dismiss(bucket: PresetBucket): void {
    this.dismissed = new Set([...this.dismissed, bucket]);
  }
}

export const modelRecommendState = new ModelRecommendState();
