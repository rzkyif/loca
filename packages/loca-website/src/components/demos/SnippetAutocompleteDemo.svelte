<script lang="ts">
  import type { ComponentProps } from "svelte";
  import SnippetAutocompleteView from "@loca/shared/ui/components/chat/SnippetAutocompleteView.svelte";
  import { snippetAutocompleteSamples } from "@loca/shared/ui/samples";
  import DemoFrame from "./DemoFrame.svelte";

  // The snippet picker that opens as you type an @-trigger in the input bar.
  let { label }: { label?: string } = $props();
</script>

<DemoFrame {label}>
  <!-- The dropdown is position:fixed; a transformed wrapper makes it a
       containing block so it stays inside the frame instead of the page. -->
  <div class="relative mx-auto h-40 w-72" style="transform: translateZ(0)">
    <SnippetAutocompleteView
      {...snippetAutocompleteSamples.default as ComponentProps<typeof SnippetAutocompleteView>}
    />
  </div>
</DemoFrame>
