<script lang="ts">
  import type { ComponentProps } from "svelte";
  import RelevantMemoriesView from "@tomat/shared/ui/components/chat/messages/RelevantMemoriesView.svelte";
  import { relevantMemoriesSamples } from "@tomat/shared/ui/samples";
  import DemoFrame from "./DemoFrame.svelte";

  // The memories Tomat surfaced as relevant to a message.
  type Scenario = keyof typeof relevantMemoriesSamples;
  let { scenario = "found", label }: { scenario?: Scenario; label?: string } = $props();
</script>

<DemoFrame {label} designWidth={620}>
  <div class="mx-auto w-full max-w-[620px]">
    <RelevantMemoriesView
      {...relevantMemoriesSamples[scenario] as ComponentProps<typeof RelevantMemoriesView>}
    />
  </div>
</DemoFrame>
