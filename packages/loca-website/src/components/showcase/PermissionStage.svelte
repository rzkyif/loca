<script lang="ts">
  import { onMount, tick } from "svelte";
  import gsap from "gsap";
  import UserInputView from "@loca/shared/ui/components/chat/UserInputView.svelte";
  import MessageEnter from "@loca/shared/ui/components/chat/MessageEnter.svelte";
  import UserMessageView from "@loca/shared/ui/components/chat/messages/UserMessageView.svelte";
  import ToolCallView from "@loca/shared/ui/components/chat/messages/ToolCallView.svelte";
  import AgentMessageView from "@loca/shared/ui/components/chat/messages/AgentMessageView.svelte";
  import { bubbleGap, useUiContext } from "@loca/shared/ui/context";
  import type { ToolCallStatus } from "@loca/shared";
  import Cursor from "./Cursor.svelte";
  import { Demo, awaitStableHeight, type SizedStageProps, type Timeline } from "../../lib/showcase";

  const ui = useUiContext();

  let { register, reportHeight }: SizedStageProps = $props();

  // A guarded tool pauses for permission: the composer becomes the permission
  // review (a shield line + the exact command) with Deny / Allow, exactly as in
  // the app. Allowing resumes the tool, which completes and the agent replies.
  const PROMPT = "Delete everything in my Downloads older than a month.";
  const ANSWER = "Done. Removed 23 old files from Downloads.";
  const DECODE_TOKENS_PER_SEC = 25;
  const ANSWER_SECONDS = ANSWER.length / 4 / DECODE_TOKENS_PER_SEC;

  const ALLOW_TITLE = "Allow for this tool call";
  const RESULT = { removed: 23 };
  // The scripted permission the composer renders while paused (the same scripted
  // STATE the client feeds from its live permission store).
  const PERMISSION = {
    toolName: "shell",
    action: "run a program",
    detail: "find ~/Downloads -mtime +30 -delete",
    declared: true,
  };

  let inputValue = $state("");
  let userText = $state("");
  let agentText = $state("");
  let showUser = $state(false);
  let showTool = $state(false);
  let toolStatus = $state<ToolCallStatus>("running");
  let toolResult = $state<unknown>(undefined);
  let inPromptMode = $state(false);
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
    showTool = false;
    toolStatus = "running";
    toolResult = undefined;
    inPromptMode = false;
    showAnswer = false;
  }

  // Two layouts share this stage (the permission review vs the final reply), so
  // measure both and size the frame to the taller one.
  async function measureContentHeight(): Promise<void> {
    measuring = true;
    showUser = true;
    userText = PROMPT;
    // Permission-review frame.
    showTool = true;
    toolStatus = "awaiting_permission";
    inPromptMode = true;
    await tick();
    const hPrompt = columnEl ? await awaitStableHeight(columnEl) : 0;
    // Final-reply frame.
    inPromptMode = false;
    toolStatus = "completed";
    toolResult = RESULT;
    showAnswer = true;
    agentText = ANSWER;
    await tick();
    const hReply = columnEl ? await awaitStableHeight(columnEl) : 0;
    reportHeight(Math.max(hPrompt, hReply));
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
      const allow = `button[title="${ALLOW_TITLE}"]`;

      demo.move(tl, input, { duration: 0.8 });
      demo.click(tl, input);
      demo.type(tl, (v) => (inputValue = v), PROMPT, { duration: 1.3 });
      demo.hold(tl, 0.3);
      demo.move(tl, send, { duration: 0.7 });
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
      demo.hold(tl, 0.6);
      // The tool needs permission: the composer becomes the review form.
      tl.add(() => {
        toolStatus = "awaiting_permission";
        inPromptMode = true;
      });
      demo.hold(tl, 1.4);
      // Allow it.
      demo.move(tl, allow, { duration: 0.7 });
      demo.hover(tl, allow, true);
      demo.click(tl, allow, () => {
        inPromptMode = false;
        toolStatus = "completed";
        toolResult = RESULT;
      });
      demo.hover(tl, allow, false);
      demo.hold(tl, 0.5);
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
      {#if showTool}
        <MessageEnter enabled={!measuring}>
          <ToolCallView
            toolName="shell"
            status={toolStatus}
            description={toolStatus === "running" ? "Running" : undefined}
            result={toolResult}
          />
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
      <!-- The permission prompt renders INSIDE the composer (the shared
           UserInputView's permission mode), exactly as the client shows it: the
           shield line + target replace the textarea and the send group becomes
           Deny / Allow, while the composer's own controls stay in place. Driven
           by scripted STATE (`permissionPrompt`), so it is single-source with the
           client and clears the composer lint without re-composing the shell. -->
      <UserInputView
        bind:value={inputValue}
        placeholder="Enter your instructions..."
        permissionPrompt={inPromptMode ? PERMISSION : null}
        onPermissionDeny={() => {}}
        onPermissionAllow={() => {}}
      />
    </div>
  </div>
  <Cursor bind:ref={cursorRef} />
</div>
