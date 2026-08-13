<script lang="ts">
  import ErrorMessageView from "@loca/shared/ui/components/chat/messages/ErrorMessageView.svelte";
  import { getTextContent, type MessageContent } from "$lib/util/types";

  let { content } = $props<{
    content: MessageContent;
  }>();

  // Content format: "error_type" or "error_type\ndetail message".
  let rawText = $derived(getTextContent(content));
  let errorType = $derived(rawText.split("\n")[0]);
  let errorDetail = $derived(
    rawText.includes("\n") ? rawText.slice(rawText.indexOf("\n") + 1) : "",
  );
</script>

<ErrorMessageView {errorType} {errorDetail} />
