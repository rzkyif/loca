<script lang="ts">
  import ObjectCardView from "@loca/shared/ui/components/objects/ObjectCardView.svelte";
  import type { Badge } from "@loca/shared/ui/components/objects/object-types.ts";
  import { type MenuRow, showObjectActionMenu } from "$lib/objects/menu";

  // Thin client wrapper of ObjectCardView: owns the per-card action menu
  // (`menuRows` + the native menu trigger), and feeds the rest straight through.
  let {
    label,
    description,
    meta,
    badges = [],
    menuRows = [],
    onOpen,
  }: {
    label: string;
    description?: string;
    meta?: string;
    badges?: Badge[];
    menuRows?: MenuRow[];
    onOpen?: () => void;
  } = $props();
</script>

<ObjectCardView
  {label}
  {description}
  {meta}
  {badges}
  hasMenu={menuRows.length > 0}
  {onOpen}
  onMenu={() => void showObjectActionMenu(menuRows)}
/>
