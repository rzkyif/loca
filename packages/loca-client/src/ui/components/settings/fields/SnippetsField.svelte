<script lang="ts">
  import { onMount } from "svelte";
  import type { McpPrompt } from "@loca/shared";
  import { confirmState, mcpState, snippetsState } from "$stores";
  import {
    recommendedSymbol,
    SNIPPET_PLACEMENT_OPTIONS,
    type Snippet,
    snippetTrigger,
  } from "$lib/snippets/snippets";
  import type { ParsedQuery } from "$lib/objects/query";
  import { type MenuRow, showFilterSortMenu, showObjectActionMenu } from "$lib/objects/menu";
  import ObjectManager from "$components/ui/ObjectManager.svelte";
  import ObjectCard from "$components/ui/ObjectCard.svelte";
  import ObjectDetailHeader from "@loca/shared/ui/components/objects/ObjectDetailHeaderView.svelte";
  import ObjectDetailScroll from "@loca/shared/ui/components/objects/ObjectDetailScrollView.svelte";
  import SnippetDetail from "./SnippetDetail.svelte";
  import McpPromptDetail from "./McpPromptDetail.svelte";

  // The manager lists two kinds of "/"-style trigger side by side: user-authored
  // snippets (client files) and the prompts an MCP server exposes (managed
  // read-only, only their enablement is editable here). A discriminated wrapper
  // keeps the two shapes apart through ObjectManager's generic plumbing.
  type Item = { kind: "snippet"; snippet: Snippet } | { kind: "prompt"; prompt: McpPrompt };

  const snippetId = (s: Snippet) => `snippet:${s.id}`;
  const promptId = (p: McpPrompt) => `prompt:${p.serverId}:${p.name}`;
  const idOf = (i: Item) => (i.kind === "snippet" ? snippetId(i.snippet) : promptId(i.prompt));

  let query = $state("");
  let selectedItem = $state<Item | null>(null);
  let reloadKey = $state(0);

  onMount(() => {
    void snippetsState.load();
    // The prompts are core-owned; make sure the mirror is current when the
    // manager opens (it also live-updates on later mcp.snapshot frames).
    void mcpState.refresh();
  });

  function placementLabel(p: Snippet["placement"]): string {
    return SNIPPET_PLACEMENT_OPTIONS.find((o) => o.value === p)?.label ?? p;
  }

  function makeUniqueName(): string {
    const existing = new Set(snippetsState.snippets.map((s) => s.name.toLowerCase()));
    let i = 1;
    while (existing.has(`snippet${i}`)) i++;
    return `snippet${i}`;
  }

  function getById(id: string): Item | undefined {
    if (id.startsWith("snippet:")) {
      const s = snippetsState.snippets.find((x) => snippetId(x) === id);
      return s ? { kind: "snippet", snippet: s } : undefined;
    }
    const p = mcpState.prompts.find((x) => promptId(x) === id);
    return p ? { kind: "prompt", prompt: p } : undefined;
  }

  function load({ query: q }: { offset: number; limit: number; query: ParsedQuery }) {
    const text = q.text.toLowerCase();
    const snippets: Item[] = snippetsState.snippets
      .filter(
        (s) =>
          !text ||
          s.name.toLowerCase().includes(text) ||
          snippetTrigger(s).toLowerCase().includes(text),
      )
      .map((snippet) => ({ kind: "snippet", snippet }));
    const prompts: Item[] = mcpState.prompts
      .filter(
        (p) =>
          !text || p.name.toLowerCase().includes(text) || p.serverName.toLowerCase().includes(text),
      )
      .map((prompt) => ({ kind: "prompt", prompt }));

    let list = [...snippets, ...prompts];
    const trig = (i: Item) =>
      i.kind === "snippet" ? snippetTrigger(i.snippet) : `/${i.prompt.name}`;
    const nameOf = (i: Item) => (i.kind === "snippet" ? i.snippet.name : i.prompt.name);
    if (q.sort === "name") list = [...list].sort((a, b) => nameOf(a).localeCompare(nameOf(b)));
    else if (q.sort === "trigger") list = [...list].sort((a, b) => trig(a).localeCompare(trig(b)));
    return Promise.resolve({ items: list, done: true });
  }

  function cardMenuRows(s: Snippet): MenuRow[] {
    return [
      {
        id: "delete",
        label: "Delete",
        onSelect: () =>
          confirmState.request({
            title: "Delete snippet",
            message: `Delete snippet "${s.name || snippetTrigger(s)}"? This cannot be undone.`,
            destructive: true,
            confirmLabel: "Delete",
            onConfirm: async () => {
              await snippetsState.delete(s.id);
              reloadKey++;
            },
          }),
      },
    ];
  }

  async function newSnippet() {
    const created = await snippetsState.create({
      name: makeUniqueName(),
      symbol: recommendedSymbol("append-system"),
      symbolPinned: false,
      placement: "append-system",
      text: "",
    });
    reloadKey++;
    selectedItem = { kind: "snippet", snippet: created };
  }
