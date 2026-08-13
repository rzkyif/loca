<script lang="ts">
  // A small static chat snippet for the manual, built from the SAME message
  // bubbles the app renders (no provider mounted, so DEFAULT_UI_CONTEXT supplies
  // the app's default settings). For the animated version see the homepage
  // showcase.
  import UserMessageView from "@tomat/shared/ui/components/chat/messages/UserMessageView.svelte";
  import AgentMessageView from "@tomat/shared/ui/components/chat/messages/AgentMessageView.svelte";
  import { bubbleGap, useUiContext } from "@tomat/shared/ui/context";
  import DemoFrame from "./DemoFrame.svelte";

  const ui = useUiContext();

  let { label }: { label?: string } = $props();
</script>

<DemoFrame {label} designWidth={620}>
  <div class="mx-auto flex w-full max-w-[620px] flex-col" style:gap={bubbleGap(ui)}>
    <UserMessageView text="How do I install Tomat?" />
    <AgentMessageView kind="content" bgClass="bubble-agent">
      {#snippet body()}
        <span class="break-words"
          >Run the one-line installer from the landing page, then launch the app. Want the per-OS
          steps?</span
        >
      {/snippet}
    </AgentMessageView>
  </div>
</DemoFrame>
