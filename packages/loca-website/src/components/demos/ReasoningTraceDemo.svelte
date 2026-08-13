<script lang="ts">
  import type { ComponentProps } from "svelte";
  import UserMessageView from "@loca/shared/ui/components/chat/messages/UserMessageView.svelte";
  import AgentMessageView from "@loca/shared/ui/components/chat/messages/AgentMessageView.svelte";
  import {
    AGENT_REASONING,
    agentMessageSamples,
    userMessageSamples,
  } from "@loca/shared/ui/samples";
  import { bubbleGap, useUiContext } from "@loca/shared/ui/context";
  import DemoFrame from "./DemoFrame.svelte";
  import Highlight from "./Highlight.svelte";

  const ui = useUiContext();

  // The Thinking trace where it appears in the app: a small reasoning bubble in
  // the message stack, not a control floating alone. The reasoning bubble is
  // highlighted; hover the frame to see it sit in the conversation. `scenario`
  // picks the reasoning state.
  type Scenario = "done" | "streaming";
  let { scenario = "done", label }: { scenario?: Scenario; label?: string } = $props();
  const reasoning = $derived(
    (scenario === "streaming"
      ? agentMessageSamples.reasoningStreaming
      : agentMessageSamples.reasoningDone) as ComponentProps<typeof AgentMessageView>,
  );
</script>

<DemoFrame {label} designWidth={620}>
  <div class="mx-auto flex w-full max-w-[620px] flex-col" style:gap={bubbleGap(ui)}>
    <UserMessageView {...userMessageSamples.default as ComponentProps<typeof UserMessageView>} />
    <Highlight label="Thinking" fit>
      <AgentMessageView {...reasoning}>
        {#snippet body()}<span class="whitespace-pre-wrap break-words">{AGENT_REASONING}</span
          >{/snippet}
      </AgentMessageView>
    </Highlight>
  </div>
</DemoFrame>
