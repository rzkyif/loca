<script lang="ts">
  import { onMount } from "svelte";
  import { type MemoryKind, type MemoryMeta, USER_MEMORY_PROVIDER } from "@loca/shared";
  import { confirmState, memoriesState } from "$stores";
  import type { Badge } from "@loca/shared/ui/components/objects/object-types";
  import { memoryTrigger } from "$stores/memories.svelte";
  import type { ParsedQuery } from "$lib/objects/query";
  import { type MenuRow, showFilterSortMenu, showObjectActionMenu } from "$lib/objects/menu";
  import { getLogger } from "$lib/util/log";
  import ObjectManager from "$components/ui/ObjectManager.svelte";
  import ObjectCard from "$components/ui/ObjectCard.svelte";
  import ObjectDetailHeader from "@loca/shared/ui/components/objects/ObjectDetailHeaderView.svelte";
  import ObjectDetailScroll from "@loca/shared/ui/components/objects/ObjectDetailScrollView.svelte";
  import MemoryDetail from "./MemoryDetail.svelte";

  const log = getLogger("memories");

  let query = $state("");
  let selectedItem = $state<MemoryMeta | null>(null);
  let reloadKey = $state(0);

  onMount(() => void memoriesState.load().catch((e) => log.warn("memory load failed:", e)));

  function makeUniqueTitle(label: string): string {
    const existing = new Set(memoriesState.memories.map((d) => d.title.toLowerCase()));
    let i = 1;
    while (existing.has(`new ${label} ${i}`)) i++;
    return `New ${label} ${i}`;
  }

  function load({ query: q }: { offset: number; limit: number; query: ParsedQuery }) {
    const text = q.text.toLowerCase();
    let list = memoriesState.memories.filter(
      (d) =>
        (!text ||
          d.title.toLowerCase().includes(text) ||
          memoryTrigger(d).toLowerCase().includes(text)) &&
        (q.filters.size === 0 ||
          (q.filters.has("knowledge") && d.kind === "knowledge") ||
          (q.filters.has("skill") && d.kind === "skill") ||
          (q.filters.has("disabled") && !d.enabled)),
    );
    if (q.sort === "title") list = [...list].sort((a, b) => a.title.localeCompare(b.title));
    else if (q.sort === "updated") list = [...list].sort((a, b) => b.updatedAtMs - a.updatedAtMs);
    return Promise.resolve({ items: list, done: true });
  }

  // Kind (always) + an Extension marker for non-user memories, then an explicit
  // enabled/disabled state chip so every card carries its status at a glance
  // (green Enabled / neutral Disabled), matching the tools and snippets lists.
  function cardBadges(d: MemoryMeta): Badge[] {
    const badges: Badge[] = [{ label: d.kind === "skill" ? "Skill" : "Knowledge" }];
    if (d.provider !== USER_MEMORY_PROVIDER) badges.push({ label: "Extension" });
    badges.push(d.enabled ? { label: "Enabled", accent: "green" } : { label: "Disabled" });
    return badges;
  }

  function cardMenuRows(d: MemoryMeta): MenuRow[] {
    const rows: MenuRow[] = [
      {
        id: d.enabled ? "disable" : "enable",
        label: d.enabled ? "Disable" : "Enable",
        onSelect: async () => {
          await memoriesState.setEnabled(d.id, !d.enabled);
          reloadKey++;
        },
      },
    ];
    // Only user-authored memories can be deleted; extension-provided ones are
    // removed by uninstalling their extension.
    if (d.provider === USER_MEMORY_PROVIDER) {
      rows.push({
        id: "delete",
        label: "Delete",
        onSelect: () =>
          confirmState.request({
            title: "Delete memory",
            message: `Delete "${d.title}"? This cannot be undone.`,
            destructive: true,
            confirmLabel: "Delete",
            onConfirm: async () => {
              await memoriesState.delete(d.id);
              reloadKey++;
            },
          }),
      });
    }
    return rows;
  }

  async function newMemory(kind: MemoryKind) {
    const label = kind === "skill" ? "skill" : "knowledge";
    const content =
      kind === "skill" ? "---\ndescription: \nsuggested-tools: []\n---\n\n# Steps\n\n1. \n" : "";
    const created = await memoriesState.create(kind, makeUniqueTitle(label), content);
    reloadKey++;
    selectedItem = created;
  }
</script>

<ObjectManager
  {load}
  idOf={(d) => d.id}
  getById={(id) => memoriesState.memories.find((d) => d.id === id)}
  searchPlaceholder="Search memories"
  bind:query
  bind:selectedItem
  bind:reloadKey
  hasFilterSort
  onFilterSort={() =>
    showFilterSortMenu({
      filters: [
        {
          label: "Kind",
          options: [
            { token: "knowledge", label: "Knowledge" },
            { token: "skill", label: "Skill" },
            { token: "disabled", label: "Disabled" },
          ],
        },
      ],
      sorts: [
        { value: "title", label: "Title" },
        { value: "updated", label: "Last updated" },
      ],
      query,
      onQueryChange: (q) => (query = q),
    })}
  hasMenu
  onMenu={() =>
    showObjectActionMenu([
      { id: "new-knowledge", label: "New Knowledge", onSelect: () => void newMemory("knowledge") },
      { id: "new-skill", label: "New Skill", onSelect: () => void newMemory("skill") },
      {
        id: "rescan",
        label: "Rescan Memories",
        onSelect: async () => {
          // Re-read the memories directory on the core so files copied in
          // by hand show up without a restart.
          await memoriesState.rescan();
          reloadKey++;
        },
      },
    ])}
>
  {#snippet card(item, open)}
    <ObjectCard
      label={item.title}
      description={item.summary || undefined}
      meta={memoryTrigger(item)}
      badges={cardBadges(item)}
      menuRows={cardMenuRows(item)}
      onOpen={open}
    />
  {/snippet}
  {#snippet detail(item)}
    <ObjectDetailHeader title={item.title} subtitle={memoryTrigger(item)} />
    <ObjectDetailScroll>
      <MemoryDetail {item} reload={() => reloadKey++} />
    </ObjectDetailScroll>
  {/snippet}
  {#snippet empty()}
    <div class="flex flex-col items-center justify-center gap-1 py-12 text-center">
      {#if query.trim()}
        <div class="text-base text-default-700">No matching memories</div>
      {:else}
        <div class="text-base text-default-700">No memories yet</div>
        <div class="text-sm text-default-500">
          Use the menu to add knowledge or a skill, then reference it with @name in chat.
        </div>
      {/if}
    </div>
  {/snippet}
</ObjectManager>
