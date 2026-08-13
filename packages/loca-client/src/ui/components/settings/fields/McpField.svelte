<script lang="ts">
  import { errMessage, type McpServer } from "@loca/shared";
  import { confirmState, mcpState } from "$stores";
  import { cores } from "$lib/core";
  import type { ParsedQuery } from "$lib/objects/query";
  import { type MenuRow, showObjectActionMenu } from "$lib/objects/menu";
  import type { Accent } from "@loca/shared/ui/components/objects/object-types";
  import ObjectManager from "$components/ui/ObjectManager.svelte";
  import ObjectCard from "$components/ui/ObjectCard.svelte";
  import ObjectDetailHeader from "@loca/shared/ui/components/objects/ObjectDetailHeaderView.svelte";
  import ObjectDetailScroll from "@loca/shared/ui/components/objects/ObjectDetailScrollView.svelte";
  import McpFieldView from "@loca/shared/ui/components/settings/McpFieldView.svelte";
  import McpDetail from "./McpDetail.svelte";

  let { horizontal = false }: { horizontal?: boolean } = $props();

  let query = $state("");
  let selectedItem = $state<McpServer | null>(null);
  let reloadKey = $state(0);

  function statusBadge(s: McpServer): { label: string; accent?: Accent } {
    if (mcpState.busy[s.id]) return { label: "Working", accent: "yellow" };
    switch (s.status) {
      case "connected":
        return { label: "Connected", accent: "green" };
      case "connecting":
        return { label: "Connecting", accent: "yellow" };
      case "error":
        return { label: "Error", accent: "red" };
      default:
        return { label: s.enabled ? "Disconnected" : "Off" };
    }
  }

  // Pull a fresh list first so a reload triggered by an mcp.snapshot frame (a
  // status change, an auto-reconnect, a list_changed) reflects the latest state
  // rather than the pre-refresh cache.
  async function load({ query: q }: { offset: number; limit: number; query: ParsedQuery }) {
    await mcpState.refresh();
    const text = q.text.toLowerCase();
    const list = mcpState.servers.filter((s) => !text || s.name.toLowerCase().includes(text));
    return { items: list, done: true };
  }

  function cardMenuRows(s: McpServer): MenuRow[] {
    const busy = mcpState.busy[s.id] ?? false;
    return [
      {
        id: "reconnect",
        label: "Reconnect",
        disabled: busy,
        onSelect: () => void mcpState.reconnect(s.id),
      },
      {
        id: "delete",
        label: "Delete",
        disabled: busy,
        onSelect: () =>
          confirmState.request({
            title: "Delete MCP server",
            message: `Delete "${s.name}"? Its tools, prompts, and resources will be removed.`,
            destructive: true,
            confirmLabel: "Delete",
            onConfirm: async () => {
              await mcpState.delete(s.id);
              reloadKey++;
            },
          }),
      },
    ];
  }

  async function newServer() {
    try {
      const created = await mcpState.create({
        name: "New server",
        kind: "stdio",
        enabled: false,
      });
      reloadKey++;
      selectedItem = created;
    } catch (e) {
      confirmState.alert({ title: "Couldn't create server", message: errMessage(e) });
    }
  }
</script>

<ObjectManager
  {load}
  idOf={(s) => s.id}
  getById={(id) => mcpState.servers.find((s) => s.id === id)}
  searchPlaceholder="Search MCP servers"
  subscribe={(onChange) =>
    cores().subscribeWs((f) => {
      if (f.kind === "mcp.snapshot") onChange();
    })}
  bind:query
  bind:selectedItem
  bind:reloadKey
  hasMenu
  onMenu={() =>
    showObjectActionMenu([
      { id: "new", label: "New MCP Server", onSelect: () => void newServer() },
    ])}
>
  {#snippet card(item, open)}
    <ObjectCard
      label={item.name}
      description={item.kind === "stdio" ? item.command : item.url}
      meta={`${item.toolCount} tools · ${item.promptCount} prompts · ${item.resourceCount} resources`}
      badges={[statusBadge(item)]}
      menuRows={cardMenuRows(item)}
      onOpen={open}
    />
  {/snippet}
  {#snippet detail(item)}
    <ObjectDetailHeader
      title={item.name}
      subtitle={item.kind === "stdio" ? "Local (stdio)" : "Remote"}
    />
    <ObjectDetailScroll>
      <McpDetail server={item} {horizontal} reload={() => reloadKey++} />
    </ObjectDetailScroll>
  {/snippet}
  {#snippet empty()}
    <McpFieldView loadError={mcpState.loadError} hasQuery={!!query.trim()} />
  {/snippet}
</ObjectManager>
