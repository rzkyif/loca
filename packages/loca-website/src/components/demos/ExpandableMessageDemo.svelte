<script lang="ts">
  import type { ComponentProps } from "svelte";
  import ExpandableMessageView from "@loca/shared/ui/components/chat/messages/ExpandableMessageView.svelte";
  import { expandableMessageSamples } from "@loca/shared/ui/samples";
  import DemoFrame from "./DemoFrame.svelte";

  // A collapsible system or automated prompt bubble, as it appears in a session.
  type Scenario = keyof typeof expandableMessageSamples;
  let { scenario = "systemPrompt", label }: { scenario?: Scenario; label?: string } = $props();
</script>

<DemoFrame {label} designWidth={620}>
  <div class="mx-auto w-full max-w-[620px]">
    <ExpandableMessageView
      {...expandableMessageSamples[scenario] as ComponentProps<typeof ExpandableMessageView>}
    />
  </div>
</DemoFrame>
