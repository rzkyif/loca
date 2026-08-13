<script lang="ts">
  import Bubble from "../../primitives/Bubble.svelte";
  import ErrorDetailView from "./ErrorDetailView.svelte";
  import { useUiContext } from "../../../context.ts";

  // Presentational error bubble: maps a model/runtime error type to a friendly
  // message and shows the optional raw detail in a scrollable code block. The
  // client wrapper parses the message content into `errorType` / `errorDetail`;
  // alignment comes from the UI context so both apps match.
  const ui = useUiContext();
  // Mobile chat bubbles always sit on the agent side (left).
  const align = $derived(ui.platform === "mobile" ? "left" : ui.getAlignment());

  let { errorType, errorDetail = "" }: { errorType: string; errorDetail?: string } = $props();

  function errorMessage(type: string): string {
    switch (type) {
      case "rate_limit_error":
        return "Rate limit exceeded. Please wait a moment and try again.";
      case "context_length_exceeded_error":
        return "Conversation is too long. Please start a new chat.";
      case "authentication_error":
        return "Authentication failed. Please check your API key.";
      case "invalid_request_error":
        return "Invalid request. Please try again.";
      case "server_error":
        return "Server error. Please try again later.";
      default:
        return "An unexpected error occurred. Please try again.";
    }
  }

  const message = $derived(errorMessage(errorType));
</script>

<Bubble selectedAlignment={align} accent="red" extraClass="flex flex-col gap-2 text-default-800">
  {message}
  {#if errorDetail}
    <ErrorDetailView detail={errorDetail} onAccentSurface />
  {/if}
</Bubble>
