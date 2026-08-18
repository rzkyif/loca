import { Hono } from "hono";
import { modelsManager } from "../../models/manager.ts";
import { downloadManager } from "../../downloads/manager.ts";
import { ensureKindModels } from "../../services/model-ensure.ts";
import { notifyRequirementsChanged } from "../../services/requirements.ts";
import { loadModelCatalog } from "../../models/catalog.ts";
import { detectHardware } from "../../models/hardware.ts";
import {
  appliedForModelSpec,
  buildCatalogViews,
  computeRecommendations,
} from "../../models/fit.ts";
import { buildSttPresetViews, resolveSttSelection } from "../../models/stt.ts";
import { buildTtsPresetViews, resolveTtsSelection } from "../../models/tts.ts";
import {
  loadCoreSettings,
  patchClientSettings,
  patchCoreSettings,
} from "@loca/core-engine/services/core-settings";
import { AppError } from "@loca/core-engine";
import { parseBody, readJson } from "@loca/core-engine/http/body";
import { bearerMiddleware, requireClient } from "../middleware/auth.ts";
import {
  type AppliedModelSettings,
  // Which device class the fit engine tunes for. Derived from the OS, at this
  // one call site, rather than inside the engine: a `detect`-shaped function
  // whose every caller passes the same value has an unobserved parameter
  // (D-145), and the profile is not `Frontend::Embedded` either, because a
  // desktop process booted embedded still has desktop memory (D-144's line).
  fitProfileForOs,
  modelDownloadBodySchema,
  modelProbeBodySchema,
  modelSelectBodySchema,
  PRESET_BUCKETS,
  type PresetBucket,
  settingKeyDestination,
  speechSelectBodySchema,
} from "@loca/shared";

/** This Core's fit profile, fixed for the life of the process because the
 *  platform is. The TypeScript core only ever runs on a desktop, so this is
 *  always `desktop` here; the mobile profile is exercised by the Rust core on a
 *  phone and by fit.test.ts, which passes the profile explicitly. */
const FIT_PROFILE = fitProfileForOs(Deno.build.os);

type ModelKind = "llm" | "stt" | "tts" | "embed";

