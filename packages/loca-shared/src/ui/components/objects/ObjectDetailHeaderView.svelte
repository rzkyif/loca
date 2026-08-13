<script lang="ts">
  import Button from "../primitives/Button.svelte";
  import ObjectBadgeView from "./ObjectBadgeView.svelte";
  import type { Badge } from "./object-types.ts";

  // The standardized detail-view header: title + badges, an optional subtitle,
  // and a right-aligned action bar. The back button lives in ObjectManager's
  // left gutter (so this header and the form below share the same left edge),
  // not here.
  interface HeaderAction {
    label: string;
    icon?: string;
    variant?: "default" | "danger";
    disabled?: boolean;
    loading?: boolean;
    title?: string;
    onSelect: () => void;
  }

  let {
    title,
    subtitle,
    badges = [],
    actions = [],
  }: {
    title: string;
    subtitle?: string;
    badges?: Badge[];
    actions?: HeaderAction[];
  } = $props();
</script>

<div class="flex items-start gap-2 shrink-0">
  <div class="flex flex-col min-w-0 flex-1 gap-0.5">
    <!-- min-h-7 ~ the gutter back button's height, so it centers on this row. -->
    <div class="flex items-center gap-2 min-w-0 min-h-7">
      <span class="text-base font-semibold text-default-800 truncate">{title}</span>
      {#each badges as badge (badge.label)}
        <ObjectBadgeView
          label={badge.label}
          icon={badge.icon}
          accent={badge.accent}
          title={badge.title}
        />
      {/each}
    </div>
    {#if subtitle}
      <span class="text-sm text-default-600 truncate">{subtitle}</span>
    {/if}
  </div>
  {#if actions.length > 0}
    <div class="flex items-center gap-2 shrink-0 min-h-7">
      {#each actions as action (action.label)}
        <Button
          variant={action.variant === "danger" ? "destructive" : "secondary"}
          icon={action.icon}
          disabled={action.disabled}
          loading={action.loading}
          title={action.title}
          onclick={action.onSelect}
        >
          {action.label}
        </Button>
      {/each}
    </div>
  {/if}
</div>
