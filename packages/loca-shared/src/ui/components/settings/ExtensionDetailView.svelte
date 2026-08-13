<script lang="ts">
  // Presentational body of an installed extension's detail pane: the drift
  // warning, the undeclared-permission policy toggle, and the tool-count line.
  // The client computes the busy flag (an in-flight policy change) and handles
  // the actual mutation; this View stays pure: props in, callback out.
  import type { Extension } from "../../../domain/extension.ts";
  import Toggle from "../primitives/Toggle.svelte";
  import FormField from "../primitives/FormField.svelte";
  import IconText from "../primitives/IconText.svelte";

  // `horizontal` mirrors the settings-panel layout flag: controls sit to the
  // right of their label when there is room, and stack below when narrow.
  let {
    extension,
    horizontal = false,
    busy = false,
    onPolicyChange = noop,
  }: {
    extension: Extension;
    horizontal?: boolean;
    busy?: boolean;
    onPolicyChange?: (policy: "deny" | "ask") => void;
  } = $props();

  function noop(): void {}

  // A drifted extension had its tools disabled; the user re-enables from the
  // extension menu ("Review & re-enable"), which re-pins the content hash.
  const drifted = $derived(extension.status === "drift");

  const POLICY_OPTIONS = [
    { value: "deny", label: "Deny" },
    { value: "ask", label: "Ask" },
  ];
</script>

<div class="flex flex-col">
  {#if drifted}
    <div class="flex flex-col gap-1 mb-1">
      <IconText icon="i-material-symbols-error-rounded" color="text-accent-red-700">
        Content Changed Since Install
      </IconText>
      <span class="text-xs text-default-700 break-words">
        This extension's files changed on disk, so its tools were disabled. Review the change, then
        choose "Review &amp; re-enable" from the extension menu to trust the current contents.
      </span>
    </div>
  {/if}

  <FormField
    label="Undeclared Permission Requests"
    description="Whether to automatically deny, or ask you, when a tool requests access this extension never declared."
    descriptionTier="always"
    {horizontal}
    class="py-3"
  >
    <Toggle
      value={extension.undeclaredPolicy}
      options={POLICY_OPTIONS}
      disabled={busy}
      ariaLabel="Undeclared Permission Requests"
      onselect={(v) => onPolicyChange(v as "deny" | "ask")}
    />
  </FormField>

  <div class="text-xs text-default-600 break-words py-2 border-t border-surface">
    Provides {extension.toolCount}
    {extension.toolCount === 1 ? "tool" : "tools"} ({extension.enabledToolCount} enabled). Turn individual
    tools on and manage their permissions under Tools.
  </div>
</div>
