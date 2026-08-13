<script lang="ts">
  import type { ComponentProps } from "svelte";
  import { onMount } from "svelte";
  import gsap from "gsap";
  import { getDefaultSettings } from "@loca/shared/domain/settings/engine";
  import { makeUiContext, setUiContext } from "@loca/shared/ui/context";
  import { BASE_MS } from "@loca/shared/ui/animations";
  import { SAMPLES } from "@loca/shared/ui/samples";
  import ChatShellView from "@loca/shared/ui/components/chat/ChatShellView.svelte";
  import CoreBarView from "@loca/shared/ui/components/chat/CoreBarView.svelte";
  import SessionBarView from "@loca/shared/ui/components/chat/SessionBarView.svelte";
  import UserInputView from "@loca/shared/ui/components/chat/UserInputView.svelte";
  import UserMessageView from "@loca/shared/ui/components/chat/messages/UserMessageView.svelte";
  import ToolCallView from "@loca/shared/ui/components/chat/messages/ToolCallView.svelte";
  import AgentMessageView from "@loca/shared/ui/components/chat/messages/AgentMessageView.svelte";
  import type { ToolCallStatus } from "@loca/shared";
  import Cursor from "./Cursor.svelte";
  import { Demo, type Timeline } from "../../lib/showcase";

  // The mobile client: the same shared ChatShellView the Android app renders,
  // under a mobile UiContext (full-width composer, user-right / agent-left
  // bubbles, top app bar), framed in a phone-sized viewport exactly like the
  // gallery's mobile card.
  const reduce =
    typeof matchMedia === "function" && matchMedia("(prefers-reduced-motion: reduce)").matches;
  const D = getDefaultSettings();
  setUiContext(
    makeUiContext({
      getSetting: (key) => (key === "stt.enabled" ? true : D[key]),
      animationDurationMs: (ms = BASE_MS) => (reduce ? 0 : ms),
      platform: "mobile",
      pointer: "coarse",
    }),
  );

  const noop = (): void => {};

  let {
    register,
    reportHeight,
  }: {
    register: (h: { timeline: Timeline; reset: () => void }) => void;
    reportHeight: (h: number) => void;
  } = $props();

  const PHONE_W = 360;
  const PHONE_H = 720;

  // A prompt sent from the phone triggers a tool on the user's desktop.
  const PROMPT = "Lock my PC, I'm heading out.";
  const ANSWER = "Locked your desktop. All secure. See you later.";
  const DECODE_TOKENS_PER_SEC = 25;
  const ANSWER_SECONDS = ANSWER.length / 4 / DECODE_TOKENS_PER_SEC;
  const RESULT = { locked: true };

  let inputValue = $state("");
  let userText = $state("");
  let agentText = $state("");
  let showUser = $state(false);
  let showTool = $state(false);
  let toolStatus = $state<ToolCallStatus>("running");
  let toolResult = $state<unknown>(undefined);
  let showAnswer = $state(false);

  let stageEl: HTMLElement | undefined = $state();
  let cursorRef: HTMLElement | undefined = $state();

  function reset(): void {
    inputValue = "";
    userText = "";
    agentText = "";
    showUser = false;
    showTool = false;
    toolStatus = "running";
    toolResult = undefined;
    showAnswer = false;
  }

  onMount(() => {
    let cancelled = false;
    let timeline: Timeline | undefined;
    reportHeight(PHONE_H);
    void (async () => {
      if (cancelled || !stageEl || !cursorRef) return;
      const demo = new Demo(cursorRef, stageEl);
      demo.placeFrac(0.5, 0.82);

      const tl = gsap.timeline({ paused: true });
      timeline = tl;
      const input = 'textarea[aria-label="Message input"]';
      const send = 'button[title="Send"]';

      demo.move(tl, input, { duration: 0.8 });
      demo.click(tl, input);
      demo.type(tl, (v) => (inputValue = v), PROMPT, { duration: 0.9 });
      demo.hold(tl, 0.3);
      demo.move(tl, send, { duration: 0.6 });
      demo.hover(tl, send, true);
      demo.click(tl, send, () => {
        userText = PROMPT;
        inputValue = "";
        showUser = true;
      });
      demo.hover(tl, send, false);
      tl.add(() => {
        showTool = true;
        toolStatus = "running";
      });
      demo.hold(tl, 1.4);
      tl.add(() => {
        toolStatus = "completed";
        toolResult = RESULT;
      });
      demo.hold(tl, 0.5);
      tl.add(() => (showAnswer = true));
      demo.type(tl, (v) => (agentText = v), ANSWER, { duration: ANSWER_SECONDS });
      register({
        timeline: tl,
        reset: () => {
          reset();
          tl.pause(0);
          demo.blur();
          demo.placeFrac(0.5, 0.82);
        },
      });
    })();
    return () => {
      cancelled = true;
      timeline?.kill();
    };
  });
</script>

{#snippet coreBar()}
  <CoreBarView
    {...SAMPLES.CoreBarView.idle as ComponentProps<typeof CoreBarView>}
    onSettings={noop}
  />
{/snippet}
{#snippet sessionBar(_z: number)}
  <SessionBarView
    {...SAMPLES.SessionBarView.default as ComponentProps<typeof SessionBarView>}
    onList={noop}
    onNew={noop}
  />
{/snippet}
{#snippet input()}
  <UserInputView bind:value={inputValue} placeholder="Message your assistant..." />
{/snippet}
{#snippet transcript()}
  <!-- Newest-first DOM order: the shell's transcript column is flex-col-reverse. -->
  {#if showAnswer}
    <AgentMessageView kind="content" bgClass="bubble-agent">
      {#snippet body()}
        <span class="whitespace-pre-wrap break-words">{agentText}</span>
      {/snippet}
    </AgentMessageView>
  {/if}
  {#if showTool}
    <ToolCallView toolName="lock_pc" status={toolStatus} result={toolResult} />
  {/if}
  {#if showUser}
    <UserMessageView text={userText} />
  {/if}
{/snippet}

<div
  bind:this={stageEl}
  class="relative w-full h-full overflow-hidden flex items-center justify-center"
>
  <!-- Phone-sized viewport, matching the gallery's mobile card. -->
  <div
    class="relative mx-auto flex shrink-0 overflow-hidden rounded-large border border-default-200 bg-surface"
    style="width: {PHONE_W}px; height: {PHONE_H}px"
  >
    <ChatShellView {coreBar} {sessionBar} {input} {transcript} />
  </div>
  <Cursor bind:ref={cursorRef} />
</div>