</script>

<ObjectManager
  {load}
  {idOf}
  {getById}
  searchPlaceholder="Search snippets and prompts"
  subscribe={(onChange) =>
    // MCP prompts live-update: repaint the list on every mcp.snapshot frame.
    mcpState.subscribeSnapshot(onChange)}
  bind:query
  bind:selectedItem
  bind:reloadKey
  hasFilterSort
  onFilterSort={() =>
    showFilterSortMenu({
      filters: [],
      sorts: [
        { value: "name", label: "Name" },
        { value: "trigger", label: "Trigger" },
      ],
      query,
      onQueryChange: (q) => (query = q),
    })}
  hasMenu
  onMenu={() =>
    showObjectActionMenu([
      { id: "new", label: "New Snippet", onSelect: () => void newSnippet() },
      {
        id: "rescan",
        label: "Rescan Snippets",
        onSelect: async () => {
          // Re-read the snippets directory so files copied in by hand show
          // up without a restart.
          await snippetsState.load();
          reloadKey++;
        },
      },
    ])}
>
  {#snippet card(item, open)}
    {#if item.kind === "snippet"}
      <ObjectCard
        label={item.snippet.name || "Untitled snippet"}
        description={item.snippet.text || undefined}
        meta={snippetTrigger(item.snippet)}
        badges={[{ label: placementLabel(item.snippet.placement) }]}
        menuRows={cardMenuRows(item.snippet)}
        onOpen={open}
      />
    {:else}
      <ObjectCard
        label={`/${item.prompt.name}`}
        description={item.prompt.description || undefined}
        meta={`${item.prompt.serverName} · MCP prompt`}
        badges={item.prompt.enabled
          ? [{ label: "Enabled", accent: "green" }]
          : [{ label: "Disabled" }]}
        onOpen={open}
      />
    {/if}
  {/snippet}
  {#snippet detail(item)}
    {#if item.kind === "snippet"}
      <ObjectDetailHeader
        title={item.snippet.name || "Untitled snippet"}
        subtitle={snippetTrigger(item.snippet)}
      />
      <ObjectDetailScroll>
        <SnippetDetail item={item.snippet} reload={() => reloadKey++} />
      </ObjectDetailScroll>
    {:else}
      <ObjectDetailHeader title={`/${item.prompt.name}`} subtitle={item.prompt.serverName} />
      <ObjectDetailScroll>
        <McpPromptDetail prompt={item.prompt} reload={() => reloadKey++} />
      </ObjectDetailScroll>
    {/if}
  {/snippet}
  {#snippet empty()}
    <div class="flex flex-col items-center justify-center gap-1 py-12 text-center">
      {#if query.trim()}
        <div class="text-base text-default-700">No matching snippets or prompts</div>
      {:else}
        <div class="text-base text-default-700">No snippets yet</div>
        <div class="text-sm text-default-500">
          Use the menu to create a snippet, then trigger it with #, @, or / in chat. MCP server
          prompts show up here too.
        </div>
      {/if}
    </div>
  {/snippet}
</ObjectManager>
