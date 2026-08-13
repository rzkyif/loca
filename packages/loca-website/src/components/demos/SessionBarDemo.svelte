<script lang="ts">
  import type { ComponentProps } from "svelte";
  import SessionBarView from "@loca/shared/ui/components/chat/SessionBarView.svelte";
  import { sessionBarSamples } from "@loca/shared/ui/samples";
  import DemoFrame from "./DemoFrame.svelte";
  import Highlight from "./Highlight.svelte";

  // The bar across the top of a session: title and context-window usage. Renders
  // the shared SessionBarView from its samples, exactly as the app does. Pass
  // `highlightTarget` (a `[data-region=...]` selector) to mark one part, such as
  // the context gauge or the core chip.
  type Scenario = keyof typeof sessionBarSamples;
  let {
    scenario = "default",
    label,
    highlightLabel,
    highlightTarget,
  }: {
    scenario?: Scenario;
    label?: string;
    highlightLabel?: string;
    highlightTarget?: string;
  } = $props();
  const viewProps: ComponentProps<typeof SessionBarView> = $derived(sessionBarSamples[scenario]);
</script>

<DemoFrame {label} highlighted={!!highlightTarget} designWidth={620}>
  <div class="mx-auto w-full max-w-[620px]">
    {#if highlightTarget}
      <Highlight label={highlightLabel} target={highlightTarget}>
        <SessionBarView {...viewProps} />
      </Highlight>
    {:else}
      <SessionBarView {...viewProps} />
    {/if}
  </div>
</DemoFrame>
