<script lang="ts">
  import type { Message } from "$lib/util/types";
  import RelevantToolsView from "@loca/shared/ui/components/chat/messages/RelevantToolsView.svelte";

  let {
    id,
    msg,
    neighborLeft = false,
    neighborRight = false,
  }: {
    id?: string;
    msg: Message;
    neighborLeft?: boolean;
    neighborRight?: boolean;
  } = $props();
</script>

<RelevantToolsView
  {id}
  phase1={msg.phase1}
  phase2={msg.phase2}
  alwaysAvailable={msg.alwaysAvailable}
  nameMatched={msg.nameMatched}
  mcp={msg.mcp}
  status={msg.status ?? "complete"}
  errorMessage={msg.errorMessage}
  {neighborLeft}
  {neighborRight}
/>
