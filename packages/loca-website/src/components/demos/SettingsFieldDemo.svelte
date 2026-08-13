<script lang="ts">
  import type { ComponentProps } from "svelte";
  import SettingsFieldView from "@loca/shared/ui/components/settings/SettingsFieldView.svelte";
  import { settingsFieldSamples } from "@loca/shared/ui/samples";
  import DemoFrame from "./DemoFrame.svelte";

  // A single settings field row, one per control type, picked from the real
  // schema so it equals a fresh app.
  type FieldType = keyof typeof settingsFieldSamples;
  let { type = "boolean", label }: { type?: FieldType; label?: string } = $props();
</script>

<DemoFrame {label} designWidth={448}>
  <div class="mx-auto w-full max-w-md">
    <SettingsFieldView
      {...settingsFieldSamples[type] as ComponentProps<typeof SettingsFieldView>}
    />
  </div>
</DemoFrame>
