<script lang="ts">
  import { settingsState } from "../../../state";
  import type { SettingField, PresetOption } from "@loca/shared";
  import FieldCard from "./FieldCard.svelte";
  import OptionCard from "@loca/shared/ui/components/primitives/OptionCard.svelte";

  let { field, onPresetSelect } = $props<{
    field: SettingField;
    onPresetSelect: (fieldId: string, option: PresetOption) => void;
  }>();
</script>

{#snippet presetButton(opt: PresetOption)}
  {@const selected = settingsState.currentSettings[field.id] === opt.id}
  {#snippet badgesSnippet()}
    {#each opt.badges ?? [] as badge}
      <span class="inline-flex items-center gap-1">
        <i class="{badge.icon} text-sm"></i>
        <span>{badge.label}</span>
      </span>
    {/each}
  {/snippet}
  <OptionCard
    {selected}
    icon={opt.icon}
    title={opt.title ?? opt.label}
    description={opt.description}
    badges={opt.badges?.length ? badgesSnippet : undefined}
    onclick={() => onPresetSelect(field.id, opt)}
  />
{/snippet}

<FieldCard {field}>
  {#if field.presetConfig}
    <div class="flex flex-col gap-2">
      {#each field.presetConfig.options as opt}
        {@render presetButton(opt)}
      {/each}
    </div>
    {#if field.presetConfig.secondaryOptions}
      <div class="flex flex-col gap-2 pt-2">
        {#each field.presetConfig.secondaryOptions as opt}
          {@render presetButton(opt)}
        {/each}
      </div>
    {/if}
  {/if}
</FieldCard>
