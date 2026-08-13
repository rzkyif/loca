/**
 * Reactive state for the Speech-to-Text catalog picker.
 *
 * A slimmed sibling of model-recommend.svelte.ts: loads the whisper catalog
 * (models + resolved curated cards) from the paired core and applies a chosen
 * card, model, or quant via the core, then reloads settings so the UI reflects
 * the new stt.* values. No fit engine, so no recompute/recheck machinery.
 */

import { errMessage } from "@loca/shared";
import { cores } from "$lib/core";
import type { SttCatalogResponse } from "$lib/core/models";
import { settingsState } from "./settings.svelte";

class SttModelsState {
  catalog = $state<SttCatalogResponse | null>(null);
  loading = $state(false);
  applying = $state<string | null>(null);
  error = $state<string | null>(null);

  async load(): Promise<void> {
    if (!cores().currentEntry()) return;
    this.loading = true;
    this.error = null;
    try {
      this.catalog = await cores().api().models.sttCatalog();
    } catch (e) {
      this.error = errMessage(e);
    } finally {
      this.loading = false;
    }
  }

  async applyPreset(presetId: string): Promise<void> {
    await this.applySelection({ presetId }, presetId);
  }

  /** Apply a catalog model with its default quant. */
  async applyModel(modelId: string): Promise<void> {
    await this.applySelection({ modelId }, "custom");
  }

  /** Apply one specific quant (by its unique modelSpec) of a catalog model. */
  async applyQuant(modelSpec: string): Promise<void> {
    await this.applySelection({ modelSpec }, "custom");
  }

  private async applySelection(
    sel: { presetId: string } | { modelId: string } | { modelSpec: string },
    tag: string,
  ): Promise<void> {
    this.applying = tag;
    this.error = null;
    try {
      await cores().api().models.sttSelect(sel);
      // Reflect the server-side stt.* writes (incl. stt.preset) in the UI.
      await settingsState.loadCoreSettings();
    } catch (e) {
      this.error = errMessage(e);
    } finally {
      this.applying = null;
    }
  }
}

export const sttModelsState = new SttModelsState();
