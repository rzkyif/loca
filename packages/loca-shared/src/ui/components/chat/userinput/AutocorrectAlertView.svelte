<script lang="ts">
  // Presentational notice shown after autocorrect rewrote the user's input:
  // the original (pre-correction) text and a button to reject the change.
  // Pure: the original text arrives as a prop, reject is a callback.
  import Alert from "../../primitives/Alert.svelte";

  const noop = (): void => {};

  let {
    original,
    onReject = noop,
  }: {
    original: string;
    onReject?: () => void;
  } = $props();
</script>

<Alert
  variant="info"
  icon={false}
  align="start"
  action={{
    icon: "i-material-symbols-refresh-rounded",
    title: "Reject autocorrect",
    onclick: onReject,
  }}
>
  <div class="flex flex-col gap-1">
    <span class="font-medium">Before Autocorrect:</span>
    <span class="whitespace-pre-wrap break-words">
      {original}
    </span>
  </div>
</Alert>
