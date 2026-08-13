<script lang="ts">
  import type { PresetOption, SettingField, SettingOption } from "@loca/shared";
  import { evalCondition } from "@loca/shared";
  import { displayToStored, storedToDisplay } from "$lib/appearance/color";
  import type { Monitor } from "$lib/util/types";
  import { colorPickerState, settingsState } from "../../state";
  import { cores } from "$lib/core";
  import SettingsFieldView from "@loca/shared/ui/components/settings/SettingsFieldView.svelte";
  // Delegated (client-only) field types, injected into the shared field view's
  // `complexField` snippet. The shared view owns every presentational type
  // (boolean / select / text / number / color / slider) so the client and the
  // website render those identically (single-source rule, AGENTS.md).
  import CommandPreviewField from "./fields/CommandPreviewField.svelte";
  import MultilineField from "./fields/MultilineField.svelte";
  import PresetField from "./fields/PresetField.svelte";
  import ModelPresetField from "./fields/ModelPresetField.svelte";
  import SttPresetField from "./fields/SttPresetField.svelte";
  import TtsPresetField from "./fields/TtsPresetField.svelte";
  import ServicesField from "./fields/ServicesField.svelte";
  import ShortcutField from "./fields/ShortcutField.svelte";
  import SnippetsField from "./fields/SnippetsField.svelte";
  import MemoriesField from "./fields/MemoriesField.svelte";
  import ScheduledPromptsField from "./fields/ScheduledPromptsField.svelte";
  import ExtensionsField from "./fields/ExtensionsField.svelte";
  import ToolsField from "./fields/ToolsField.svelte";
  import McpField from "./fields/McpField.svelte";
  import CoresField from "./fields/CoresField.svelte";
  import StorageField from "./fields/StorageField.svelte";

  let {
    field,
    monitors,
    fonts,
    error,
    horizontal = false,
    onChange,
    onReset,
    onPresetSelect,
  } = $props<{
    field: SettingField;
    monitors: Monitor[];
    fonts: string[];
    error: string | null;
    horizontal?: boolean;
    onChange: (key: string, value: any) => void;
    onReset: (fieldId: string) => void;
    onPresetSelect: (fieldId: string, option: PresetOption) => void;
  }>();

  const visible = $derived(evalCondition(field.visibleWhen, settingsState.currentSettings));
  const editable = $derived(evalCondition(field.editableWhen, settingsState.currentSettings));
  const value = $derived(settingsState.currentSettings[field.id]);
  const showReset = $derived(!!onReset && editable && value !== field.defaultValue);

  // Theme-aware color preview (the swatch + value render the dark-inverted color
  // when the app is in dark mode), mirrored from the picker's own logic.
  const themeMql =
    typeof window !== "undefined" ? window.matchMedia("(prefers-color-scheme: dark)") : null;
  let systemDark = $state(themeMql?.matches ?? false);
  $effect(() => {
    if (!themeMql) return;
    const handler = (e: MediaQueryListEvent) => (systemDark = e.matches);
    themeMql.addEventListener("change", handler);
    return () => themeMql.removeEventListener("change", handler);
  });
  const isDark = $derived.by(() => {
    const theme = settingsState.currentSettings["appearance.theme"];
    if (theme === "dark") return true;
    if (theme === "light") return false;
    return systemDark;
  });

  function openColorPicker(anchor: HTMLElement): void {
    const lockedLightness = field.type === "color" ? field.lockedLightness : undefined;
    colorPickerState.open({
      anchor: anchor as HTMLButtonElement,
      initialColor: storedToDisplay(String(value ?? ""), isDark, lockedLightness),
      onApply: (displayed: string) =>
        onChange(field.id, displayToStored(displayed, isDark, lockedLightness)),
      lockLightness: lockedLightness != null,
    });
  }

  // Voices of the currently-selected TTS model (optionsSource "tts_voices"),
  // fetched from the core and refreshed whenever the model changes.
  let ttsVoices = $state<SettingOption[]>([]);
  $effect(() => {
    if (field.type !== "select" || field.optionsSource !== "tts_voices") return;
    void settingsState.currentSettings["tts.modelType"];
    void settingsState.currentSettings["tts.modelPath"];
    void loadTtsVoices();
  });
  async function loadTtsVoices(): Promise<void> {
    if (!cores().currentEntry()) return;
    try {
      const voices = await cores().api().tts.voices();
      ttsVoices = voices.map((v) => ({ value: v.id, label: v.label }));
    } catch {
      ttsVoices = [];
    }
  }

  const selectOptions = $derived.by<SettingOption[] | undefined>(() => {
    if (field.type !== "select") return undefined;
    if (field.optionsSource === "monitors") {
      return [
        { value: "primary", label: "Primary Monitor" },
        ...monitors.map((m: Monitor) => ({ value: m.id.toString(), label: m.name })),
      ];
    }
    if (field.optionsSource === "fonts") {
      return [
        { value: "default", label: "Default" },
        ...fonts.map((f: string) => ({ value: f, label: f })),
      ];
    }
    if (field.optionsSource === "tts_voices") return ttsVoices;
    return field.options ?? [];
  });

  const placeholder = $derived.by<string | undefined>(() => {
    if (
      field.type === "password" &&
      settingsState.isSecretConfigured(field.id) &&
      !settingsState.currentSettings[field.id]
    ) {
      return "•••••••••• saved";
    }
    return "placeholder" in field ? field.placeholder : undefined;
  });
</script>

{#if visible}
  <SettingsFieldView
    {field}
    {value}
    {error}
    {editable}
    {horizontal}
    {showReset}
    {isDark}
    {selectOptions}
    {placeholder}
    onChange={(v) => onChange(field.id, v)}
    onReset={() => onReset(field.id)}
    onOpenColorPicker={openColorPicker}
  >
    {#snippet complexField(f)}
      {#if f.type === "model_preset"}
        <ModelPresetField field={f} {onPresetSelect} />
      {:else if f.type === "stt_preset"}
        <SttPresetField field={f} {onPresetSelect} />
      {:else if f.type === "tts_preset"}
        <TtsPresetField field={f} {onPresetSelect} />
      {:else if f.type === "preset"}
        <PresetField field={f} {onPresetSelect} />
      {:else if f.type === "command_preview"}
        <CommandPreviewField field={f} />
      {:else if f.type === "multiline"}
        <MultilineField field={f} {error} {onChange} {onReset} />
      {:else if f.type === "services"}
        <ServicesField field={f} scope={f.scope} {horizontal} />
      {:else if f.type === "storage"}
        <StorageField field={f} scope={f.scope} />
      {:else if f.type === "shortcut"}
        <ShortcutField field={f} {error} {horizontal} {onChange} {onReset} />
      {:else if f.type === "object_management"}
        {#if f.objectType === "snippets"}
          <SnippetsField />
        {:else if f.objectType === "memories"}
          <MemoriesField />
        {:else if f.objectType === "scheduled_prompts"}
          <ScheduledPromptsField />
        {:else if f.objectType === "tools"}
          <ToolsField {horizontal} />
        {:else if f.objectType === "extensions"}
          <ExtensionsField {horizontal} />
        {:else if f.objectType === "mcp"}
          <McpField {horizontal} />
        {:else if f.objectType === "cores"}
          <CoresField />
        {/if}
      {/if}
    {/snippet}
  </SettingsFieldView>
{/if}
