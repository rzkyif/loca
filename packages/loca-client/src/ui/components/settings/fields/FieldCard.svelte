<script lang="ts">
  import type { Snippet } from "svelte";
  import type { SettingField } from "@loca/shared";
  import { evalCondition } from "@loca/shared";
  import { settingsState } from "../../../state";
  import FormField from "@loca/shared/ui/components/primitives/FormField.svelte";

  let {
    field,
    error = null,
    horizontal = false,
    onReset,
    children,
  } = $props<{
    field: SettingField;
    error?: string | null;
    horizontal?: boolean;
    onReset?: (fieldId: string) => void;
    children: Snippet;
  }>();

  const editable = $derived(evalCondition(field.editableWhen, settingsState.currentSettings));
  const currentValue = $derived(settingsState.currentSettings[field.id]);
  const isModified = $derived(currentValue !== field.defaultValue);

  const showReset = $derived(!!onReset && editable && isModified);
</script>

<FormField
  fieldId={field.id}
  label={field.name}
  description={field.description}
  descriptionTier={field.descriptionTier}
  {horizontal}
  {error}
  onReset={onReset ? () => onReset(field.id) : undefined}
  {showReset}
>
  {@render children()}
</FormField>
