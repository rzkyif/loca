// Speech-to-Text and Text-to-Speech catalog pickers: load the catalog (models +
// resolved curated cards) from the paired core, apply a card / model / quant
// through it, then reload settings so the UI reflects the new stt.* / tts.*
// values. No fit engine.

import { errMessage } from "@loca/shared";
import { cores } from "$lib/core";
import type { SttCatalogResponse, TtsCatalogResponse } from "$lib/core";
import { settingsState } from "./settings.svelte";

type Selection = { presetId: string } | { modelId: string } | { modelSpec: string };

class SpeechModelsState<C> {
  catalog = $state<C | null>(null);
  loading = $state(false);
  applying = $state<string | null>(null);
  error = $state<string | null>(null);

  constructor(
    private readonly fetchCatalog: () => Promise<C>,
    private readonly select: (sel: Selection) => Promise<unknown>,
  ) {}

  async load(): Promise<void> {
    if (!cores().currentEntry()) return;
    this.loading = true;
    this.error = null;
    try {
      this.catalog = await this.fetchCatalog();
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

  /** Apply one specific quant (by its modelSpec) of a catalog model. */
  async applyQuant(modelSpec: string): Promise<void> {
    await this.applySelection({ modelSpec }, "custom");
  }

  private async applySelection(sel: Selection, tag: string): Promise<void> {
    this.applying = tag;
    this.error = null;
    try {
      await this.select(sel);
      await settingsState.loadCoreSettings();
    } catch (e) {
      this.error = errMessage(e);
    } finally {
      this.applying = null;
    }
  }
}

export const sttModelsState = new SpeechModelsState<SttCatalogResponse>(
  () => cores().api().models.sttCatalog(),
  (sel) => cores().api().models.sttSelect(sel),
);

export const ttsModelsState = new SpeechModelsState<TtsCatalogResponse>(
  () => cores().api().models.ttsCatalog(),
  (sel) => cores().api().models.ttsSelect(sel),
);
