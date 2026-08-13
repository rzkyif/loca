<script lang="ts">
  import type { ComponentProps } from "svelte";
  import UserMessageView from "@loca/shared/ui/components/chat/messages/UserMessageView.svelte";
  import AgentMessageView from "@loca/shared/ui/components/chat/messages/AgentMessageView.svelte";
  import {
    AGENT_ANSWER,
    AGENT_REASONING,
    agentMessageSamples,
    userMessageSamples,
  } from "@loca/shared/ui/samples";
  import { bubbleGap, useUiContext } from "@loca/shared/ui/context";
  import DemoFrame from "./DemoFrame.svelte";

  const ui = useUiContext();

  // One exchange: your bubble, the agent's collapsed Thinking bubble, then its
  // reply. The same shared message Views the app renders, fed from samples.
  let { label }: { label?: string } = $props();
</script>

<DemoFrame {label} designWidth={620}>
  <div class="mx-auto flex w-full max-w-[620px] flex-col" style:gap={bubbleGap(ui)}>
    <UserMessageView {...userMessageSamples.default as ComponentProps<typeof UserMessageView>} />
    <AgentMessageView
      {...agentMessageSamples.reasoningDone as ComponentProps<typeof AgentMessageView>}
    >
      {#snippet body()}<span class="whitespace-pre-wrap break-words">{AGENT_REASONING}</span
        >{/snippet}
    </AgentMessageView>
    <AgentMessageView {...agentMessageSamples.content as ComponentProps<typeof AgentMessageView>}>
      {#snippet body()}<span class="whitespace-pre-wrap break-words">{AGENT_ANSWER}</span>{/snippet}
    </AgentMessageView>
  </div>
</DemoFrame>
