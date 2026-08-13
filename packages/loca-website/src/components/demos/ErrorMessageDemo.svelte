<script lang="ts">
  import type { ComponentProps } from "svelte";
  import ErrorMessageView from "@loca/shared/ui/components/chat/messages/ErrorMessageView.svelte";
  import { errorMessageSamples } from "@loca/shared/ui/samples";
  import DemoFrame from "./DemoFrame.svelte";

  // How a failed turn appears in the session.
  type Scenario = keyof typeof errorMessageSamples;
  let { scenario = "withDetail", label }: { scenario?: Scenario; label?: string } = $props();
</script>

<DemoFrame {label} designWidth={620}>
  <div class="mx-auto w-full max-w-[620px]">
    <ErrorMessageView
      {...errorMessageSamples[scenario] as ComponentProps<typeof ErrorMessageView>}
    />
  </div>
</DemoFrame>
