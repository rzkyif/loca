<script lang="ts">
  import type { ComponentProps } from "svelte";
  import AttachmentListView from "@loca/shared/ui/components/chat/AttachmentListView.svelte";
  import { attachmentListSamples } from "@loca/shared/ui/samples";
  import DemoFrame from "./DemoFrame.svelte";

  // Files and images staged on a message before you send it.
  type Scenario = keyof typeof attachmentListSamples;
  let { scenario = "documents", label }: { scenario?: Scenario; label?: string } = $props();
</script>

<DemoFrame {label} designWidth={620}>
  <div class="mx-auto w-full max-w-[620px]">
    <AttachmentListView
      {...attachmentListSamples[scenario] as ComponentProps<typeof AttachmentListView>}
    />
  </div>
</DemoFrame>
