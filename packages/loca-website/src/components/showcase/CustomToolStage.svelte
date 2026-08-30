<script lang="ts">
  import { onMount } from "svelte";
  import gsap from "gsap";
  import RelevantToolsView from "@loca/shared/ui/components/chat/messages/RelevantToolsView.svelte";
  import ToolCallView from "@loca/shared/ui/components/chat/messages/ToolCallView.svelte";
  import UserMessageView from "@loca/shared/ui/components/chat/messages/UserMessageView.svelte";
  import AgentMessageView from "@loca/shared/ui/components/chat/messages/AgentMessageView.svelte";
  import UserInputView from "@loca/shared/ui/components/chat/UserInputView.svelte";
  import MessageEnter from "@loca/shared/ui/components/chat/MessageEnter.svelte";
  import MessageStackView from "@loca/shared/ui/components/chat/MessageStackView.svelte";
  import { bubbleGap, useUiContext } from "@loca/shared/ui/context";
  import type { AskUserQuestion } from "@loca/shared";
  import hljs from "highlight.js/lib/core";
  import typescript from "highlight.js/lib/languages/typescript";
  import json from "highlight.js/lib/languages/json";
  import Cursor from "./Cursor.svelte";
  import BubbleShadow from "./BubbleShadow.svelte";
  import { Demo, type SizedStageProps, type Timeline } from "../../lib/showcase";

  hljs.registerLanguage("typescript", typescript);
  hljs.registerLanguage("json", json);

  let { register, reportHeight }: SizedStageProps = $props();

  // Fixed-height stage: the extension editor holds the top half while the chat
  // below scrolls. The chat starts as just the input, vertically centered, and
  // scrolls as the tool flow grows.
  const FRAME_H = 820;
  const REL_ID = "ct-relevant";
  const ui = useUiContext();

  // Top half: a hand-written extension, shown two-up (handler + manifest). This is
  // website-only editor chrome, not a client surface.
  const INDEX_TS = `import { dispenseFood } from "@acme/petnet";
import type { ToolContext } from "loca:tool";

// the feed_cat tool
export async function feedCat(
  args: { portion?: string },
  ctx: ToolContext,
) {
  const [bowl] = await ctx.askUser([
    {
      question: "Which bowl should I fill?",
      options: [
        { label: "Kitchen", value: "kitchen" },
        { label: "Patio", value: "patio" },
      ],
    },
  ]);
  await dispenseFood({ bowl, grams: 45 });
  return { fed: true, bowl };
}`;
  const TOOLS_JSON = `{
  "name": "feed_cat",
  "description": "Dispense a portion from the smart feeder.",
  "parameters": {
    "type": "object",
    "properties": {
      "portion": { "type": "string" }
    }
  },
  "triggers": ["feed my cat", "feed Mochi"],
  "function": "feedCat",
  "permissions": { "net": ["petnet.local"] }
}`;
  const INDEX_HTML = hljs.highlight(INDEX_TS, { language: "typescript" }).value;
  const TOOLS_HTML = hljs.highlight(TOOLS_JSON, { language: "json" }).value;

  const PROMPT = "Feed Mochi, please.";
  const ANSWER = "All set. I filled the kitchen bowl with a 45g portion for Mochi.";
  const DECODE_TOKENS_PER_SEC = 25;
  const ANSWER_SECONDS = ANSWER.length / 4 / DECODE_TOKENS_PER_SEC;

  const feedQuestions: AskUserQuestion[] = [
    {
      question: "Which bowl should I fill?",
      options: [
        { label: "Kitchen", value: "kitchen", description: "the usual spot" },
        { label: "Patio", value: "patio" },
      ],
    },
  ];

  // Chat state, driven by the timeline.
  let inputValue = $state("");
  let userText = $state("");
  let showUser = $state(false);
  let showRelevant = $state(false);
  let showFeed = $state(false);
  let feedStatus = $state<"running" | "awaiting_user" | "completed">("running");
  let feedDrafts = $state<
    | Record<number, { text: string; picks: string[]; freestyleActive: boolean; rows: string[][] }>
    | undefined
  >(undefined);
  let feedResult = $state<unknown>(undefined);
  let showAnswer = $state(false);
  let answerText = $state("");

  let stageEl: HTMLElement | undefined = $state();
  let cursorRef: HTMLElement | undefined = $state();
  let chatScroll: HTMLElement | undefined = $state();

  // The relevant-tools and tool-call bubbles are small bubbles, so they share one
  // horizontal message stack. While the tool awaits the user's answer its bubble
  // stays in the stack, collapsed and yellow; the answer form itself renders in
  // the composer below (the askUser form moved out of the bubble).
  const align = $derived(ui.getAlignment());
  const stackItems = $derived(
    [showRelevant ? "relevant" : null, showFeed ? "feed" : null].filter((x): x is string => !!x),
  );

  function reset(): void {
    inputValue = "";
    userText = "";
    showUser = false;
    showRelevant = false;
    showFeed = false;
    feedStatus = "running";
    feedDrafts = undefined;
    feedResult = undefined;
    showAnswer = false;
    answerText = "";
    ui.expansionSet(REL_ID, false);
    if (chatScroll) chatScroll.scrollTop = 0;
  }

  onMount(() => {
    let cancelled = false;
    let timeline: Timeline | undefined;
    reportHeight(FRAME_H);
    ui.expansionSet(REL_ID, false);
    void (async () => {
      if (cancelled || !stageEl || !cursorRef) return;
      const demo = new Demo(cursorRef, stageEl);
      demo.placeFrac(0.5, 0.7);

      const tl = gsap.timeline({ paused: true });
      timeline = tl;
      const input = 'textarea[aria-label="Message input"]';
      const send = 'button[title="Send"]';

      // 1. Trigger the tool by prompt.
      demo.move(tl, input, { duration: 0.7 });
      demo.click(tl, input);
      demo.type(tl, (v) => (inputValue = v), PROMPT, { duration: 1.3 });
      demo.hold(tl, 0.3);
      demo.move(tl, send, { duration: 0.6 });
      demo.hover(tl, send, true);
      demo.click(tl, send, () => {
        userText = PROMPT;
        inputValue = "";
        showUser = true;
      });
      demo.hover(tl, send, false);
      demo.scroll(tl, () => chatScroll, 9999, { duration: 0.5 });

      // 2. The relevant-tools bubble appears collapsed, then the agent's tool
      //    call follows right behind it and holds waiting for input (neither
      //    waits on a cursor click to appear).
      demo.hold(tl, 0.4);
      tl.add(() => (showRelevant = true));
      demo.scroll(tl, () => chatScroll, 9999, { duration: 0.4 });
      demo.hold(tl, 0.4);
      tl.add(() => {
        showFeed = true;
        feedStatus = "running";
      });
      demo.scroll(tl, () => chatScroll, 9999, { duration: 0.4 });
      demo.hold(tl, 0.45);
      tl.add(() => {
        feedStatus = "awaiting_user";
        feedDrafts = { 0: { text: "", picks: [], freestyleActive: false, rows: [] } };
      });
      demo.scroll(tl, () => chatScroll, 9999, { duration: 0.4 });
      demo.hold(tl, 0.6);

      // 3. The cursor answers the tool's question.
      demo.move(tl, "[data-tc-nav]", { duration: 0.7 });
      demo.hover(tl, "[data-tc-nav]", true);
      demo.click(tl, "[data-tc-nav]", () => {
        feedDrafts = { 0: { text: "", picks: ["kitchen"], freestyleActive: false, rows: [] } };
      });
      demo.hover(tl, "[data-tc-nav]", false);
      demo.hold(tl, 0.7);
      // 4. The tool resumes and completes.
      tl.add(() => {
        feedStatus = "completed";
        feedDrafts = undefined;
        feedResult = { fed: true, bowl: "kitchen", grams: 45 };
      });
      demo.scroll(tl, () => chatScroll, 9999, { duration: 0.4 });
      demo.hold(tl, 0.6);

      // 5. The agent's closing reply.
      tl.add(() => (showAnswer = true));
      demo.type(tl, (v) => (answerText = v), ANSWER, { duration: ANSWER_SECONDS });
      demo.scroll(tl, () => chatScroll, 9999, { duration: 0.4 });
      register({
        timeline: tl,
        reset: () => {
          reset();
          tl.pause(0);
          demo.blur();
          demo.placeFrac(0.5, 0.7);
        },
      });
    })();
    return () => {
      cancelled = true;
      timeline?.kill();
    };
  });
