<script lang="ts">
  import type { ComponentProps } from "svelte";
  import UserInputView from "@loca/shared/ui/components/chat/UserInputView.svelte";
  import { userInputSamples } from "@loca/shared/ui/samples";
  import DemoFrame from "./DemoFrame.svelte";
  import Highlight from "./Highlight.svelte";

  // The input bar at the bottom of a session. The `voiceReady` scenario shows
  // the Voice Input mic button. Pass `highlightTarget` (a `[data-region=...]`
  // selector) to mark one part, such as the mic.
  type Scenario = keyof typeof userInputSamples;
  let {
    scenario = "withText",
    label,
    highlightLabel,
    highlightTarget,
  }: {
    scenario?: Scenario;
    label?: string;
    highlightLabel?: string;
    highlightTarget?: string;
  } = $props();
  const viewProps: ComponentProps<typeof UserInputView> = $derived(userInputSamples[scenario]);
</script>

<DemoFrame {label} highlighted={!!highlightTarget} designWidth={620}>
  <div class="mx-auto w-full max-w-[620px]">
    {#if highlightTarget}
      <Highlight label={highlightLabel} target={highlightTarget}>
        <UserInputView {...viewProps} />
      </Highlight>
    {:else}
      <UserInputView {...viewProps} />
    {/if}
  </div>
</DemoFrame>