export function modelsRoutes(): Hono {
  const r = new Hono();
  r.use("*", bearerMiddleware());

  r.get("/", async (c) => {
    return c.json(await modelsManager().list());
  });

  r.post("/download", async (c) => {
    const { items } = parseBody(modelDownloadBodySchema, await readJson(c));
    const jobIds = modelsManager().download(
      items.map((i) => ({ source: i.source, group: i.group })),
    );
    return c.json({ jobIds });
  });

  // Drop any Completed rows whose file has since been deleted before returning
  // the queue, so a download whose model was removed (in-app or externally)
  // self-clears from the list instead of lingering.
  r.get("/downloads", async (c) => {
    await downloadManager().reconcileCompleted();
    return c.json(modelsManager().downloads());
  });

  r.post("/downloads/:id/cancel", (c) => {
    downloadManager().cancel(c.req.param("id"));
    return c.body(null, 204);
  });

  r.post("/downloads/:id/retry", (c) => {
    downloadManager().retry(c.req.param("id"));
    return c.body(null, 204);
  });

  r.delete("/downloads/:id", (c) => {
    downloadManager().remove(c.req.param("id"));
    return c.body(null, 204);
  });

  r.post("/probe", async (c) => {
    const { sources } = parseBody(modelProbeBodySchema, await readJson(c));
    return c.json(await modelsManager().probe(sources));
  });

  // Bulk-ensure every model file the named sidecar kind needs given the
  // current settings. Skips files already on disk; enqueues downloads for
  // the rest. Returns the list of files that were enqueued (jobIds) and
  // those that were already present. Note: `sidecar-boot` auto-calls this
  // internally on settings change, so the client only needs to hit /ensure
  // for explicit "Download required models" buttons.
  r.post("/ensure", async (c) => {
    const body = (await readJson(c)) as { kind?: string };
    if (
      body.kind !== "llm" &&
      body.kind !== "stt" &&
      body.kind !== "tts" &&
      body.kind !== "embed"
    ) {
      throw new AppError("validation_error", "kind must be one of: llm, stt, tts, embed");
    }
    return c.json(await ensureKindModels(body.kind as ModelKind));
  });

  // --- adaptive presets + model browser ------------------------------------

  // The three computed presets for this device, plus what settings currently
  // point at (so the client can flag a better option).
  r.get("/recommend", async (c) => {
    const [catalog, hw, settings] = await Promise.all([
      loadModelCatalog(),
      detectHardware(),
      loadCoreSettings(),
    ]);
    return c.json(computeRecommendations(catalog, hw, appliedFromSettings(settings), FIT_PROFILE));
  });

  // Force-refresh the catalog + re-probe hardware, then recompute. Used by the
  // non-destructive "Check for better models" action.
  r.post("/recommend/recheck", async (c) => {
    const [catalog, hw, settings] = await Promise.all([
      loadModelCatalog({ force: true }),
      detectHardware(true),
      loadCoreSettings(),
    ]);
    // Re-probing hardware can change the desired GPU binary variant (e.g. a GPU
    // or driver appeared), so re-evaluate the required-files snapshot: a
    // now-wrong-variant binary surfaces as a pending reinstall.
    void notifyRequirementsChanged();
    return c.json(computeRecommendations(catalog, hw, appliedFromSettings(settings), FIT_PROFILE));
  });

  // Full model list annotated with fit-on-this-device + the settings each would
  // apply (the model browser).
  r.get("/catalog", async (c) => {
    const [catalog, hw] = await Promise.all([loadModelCatalog(), detectHardware()]);
    return c.json({
      generatedAt: catalog.generatedAt,
      models: buildCatalogViews(catalog, hw, FIT_PROFILE),
    });
  });

  // Apply a preset bucket or a specific catalog model: write the llm.* settings
  // (which makes sidecar-boot restart llama). Does NOT download: writing
  // settings triggers a requirements recompute (ws/hub), which surfaces any
  // missing model file in the client's pending-downloads confirm modal.
  // Downloading multi-GB weights is always the user's explicit choice, never a
  // side effect of selecting a preset.
  r.post("/select", async (c) => {
    const me = requireClient(c);
    const body = parseBody(modelSelectBodySchema, await readJson(c));
    const apply = await resolveSelection(body);
    // The patch mixes shared model/server keys (core) with the model's
    // recommended sampling/reasoning tuning (client-on-core). Partition by
    // destination so the shared store only ever holds `core` keys: the model
    // restart is core-wide, but the recommended tuning lands in this client's
    // own overlay rather than rewriting every other client's sampling baseline.
    const patch = applyToPatch(apply.settings, apply.preset);
    const core: Record<string, unknown> = {};
    const perClient: Record<string, unknown> = {};
    for (const [k, v] of Object.entries(patch)) {
      if (settingKeyDestination(k) === "client-on-core") perClient[k] = v;
      else core[k] = v;
    }
    if (Object.keys(core).length > 0) await patchCoreSettings(core);
    if (Object.keys(perClient).length > 0) await patchClientSettings(me.id, perClient);
    return c.json({ applied: apply });
  });

  // --- Speech-to-Text catalog picker ----------------------------------------

  // The whole whisper lineup plus the resolved curated cards. No fit engine:
  // whisper models are 31 MB to 3.1 GB, so everything fits everywhere.
  r.get("/stt/catalog", async (c) => {
    const catalog = await loadModelCatalog();
    return c.json({
      generatedAt: catalog.generatedAt,
      models: catalog.stt.models,
      presets: buildSttPresetViews(catalog),
    });
  });

  // Apply a curated card or a specific model/quant: write the stt.* settings
  // (which makes sidecar-boot reconfigure the speech sidecar). Like the LLM select,
  // never downloads; the settings write surfaces any missing model file in the
  // client's pending-downloads confirm modal.
  r.post("/stt/select", async (c) => {
    const body = parseBody(speechSelectBodySchema, await readJson(c));
    const [catalog, hw] = await Promise.all([loadModelCatalog(), detectHardware()]);
    const applied = resolveSttSelection(catalog, hw, body);
    await patchCoreSettings({
      "stt.provider": "local",
      "stt.preset": applied.preset,
      "stt.modelType": applied.settings.modelType,
      "stt.modelPath": applied.settings.modelPath,
      "stt.modelFiles": applied.settings.modelFiles,
      "stt.threads": applied.settings.threads,
    });
    return c.json({ applied });
  });

  // --- Text-to-Speech catalog picker ----------------------------------------

  // The whole TTS lineup plus the resolved curated cards (with each model's
  // voices, so the voice dropdown can populate from the selection). No fit
  // engine: TTS bundles are small and all fit everywhere.
  r.get("/tts/catalog", async (c) => {
    const catalog = await loadModelCatalog();
    return c.json({
      generatedAt: catalog.generatedAt,
      models: catalog.tts.models,
      presets: buildTtsPresetViews(catalog),
    });
  });

  // Apply a curated card or a specific model/quant: write the tts.* settings
  // (which makes sidecar-boot reconfigure the speech sidecar). Like STT select,
  // never downloads; the settings write surfaces any missing file in the
  // client's pending-downloads confirm modal. Picking a model resets tts.voice
  // to that model's default voice.
  r.post("/tts/select", async (c) => {
    const body = parseBody(speechSelectBodySchema, await readJson(c));
    const catalog = await loadModelCatalog();
    const applied = resolveTtsSelection(catalog, body);
    await patchCoreSettings({
      "tts.preset": applied.preset,
      "tts.modelType": applied.settings.modelType,
      "tts.modelPath": applied.settings.modelPath,
      "tts.modelFiles": applied.settings.modelFiles,
      "tts.voice": applied.settings.voice,
    });
    return c.json({ applied });
  });

  // Delete a model file by its path. Registered last: the `{.+}` catch-all
  // would otherwise shadow the specific `/downloads/...` routes above (a DELETE
  // to /downloads/:id would be misread as deleting a file at "downloads/<id>").
  r.delete("/:relPath{.+}", async (c) => {
    await modelsManager().delete(c.req.param("relPath"));
    // A deleted model file can re-expose a requirement (computeRequirements
    // only re-probes when notified), so recompute now; otherwise the client's
    // pending-downloads snapshot keeps reporting the file as present.
    void notifyRequirementsChanged();
    return c.body(null, 204);
  });

  return r;
}

