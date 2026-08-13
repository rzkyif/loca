<script lang="ts">
  import { ttsState } from "$stores/tts.svelte";
  import { expansionState } from "$stores/expansion.svelte";
  import { getTextContent, type MessageContent } from "$lib/util/types";
  import AgentMessageView from "@loca/shared/ui/components/chat/messages/AgentMessageView.svelte";
  import MessageMarkdown from "./MessageMarkdown.svelte";
  import { showAgentMessageMenu, showReasoningMessageMenu } from "$lib/chat/message-menu";

  // AgentMessage is a discriminated bubble: a `kind: "reasoning"` instance
  // shows the reasoning trace produced by the model in a `role: "reasoning"`
  // message, and a `kind: "content"` instance shows the markdown answer in a
  // `role: "assistant"` message. Reasoning and content are persisted as
  // separate Messages so the rest of the pipeline (LLM context, deletion,
  // reprocess) sees them as independent records.
  let {
    kind,
    id,
    content,
    modelUsed = "default",
    reasoningDurationMs,
    isStreaming = false,
    onReprocess,
    onDelete,
    neighborLeft = false,
    neighborRight = false,
  } = $props<{
    kind: "reasoning" | "content";
    id?: string;
    content: MessageContent;
    modelUsed?: "default" | "secondary";
    reasoningDurationMs?: number;
    isStreaming?: boolean;
    onReprocess?: () => void;
    onDelete?: () => void;
    neighborLeft?: boolean;
    neighborRight?: boolean;
  }>();

  // Reasoning bubbles participate in the small-bubble stack chain, so their
  // expanded state lives in the shared map (read by +page.svelte to break
  // chains around expanded bubbles), which is the single source of truth. The
  // function binding below reads/writes it directly, no local mirror. Content
  // bubbles are large and never expandable, so they don't read this.

  let displayText = $derived(getTextContent(content));
  let bgClass = $derived(modelUsed === "secondary" ? "bubble-agent-secondary" : "bubble-agent");

  // TTS only attaches to content bubbles; reasoning is never read aloud.
  let isMine = $derived(
    kind === "content" &&
      id !== undefined &&
      ttsState.enabled &&
      ttsState.loaded &&
      ttsState.currentMessageId === id,
  );
  let isSpeakingThis = $derived(isMine && ttsState.liveSourceCount > 0);

  // Right-click (desktop) and long-press (touch) open the same message menu.
  function openMessageMenu() {
    if (kind === "reasoning") {
      void showReasoningMessageMenu({ reasoning: displayText });
    } else {
      if (!id) return;
      void showAgentMessageMenu({
        messageId: id,
        isStreaming,
        result: displayText,
        ttsActive,
        isSpeakingThis,
        isSynthesizing: isMine && ttsState.liveSourceCount === 0 && ttsState.synthInflight,
        onReprocess,
        onDelete,
      });
    }
  }
  let ttsBusyThis = $derived(isMine && (ttsState.liveSourceCount > 0 || ttsState.synthInflight));
  let ttsActive = $derived(
    kind === "content" && id !== undefined && ttsState.enabled && ttsState.loaded,
  );
</script>

<AgentMessageView
  {kind}
  {bgClass}
  {isStreaming}
  {reasoningDurationMs}
  bind:reasoningExpanded={
    () => (id !== undefined ? (expansionState.get(id) ?? false) : false),
    (v) => {
      if (id !== undefined) expansionState.set(id, v);
    }
  }
  active={ttsBusyThis}
  pulse={ttsBusyThis && !isSpeakingThis}
  {neighborLeft}
  {neighborRight}
  oncontextmenu={(e) => {
    e.preventDefault();
    openMessageMenu();
  }}
  onlongpress={openMessageMenu}
>
  {#snippet body()}
    <!-- isStreaming drives MessageMarkdown's parse throttle; the reasoning trace
         streams token-by-token too, so it must get it as well or it re-parses +
         sanitizes the whole growing buffer on every delta. -->
    <MessageMarkdown content={displayText} {isStreaming} />
  {/snippet}
</AgentMessageView>
