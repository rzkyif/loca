<script lang="ts">
  import { settingsState } from "../../../state";
  import { buildPreview, getCommandSettingIds } from "$lib/llm/command";
  import type { CommandType } from "$lib/llm/command";
  import type { SettingField } from "@loca/shared";
  import FieldCard from "./FieldCard.svelte";
  import CommandPreviewFieldView from "@loca/shared/ui/components/settings/CommandPreviewFieldView.svelte";

  let { field } = $props<{
    field: SettingField;
  }>();

  const previewPromise = $derived.by(() => {
    const type = field.commandType as CommandType | undefined;
    if (!type) return Promise.resolve("");

    const values: Record<string, any> = {};
    for (const id of getCommandSettingIds(type)) {
      values[id] = settingsState.currentSettings[id];
    }

    return buildPreview(type, values);
  });
</script>

{#if field.commandType}
  <FieldCard {field}>
    {#await previewPromise}
      <CommandPreviewFieldView preview="Loading..." />
    {:then preview}
      <CommandPreviewFieldView {preview} />
    {/await}
  </FieldCard>
{/if}