/** Resolve a {bucket|modelId|modelSpec} selection into the settings to apply. */
async function resolveSelection(body: {
  bucket?: string;
  modelId?: string;
  modelSpec?: string;
}): Promise<{ preset: string; settings: AppliedModelSettings }> {
  const [catalog, hw, settings] = await Promise.all([
    loadModelCatalog(),
    detectHardware(),
    loadCoreSettings(),
  ]);
  if (body.bucket) {
    if (!(PRESET_BUCKETS as readonly string[]).includes(body.bucket)) {
      throw new AppError("validation_error", `unknown bucket: ${body.bucket}`);
    }
    const rec = computeRecommendations(catalog, hw, appliedFromSettings(settings), FIT_PROFILE)
      .buckets[body.bucket as PresetBucket];
    if (!rec) {
      throw new AppError("validation_error", `no model fits the "${body.bucket}" preset`);
    }
    return { preset: body.bucket, settings: rec.apply };
  }
  // A specific quant (by its unique modelSpec) from the manual picker.
  if (body.modelSpec) {
    const resolved = appliedForModelSpec(catalog, hw, body.modelSpec, FIT_PROFILE);
    if (!resolved) {
      throw new AppError("not_found", `quant not in catalog: ${body.modelSpec}`);
    }
    return { preset: "custom", settings: resolved.apply };
  }
  if (body.modelId) {
    const view = buildCatalogViews(catalog, hw, FIT_PROFILE).find((m) => m.id === body.modelId);
    if (!view) {
      throw new AppError("not_found", `model not in catalog: ${body.modelId}`);
    }
    return { preset: "custom", settings: view.apply };
  }
  throw new AppError("validation_error", "bucket, modelId, or modelSpec required");
}

function applyToPatch(a: AppliedModelSettings, preset: string): Record<string, unknown> {
  return {
    "llm.provider": "local",
    "llm.preset": preset,
    "llm.modelPath": a.modelPath,
    "llm.mmprojPath": a.mmprojPath ?? "",
    "llm.contextSize": a.contextSize,
    "llm.threads": a.threads,
    "llm.gpuLayers": a.gpuLayers,
    "llm.flashAttn": a.flashAttn,
    // Deliberately NOT writing llm.supportImages: image support is a user choice
    // (off by default), decoupled from the preset, so selecting a vision-capable
    // model must not silently turn vision on (and pull in the mmproj download).
    // mmprojPath is still set so vision works immediately if the user opts in.
    "llm.idleUnloadSeconds": a.idleUnloadSeconds,
    "llm.reasoningBudget": a.reasoningBudget,
    // Sampling is always written (even at the default), so switching models
    // resets these to the new model's tuning rather than leaving stale values.
    "llm.temperature": a.temperature,
    "llm.topP": a.topP,
    "llm.topK": a.topK,
    "llm.minP": a.minP,
    "llm.repeatPenalty": a.repeatPenalty,
    "llm.dryMultiplier": a.dryMultiplier,
    "llm.presencePenalty": a.presencePenalty,
    // Persisted as a `;`-joined chain (empty = the server's default order).
    "llm.samplers": a.samplers?.join(";") ?? "",
  };
}

function appliedFromSettings(s: Record<string, unknown>): { preset?: string; modelPath?: string } {
  const preset = typeof s["llm.preset"] === "string" ? (s["llm.preset"] as string) : undefined;
  const modelPath =
    typeof s["llm.modelPath"] === "string" ? (s["llm.modelPath"] as string) : undefined;
  return { preset, modelPath };
}
