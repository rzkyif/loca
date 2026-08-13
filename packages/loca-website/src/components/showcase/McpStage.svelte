<script lang="ts">
  import { onMount, tick } from "svelte";
  import gsap from "gsap";
  import UserInputView from "@loca/shared/ui/components/chat/UserInputView.svelte";
  import MessageEnter from "@loca/shared/ui/components/chat/MessageEnter.svelte";
  import UserMessageView from "@loca/shared/ui/components/chat/messages/UserMessageView.svelte";
  import RelevantToolsView from "@loca/shared/ui/components/chat/messages/RelevantToolsView.svelte";
  import ToolCallView from "@loca/shared/ui/components/chat/messages/ToolCallView.svelte";
  import AgentMessageView from "@loca/shared/ui/components/chat/messages/AgentMessageView.svelte";
  import MessageStackView from "@loca/shared/ui/components/chat/MessageStackView.svelte";
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

  // A tool from a connected calendar MCP server, matched and called just like a
  // built-in tool. The relevant-tools bubble lists what matched; the tool call
  // runs and completes, then the agent replies.
  const PROMPT = "Add 'dentist' to my calendar next Tuesday at 3.";
  const ANSWER =
    "Added 'Dentist' to your calendar for Tuesday at 3:00 PM and set a 30-minute reminder.";

  const DECODE_TOKENS_PER_SEC = 25;
  const ANSWER_SECONDS = ANSWER.length / 4 / DECODE_TOKENS_PER_SEC;

  let inputValue = $state("");
  let userText = $state("");
  let agentText = $state("");
  let showUser = $state(false);
  let showRelevant = $state(false);
  let showTool = $state(false);
  let toolStatus = $state<"running" | "completed">("running");
  let toolResult = $state<unknown>(undefined);
  let showAnswer = $state(false);
  let measuring = $state(false);

  let stageEl: HTMLElement | undefined = $state();
  let cursorRef: HTMLElement | undefined = $state();
  let columnEl: HTMLElement | undefined = $state();

  const RESULT = { title: "Dentist", start: "Tuesday 3:00 PM", reminder: "30 min" };

  // The relevant-tools and tool-call bubbles are small bubbles, so the client
  // renders them in one horizontal message stack (both collapsed). The agent
  // reply is a normal large bubble below.
  const stackItems = $derived(
    [showRelevant ? "relevant" : null, showTool ? "tool" : null].filter((x): x is string => !!x),
  );
  const align = $derived(ui.getAlignment());

  function reset(): void {
    inputValue = "";
    userText = "";
    agentText = "";
    showUser = false;
    showRelevant = false;
    showTool = false;
    toolStatus = "running";
    toolResult = undefined;
    showAnswer = false;
  }

  async function measureContentHeight(): Promise<void> {
    measuring = true;
    showUser = true;
    showRelevant = true;
    showTool = true;
    toolStatus = "completed";
    toolResult = RESULT;
    showAnswer = true;
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
      demo.type(tl, (v) => (inputValue = v), PROMPT, { duration: 1.6 });
      demo.hold(tl, 0.3);
      demo.move(tl, send, { duration: 0.7 });
      demo.hover(tl, send, true);
      demo.click(tl, send, () => {
        userText = PROMPT;
        inputValue = "";
        showUser = true;
      });
      demo.hover(tl, send, false);
      tl.add(() => (showRelevant = true));
      demo.hold(tl, 0.7);
      tl.add(() => {
        showTool = true;
        toolStatus = "running";
      });
      demo.hold(tl, 1.6);
      tl.add(() => {
        toolStatus = "completed";
        toolResult = RESULT;
      });
      demo.hold(tl, 0.6);
      tl.add(() => (showAnswer = true));
      demo.type(tl, (v) => (agentText = v), ANSWER, { duration: ANSWER_SECONDS });
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
      {#if stackItems.length}
        <MessageEnter enabled={!measuring}>
          <div
            class="w-fit"
            class:mx-auto={align === "center"}
            class:ml-auto={align === "right"}
            class:mr-auto={align === "left"}
          >
            <MessageStackView count={stackItems.length} alignment={align}>
              {#snippet bubble(i)}
                {@const neighborLeft = i > 0}
                {@const neighborRight = i < stackItems.length - 1}
                {#if stackItems[i] === "relevant"}
                  <RelevantToolsView
                    {neighborLeft}
                    {neighborRight}
                    phase1={[
                      {
                        toolId: "calendar.create_event",
                        name: "create_event",
                        description: "Add an event to your calendar.",
                        score: 0.88,
                      },
                      {
                        toolId: "calendar.list_events",
                        name: "list_events",
                        description: "List upcoming events.",
                        score: 0.52,
                      },
                    ]}
                    phase2={[
                      {
                        toolId: "calendar.create_event",
                        name: "create_event",
                        description: "Add an event to your calendar.",
                      },
                    ]}
                  />
                {:else}
                  <ToolCallView
                    {neighborLeft}
                    {neighborRight}
                    toolName="create_event"
                    status={toolStatus}
                    description={toolStatus === "running" ? "Adding event" : undefined}
                    progress={toolStatus === "running" ? 0.5 : undefined}
                    result={toolResult}
                  />
                {/if}
              {/snippet}
            </MessageStackView>
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
      <UserInputView bind:value={inputValue} placeholder="Enter your instructions..." />
    </div>
  </div>
  <Cursor bind:ref={cursorRef} />
</div>
