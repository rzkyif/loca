<script lang="ts">
  import type { ComponentProps } from "svelte";
  import RelevantToolsView from "@tomat/shared/ui/components/chat/messages/RelevantToolsView.svelte";
  import { relevantToolsSamples } from "@tomat/shared/ui/samples";
  import DemoFrame from "./DemoFrame.svelte";

  // The tools Tomat offered the agent for a message, ranked by relevance.
  let { label }: { label?: string } = $props();
</script>

<DemoFrame {label} designWidth={620}>
  <div class="mx-auto w-full max-w-[620px]">
    <RelevantToolsView
      {...relevantToolsSamples.twoPhase as ComponentProps<typeof RelevantToolsView>}
    />
  </div>
</DemoFrame>
