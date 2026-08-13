<script lang="ts">
  import SnippetAutocompleteView, {
    type TriggerOption,
  } from "@loca/shared/ui/components/chat/SnippetAutocompleteView.svelte";

  let {
    options,
    selectedIndex,
    anchor,
    onSelect,
  }: {
    options: TriggerOption[];
    selectedIndex: number;
    anchor: { top: number; left: number };
    onSelect: (option: TriggerOption) => void;
  } = $props();
</script>

<SnippetAutocompleteView {options} {selectedIndex} {anchor} {onSelect} />
