<script lang="ts">
  import AgentMessage from "$components/chat/messages/AgentMessage.svelte";
  import ErrorMessage from "$components/chat/messages/ErrorMessage.svelte";
  import AutomatedPrompt from "$components/chat/messages/AutomatedPrompt.svelte";
  import SystemMessage from "$components/chat/messages/SystemMessage.svelte";
  import DisplayBubble from "$components/chat/messages/DisplayBubble.svelte";
  import ToolCall from "$components/chat/messages/ToolCall.svelte";
  import RelevantTools from "$components/chat/messages/RelevantTools.svelte";
  import RelevantMemories from "$components/chat/messages/RelevantMemories.svelte";
  import UserMessage from "$components/chat/messages/UserMessage.svelte";
  import ChatShell from "$components/chat/ChatShell.svelte";
  import Bubble from "@loca/shared/ui/components/primitives/Bubble.svelte";
  import { useUiContext } from "@loca/shared/ui/context";
  import MessageStackGroup from "$components/chat/MessageStackGroup.svelte";
  import MessageEnter from "$components/chat/MessageEnter.svelte";
  import {
    asMessageContent,
    displayContentOf,
    getTextContent,
    type Message,
    type MessageContent,
  } from "$lib/util/types";
  import { messagesState, sessionsState, settingsState, streamingState } from "$stores";
  import { BASE_MS, getDuration, hasMessageAnimated } from "$lib/appearance/animations";

  // The transcript model (message grouping + entry-stagger delays) and its
  // markup, wrapping the shared ChatShell. Rendered identically on desktop (in
  // the sliding panel layer) and mobile (as the chat screen); both pass the same
  // `messageGroups`/`messageList` into ChatShell, so it lives here once rather
  // than duplicated across the route's two layouts. Boot owns `sessionLoading`
  // (the "Loading latest session…" placeholder gate) and passes it in.
  let { sessionLoading }: { sessionLoading: boolean } = $props();

  const ui = useUiContext();
  const onMobile = ui.platform === "mobile";

  // Find the most recent user message (messages is newest-first).
  // Used to default the editing target to the latest sent message.
  let lastUserMsg = $derived(messagesState.messages.find((m) => m.role === "user"));
  let lastUserMsgId = $derived(lastUserMsg?.id ?? null);

  // Single shared "which user message is in edit mode": only one bubble can
  // edit at a time. Defaults to (and resets to) the latest user message
  // whenever a new turn arrives or the latest is deleted; double-clicking a
  // different user bubble switches the target without losing pending debounced
  // edits (UserMessage flushes on its own when editing flips off).
  let editingUserMsgId = $state<string | null>(null);
  $effect(() => {
    editingUserMsgId = lastUserMsgId;
  });

  // Visible while the assistant turn is in flight but we haven't received
  // anything yet, neither reasoning nor content. Drives a transient
  // small-bubble spinner so the user has a visual cue between sending and
  // first-token arrival. As soon as either reasoning or content fires, this
  // turns false and the corresponding real bubble takes over.
  let showStreamingLoadingBubble = $derived(
    streamingState.isActive && streamingState.awaitingFirstDelta,
  );

  // A "small bubble" message is one rendered as a `size="small"` Bubble
  // (system prompt, reasoning, tool_filter, tool, plus the synthetic loading
  // sentinel). Consecutive small bubbles are grouped into a MessageStackGroup,
  // which renders one or more horizontally-scrollable substacks of collapsed
  // bubbles separated by standalone rows for any bubbles whose Expandable is
  // open. Expanding a bubble in a substack splits that substack around the
  // bubble; collapsing merges the surrounding substacks back together.
  // `messagesSeen` in animations.ts dedupes the slide-in transition so
  // re-mounts caused by segment regrouping don't replay it.
  function isSmallBubbleMsg(msg: Message): boolean {
    if (msg.role === "system") return true;
    if (msg.role === "reasoning") return true;
    if (msg.role === "loading") return true;
    if (msg.role === "tool") return true;
    if (msg.role === "tool_filter") return true;
    if (msg.role === "memory_filter") return true;
    if (msg.role === "display") return true;
    // Core-authored prompt of an automated session: a collapsed
    // "Automated Prompt" bubble instead of a full user bubble.
    if (msg.role === "user" && msg.automated) return true;
    return false;
  }

  type RenderGroup =
    | { kind: "stack"; key: string; messages: Message[] }
    | { kind: "single"; key: string; message: Message };

  // Stable id for the synthetic loading sentinel, consistent across the
  // bubble's mounted lifetime so keyed each blocks don't churn while it's
  // visible. The dedup-by-msgId guard inside `messageEnter` is bypassed
  // separately for this id so each appearance still animates in/out.
  const LOADING_MSG_ID = "__streaming_loading__";

  // Filter out hidden messages (empty assistant placeholders mid-stream and
  // reasoning when the setting is off) before grouping so the chain logic
  // sees only what'll actually render. Then inject a synthetic small-bubble
  // loading sentinel at the newest position OF THE RUNNING TURN when we're
  // awaiting the first response chunk. That lets it stack with adjacent
  // small bubbles (tool_filter, reasoning, system) via the existing
  // grouping pipeline instead of being a standalone element outside it.
  let displayedMessages = $derived.by<Message[]>(() => {
    // Read the visibility flags ONCE per recompute rather than per message: this
    // filter runs over the whole array on every streaming delta, and a reactive
    // settings lookup per message multiplied that cost by the transcript length.
    const showReasoning = settingsState.currentSettings["llm.showReasoning"];
    const showSystemPrompt = settingsState.currentSettings["prompts.showSystemPrompt"];
    const showEmptyToolSelection = settingsState.currentSettings["tools.showEmptySelection"];
    const showEmptyMemorySelection = settingsState.currentSettings["memories.showEmptySelection"];
    const real = messagesState.messages.filter((msg) => {
      const isEmptyAssistant = msg.role === "assistant" && getTextContent(msg.content) === "";
      const isHiddenReasoning = msg.role === "reasoning" && !showReasoning;
      const isHiddenSystem = msg.role === "system" && !showSystemPrompt;
      // Empty filter bubbles (no relevant tools / memories found) are hidden
      // unless the user opts to keep them. Errors always show. "No relevant
      // tools" is the filter result, not toolsSent: always-available tools
      // would otherwise keep the bubble on every turn. A directly-named tool
      // (nameMatched) is the exception: the user asked for it by name, so the
      // bubble must show it even though it bypassed phase 1/2.
      const toolFilterBase =
        msg.phase2 !== undefined ? msg.phase2.length : (msg.phase1?.length ?? 0);
      const isHiddenEmptyToolFilter =
        msg.role === "tool_filter" &&
        msg.status !== "error" &&
        toolFilterBase === 0 &&
        (msg.nameMatched?.length ?? 0) === 0 &&
        !showEmptyToolSelection;
      const isHiddenEmptyMemoryFilter =
        msg.role === "memory_filter" &&
        msg.status !== "error" &&
        (msg.relevant?.length ?? 0) === 0 &&
        !showEmptyMemorySelection;
      return (
        !isEmptyAssistant &&
        !isHiddenReasoning &&
        !isHiddenSystem &&
        !isHiddenEmptyToolFilter &&
        !isHiddenEmptyMemoryFilter
      );
    });
    if (!showStreamingLoadingBubble) return real;
    const loadingMsg: Message = {
      id: LOADING_MSG_ID,
      role: "loading",
      content: "",
    };
    // Mid-history regenerate: the streaming layer is inserting bubbles
    // between the anchor user message and the next-newer user message, so
    // the sentinel must land in that same slot - just newer than the
    // next-newer user message (or at index 0 if none) - to appear at the
    // top of the running turn instead of at the very top of the array.
    const anchorId = streamingState.turnAnchorId;
    if (anchorId === null) return [loadingMsg, ...real];
    const anchorIdx = real.findIndex((m) => m.role === "user" && m.id === anchorId);
    if (anchorIdx < 0) return [loadingMsg, ...real];
    let insertIdx = 0;
    for (let i = anchorIdx - 1; i >= 0; i--) {
      if (real[i].role === "user") {
        insertIdx = i + 1;
        break;
      }
    }
    return [...real.slice(0, insertIdx), loadingMsg, ...real.slice(insertIdx)];
  });

  function msgKey(msg: Message, fallback: string): string {
    return msg.id ?? msg.callId ?? fallback;
  }

  // Assistant bubble text, with the interrupted note appended at render
  // time: the persisted content stays the clean partial text, the note is
  // presentation only.
  function assistantContent(msg: Message): MessageContent {
    const base = asMessageContent(msg.content);
    if (msg.truncated) {
      const text = getTextContent(base);
      const note = "> _Reply cut off: the context window is full._";
      return text ? `${text}\n\n${note}` : note;
    }
    if (!msg.interrupted) return base;
    const text = getTextContent(base);
    return text ? `${text}\n\n> _Interrupted._` : "> _Interrupted._";
  }

  let messageGroups = $derived.by<RenderGroup[]>(() => {
    const groups: RenderGroup[] = [];
    let stack: Message[] = [];
    // messages is newest-first, but the user wants stacked small
    // bubbles in old→new visual order (oldest at the screen-facing edge,
    // wrapping rightward and downward). We collect into `stack` in the
    // newest-first iteration order, then reverse on flush so DOM[0] of the
    // stack is the OLDEST message, placing it leftmost (left/center
    // alignment with flex-row) or rightmost (right alignment with
    // flex-row-reverse).
    const flushStack = () => {
      if (stack.length === 0) return;
      stack.reverse();
      const head = stack[0];
      groups.push({
        kind: "stack",
        key: `stack:${msgKey(head, `s-${groups.length}`)}`,
        messages: stack,
      });
      stack = [];
    };
    for (let i = 0; i < displayedMessages.length; i++) {
      const msg = displayedMessages[i];
      if (isSmallBubbleMsg(msg)) {
        stack.push(msg);
      } else {
        flushStack();
        groups.push({
          kind: "single",
          key: `single:${msgKey(msg, `i-${i}`)}`,
          message: msg,
        });
      }
    }
    flushStack();
    return groups;
  });

  // Entry-animation stagger, in vertical order: bubbles that mount in the
  // same flush (e.g. the system prompt + the first user message + the
  // loading sentinel) slide in top-to-bottom, each waiting one BASE_MS slot
  // per not-yet-animated bubble above it. Bubbles that already animated
  // (hasMessageAnimated) are settled and don't occupy a slot; their own
  // delay value is moot because claimMessageEnter dedupes by msgId.
  function enterDelayKey(msg: Message): string | null {
    if (msg.role === "loading") return LOADING_MSG_ID;
    return msg.id ?? msg.callId ?? null;
  }
  // Bulk mounts (switching to a session whose bubbles haven't animated this
  // run) keep the everything-at-once entrance; the stagger is only for the
  // small batches a single turn produces.
  const MAX_STAGGER_COHORT = 4;
  let enterDelays = $derived.by<Map<string, number>>(() => {
    // displayedMessages is newest-first; collect oldest-first (top of screen
    // first) so delays grow downward.
    const entering: string[] = [];
    for (let i = displayedMessages.length - 1; i >= 0; i--) {
      const msg = displayedMessages[i];
      const key = enterDelayKey(msg);
      if (!key) continue;
      // The loading sentinel re-animates on every appearance (its msgId is
      // withheld from the dedupe), so it always counts as entering.
      if (msg.role !== "loading" && hasMessageAnimated(key)) continue;
      entering.push(key);
    }
    const delays = new Map<string, number>();
    if (entering.length > MAX_STAGGER_COHORT) return delays;
    const slot = getDuration(BASE_MS);
    entering.forEach((key, i) => delays.set(key, i * slot));
    return delays;
  });
