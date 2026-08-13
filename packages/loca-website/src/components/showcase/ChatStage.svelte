<script lang="ts">
  import { onMount, tick } from "svelte";
  import gsap from "gsap";
  import SessionBarView from "@loca/shared/ui/components/chat/SessionBarView.svelte";
  import UserInputView from "@loca/shared/ui/components/chat/UserInputView.svelte";
  import MessageEnter from "@loca/shared/ui/components/chat/MessageEnter.svelte";
  import UserMessageView from "@loca/shared/ui/components/chat/messages/UserMessageView.svelte";
  import AgentMessageView from "@loca/shared/ui/components/chat/messages/AgentMessageView.svelte";
  import { bubbleGap, makeUiContext, setUiContext, useUiContext } from "@loca/shared/ui/context";
  import { getDefaultSettings } from "@loca/shared/domain/settings/engine";
  import { BASE_MS } from "@loca/shared/ui/animations";
  import type { Alignment } from "@loca/shared/ui/types";
  import Cursor from "./Cursor.svelte";
  import { awaitStableHeight, Demo, type Timeline } from "../../lib/showcase";

  // The flagship demo ends by flipping the chat alignment (right, then left), so
  // unlike the other stages it owns its OWN subtree UI context whose
  // `layout.alignment` reads a reactive local var; mutating it re-runs the
  // alignment `$derived` inside the message Views, sliding the bubbles to the new
  // edge. Everything else mirrors the root showcase context (stt forced on,
  // animations collapsed under reduced motion).
  const reduce =
    typeof matchMedia === "function" && matchMedia("(prefers-reduced-motion: reduce)").matches;
  const chatDefaults = getDefaultSettings();
  let alignment = $state<Alignment>("center");
  setUiContext(
    makeUiContext({
      getSetting: (key) =>
        key === "stt.enabled" ? true : key === "layout.alignment" ? alignment : chatDefaults[key],
      animationDurationMs: (ms = BASE_MS) => (reduce ? 0 : ms),
    }),
  );
  const ui = useUiContext();

  let {
    register,
    reportHeight,
  }: {
    register: (h: { timeline: Timeline; reset: () => void }) => void;
    reportHeight: (h: number) => void;
  } = $props();

  const PROMPT = "Help me write a fun invite for game night at my place this Friday.";
  const REASONING = "Keep it short and upbeat: the plan, the time, and what to bring.";
  const ANSWER =
    "Here's an invite you can send:\nGame night at mine this Friday, 7pm! Bring a snack and your competitive spirit. Board games, pizza, and zero mercy. Let me know if you're in.";

  // Stream the answer at a ~2B model's realistic decode rate on consumer
  // hardware: a 2B Q4 model runs ~35-55 tok/s on Apple Silicon and ~15-25 tok/s
  // CPU-only, so ~25 tok/s is a representative midpoint for a typical local
  // machine. Tokens are estimated from length (~4 chars/token for English BPE).
  // Sources: singhajit.com LLM inference benchmarks (Qwen2-1.5B ~35-58 tok/s on
  // M1 / Mac Mini; CPU-only 7B ~5-15 tok/s, so a 2B is higher).
  const DECODE_TOKENS_PER_SEC = 25;
  const ANSWER_SECONDS = ANSWER.length / 4 / DECODE_TOKENS_PER_SEC;

  // Mock chat state, driven by the timeline.
  let inputValue = $state("");
  let userText = $state("");
  let agentText = $state("");
  let showSessionBar = $state(false);
  let showUser = $state(false);
  let showReasoning = $state(false);
  let reasoningStreaming = $state(false);
  let showAnswer = $state(false);
  // Suppresses the bubble entry animation while we mount every bubble to measure
  // the fullest content height: an animating bubble starts at max-height 0, which
  // would mis-measure the column. Off during playback so entrances animate.
  let measuring = $state(false);

  let stageEl: HTMLElement | undefined = $state();
  let cursorRef: HTMLElement | undefined = $state();
  let columnEl: HTMLElement | undefined = $state();

  function reset(): void {
    inputValue = "";
    userText = "";
    agentText = "";
    showSessionBar = false;
    showUser = false;
    showReasoning = false;
    reasoningStreaming = false;
    showAnswer = false;
    alignment = "center";
  }

  // Report the fullest content height (every bubble shown; the thinking bubble
  // stays collapsed, as in the demo) so the showcase sizes this stage's frame to a
  // height the stream never exceeds, then restore the start state. tick() flushes
  // both updates before the browser paints, so neither transient state is shown.
  async function measureContentHeight(): Promise<void> {
    measuring = true;
    showUser = true;
    showReasoning = true;
    reasoningStreaming = true;
    showAnswer = true;
    showSessionBar = true;
    userText = PROMPT;
    agentText = ANSWER;
    await tick();
    if (columnEl) reportHeight(await awaitStableHeight(columnEl));
    reset();
    measuring = false;
    await tick();
  }

  onMount(() => {
    let cancelled = false;
    let timeline: Timeline | undefined;
    // Measure first, then build and register: registering gates the showcase's
    // play loop, so the timeline never runs against the transient measured state.
    void (async () => {
      if (!stageEl || !cursorRef) return;
      const demo = new Demo(cursorRef, stageEl);
      // Rest the cursor at its inactive centre before measuring, so it is in
      // place on the first paint instead of jumping there once measuring ends.
      demo.placeFrac(0.5, 0.6);
      await measureContentHeight();
      if (cancelled) return;

      const tl = gsap.timeline({ paused: true });
      timeline = tl;
      const input = 'textarea[aria-label="Message input"]';
      const send = 'button[title="Send"]';

      demo.move(tl, input, { duration: 0.8 });
      demo.click(tl, input);
      demo.type(tl, (v) => (inputValue = v), PROMPT, { duration: 2.1 });
      demo.hold(tl, 0.3);
      demo.move(tl, send, { duration: 0.7 });
      demo.hover(tl, send, true);
      demo.click(tl, send, () => {
        userText = PROMPT;
        inputValue = "";
        showUser = true;
        showSessionBar = true;
      });
      demo.hover(tl, send, false);
      tl.add(() => {
        showReasoning = true;
        reasoningStreaming = true;
      });
      demo.hold(tl, 3.2);
      tl.add(() => {
        reasoningStreaming = false;
        showAnswer = true;
      });
      demo.type(tl, (v) => (agentText = v), ANSWER, {
        duration: ANSWER_SECONDS,
      });
      demo.hold(tl, 0.6);

      // Closing beat: flip the chat alignment to show the layout modes. The
      // cursor taps the composer's align controls; the bubbles slide to the new
      // edge as the reactive `alignment` re-runs each message's alignment derive.
      const alignRight = 'button[title="Align Right"]';
      const alignLeft = 'button[title="Align Left"]';
      demo.move(tl, alignRight, { duration: 0.7 });
      demo.hover(tl, alignRight, true);
      demo.click(tl, alignRight, () => (alignment = "right"));
      demo.hover(tl, alignRight, false);
      demo.hold(tl, 1.1);
      demo.move(tl, alignLeft, { duration: 0.7 });
      demo.hover(tl, alignLeft, true);
      demo.click(tl, alignLeft, () => (alignment = "left"));
      demo.hover(tl, alignLeft, false);

      register({
        timeline: tl,
        reset: () => {
          reset();
          // pause(0) seeks to the start with suppressEvents (default), so resetting
          // does not re-fire the timeline's state callbacks.
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
  <!-- The column renders at the app's content width (APP_W = 620px: the 700px
       window minus the bubble's 5rem max-w cap), so every bubble, the input, and
       the session bar wrap and size exactly like the client. The showcase frame
       is sized to this column (plus the shadow margin) and scaled as a whole, so
       no inner zoom is needed. Vertically-centered: messages on top, then the
       input, then the session bar at the very bottom (matching the app). As
       messages stream in above, the centered group pushes the input down. -->
  <div class="shrink-0">
    <div bind:this={columnEl} class="w-[620px] flex flex-col" style:gap={bubbleGap(ui)}>
      {#if showUser}
        <MessageEnter enabled={!measuring}>
          <UserMessageView text={userText} />
        </MessageEnter>
      {/if}
      {#if showReasoning}
        <MessageEnter enabled={!measuring}>
          <AgentMessageView
            kind="reasoning"
            isStreaming={reasoningStreaming}
            reasoningDurationMs={3200}
          >
            {#snippet body()}
              <span>{REASONING}</span>
            {/snippet}
          </AgentMessageView>
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
      <UserInputView
        bind:value={inputValue}
        placeholder="Enter your instructions..."
        onAlign={(v) => (alignment = v)}
      />
      {#if showSessionBar}
        <MessageEnter enabled={!measuring} centerDirection="down">
          <SessionBarView
            tokenUsage={{ used: 980, max: 8192 }}
            showTitle
            defaultTitle="Landlord reply"
            titleText="Landlord reply"
          />
        </MessageEnter>
      {/if}
    </div>
  </div>
  <Cursor bind:ref={cursorRef} />
</div>
