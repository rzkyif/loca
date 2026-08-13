<script lang="ts">
  import {
    type CreativityLevel,
    creativityDropdownOptions,
    creativitySelection,
    creativityTemperature,
    CUSTOM_VALUE,
    findField,
    type ModelPresetField,
    type PresetBucket,
    type PresetOption,
    type QuickOption,
    type ThinkingLevel,
    thinkingDropdownOptions,
    thinkingLevelUpdates,
    thinkingSelection,
  } from "@loca/shared";
  import { settingsState } from "$stores/settings.svelte";
  import { modelRecommendState } from "$stores/model-recommend.svelte";
  import { useSettingsForm } from "$composables/use-settings-form.svelte";
  import { cores } from "$lib/core";
  import { shortModelName, shortQuantName } from "$lib/util/format";
  import QuickModelBarView, {
    type QuickSelect,
  } from "@loca/shared/ui/components/chat/userinput/QuickModelBarView.svelte";

  // The chat input's quick model controls: a condensed, non-technical view over
  // the granular llm.* settings (which stay editable in full Settings). Every
  // change flows through the same useSettingsForm / modelRecommendState paths the
  // Settings UI uses, so it triggers identical effects. Controls stay enabled
  // while downloads are pending so a model change can be reverted without
  // committing to a download.

  const rs = modelRecommendState;
  const form = useSettingsForm();

  // llm.preset's bucket + custom options live in the shared schema; the labels
  // there (Smallest / Balanced / Smartest) are already short enough for the bar.
  // llm.preset is a model_preset field; narrow so presetConfig is in view.
  const presetField = findField("llm.preset") as ModelPresetField | undefined;
  const bucketOptions = (presetField?.presetConfig.options ?? []) as PresetOption[];
  const customOption = (presetField?.presetConfig.secondaryOptions ?? [])[0] as
    PresetOption | undefined;

  const provider = $derived(
    settingsState.currentSettings["llm.provider"] === "external" ? "external" : "local",
  );
  const preset = $derived(settingsState.currentSettings["llm.preset"]);
  const contextSize = $derived(Number(settingsState.currentSettings["llm.contextSize"]) || 4096);

  // Mirror the Custom card's dropdown wiring from ModelPresetField.svelte.
  const MANUAL = "__manual__";
  // Bucket entries are folded into the model dropdown so the user can leave
  // Custom mode without opening full Settings.
  const PRESET_PREFIX = "__preset__:";

  $effect(() => {
    if (provider === "local" && cores().currentEntry()) {
      if (!rs.recommendations && !rs.loading) void rs.load();
      if (!rs.catalog) void rs.loadCatalog();
    }
  });

  const selectedModelView = $derived(
    (rs.catalog ?? []).find((m) =>
      m.quants.some((q) => q.modelSpec === settingsState.currentSettings["llm.modelPath"]),
    ) ?? null,
  );

  // Explicit "Manual" choice can't be derived from the path (a catalog model may
  // still be configured), so track it and clear it whenever the path changes.
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
  const selectedQuant = $derived(settingsState.currentSettings["llm.modelPath"] as string);

  // --- option lists ---------------------------------------------------------

  const presetOptions = $derived([
    ...bucketOptions.map((o) => ({ value: o.id, label: o.title ?? o.label })),
    ...(customOption
      ? [
          {
            value: customOption.id,
            label: customOption.title ?? customOption.label,
          },
        ]
      : []),
  ]);

  const modelOptions = $derived([
    ...bucketOptions.map((o) => ({
      value: PRESET_PREFIX + o.id,
      label: o.title ?? o.label,
    })),
    ...(rs.catalog ?? []).map((m) => ({
      value: m.id,
      label: shortModelName(m),
      disabled: !m.fits,
    })),
    { value: MANUAL, label: "Manual" },
  ]);

  const quantOptions = $derived(
    (selectedModelView?.quants ?? []).map((q) => ({
      value: q.modelSpec,
      label: shortQuantName(q),
      disabled: !q.fits,
    })),
  );

  // --- behaviour controls ---------------------------------------------------

  // A control whose underlying setting matches no level shows the raw value as a
  // disabled, non-selectable option at the top of its dropdown.
  function withCustom(
    selection: { value: string; customLabel?: string },
    options: QuickOption[],
  ): QuickOption[] {
    if (selection.value !== CUSTOM_VALUE) return options;
    const label = selection.customLabel ?? "";
    return [{ value: CUSTOM_VALUE, label, display: label, disabled: true }, ...options];
  }

  const thinking = $derived(thinkingSelection(settingsState.currentSettings, provider));
  const thinkingDropdown = $derived(
    withCustom(thinking, thinkingDropdownOptions(provider, contextSize)),
  );

  const creativity = $derived(creativitySelection(settingsState.currentSettings));
  const creativityDropdown = $derived(withCustom(creativity, creativityDropdownOptions()));

  // --- handlers -------------------------------------------------------------

  function onPresetChange(value: string): void {
    if (customOption && value === customOption.id) {
      // "Custom" enters custom -> manual: switch the preset without changing the
      // model, and show the model dropdown with "Manual" selected so the catalog
      // + Manual options are in reach.
      manualSelected = true;
      void form.handlePresetSelect("llm.preset", customOption);
      return;
    }
    void rs.applyBucket(value as PresetBucket);
  }

  function onModelSelect(value: string): void {
    if (value.startsWith(PRESET_PREFIX)) {
      manualSelected = false;
      void rs.applyBucket(value.slice(PRESET_PREFIX.length) as PresetBucket);
      return;
    }
    if (value === MANUAL) {
      manualSelected = true;
      if (customOption) void form.handlePresetSelect("llm.preset", customOption);
      return;
    }
    manualSelected = false;
    void rs.applyModel(value);
  }

  function onQuantSelect(modelSpec: string): void {
    void rs.applyQuant(modelSpec);
  }

  async function onThinkingChange(level: string): Promise<void> {
    // The custom budget entry is disabled and not selectable.
    if (level === CUSTOM_VALUE) return;
    const updates = thinkingLevelUpdates(level as ThinkingLevel, provider, contextSize);
    // Apply each key through handleChange so validation + dependency re-eval run
    // exactly as they would from the Settings UI.
    for (const [key, value] of Object.entries(updates)) {
      await form.handleChange(key, value);
    }
  }

  function onCreativityChange(level: string): void {
    if (level === CUSTOM_VALUE) return;
    void form.handleChange("llm.temperature", creativityTemperature(level as CreativityLevel));
  }

  // Map the derived selections onto the presentational view's slot descriptors.
  // The left column is hidden for the external provider; in custom mode it
  // becomes the model picker (+ a quantization picker once a catalog model is
  // chosen).
  const modelSlot = $derived<QuickSelect | undefined>(
    provider !== "local"
      ? undefined
      : preset !== "custom"
        ? {
            value: preset as string,
            options: presetOptions,
            onchange: onPresetChange,
            ariaLabel: "Smart preset",
          }
        : {
            value: selectedModel,
            options: modelOptions,
            onchange: onModelSelect,
            ariaLabel: "Model",
          },
  );
  const quantSlot = $derived<QuickSelect | undefined>(
    provider === "local" && preset === "custom" && !manualSelected && selectedModelView
      ? { value: selectedQuant, options: quantOptions, onchange: onQuantSelect }
      : undefined,
  );
</script>

<QuickModelBarView
  model={modelSlot}
  quant={quantSlot}
  thinking={{ value: thinking.value, options: thinkingDropdown, onchange: onThinkingChange }}
  creativity={{
    value: creativity.value,
    options: creativityDropdown,
    onchange: onCreativityChange,
  }}
/>
