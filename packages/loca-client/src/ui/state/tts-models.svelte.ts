/**
 * Reactive state for the Text-to-Speech catalog picker.
 *
 * Sibling of stt-models.svelte.ts: loads the TTS catalog (models + resolved
 * curated cards, each carrying its voices) from the paired core and applies a
 * chosen card, model, or quant via the core, then reloads settings so the UI
 * reflects the new tts.* values (including the reset tts.voice). No fit engine.
 */

import { errMessage } from "@loca/shared";
import { cores } from "$lib/core";
import type { TtsCatalogResponse } from "$lib/core/models";
import { settingsState } from "./settings.svelte";

class TtsModelsState {
  catalog = $state<TtsCatalogResponse | null>(null);
  loading = $state(false);
  applying = $state<string | null>(null);
  error = $state<string | null>(null);

  async load(): Promise<void> {
    if (!cores().currentEntry()) return;
    this.loading = true;
    this.error = null;
    try {
      this.catalog = await cores().api().models.ttsCatalog();
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

  /** Apply one specific quant (by its primary modelSpec) of a catalog model. */
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
      await cores().api().models.ttsSelect(sel);
      // Reflect the server-side tts.* writes (incl. tts.preset, tts.voice).
      await settingsState.loadCoreSettings();
    } catch (e) {
      this.error = errMessage(e);
    } finally {
      this.applying = null;
    }
  }
}

export const ttsModelsState = new TtsModelsState();