</script>

<ChatShell stackDepth={messageGroups.length} transcript={messageList} />

{#snippet messageList()}
  {#if sessionLoading}
    <div class="relative pointer-events-none" style:z-index={messageGroups.length + 1}>
      <Bubble
        selectedAlignment={onMobile ? "left" : settingsState.getAlignment()}
        borderColorClass="border-default-400"
        extraClass="flex items-center gap-2"
      >
        <i class="i-line-md:loading-alt-loop text-xl"></i>
        <span>Loading latest session…</span>
      </Bubble>
    </div>
  {/if}

  <!-- Force a clean teardown of the entire message subtree on every
               session boundary. Without the key, the cancelled tool's
               Expandable body (transition:expand|global) and the various
               per-component effects (auto-close, expansion-state writers,
               MessageStackGroup's effect.pre + transitionTimers) can race
               with `messages = []` and leave stale DOM behind after a
               delete-with-active-tool-call. The key bumps inside
               `sessionsState.resetAllSessionState`. -->
  {#key sessionsState.epoch}
    {#each messageGroups as group, gi (group.key)}
      <!-- Descending z down the transcript (gi 0 = newest = bottom):
                   see the stacking comment on the SessionBar wrapper above. -->
      <div class="relative pointer-events-none" style:z-index={messageGroups.length - gi}>
        {#if group.kind === "stack"}
          <MessageStackGroup messages={group.messages}>
            {#snippet item({ msg, idx, neighborLeft, neighborRight })}
              <MessageEnter
                alignment={onMobile ? "left" : settingsState.getAlignment()}
                msgId={msg.role === "loading" ? undefined : msgKey(msg, `g-${idx}`)}
                delayMs={enterDelays.get(enterDelayKey(msg) ?? "") ?? 0}
                class="pointer-events-none"
              >
                {#if msg.role === "loading"}
                  <Bubble
                    selectedAlignment={onMobile ? "left" : settingsState.getAlignment()}
                    size="small"
                    extraClass="flex items-center"
                    {neighborLeft}
                    {neighborRight}
                  >
                    <i class="i-line-md:loading-alt-loop text-base"></i>
                  </Bubble>
                {:else if msg.role === "reasoning"}
                  <AgentMessage
                    kind="reasoning"
                    id={msg.id}
                    content={asMessageContent(msg.content)}
                    modelUsed={msg.modelUsed}
                    reasoningDurationMs={msg.reasoningDurationMs}
                    isStreaming={streamingState.isLive(msg.id)}
                    {neighborLeft}
                    {neighborRight}
                  />
                {:else if msg.role === "system"}
                  <SystemMessage
                    id={msg.id}
                    content={msg.content as string}
                    {neighborLeft}
                    {neighborRight}
                  />
                {:else if msg.role === "user"}
                  <AutomatedPrompt
                    id={msg.id}
                    content={getTextContent(asMessageContent(msg.content))}
                    {neighborLeft}
                    {neighborRight}
                  />
                {:else if msg.role === "tool"}
                  <ToolCall id={msg.id} {msg} {neighborLeft} {neighborRight} />
                {:else if msg.role === "tool_filter"}
                  <RelevantTools id={msg.id} {msg} {neighborLeft} {neighborRight} />
                {:else if msg.role === "memory_filter"}
                  <RelevantMemories id={msg.id} {msg} {neighborLeft} {neighborRight} />
                {:else if msg.role === "display"}
                  {@const display = displayContentOf(msg)}
                  {#if display}
                    <DisplayBubble id={msg.id} content={display} {neighborLeft} {neighborRight} />
                  {/if}
                {/if}
              </MessageEnter>
            {/snippet}
          </MessageStackGroup>
        {:else}
          {@const msg = group.message}
          <MessageEnter
            alignment={settingsState.getAlignment()}
            msgId={msgKey(msg, group.key)}
            delayMs={enterDelays.get(enterDelayKey(msg) ?? "") ?? 0}
            class="relative pointer-events-none"
          >
            {#if msg.role === "user"}
              <UserMessage
                content={asMessageContent(msg.content)}
                editing={msg.id != null && msg.id === editingUserMsgId}
                onStartEdit={() => (editingUserMsgId = msg.id ?? null)}
                onStopEdit={() => (editingUserMsgId = null)}
                onEdit={(newContent) => messagesState.updateUserMessage(msg.id, newContent)}
                onReprocess={msg.id ? () => messagesState.reprocessUserMessage(msg.id!) : undefined}
                onDelete={msg.id ? () => messagesState.deleteUserMessage(msg.id!) : undefined}
              />
            {:else if msg.role === "error"}
              <ErrorMessage content={asMessageContent(msg.content)} />
            {:else if msg.role === "assistant"}
              <AgentMessage
                kind="content"
                id={msg.id}
                content={assistantContent(msg)}
                modelUsed={msg.modelUsed}
                isStreaming={streamingState.isLive(msg.id)}
                onReprocess={msg.id
                  ? () => messagesState.reprocessAgentMessage(msg.id!)
                  : undefined}
                onDelete={msg.id ? () => messagesState.deleteAgentMessage(msg.id!) : undefined}
              />
            {/if}
          </MessageEnter>
        {/if}
      </div>
    {/each}
  {/key}
{/snippet}
