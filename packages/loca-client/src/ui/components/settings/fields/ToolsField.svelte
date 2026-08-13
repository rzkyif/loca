<script lang="ts">
  import { onMount } from "svelte";
  import { errMessage, type Tool } from "@loca/shared";
  import { extensionsState, mcpState } from "$stores";
  import type { ParsedQuery } from "$lib/objects/query";
  import { type MenuRow, showFilterSortMenu } from "$lib/objects/menu";
  import ObjectManager from "$components/ui/ObjectManager.svelte";
  import ObjectCard from "$components/ui/ObjectCard.svelte";
  import ObjectDetailHeader from "@loca/shared/ui/components/objects/ObjectDetailHeaderView.svelte";
  import ObjectDetailScroll from "@loca/shared/ui/components/objects/ObjectDetailScrollView.svelte";
  import ToolsFieldView from "@loca/shared/ui/components/settings/ToolsFieldView.svelte";
  import ToolDetail from "./ToolDetail.svelte";

  let { horizontal = false }: { horizontal?: boolean } = $props();

  let query = $state("");
  let selectedItem = $state<Tool | null>(null);
  let reloadKey = $state(0);
  let loadError = $state<string | null>(null);

  onMount(() => void reload());

  async function reload() {
    try {
      await extensionsState.loadAllTools();
      loadError = null;
    } catch (e) {
      loadError = errMessage(e);
    }
    reloadKey++;
  }

  function load({ query: q }: { offset: number; limit: number; query: ParsedQuery }) {
    const text = q.text.toLowerCase();
    let list = extensionsState.allTools.filter(
      (t) =>
        (!text ||
          t.name.toLowerCase().includes(text) ||
          (t.providerName ?? "").toLowerCase().includes(text)) &&
        (q.filters.size === 0 ||
          (q.filters.has("enabled") && t.enabled) ||
          (q.filters.has("disabled") && !t.enabled)),
    );
    if (q.sort === "provider") {
      list = [...list].sort((a, b) => (a.providerName ?? "").localeCompare(b.providerName ?? ""));
    } else list = [...list].sort((a, b) => a.name.localeCompare(b.name));
    return Promise.resolve({ items: list, done: true });
  }

  function cardMenuRows(t: Tool): MenuRow[] {
    return [
      {
        id: t.enabled ? "disable" : "enable",
        label: t.enabled ? "Disable" : "Enable",
        onSelect: async () => {
          if (t.providerKind === "mcp") {
            await mcpState.setToolEnabled(t.extensionId, t.name, !t.enabled);
            await extensionsState.loadAllTools();
          } else if (t.enabled) await extensionsState.disableTool(t.extensionId, t.name);
          else await extensionsState.enableTool(t.extensionId, t.name);
          reloadKey++;
        },
      },
    ];
  }
</script>

<ObjectManager
  {load}
  idOf={(t) => t.id}
  getById={(id) => extensionsState.allTools.find((t) => t.id === id)}
  searchPlaceholder="Search tools"
  bind:query
  bind:selectedItem
  bind:reloadKey
  hasFilterSort
  onFilterSort={() =>
    showFilterSortMenu({
      filters: [
        {
          label: "Status",
          options: [
            { token: "enabled", label: "Enabled" },
            { token: "disabled", label: "Disabled" },
          ],
        },
      ],
      sorts: [
        { value: "name", label: "Name" },
        { value: "provider", label: "Provider" },
      ],
      query,
      onQueryChange: (q) => (query = q),
    })}
>
  {#snippet card(item, open)}
    <ObjectCard
      label={item.name}
      description={item.description || undefined}
      meta={item.providerName}
      badges={item.enabled ? [{ label: "Enabled", accent: "green" }] : [{ label: "Disabled" }]}
      menuRows={cardMenuRows(item)}
      onOpen={open}
    />
  {/snippet}
  {#snippet detail(item)}
    <ObjectDetailHeader title={item.name} subtitle={item.providerName} />
    <ObjectDetailScroll>
      <ToolDetail tool={item} {horizontal} {reload} />
    </ObjectDetailScroll>
  {/snippet}
  {#snippet empty()}
    <ToolsFieldView {loadError} hasQuery={!!query.trim()} />
  {/snippet}
</ObjectManager>
