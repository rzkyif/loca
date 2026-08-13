<script lang="ts">
  import type { Message } from "$lib/util/types";
  import ToolCallView from "@loca/shared/ui/components/chat/messages/ToolCallView.svelte";
  import MessageMarkdown from "./MessageMarkdown.svelte";
  import { settingsState } from "../../../state";
  import { expansionState, isExpanded } from "$stores/expansion.svelte";

  // Thin wrapper over the shared ToolCallView: maps the flat wire fields +
  // ephemera overlay onto the View's props, supplies the agent name and the
  // markdown renderer the shared component can't import, and binds expansion to
  // the shared expansion map (read by MessageStackGroup to split substacks
  // around an open bubble). While awaiting input the bubble reads yellow but is
  // form-free; the askUser form itself lives in the composer (UserInput).
  let {
    id,
    msg,
    neighborLeft = false,
    neighborRight = false,
  } = $props<{
    id?: string;
    msg: Message;
    neighborLeft?: boolean;
    neighborRight?: boolean;
  }>();

  let agentName = $derived(
    (settingsState.currentSettings["general.context.agentName"] as string) ?? "",
  );
  let parsedArgs = $derived.by<Record<string, unknown>>(() => {
    try {
      return msg.arguments ? (JSON.parse(msg.arguments) as Record<string, unknown>) : {};
    } catch {
      return {};
    }
  });
</script>

<ToolCallView
  toolName={msg.toolName}
  status={msg.status ?? "completed"}
  label={msg.label}
  description={msg.description}
  args={parsedArgs}
  result={msg.result}
  error={msg.error}
  progress={msg.progress}
  logs={msg.ephemera?.logs ?? []}
  {agentName}
  {neighborLeft}
  {neighborRight}
  bind:expanded={
    () => (id !== undefined ? isExpanded(id, false) : false),
    (v) => {
      if (id !== undefined) expansionState.set(id, v);
    }
  }
>
  {#snippet memoryContent({ content })}
    <MessageMarkdown {content} />
  {/snippet}
</ToolCallView>
