<script lang="ts">
  import { onMount, tick } from "svelte";
  import gsap from "gsap";
  import UserInputView from "@loca/shared/ui/components/chat/UserInputView.svelte";
  import MessageEnter from "@loca/shared/ui/components/chat/MessageEnter.svelte";
  import UserMessageView from "@loca/shared/ui/components/chat/messages/UserMessageView.svelte";
  import RelevantMemoriesView from "@loca/shared/ui/components/chat/messages/RelevantMemoriesView.svelte";
  import AgentMessageView from "@loca/shared/ui/components/chat/messages/AgentMessageView.svelte";
  import { bubbleGap, useUiContext } from "@loca/shared/ui/context";
  import Cursor from "./Cursor.svelte";
  import { Demo, awaitStableHeight, type SizedStageProps, type Timeline } from "../../lib/showcase";

  const ui = useUiContext();

  let { register, reportHeight }: SizedStageProps = $props();

  const PROMPT = "Make me a grocery list for this week's dinners.";
  const ANSWER =
    "Here's your list:\n\nProduce: spinach, bell peppers, cherry tomatoes.\nPantry: chickpeas, brown rice, olive oil.\nDairy: feta, plain yogurt.\n\nKept it vegetarian and peanut-free, grouped by aisle like you like.";

  const DECODE_TOKENS_PER_SEC = 25;
  const ANSWER_SECONDS = ANSWER.length / 4 / DECODE_TOKENS_PER_SEC;

  // Two recalled memories: one knowledge fact and one skill, each with its
  // relevance score, exactly as the "Found N relevant memories" bubble lists them.
  // The list applies both (the skill's format, the fact's dietary constraint)
  // with no tool call, so the demo shows recall without needing an external action.
  const MEMORIES = [
    {
      memoryId: "s1",
      title: "Skill: how you like lists",
      score: 0.89,
      summary: "Group by aisle, keep it short.",
    },
    {
      memoryId: "k1",
      title: "Your diet",
      score: 0.82,
      summary: "Vegetarian, and no peanuts.",
    },
  ];

  let inputValue = $state("");
  let userText = $state("");
  let agentText = $state("");
  let showUser = $state(false);
  let showMemories = $state(false);
  let showAnswer = $state(false);
  let measuring = $state(false);

  let stageEl: HTMLElement | undefined = $state();
  let cursorRef: HTMLElement | undefined = $state();
  let columnEl: HTMLElement | undefined = $state();

  function reset(): void {
    inputValue = "";
    userText = "";
    agentText = "";
    showUser = false;
    showMemories = false;
    showAnswer = false;
  }

  async function measureContentHeight(): Promise<void> {
    measuring = true;
    showUser = true;
    showMemories = true;
    showAnswer = true;
    userText = PROMPT;
    agentText = ANSWER;
    // Wait for the expanded "relevant memories" bubble (and fonts) to settle to a
    // stable height before measuring, so the frame never under-measures and clips
    // the column top and bottom.
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
      demo.placeFrac(0.5, 0.6);
      await measureContentHeight();
      if (cancelled) return;

      const tl = gsap.timeline({ paused: true });
      timeline = tl;
      const input = 'textarea[aria-label="Message input"]';
      const send = 'button[title="Send"]';

      demo.move(tl, input, { duration: 0.8 });
      demo.click(tl, input);
      demo.type(tl, (v) => (inputValue = v), PROMPT, { duration: 1.8 });
      demo.hold(tl, 0.3);
      demo.move(tl, send, { duration: 0.7 });
      demo.hover(tl, send, true);
      demo.click(tl, send, () => {
        userText = PROMPT;
        inputValue = "";
        showUser = true;
      });
      demo.hover(tl, send, false);
      tl.add(() => (showMemories = true));
      tl.add(() => (showAnswer = true));
      demo.type(tl, (v) => (agentText = v), ANSWER, {
        duration: ANSWER_SECONDS,
      });
      register({
        timeline: tl,
        reset: () => {
          reset();
          tl.pause(0);
          demo.blur();
          demo.placeFrac(0.5, 0.6);
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
      {#if showUser}
        <MessageEnter enabled={!measuring}>
          <UserMessageView text={userText} />
        </MessageEnter>
      {/if}
      {#if showMemories}
        <MessageEnter enabled={!measuring}>
          <RelevantMemoriesView memories={MEMORIES} defaultExpanded />
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
      <UserInputView bind:value={inputValue} placeholder="Enter your instructions..." />
    </div>
  </div>
  <Cursor bind:ref={cursorRef} />
</div>
