<script lang="ts">
  import type { ComponentProps } from "svelte";
  import SettingsSidebarView from "@loca/shared/ui/components/settings/SettingsSidebarView.svelte";
  import { settingsSidebarSamples } from "@loca/shared/ui/samples";
  import DemoFrame from "./DemoFrame.svelte";
  import SettingsDemoFooter from "./SettingsDemoFooter.svelte";

  // The settings sidebar: every group in the order the app shows them, with the
  // pinned Downloads + version footer the app always renders at its bottom. Sized
  // to the app's sidebar width so it reads like the real column.
  type Scenario = keyof typeof settingsSidebarSamples;
  let { scenario = "expanded", label }: { scenario?: Scenario; label?: string } = $props();
</script>

<DemoFrame {label}>
  <div class="mx-auto h-[26rem] w-60">
    <SettingsSidebarView
      {...settingsSidebarSamples[scenario] as ComponentProps<typeof SettingsSidebarView>}
    >
      {#snippet footer(collapsed)}
        <SettingsDemoFooter {collapsed} />
      {/snippet}
    </SettingsSidebarView>
  </div>
</DemoFrame>
