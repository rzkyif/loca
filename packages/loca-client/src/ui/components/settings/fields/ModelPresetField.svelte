<script lang="ts">
  import type { PresetBucket, PresetOption, SettingField } from "@loca/shared";
  import { settingsState } from "../../../state";
  import { modelRecommendState } from "../../../state/model-recommend.svelte";
  import { cores } from "$lib/core";
  import FieldCard from "./FieldCard.svelte";
  import ModelPresetFieldView from "@loca/shared/ui/components/settings/ModelPresetFieldView.svelte";

  // The adaptive LLM preset picker. Unlike the generic PresetField, each card's
  // model + tuning is computed by the core for this device; selecting a card
  // calls the select API rather than applying static defaults. This shell owns
  // the recommendation + catalog stores and feeds the pure ModelPresetFieldView.
  let { field, onPresetSelect } = $props<{
    field: SettingField;
    onPresetSelect: (fieldId: string, option: PresetOption) => void;
  }>();

  const rs = modelRecommendState;

  // The dropdown value when the current llm.* config matches no catalog model.
  const MANUAL = "__manual__";

  // Load recommendations + catalog once the core is paired (and not already
  // loaded). The catalog backs the Custom card's model + quant dropdowns.
  $effect(() => {
    if (cores().currentEntry()) {
      if (!rs.recommendations && !rs.loading) void rs.load();
      if (!rs.catalog) void rs.loadCatalog();
    }
  });

  const bucketOptions = $derived((field.presetConfig?.options ?? []) as PresetOption[]);
  const customOption = $derived(
    (field.presetConfig?.secondaryOptions ?? [])[0] as PresetOption | undefined,
  );

  function gb(bytes: number): string {
    return `${(bytes / 1e9).toFixed(1)} GB`;
  }

  function bucketRec(id: string) {
    return rs.recommendations?.buckets[id as PresetBucket] ?? null;
  }

  // --- recheck button states ----------------------------------------------
  const checkLabel = $derived(
    rs.checking
      ? "Checking…"
      : rs.checkResult === "found"
        ? "Newer Models Found"
        : rs.checkResult === "none"
          ? "No Newer Models Found"
          : "Check for Newer Models",
  );
  const checkIcon = $derived(
    rs.checking
      ? "i-line-md:loading-loop"
      : rs.checkResult === "found"
        ? "i-material-symbols-auto-awesome-rounded"
        : rs.checkResult === "none"
          ? "i-material-symbols-check-rounded"
          : "i-material-symbols-refresh-rounded",
  );

  // --- bucket cards -------------------------------------------------------
  const buckets = $derived(
    bucketOptions.map((opt) => {
      const bucket = opt.id as PresetBucket;
      const rec = bucketRec(opt.id);
      const title = opt.title ?? opt.label;
      const better = rs.betterAvailable(bucket);
      return {
        id: opt.id,
        title,
        description: opt.description,
        selected: settingsState.currentSettings[field.id] === opt.id,
        selectable: !!rec,
        badges: rec
          ? [
              { icon: "i-material-symbols-psychology-alt-rounded", text: rec.name },
              { icon: "i-material-symbols-memory-rounded", text: gb(rec.footprintBytes) },
              { icon: "i-material-symbols-bolt-rounded", text: rec.quant },
            ]
          : null,
        placeholder: rec
          ? undefined
          : rs.loading
            ? "Computing for your device…"
            : "No model fits this tier",
        better: better
          ? {
              message: `A better model is available for ${title}: ${rec?.name}`,
              applying: rs.applying === bucket,
              onApply: () => rs.applyBucket(bucket),
              onDismiss: () => rs.dismiss(bucket),
            }
          : undefined,
      };
    }),
  );

  // --- manual model + quantization dropdowns ------------------------------

  // Model dropdown: every catalog model plus "Manual Configuration". Size is a
  // range across the model's quants (the exact size is picked in the quant
  // dropdown). A model that won't fit this device is shown but disabled.
  const modelOptions = $derived([
    ...(rs.catalog ?? []).map((m) => {
      const sizes = m.quants.map((q) => q.footprintBytes / 1e9);
      const min = Math.min(...sizes);
      const max = Math.max(...sizes);
      const range = min === max ? `${min.toFixed(1)} GB` : `${min.toFixed(1)}-${max.toFixed(1)} GB`;
      return {
        value: m.id,
        label: `${m.name} · ${range}${m.fits ? "" : " · won't fit"}`,
        disabled: !m.fits,
      };
    }),
    { value: MANUAL, label: "Manual Configuration" },
  ]);

  // The model dropdown reflects whichever catalog model the current llm.modelPath
  // points at; if the user has hand-edited the config to a model not in the
  // catalog, it falls back to "Manual Configuration".
  const selectedModelView = $derived(
    (rs.catalog ?? []).find((m) =>
      m.quants.some((q) => q.modelSpec === settingsState.currentSettings["llm.modelPath"]),
    ) ?? null,
  );

  // Explicit "Manual Configuration" choice. Picking it doesn't change the model
  // path (a catalog model may still be configured), so it can't be derived from
  // the path alone; track it, and clear it whenever the path actually changes
  // (a bucket applied, or another model picked).
  let manualSelected = $state(false);
  let lastModelPath: string | undefined = undefined;
  $effect(() => {
    const path = settingsState.currentSettings["llm.modelPath"] as string | undefined;
    if (path !== lastModelPath) {
      lastModelPath = path;
      manualSelected = false;
    }
  });

  const selectedModel = $derived(manualSelected ? MANUAL : (selectedModelView?.id ?? MANUAL));

  // Quantization dropdown for the selected model. The fit engine flags one as
  // "recommended" (the quality/size sweet spot, capped below Q8's diminishing
  // returns); oversized quants are shown but disabled.
  const quantOptions = $derived(
    (selectedModelView?.quants ?? []).map((q) => ({
      value: q.modelSpec,
      label:
        `${q.quant}${q.variantLabel !== "standard" ? ` (${q.variantLabel})` : ""}` +
        ` · ${gb(q.footprintBytes)}${q.recommended ? " · recommended" : ""}${q.fits ? "" : " · won't fit"}`,
      disabled: !q.fits,
    })),
  );
  const selectedQuant = $derived(settingsState.currentSettings["llm.modelPath"] as string);

  const custom = $derived(
    customOption
      ? {
          title: customOption.title ?? customOption.label,
          description: customOption.description,
          selected: settingsState.currentSettings[field.id] === customOption.id,
          model: { value: selectedModel, options: modelOptions },
          quant:
            !manualSelected && selectedModelView
              ? { value: selectedQuant, options: quantOptions }
              : null,
        }
      : null,
  );

  function onModelSelect(value: string): void {
    if (value === MANUAL) {
      // Switch to Custom without touching the llama-server fields, and hide the
      // quant dropdown until a concrete model is chosen again.
      manualSelected = true;
      if (customOption) onPresetSelect(field.id, customOption);
      return;
    }
    manualSelected = false;
    void rs.applyModel(value);
  }

  function onQuantSelect(modelSpec: string): void {
    void rs.applyQuant(modelSpec);
  }
</script>

<FieldCard {field}>
  <ModelPresetFieldView
    error={rs.error}
    {checkLabel}
    {checkIcon}
    checkDisabled={rs.checking}
    {buckets}
    {custom}
    onCheck={() => rs.recheck()}
    onSelectBucket={(id) => rs.applyBucket(id as PresetBucket)}
    onSelectCustom={() => customOption && onPresetSelect(field.id, customOption)}
    {onModelSelect}
    {onQuantSelect}
  />
</FieldCard>
