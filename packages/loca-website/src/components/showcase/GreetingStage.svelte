<script lang="ts">
  import { onMount, tick } from "svelte";
  import gsap from "gsap";
  import UserInputView from "@loca/shared/ui/components/chat/UserInputView.svelte";
  import MessageEnter from "@loca/shared/ui/components/chat/MessageEnter.svelte";
  import ExpandableMessageView from "@loca/shared/ui/components/chat/messages/ExpandableMessageView.svelte";
  import AgentMessageView from "@loca/shared/ui/components/chat/messages/AgentMessageView.svelte";
  import { bubbleGap, useUiContext } from "@loca/shared/ui/context";
  import Cursor from "./Cursor.svelte";
  import { awaitStableHeight, Demo, type Timeline } from "../../lib/showcase";

  const ui = useUiContext();

  let {
    register,
    reportHeight,
  }: {
    register: (h: { timeline: Timeline; reset: () => void }) => void;
    reportHeight: (h: number) => void;
  } = $props();

  // A greeting fires on its own when the app opens: the automated prompt that ran
  // appears (collapsed, like any core-authored message), then the agent's reply
  // streams in. No user typing.
  const GREETING = "Good morning! Hype me up for the day and give me one small goal to start with.";
  const ANSWER =
    "Morning! Today is a clean slate. Your one small goal: pick the single thing that would make today feel like a win, and start there. You've got this.";

  const DECODE_TOKENS_PER_SEC = 25;
  const ANSWER_SECONDS = ANSWER.length / 4 / DECODE_TOKENS_PER_SEC;

  // The automated prompt renders collapsed by default (like the client's
  // AutomatedPrompt), so the cursor expands it via the shared expansion registry
  // after the reply finishes.
  const GREETING_ID = "greeting-automated";

  let agentText = $state("");
  let showGreeting = $state(false);
  let showAnswer = $state(false);
  let measuring = $state(false);

  let stageEl: HTMLElement | undefined = $state();
  let cursorRef: HTMLElement | undefined = $state();
  let columnEl: HTMLElement | undefined = $state();

  function reset(): void {
    agentText = "";
    showGreeting = false;
    showAnswer = false;
    ui.expansionSet(GREETING_ID, false);
  }

  async function measureContentHeight(): Promise<void> {
    measuring = true;
    showGreeting = true;
    showAnswer = true;
    agentText = ANSWER;
    // Measure with the prompt expanded (its final state after the cursor opens
    // it) so the frame is tall enough for it.
    ui.expansionSet(GREETING_ID, true);
    await tick();
    if (columnEl) reportHeight(await awaitStableHeight(columnEl));
    reset();
    measuring = false;
    await tick();
  }

  onMount(() => {
    let cancelled = false;
    let timeline: Timeline | undefined;
    void (async () => {
      if (!stageEl || !cursorRef) return;
      const demo = new Demo(cursorRef, stageEl);
      demo.placeFrac(0.5, 0.65);
      await measureContentHeight();
      if (cancelled) return;

      const tl = gsap.timeline({ paused: true });
      timeline = tl;

      demo.hold(tl, 0.4);
      tl.add(() => (showGreeting = true));
      tl.add(() => (showAnswer = true));
      demo.type(tl, (v) => (agentText = v), ANSWER, {
        duration: ANSWER_SECONDS,
      });
      demo.hold(tl, 0.7);
      // The cursor opens the collapsed automated-prompt bubble to reveal it.
      demo.move(tl, "[data-greeting]", { duration: 0.7 });
      demo.hover(tl, "[data-greeting]", true);
      demo.click(tl, "[data-greeting]", () => ui.expansionSet(GREETING_ID, true));
      demo.hover(tl, "[data-greeting]", false);
      register({
        timeline: tl,
        reset: () => {
          reset();
          tl.pause(0);
          demo.blur();
          demo.placeFrac(0.5, 0.65);
        },
      });
    })();
    return () => {
      cancelled = true;
      timeline?.kill();
    };
  });
</script>

<div
  bind:this={stageEl}
  class="relative w-full h-full overflow-hidden flex items-center justify-center"
>
  <div class="shrink-0">
    <div bind:this={columnEl} class="w-[620px] flex flex-col" style:gap={bubbleGap(ui)}>
      {#if showGreeting}
        <MessageEnter enabled={!measuring}>
          <div data-greeting>
            <ExpandableMessageView
              id={GREETING_ID}
              title="Automated Prompt"
              text={GREETING}
              applyColorOverride
            />
          </div>
        </MessageEnter>
      {/if}
      {#if showAnswer}
        <MessageEnter enabled={!measuring}>
          <AgentMessageView kind="content" bgClass="bubble-agent">
            {#snippet body()}
              <span class="whitespace-pre-wrap break-words">{agentText}</span>
            {/snippet}
          </AgentMessageView>
        </MessageEnter>
      {/if}
      <UserInputView placeholder="Enter your instructions..." />
    </div>
  </div>
  <Cursor bind:ref={cursorRef} />
</div>