</script>

<div bind:this={stageEl} class="relative w-full h-full overflow-hidden">
  <div class="w-[620px] h-full mx-auto flex flex-col gap-3 py-1">
    <!-- Top half: website-only editor chrome (not a client surface), two-up.
         Wears the shared bubble drop shadow + blur halo via BubbleShadow. -->
    <div class="relative shrink-0 h-[348px]">
      <BubbleShadow />
      <div
        class="bubble-promote relative z-10 h-full rounded-large overflow-hidden border border-default-200 bg-surface"
      >
        <div class="grid grid-cols-2 h-full">
          <div data-pane="index" class="flex flex-col min-h-0">
            <div class="px-3 py-1.5 text-[11px] font-mono text-default-600 bg-surface">
              index.ts
            </div>
            <pre
              class="demo-code flex-1 min-h-0 overflow-hidden text-[10px] font-mono text-default-800 px-3 py-2 whitespace-pre leading-[1.45]">{@html INDEX_HTML}</pre>
          </div>
          <div data-pane="tools" class="flex flex-col min-h-0">
            <div class="px-3 py-1.5 text-[11px] font-mono text-default-600 bg-surface">
              loca.json
            </div>
            <pre
              class="demo-code flex-1 min-h-0 overflow-hidden text-[10px] font-mono text-default-800 px-3 py-2 whitespace-pre leading-[1.45]">{@html TOOLS_HTML}</pre>
          </div>
        </div>
      </div>
    </div>

    <!-- Bottom half: the real shared chat surface, scrollable; input centered
         until the tool flow grows past the viewport. -->
    <div bind:this={chatScroll} class="flex-1 min-h-0 overflow-y-auto no-scrollbar">
      <div class="min-h-full flex flex-col justify-center" style:gap={bubbleGap(ui)}>
        {#if showUser}
          <MessageEnter>
            <UserMessageView text={userText} />
          </MessageEnter>
        {/if}
        {#if stackItems.length}
          <MessageEnter>
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
                      id={REL_ID}
                      {neighborLeft}
                      {neighborRight}
                      phase1={[
                        {
                          toolId: "petcare.feed_cat",
                          name: "feed_cat",
                          description: "Dispense a portion from the smart feeder.",
                          score: 0.82,
                        },
                        {
                          toolId: "home.show_todos",
                          name: "show_todos",
                          description: "Show today's todo list.",
                          score: 0.41,
                        },
                      ]}
                      phase2={[
                        {
                          toolId: "petcare.feed_cat",
                          name: "feed_cat",
                          description: "Dispense a portion from the smart feeder.",
                        },
                      ]}
                    />
                  {:else}
                    <ToolCallView
                      {neighborLeft}
                      {neighborRight}
                      toolName="feed_cat"
                      status={feedStatus}
                      description={feedStatus === "running" ? "Dispensing" : undefined}
                      progress={feedStatus === "running" ? 0.5 : undefined}
                      result={feedResult}
                    />
                  {/if}
                {/snippet}
              </MessageStackView>
            </div>
          </MessageEnter>
        {/if}
        {#if showAnswer}
          <MessageEnter>
            <AgentMessageView kind="content" bgClass="bubble-agent">
              {#snippet body()}
                <span class="whitespace-pre-wrap break-words">{answerText}</span>
              {/snippet}
            </AgentMessageView>
          </MessageEnter>
        {/if}
        <!-- The composer hosts the tool's askUser form while it awaits input: the
             question + options render in place of the textarea, neutral, and the
             left controls hide (matching the client's askUser composer mode). -->
        <!-- Single-select choice commits by clicking an option tile, so `actions`
             is empty (no row buttons), matching the client's askUser composer. -->
        <UserInputView
          bind:value={inputValue}
          placeholder="Enter your instructions..."
          askUserPrompt={feedStatus === "awaiting_user"
            ? { questions: feedQuestions, drafts: feedDrafts ?? {}, actions: [], autoFocus: false }
            : null}
        />
      </div>
    </div>
  </div>
  <Cursor bind:ref={cursorRef} />
</div>
