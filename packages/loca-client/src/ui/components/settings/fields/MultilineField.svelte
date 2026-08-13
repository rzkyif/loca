<script lang="ts">
  import type { SettingField } from "@loca/shared";
  import { evalCondition } from "@loca/shared";
  import { settingsState } from "../../../state";
  import FieldCard from "./FieldCard.svelte";
  import Textarea from "@loca/shared/ui/components/primitives/Textarea.svelte";

  let { field, error, onChange, onReset } = $props<{
    field: SettingField;
    error: string | null;
    onChange: (key: string, value: any) => void;
    onReset: (fieldId: string) => void;
  }>();

  const editable = $derived(evalCondition(field.editableWhen, settingsState.currentSettings));
  const currentValue = $derived(settingsState.currentSettings[field.id]);
  const hasError = $derived(!!error);
</script>

<FieldCard {field} {error} {onReset}>
  <Textarea
    value={currentValue ?? ""}
    autoResize="scroll"
    placeholder={field.placeholder || ""}
    disabled={!editable}
    error={hasError}
    mono={!!field.mono}
    ariaLabel={field.name}
    oninput={(v) => onChange(field.id, v)}
  />
</FieldCard>
