import type {
  AppliedModelSettings,
  AppliedSttSettings,
  AppliedTtsSettings,
  CatalogModelView,
  DownloadModelsRequest,
  DownloadModelsResponse,
  ListDownloadsResponse,
  ListModelsResponse,
  PresetBucket,
  ProbeModelsResponse,
  RecommendationSet,
  SttCatalogModel,
  SttPresetView,
  TtsCatalogModel,
  TtsPresetView,
} from "@loca/shared";
import type { CoreClient } from "./client";

export interface CatalogResponse {
  generatedAt: string;
  models: CatalogModelView[];
}

export interface SelectModelResponse {
  applied: { preset: string; settings: AppliedModelSettings };
}

export interface SttCatalogResponse {
  generatedAt: string;
  models: SttCatalogModel[];
  presets: SttPresetView[];
}

export interface SelectSttModelResponse {
  applied: { preset: string; settings: AppliedSttSettings };
}

export interface TtsCatalogResponse {
  generatedAt: string;
  models: TtsCatalogModel[];
  presets: TtsPresetView[];
}

export interface SelectTtsModelResponse {
  applied: { preset: string; settings: AppliedTtsSettings };
}

export class ModelsApi {
  constructor(private readonly client: CoreClient) {}

  list(): Promise<ListModelsResponse> {
    return this.client.get("/api/v1/models");
  }

  download(req: DownloadModelsRequest): Promise<DownloadModelsResponse> {
    return this.client.post("/api/v1/models/download", req);
  }

  delete(relPath: string): Promise<void> {
    return this.client.del(`/api/v1/models/${encodeURI(relPath)}`) as Promise<void>;
  }

  downloads(): Promise<ListDownloadsResponse> {
    return this.client.get("/api/v1/models/downloads");
  }

  cancel(id: string): Promise<void> {
    return this.client.post(`/api/v1/models/downloads/${encodeURIComponent(id)}/cancel`, {});
  }

  retry(id: string): Promise<void> {
    return this.client.post(`/api/v1/models/downloads/${encodeURIComponent(id)}/retry`, {});
  }

  remove(id: string): Promise<void> {
    return this.client.del(`/api/v1/models/downloads/${encodeURIComponent(id)}`) as Promise<void>;
  }

  probe(sources: string[]): Promise<ProbeModelsResponse> {
    return this.client.post("/api/v1/models/probe", { sources });
  }

  // --- adaptive presets + model browser ----------------------------------

  /** The three computed presets for this device + what's currently applied. */
  recommend(): Promise<RecommendationSet> {
    return this.client.get("/api/v1/models/recommend");
  }

  /** Force-refresh the catalog + re-probe hardware, then recompute. */
  recheck(): Promise<RecommendationSet> {
    return this.client.post("/api/v1/models/recommend/recheck", {});
  }

  /** Full model list annotated with fit-on-this-device (the browser). */
  catalog(): Promise<CatalogResponse> {
    return this.client.get("/api/v1/models/catalog");
  }

  /** Apply a preset bucket, a catalog model (its recommended quant), or a
   *  specific quant by its modelSpec: writes llm.* settings. */
  select(
    sel: { bucket: PresetBucket } | { modelId: string } | { modelSpec: string },
  ): Promise<SelectModelResponse> {
    return this.client.post("/api/v1/models/select", sel);
  }

  // --- Speech-to-Text catalog picker --------------------------------------

  /** The whisper model lineup plus the resolved curated cards. */
  sttCatalog(): Promise<SttCatalogResponse> {
    return this.client.get("/api/v1/models/stt/catalog");
  }

  /** Apply a curated card, a catalog model (its default quant), or a specific
   *  quant by its modelSpec: writes stt.* settings. */
  sttSelect(
    sel: { presetId: string } | { modelId: string } | { modelSpec: string },
  ): Promise<SelectSttModelResponse> {
    return this.client.post("/api/v1/models/stt/select", sel);
  }

  // --- Text-to-Speech catalog picker --------------------------------------

  /** The TTS model lineup plus the resolved curated cards (with voices). */
  ttsCatalog(): Promise<TtsCatalogResponse> {
    return this.client.get("/api/v1/models/tts/catalog");
  }

  /** Apply a curated card, a catalog model (its default quant), or a specific
   *  quant by its modelSpec: writes tts.* settings. */
  ttsSelect(
    sel: { presetId: string } | { modelId: string } | { modelSpec: string },
  ): Promise<SelectTtsModelResponse> {
    return this.client.post("/api/v1/models/tts/select", sel);
  }
}
