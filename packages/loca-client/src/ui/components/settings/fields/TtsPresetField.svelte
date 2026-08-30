<script lang="ts">
  import type { PresetOption, SettingField, TtsCatalogModel } from "@loca/shared";
  import { primaryFileSpec, TTS_PRIMARY_ROLE } from "@loca/shared";
  import { settingsState } from "../../../state";
  import { ttsModelsState } from "$stores/speech-models.svelte";
  import { cores } from "$lib/core";
  import FieldCard from "./FieldCard.svelte";
  import TtsPresetFieldView from "@loca/shared/ui/components/settings/TtsPresetFieldView.svelte";

  // The Text-to-Speech catalog picker. Mirrors SttPresetField: each card binds to
  // a catalog model and selecting one calls the select API; there is no fit
  // engine (TTS bundles are small and fit everywhere). This shell owns the
  // catalog store and feeds the pure TtsPresetFieldView.
  let { field, onPresetSelect } = $props<{
    field: SettingField;
    onPresetSelect: (fieldId: string, option: PresetOption) => void;
  }>();

  const ts = ttsModelsState;

  const MANUAL = "__manual__";

  $effect(() => {
    if (cores().currentEntry() && !ts.catalog && !ts.loading) void ts.load();
  });

  const cardOptions = $derived((field.presetConfig?.options ?? []) as PresetOption[]);
  const customOption = $derived(
    (field.presetConfig?.secondaryOptions ?? [])[0] as PresetOption | undefined,
  );

  function size(bytes: number): string {
    return bytes >= 1e9 ? `${(bytes / 1e9).toFixed(1)} GB` : `${Math.round(bytes / 1e6)} MB`;
  }

  function voiceCount(n: number): string {
    return n === 1 ? "1 voice" : `${n} voices`;
  }

  function cardPreset(id: string) {
    return ts.catalog?.presets.find((p) => p.id === id) ?? null;
  }

  // --- preset cards -------------------------------------------------------
  const presets = $derived(
    cardOptions.map((opt) => {
      const preset = cardPreset(opt.id);
      return {
        id: opt.id,
        title: opt.title ?? opt.label,
        description: opt.description,
        selected: settingsState.currentSettings[field.id] === opt.id,
        selectable: !!preset,
        badges: preset
          ? [
              { icon: "i-material-symbols-graphic-eq-rounded", text: preset.name },
              { icon: "i-material-symbols-memory-rounded", text: size(preset.fileSizeBytes) },
              {
                icon: "i-material-symbols-record-voice-over-rounded",
                text: voiceCount(preset.voices.length),
              },
            ]
          : null,
        placeholder: preset ? undefined : ts.loading ? "Loading catalog…" : undefined,
      };
    }),
  );

  // --- manual model + quantization dropdowns ------------------------------

  const modelOptions = $derived([
    ...(ts.catalog?.models ?? []).map((m: TtsCatalogModel) => {
      const sizes = m.quants.map((q) => q.fileSizeBytes);
      const min = Math.min(...sizes);
      const max = Math.max(...sizes);
      const range = min === max ? size(min) : `${size(min)}-${size(max)}`;
      return { value: m.id, label: `${m.name} · ${range}` };
    }),
    { value: MANUAL, label: "Manual Configuration" },
  ]);

  const selectedModelView = $derived(
    (ts.catalog?.models ?? []).find((m: TtsCatalogModel) =>
      m.quants.some(
        (q) =>
          primaryFileSpec(q, TTS_PRIMARY_ROLE[m.family]) ===
          settingsState.currentSettings["tts.modelPath"],
      ),
    ) ?? null,
  );

  let manualSelected = $state(false);
  let lastModelPath: string | undefined = undefined;
  $effect(() => {
    const path = settingsState.currentSettings["tts.modelPath"] as string | undefined;
    if (path !== lastModelPath) {
      lastModelPath = path;
      manualSelected = false;
    }
  });

  const selectedModel = $derived(manualSelected ? MANUAL : (selectedModelView?.id ?? MANUAL));

  const quantOptions = $derived(
    (selectedModelView?.quants ?? []).map((q) => ({
      value: selectedModelView
        ? primaryFileSpec(q, TTS_PRIMARY_ROLE[selectedModelView.family])
        : "",
      label: `${q.quant} · ${size(q.fileSizeBytes)}`,
    })),
  );
  const selectedQuant = $derived(settingsState.currentSettings["tts.modelPath"] as string);

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
      manualSelected = true;
      if (customOption) onPresetSelect(field.id, customOption);
      return;
    }
    manualSelected = false;
    void ts.applyModel(value);
  }

  function onQuantSelect(modelSpec: string): void {
    void ts.applyQuant(modelSpec);
  }
</script>

<FieldCard {field}>
  <TtsPresetFieldView
    error={ts.error}
    {presets}
    {custom}
    onSelectPreset={(id) => ts.applyPreset(id)}
    onSelectCustom={() => customOption && onPresetSelect(field.id, customOption)}
    {onModelSelect}
    {onQuantSelect}
  />
</FieldCard>
