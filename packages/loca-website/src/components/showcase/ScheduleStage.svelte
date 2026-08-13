<script lang="ts">
  import { onMount } from "svelte";
  import gsap from "gsap";
  import { getDefaultSettings, SETTINGS_SCHEMA } from "@loca/shared/domain/settings/engine";
  import type { SettingField } from "@loca/shared/domain/settings/types";
  import { scheduledPromptDetailSamples } from "@loca/shared/ui/samples";
  import SettingsShellView from "@loca/shared/ui/components/settings/SettingsShellView.svelte";
  import SettingsContentView from "@loca/shared/ui/components/settings/SettingsContentView.svelte";
  import ScheduledPromptDetailView from "@loca/shared/ui/components/settings/ScheduledPromptDetailView.svelte";
  import ObjectManagerView from "@loca/shared/ui/components/objects/ObjectManagerView.svelte";
  import ObjectCardView from "@loca/shared/ui/components/objects/ObjectCardView.svelte";
  import ObjectDetailHeaderView from "@loca/shared/ui/components/objects/ObjectDetailHeaderView.svelte";
  import ObjectDetailScrollView from "@loca/shared/ui/components/objects/ObjectDetailScrollView.svelte";
  import SettingsDemoFooter from "../demos/SettingsDemoFooter.svelte";
  import Cursor from "./Cursor.svelte";
  import { Demo, type Timeline } from "../../lib/showcase";

  let { register }: { register: (h: { timeline: Timeline; reset: () => void }) => void } = $props();

  const groups = SETTINGS_SCHEMA.filter((g) => !g.hidden).map((g) => ({
    id: g.id,
    name: g.name,
    icon: g.icon,
    iconInactive: g.iconInactive ?? g.icon,
  }));
  const values = getDefaultSettings();

  type Shell = { reset: () => void };
  let shell = $state<Shell>();
  let selectedGroupId = $state("scheduledPrompts");

  // The managed objects: one saved scheduled prompt, opened to its detail editor
  // (the real settings page opens an object-management shell, not a bare form).
  type Prompt = { id: string; title: string };
  const prompts: Prompt[] = [{ id: "sp-morning", title: "Morning briefing" }];

  type Schedule = typeof scheduledPromptDetailSamples.hasRun;
  let schedule = $state<Schedule>(structuredClone(scheduledPromptDetailSamples.hasRun));

  let stageEl: HTMLElement | undefined = $state();
  let cursorRef: HTMLElement | undefined = $state();

  function toggleWeekday(day: number): void {
    const set = new Set(schedule.weekdays ?? []);
    if (set.has(day)) set.delete(day);
    else set.add(day);
    schedule = { ...schedule, weekdays: [...set].sort((a, b) => a - b) };
  }

  function reset(): void {
    schedule = structuredClone(scheduledPromptDetailSamples.hasRun);
    shell?.reset();
    selectedGroupId = "scheduledPrompts";
  }

  onMount(() => {
    if (!stageEl || !cursorRef) return;
    const demo = new Demo(cursorRef, stageEl);
    demo.placeFrac(0.5, 0.5);

    const tl = gsap.timeline({ paused: true });
    // The weekday chips: an inactive one carries aria-pressed="false" (the
    // enabled / run-missed toggles in this sample are pressed, so the first match
    // is the leading off weekday, Sunday).
    const offDay = 'button[aria-pressed="false"]';

    demo.hold(tl, 0.6);
    // Add the two weekend days to the recurring schedule.
    demo.move(tl, offDay, { duration: 0.9 });
    demo.hover(tl, offDay, true);
    demo.click(tl, offDay, () => toggleWeekday(0));
    demo.hover(tl, offDay, false);
    demo.hold(tl, 0.6);
    demo.move(tl, offDay, { duration: 0.7 });
    demo.hover(tl, offDay, true);
    demo.click(tl, offDay, () => toggleWeekday(6));
    demo.hover(tl, offDay, false);
    register({
      timeline: tl,
      reset: () => {
        reset();
        tl.pause(0);
        demo.blur();
        demo.placeFrac(0.5, 0.5);
      },
    });
    return () => tl.kill();
  });
</script>

{#snippet sidebarFooter(collapsed: boolean)}
  <SettingsDemoFooter {collapsed} />
{/snippet}

<!-- The object_management field renders the real master/detail shell, opened to
     the saved prompt's detail editor. -->
{#snippet omField(_f: SettingField)}
  <ObjectManagerView
    items={prompts}
    idOf={(p) => p.id}
    selectedItem={prompts[0]}
    live={prompts[0]}
    searchPlaceholder="Search scheduled prompts"
    hasMenu
  >
    {#snippet card(item, open)}
      <ObjectCardView
        label={item.title}
        description="Summarize my unread messages and today's calendar."
        meta="Next run in 3 hours"
        hasMenu
        onOpen={open}
      />
    {/snippet}
    {#snippet detailPane(item)}
      <ObjectDetailHeaderView title={item.title} subtitle="Weekly · weekdays at 09:00" />
      <ObjectDetailScrollView>
        <ScheduledPromptDetailView {...schedule} onToggleWeekday={(day) => toggleWeekday(day)} />
      </ObjectDetailScrollView>
    {/snippet}
    {#snippet empty()}
      <div class="text-default-500 text-sm p-4">No scheduled prompts yet.</div>
    {/snippet}
  </ObjectManagerView>
{/snippet}

<div
  bind:this={stageEl}
  class="relative w-full h-full overflow-hidden flex items-center justify-center"
>
  <div class="shrink-0">
    <SettingsShellView
      bind:this={shell}
      {groups}
      bind:selectedGroupId
      sizeClass="w-[620px] h-[960px]"
      {sidebarFooter}
    >
      {#snippet groupContent(gid)}
        <SettingsContentView groupId={gid} {values} field={omField} />
      {/snippet}
    </SettingsShellView>
  </div>
  <Cursor bind:ref={cursorRef} />
</div>
