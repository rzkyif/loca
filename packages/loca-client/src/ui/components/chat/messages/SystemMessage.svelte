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
  title="System Prompt"
  text={content}
  applyColorOverride
  {neighborLeft}
  {neighborRight}
/>
