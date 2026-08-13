<script lang="ts">
  import type { ComponentProps } from "svelte";
  import SettingsShellView from "@loca/shared/ui/components/settings/SettingsShellView.svelte";
  import SettingsContentView from "@loca/shared/ui/components/settings/SettingsContentView.svelte";
  import { SETTINGS_SCHEMA } from "@loca/shared/domain/settings/engine";
  import { SAMPLE_VALUES, settingsShellSamples } from "@loca/shared/ui/samples";
  import DemoFrame from "./DemoFrame.svelte";
  import SettingsDemoFooter from "./SettingsDemoFooter.svelte";

  // The whole settings panel (sidebar + group header + content + footer), composed
  // from the same shared Views the app uses, on fresh-app default values. It
  // renders at the app's real panel WIDTH (APP_W = 620, so every control, font,
  // and gap is 1:1 with the client and nothing squishes horizontally) and a
  // compact height (the app's panel height is responsive, so a shorter one is
  // just the app on a smaller window: the content area scrolls and the footer
  // pins, exactly as in the client). DemoFrame scales the whole panel down only
  // when the column is narrower than 620 (phones), never distorting it. `group`
  // selects which group is open; defaults to the first.
  let { group, label }: { group?: string; label?: string } = $props();
  const base = settingsShellSamples.default as ComponentProps<typeof SettingsShellView>;
  const selectedGroupId = $derived(group ?? base.selectedGroupId);

  // The demo renders statically (no hydration), so SettingsContentView's `$effect`
  // that opens the default-expanded sections never runs and every section would
  // collapse to just its header. Pass the same set explicitly (the component's
  // documented controlled `expanded` prop): the keys the effect would add, one
  // per labeled, not-defaultCollapsed section, so the live app and the demo show
  // the same open sections.
  function expandedFor(gid: string): Set<string> {
    const keys = new Set<string>();
    SETTINGS_SCHEMA.find((g) => g.id === gid)?.sections.forEach((s, i) => {
      if (s.label && !s.defaultCollapsed) keys.add(`${gid}-${i}`);
    });
    return keys;
  }
</script>

<DemoFrame {label} designWidth={620}>
  <SettingsShellView {...base} {selectedGroupId} sizeClass="w-[620px] h-[460px]">
    {#snippet groupContent(gid)}
      <SettingsContentView groupId={gid} values={SAMPLE_VALUES} expanded={expandedFor(gid)} />
    {/snippet}
    {#snippet sidebarFooter(collapsed)}
      <SettingsDemoFooter {collapsed} />
    {/snippet}
  </SettingsShellView>
</DemoFrame>
