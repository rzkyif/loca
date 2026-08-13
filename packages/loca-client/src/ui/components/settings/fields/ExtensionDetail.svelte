<script lang="ts">
  import { errMessage, type Extension } from "@loca/shared";
  import { confirmState, extensionsState } from "$stores";
  import ExtensionDetailView from "@loca/shared/ui/components/settings/ExtensionDetailView.svelte";

  // `horizontal` mirrors the settings-panel layout flag: controls sit to the
  // right of their label when there is room, and stack below when narrow.
  let {
    extension,
    horizontal = false,
  }: {
    extension: Extension;
    horizontal?: boolean;
  } = $props();

  let busy = $state(false);

  async function handlePolicyChange(policy: "deny" | "ask") {
    busy = true;
    try {
      await extensionsState.setUndeclaredPolicy(extension.id, policy);
    } catch (e) {
      confirmState.alert({ title: "Action failed", message: errMessage(e) });
    } finally {
      busy = false;
    }
  }
</script>

<ExtensionDetailView {extension} {horizontal} {busy} onPolicyChange={handlePolicyChange} />
