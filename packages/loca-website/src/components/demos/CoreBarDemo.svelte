<script lang="ts">
  import type { ComponentProps } from "svelte";
  import CoreBarView from "@loca/shared/ui/components/chat/CoreBarView.svelte";
  import { coreBarSamples } from "@loca/shared/ui/samples";
  import DemoFrame from "./DemoFrame.svelte";
  import Highlight from "./Highlight.svelte";

  // The bar pinned at the bottom of the app: which core you're connected to, its
  // status, and a quick switcher. Renders the shared CoreBarView from its
  // samples, exactly as the app does. Pass `highlightTarget` (a
  // `[data-region=...]` selector) to mark one part, such as the status.
  type Scenario = keyof typeof coreBarSamples;
  let {
    scenario = "idle",
    label,
    highlightLabel,
    highlightTarget,
  }: {
    scenario?: Scenario;
    label?: string;
    highlightLabel?: string;
    highlightTarget?: string;
  } = $props();
  const viewProps: ComponentProps<typeof CoreBarView> = $derived(coreBarSamples[scenario]);
</script>

<DemoFrame {label} highlighted={!!highlightTarget} designWidth={620}>
  <div class="mx-auto w-full max-w-[620px]">
    {#if highlightTarget}
      <Highlight label={highlightLabel} target={highlightTarget}>
        <CoreBarView {...viewProps} />
      </Highlight>
    {:else}
      <CoreBarView {...viewProps} />
    {/if}
  </div>
</DemoFrame>
