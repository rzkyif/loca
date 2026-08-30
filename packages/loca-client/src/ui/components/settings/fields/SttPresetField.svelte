<script lang="ts">
  import type { PresetOption, SettingField, SttCatalogModel } from "@loca/shared";
  import { primaryFileSpec, STT_PRIMARY_ROLE } from "@loca/shared";
  import { settingsState } from "../../../state";
  import { sttModelsState } from "$stores/speech-models.svelte";
  import { cores } from "$lib/core";
  import FieldCard from "./FieldCard.svelte";
  import SttPresetFieldView from "@loca/shared/ui/components/settings/SttPresetFieldView.svelte";

  // The Speech-to-Text catalog picker. Like ModelPresetField, each card binds
  // to a catalog model and selecting one calls the select API; unlike it there
  // is no fit engine (whisper models fit everywhere), so no buckets or recheck.
  // This shell owns the catalog store and feeds the pure SttPresetFieldView.
  let { field, onPresetSelect } = $props<{
    field: SettingField;
    onPresetSelect: (fieldId: string, option: PresetOption) => void;
  }>();

  const ss = sttModelsState;

  // The dropdown value when the current stt.modelPath matches no catalog model.
  const MANUAL = "__manual__";

  // Load the catalog once the core is paired (and not already loaded). It backs
  // both the card badges and the Custom card's model + quant dropdowns.
  $effect(() => {
    if (cores().currentEntry() && !ss.catalog && !ss.loading) void ss.load();
  });

  const cardOptions = $derived((field.presetConfig?.options ?? []) as PresetOption[]);
  const customOption = $derived(
    (field.presetConfig?.secondaryOptions ?? [])[0] as PresetOption | undefined,
  );

  // Whisper spans 31 MB to 3.1 GB, so GB-only formatting would render "0.0 GB".
  function size(bytes: number): string {
    return bytes >= 1e9 ? `${(bytes / 1e9).toFixed(1)} GB` : `${Math.round(bytes / 1e6)} MB`;
  }

  function language(english: boolean): string {
    return english ? "English only" : "Multilingual";
  }

  function cardPreset(id: string) {
    return ss.catalog?.presets.find((p) => p.id === id) ?? null;
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
              { icon: "i-material-symbols-language", text: language(preset.english) },
            ]
          : null,
        placeholder: preset ? undefined : ss.loading ? "Loading catalog…" : undefined,
      };
    }),
  );

  // --- manual model + quantization dropdowns ------------------------------

  // Model dropdown: every catalog model plus "Manual Configuration". Size is a
  // range across the model's quants (the exact size is picked in the quant
  // dropdown).
  const modelOptions = $derived([
    ...(ss.catalog?.models ?? []).map((m: SttCatalogModel) => {
      const sizes = m.quants.map((q) => q.fileSizeBytes);
      const min = Math.min(...sizes);
      const max = Math.max(...sizes);
      const range = min === max ? size(min) : `${size(min)}-${size(max)}`;
      return {
        value: m.id,
        label: `${m.name} · ${language(m.english)} · ${range}`,
      };
    }),
    { value: MANUAL, label: "Manual Configuration" },
  ]);

  // The model dropdown reflects whichever catalog model the current
  // stt.modelPath points at; a hand-edited path not in the catalog falls back
  // to "Manual Configuration".
  const selectedModelView = $derived(
    (ss.catalog?.models ?? []).find((m: SttCatalogModel) =>
      m.quants.some(
        (q) =>
          primaryFileSpec(q, STT_PRIMARY_ROLE[m.family]) ===
          settingsState.currentSettings["stt.modelPath"],
      ),
    ) ?? null,
  );

  // Explicit "Manual Configuration" choice. Picking it doesn't change the model
  // path (a catalog model may still be configured), so it can't be derived from
  // the path alone; track it, and clear it whenever the path actually changes
  // (a card applied, or another model picked).
  let manualSelected = $state(false);
  let lastModelPath: string | undefined = undefined;
  $effect(() => {
    const path = settingsState.currentSettings["stt.modelPath"] as string | undefined;
    if (path !== lastModelPath) {
      lastModelPath = path;
      manualSelected = false;
    }
  });

  const selectedModel = $derived(manualSelected ? MANUAL : (selectedModelView?.id ?? MANUAL));

  const quantOptions = $derived(
    (selectedModelView?.quants ?? []).map((q) => ({
      value: selectedModelView
        ? primaryFileSpec(q, STT_PRIMARY_ROLE[selectedModelView.family])
        : "",
      label: `${q.quant} · ${size(q.fileSizeBytes)}`,
    })),
  );
  const selectedQuant = $derived(settingsState.currentSettings["stt.modelPath"] as string);

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
      // Switch to Custom without touching the whisper-server fields, and hide
      // the quant dropdown until a concrete model is chosen again.
      manualSelected = true;
      if (customOption) onPresetSelect(field.id, customOption);
      return;
    }
    manualSelected = false;
    void ss.applyModel(value);
  }

  function onQuantSelect(modelSpec: string): void {
    void ss.applyQuant(modelSpec);
  }
</script>

<FieldCard {field}>
  <SttPresetFieldView
    error={ss.error}
    {presets}
    {custom}
    onSelectPreset={(id) => ss.applyPreset(id)}
    onSelectCustom={() => customOption && onPresetSelect(field.id, customOption)}
    {onModelSelect}
    {onQuantSelect}
  />
</FieldCard>
