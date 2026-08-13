<!-- Collapsed bubble for a core-authored user message (a scheduled prompt or
     greeting instruction): the same shared shell as SystemMessage, collapsed
     by default so the automated session opens on the agent's response. -->
<script lang="ts">
  import ExpandableMessageView from "@loca/shared/ui/components/chat/messages/ExpandableMessageView.svelte";

  let {
    id,
    content,
    neighborLeft = false,
    neighborRight = false,
  }: {
    id?: string;
    content: string;
    neighborLeft?: boolean;
    neighborRight?: boolean;
  } = $props();
</script>

<ExpandableMessageView
  {id}
  title="Automated Prompt"
  text={content}
  applyColorOverride
  {neighborLeft}
  {neighborRight}
/>
