<script lang="ts">
  import type { Message } from "$lib/util/types";
  import RelevantMemoriesView from "@loca/shared/ui/components/chat/messages/RelevantMemoriesView.svelte";

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

<RelevantMemoriesView
  {id}
  memories={msg.relevant ?? []}
  status={msg.status ?? "complete"}
  errorMessage={msg.errorMessage}
  {neighborLeft}
  {neighborRight}
/>
