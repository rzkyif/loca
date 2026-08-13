<script lang="ts">
  import ActionSheet, {
    type ActionSheetItem,
  } from "@loca/shared/ui/components/primitives/ActionSheet.svelte";
  import { actionSheetHost } from "$lib/menu/action-sheet-host.svelte";

  // Thin client wrapper of the shared ActionSheet primitive, driven by the
  // action-sheet host store. Maps the platform ContextMenuItem[] (id/label/
  // enabled, separators) to the shared item shape: separators are dropped (a
  // sheet separates with spacing, not rules), a disabled item is dimmed, and a
  // destructive look is inferred from the id/label so deletes read as red.
  const items = $derived<ActionSheetItem[]>(
    actionSheetHost.items
      .filter(
        (i): i is { id: string; label: string; enabled?: boolean; checked?: boolean } =>
          !("separator" in i),
      )
      .map((i) => ({
        label: i.label,
        disabled: i.enabled === false,
        destructive:
          /delete|remove|unpair|forget/i.test(i.id) || /delete|remove|unpair|forget/i.test(i.label),
        onSelect: () => actionSheetHost.select(i.id),
      })),
  );
</script>

<ActionSheet
  open={actionSheetHost.open}
  onclose={() => actionSheetHost.select(null)}
  title={actionSheetHost.title}
  {items}
/>
