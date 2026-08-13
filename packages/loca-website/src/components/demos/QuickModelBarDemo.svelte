<script lang="ts">
  import type { ComponentProps } from "svelte";
  import UserInputView from "@loca/shared/ui/components/chat/UserInputView.svelte";
  import QuickModelBarView from "@loca/shared/ui/components/chat/userinput/QuickModelBarView.svelte";
  import { quickModelBarSamples, userInputSamples } from "@loca/shared/ui/samples";
  import DemoFrame from "./DemoFrame.svelte";
  import Highlight from "./Highlight.svelte";

  // The quick controls (model preset, Thinking, Creativity) shown where they
  // actually live: below the textarea inside the composer, via the same
  // `belowContent` slot the client fills. Highlight marks the bar; hover the
  // frame to see the composer without the overlay.
  let { label }: { label?: string } = $props();
  const input = userInputSamples.withText as ComponentProps<typeof UserInputView>;
  const bar = quickModelBarSamples.local as ComponentProps<typeof QuickModelBarView>;
</script>

<DemoFrame {label} highlighted designWidth={620}>
  <div class="mx-auto w-full max-w-[620px]">
    <UserInputView {...input}>
      {#snippet belowContent()}
        <Highlight label="Quick controls">
          <QuickModelBarView {...bar} />
        </Highlight>
      {/snippet}
    </UserInputView>
  </div>
</DemoFrame>
