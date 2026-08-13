<script lang="ts">
  import { onMount } from "svelte";
  import type { ScheduledPrompt } from "@loca/shared";
  import { confirmState, scheduledPromptsState } from "$stores";
  import { describeSchedule, nextRunText } from "$stores/scheduled-prompts.svelte";
  import type { Badge } from "@loca/shared/ui/components/objects/object-types";
  import type { ParsedQuery } from "$lib/objects/query";
  import { type MenuRow, showFilterSortMenu, showObjectActionMenu } from "$lib/objects/menu";
  import { getLogger } from "$lib/util/log";
  import ObjectManager from "$components/ui/ObjectManager.svelte";
  import ObjectCard from "$components/ui/ObjectCard.svelte";
  import ObjectDetailHeader from "@loca/shared/ui/components/objects/ObjectDetailHeaderView.svelte";
  import ObjectDetailScroll from "@loca/shared/ui/components/objects/ObjectDetailScrollView.svelte";
  import ScheduledPromptDetail from "./ScheduledPromptDetail.svelte";

  const log = getLogger("scheduled-prompts");

  let query = $state("");
  let selectedItem = $state<ScheduledPrompt | null>(null);
  let reloadKey = $state(0);

  onMount(
    () =>
      void scheduledPromptsState.load().catch((e) => log.warn("scheduled prompt load failed:", e)),
  );

  function makeUniqueTitle(): string {
    const existing = new Set(scheduledPromptsState.prompts.map((p) => p.title.toLowerCase()));
    let i = 1;
    while (existing.has(`new scheduled prompt ${i}`)) i++;
    return `New scheduled prompt ${i}`;
  }

  function load({ query: q }: { offset: number; limit: number; query: ParsedQuery }) {
    const text = q.text.toLowerCase();
    let list = scheduledPromptsState.prompts.filter(
      (p) =>
        (!text ||
          p.title.toLowerCase().includes(text) ||
          p.instruction.toLowerCase().includes(text)) &&
        (q.filters.size === 0 || (q.filters.has("disabled") && !p.enabled)),
    );
    if (q.sort === "title") list = [...list].sort((a, b) => a.title.localeCompare(b.title));
    else if (q.sort === "next") {
      list = [...list].sort(
        (a, b) =>
          (a.nextRunAtMs ?? Number.MAX_SAFE_INTEGER) - (b.nextRunAtMs ?? Number.MAX_SAFE_INTEGER),
      );
    }
    return Promise.resolve({ items: list, done: true });
  }

  // A single enabled/disabled state chip so every card carries its status at a
  // glance (green Enabled / neutral Disabled), matching the tools and memories
  // lists; the exact next-run time still rides in the card meta line.
  function cardBadges(p: ScheduledPrompt): Badge[] {
    return [p.enabled ? { label: "Enabled", accent: "green" } : { label: "Disabled" }];
  }

  function cardMenuRows(p: ScheduledPrompt): MenuRow[] {
    return [
      {
        id: "run",
        label: "Run Now",
        onSelect: async () => {
          await scheduledPromptsState.run(p.id);
          reloadKey++;
        },
      },
      {
        id: "delete",
        label: "Delete",
        onSelect: () =>
          confirmState.request({
            title: "Delete scheduled prompt",
            message: `Delete scheduled prompt "${p.title}"? This cannot be undone.`,
            destructive: true,
            confirmLabel: "Delete",
            onConfirm: async () => {
              await scheduledPromptsState.delete(p.id);
              reloadKey++;
            },
          }),
      },
    ];
  }

  async function newScheduledPrompt() {
    const created = await scheduledPromptsState.create({
      title: makeUniqueTitle(),
      instruction: "Say hello.",
      schedule: { kind: "weekly", weekdays: [1], hour: 9, minute: 0 },
      runMissed: false,
    });
    reloadKey++;
    selectedItem = created;
  }
</script>

<ObjectManager
  {load}
  idOf={(p) => p.id}
  getById={(id) => scheduledPromptsState.prompts.find((p) => p.id === id)}
  searchPlaceholder="Search scheduled prompts"
  bind:query
  bind:selectedItem
  bind:reloadKey
  hasFilterSort
  onFilterSort={() =>
    showFilterSortMenu({
      filters: [{ label: "Status", options: [{ token: "disabled", label: "Disabled" }] }],
      sorts: [
        { value: "title", label: "Title" },
        { value: "next", label: "Next run" },
      ],
      query,
      onQueryChange: (q) => (query = q),
    })}
  hasMenu
  onMenu={() =>
    showObjectActionMenu([
      {
        id: "new",
        label: "New Scheduled Prompt",
        onSelect: () => void newScheduledPrompt(),
      },
    ])}
>
  {#snippet card(item, open)}
    <ObjectCard
      label={item.title}
      description={describeSchedule(item.schedule)}
      meta={nextRunText(item)}
      badges={cardBadges(item)}
      menuRows={cardMenuRows(item)}
      onOpen={open}
    />
  {/snippet}
  {#snippet detail(item)}
    <ObjectDetailHeader title={item.title} subtitle={describeSchedule(item.schedule)} />
    <ObjectDetailScroll>
      <ScheduledPromptDetail {item} reload={() => reloadKey++} />
    </ObjectDetailScroll>
  {/snippet}
  {#snippet empty()}
    <div class="flex flex-col items-center justify-center gap-1 py-12 text-center">
      {#if query.trim()}
        <div class="text-base text-default-700">No matching scheduled prompts</div>
      {:else}
        <div class="text-base text-default-700">No scheduled prompts yet</div>
        <div class="text-sm text-default-500">
          Use the menu to create one, or ask the agent to schedule a prompt in chat.
        </div>
      {/if}
    </div>
  {/snippet}
</ObjectManager>
