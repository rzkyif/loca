<script lang="ts">
  import { onMount, tick } from "svelte";
  import gsap from "gsap";
  import SessionBarView from "@loca/shared/ui/components/chat/SessionBarView.svelte";
  import UserInputView from "@loca/shared/ui/components/chat/UserInputView.svelte";
  import MessageEnter from "@loca/shared/ui/components/chat/MessageEnter.svelte";
  import UserMessageView from "@loca/shared/ui/components/chat/messages/UserMessageView.svelte";
  import AgentMessageView from "@loca/shared/ui/components/chat/messages/AgentMessageView.svelte";
  import Markdown, { preloadMarkdown } from "@loca/shared/ui/components/primitives/Markdown.svelte";
  import { bubbleGap, useUiContext } from "@loca/shared/ui/context";
  import Cursor from "./Cursor.svelte";
  import { Demo, type SizedStageProps, type Timeline } from "../../lib/showcase";

  const ui = useUiContext();

  let { register, reportHeight }: SizedStageProps = $props();

  // Two exchanges in one session: a simple question the fast default model
  // answers, then a complex one Loca auto-routes to the stronger secondary
  // model (the `bubble-agent-secondary` answer bubble is the visual tell, the
  // same class the client sets from `modelUsed === "secondary"`).
  const Q1 = "How far is Kyoto from Tokyo?";
  const A1 = "About 450 km, roughly 2 hours 15 minutes on the shinkansen.";
  const Q2 = "Plan a cozy 3-day weekend in Kyoto on a small budget and weigh the tradeoffs.";
  const REASONING2 =
    "This needs multi-step planning and tradeoff analysis, not a one-line fact, so route it to the stronger model.";
  // Markdown, so the stronger model's reply renders with structure (headings,
  // lists, emphasis), both signalling the heavier answer and showcasing the
  // same markdown rendering the client uses.
  const A2 = `## A relaxed plan

Base yourself near **Gion** so most days stay walkable:

- **Day 1**: temples in the east, then street food along Pontocho for dinner.
- **Day 2**: the Arashiyama bamboo grove early, a slow afternoon by the river.
- **Day 3**: Fushimi Inari at *sunrise* before the crowds, souvenirs on the way out.

### The tradeoff

A central stay costs more per night but saves on transit and time; a cheaper room out of town means longer commutes. Spend where your days actually happen.`;

  const DECODE_TOKENS_PER_SEC = 25;
  const A1_SECONDS = A1.length / 4 / DECODE_TOKENS_PER_SEC;
  const A2_SECONDS = A2.length / 4 / DECODE_TOKENS_PER_SEC;

  // Mock chat state, driven by the timeline.
  let inputValue = $state("");
  let user1 = $state("");
  let answer1 = $state("");
  let user2 = $state("");
  let answer2 = $state("");
  let showSessionBar = $state(false);
  let showUser1 = $state(false);
  let showAnswer1 = $state(false);
  let showUser2 = $state(false);
  let showReasoning2 = $state(false);
  let reasoning2Streaming = $state(false);
  let showAnswer2 = $state(false);
  // The secondary model's reply renders through the shared <Markdown> component,
  // the exact renderer the client's chat bubbles use; `answer2Streaming` drives
  // its parse throttle while the buffer grows, then flips off for a final clean
  // parse, just as the client does on stream end.
  let answer2Streaming = $state(false);
  // Gate bubble entry animations off while measuring full content height (an
  // animating bubble starts collapsed at max-height 0 and would mis-measure).
  let measuring = $state(false);

  let stageEl: HTMLElement | undefined = $state();
  let cursorRef: HTMLElement | undefined = $state();
  let columnEl: HTMLElement | undefined = $state();

  function reset(): void {
    inputValue = "";
    user1 = "";
    answer1 = "";
    user2 = "";
    answer2 = "";
    showSessionBar = false;
    showUser1 = false;
    showAnswer1 = false;
    showUser2 = false;
    showReasoning2 = false;
    reasoning2Streaming = false;
    showAnswer2 = false;
    answer2Streaming = false;
  }

  // Measure the fullest content height (every bubble shown) so the showcase
  // sizes the frame to a height the stream never exceeds, then restore.
  async function measureContentHeight(): Promise<void> {
    measuring = true;
    showUser1 = true;
    showAnswer1 = true;
    showUser2 = true;
    showReasoning2 = true;
    reasoning2Streaming = true;
    showAnswer2 = true;
    showSessionBar = true;
    user1 = Q1;
    answer1 = A1;
    user2 = Q2;
    answer2 = A2;
    // Two ticks: the first mounts <Markdown> and runs its effect (which sets the
    // parsed HTML synchronously now the renderer is warm); the second flushes
    // that HTML into the DOM, so the column measures at its true full height.
    await tick();
    await tick();
    if (columnEl) reportHeight(columnEl.offsetHeight);
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
      // Rest the cursor at its inactive centre before measuring, so it is in
      // place on the first paint instead of jumping there once measuring ends.
      demo.placeFrac(0.5, 0.6);
      // Warm the markdown renderer first, so the height measurement (and the
      // live stream) parse synchronously instead of flashing the loading spinner.
      await preloadMarkdown();
      await measureContentHeight();
      if (cancelled) return;

      const tl = gsap.timeline({ paused: true });
      timeline = tl;
      const input = 'textarea[aria-label="Message input"]';
      const send = 'button[title="Send"]';

      // Exchange 1: simple question, fast default model.
      demo.move(tl, input, { duration: 0.7 });
      demo.click(tl, input);
      demo.type(tl, (v) => (inputValue = v), Q1, { duration: 1.4 });
      demo.hold(tl, 0.3);
      demo.move(tl, send, { duration: 0.6 });
      demo.hover(tl, send, true);
      demo.click(tl, send, () => {
        user1 = Q1;
        inputValue = "";
        showUser1 = true;
        showSessionBar = true;
      });
      demo.hover(tl, send, false);
      demo.hold(tl, 0.5);
      tl.add(() => (showAnswer1 = true));
      demo.type(tl, (v) => (answer1 = v), A1, { duration: A1_SECONDS });
      demo.hold(tl, 1.2);

      // Exchange 2: complex question, auto-routed to the secondary model.
      demo.move(tl, input, { duration: 0.7 });
      demo.click(tl, input);
      demo.type(tl, (v) => (inputValue = v), Q2, { duration: 2.4 });
      demo.hold(tl, 0.3);
      demo.move(tl, send, { duration: 0.6 });
      demo.hover(tl, send, true);
      demo.click(tl, send, () => {
        user2 = Q2;
        inputValue = "";
        showUser2 = true;
      });
      demo.hover(tl, send, false);
      // A brief routing beat, then the stronger model thinks and answers.
      tl.add(() => {
        showReasoning2 = true;
        reasoning2Streaming = true;
      });
      demo.hold(tl, 2.6);
      tl.add(() => {
        reasoning2Streaming = false;
        showAnswer2 = true;
        answer2Streaming = true;
      });
      demo.type(tl, (v) => (answer2 = v), A2, { duration: A2_SECONDS });
      tl.add(() => (answer2Streaming = false));
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
      {#if showUser1}
        <MessageEnter enabled={!measuring}>
          <UserMessageView text={user1} />
        </MessageEnter>
      {/if}
      {#if showAnswer1}
        <MessageEnter enabled={!measuring}>
          <AgentMessageView kind="content" bgClass="bubble-agent">
            {#snippet body()}
              <span class="whitespace-pre-wrap break-words">{answer1}</span>
            {/snippet}
          </AgentMessageView>
        </MessageEnter>
      {/if}
      {#if showUser2}
        <MessageEnter enabled={!measuring}>
          <UserMessageView text={user2} />
        </MessageEnter>
      {/if}
      {#if showReasoning2}
        <MessageEnter enabled={!measuring}>
          <AgentMessageView
            kind="reasoning"
            isStreaming={reasoning2Streaming}
            reasoningDurationMs={2600}
          >
            {#snippet body()}
              <span>{REASONING2}</span>
            {/snippet}
          </AgentMessageView>
        </MessageEnter>
      {/if}
      {#if showAnswer2}
        <MessageEnter enabled={!measuring}>
          <AgentMessageView kind="content" bgClass="bubble-agent-secondary">
            {#snippet body()}
              <Markdown content={answer2} isStreaming={answer2Streaming} />
            {/snippet}
          </AgentMessageView>
        </MessageEnter>
      {/if}
      <UserInputView bind:value={inputValue} placeholder="Enter your instructions..." />
      {#if showSessionBar}
        <MessageEnter enabled={!measuring} centerDirection="down">
          <SessionBarView
            tokenUsage={{ used: 1240, max: 8192 }}
            showTitle
            defaultTitle="Dual model"
            titleText="Dual model"
          />
        </MessageEnter>
      {/if}
    </div>
  </div>
  <Cursor bind:ref={cursorRef} />
</div>
