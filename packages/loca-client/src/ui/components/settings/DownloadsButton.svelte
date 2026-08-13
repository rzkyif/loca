<script lang="ts">
  import { downloadsState } from "../../state";
  import { useBlink } from "$composables/use-blink.svelte";
  import SidebarItem from "@loca/shared/ui/components/primitives/SidebarItem.svelte";

  let { collapsed, disabled = false } = $props<{
    collapsed: boolean;
    disabled?: boolean;
  }>();

  // Every state here derives from the authoritative `missing` requirements set,
  // NOT the transfer queue, so this button and the chat gate can never disagree:
  //  - needsApproval: files await the user's go-ahead -> reopen the confirm modal.
  //  - failed: the last attempt couldn't resolve/install -> reopen it to retry.
  //  - installing: approved and in progress -> open the manager to watch it.
  //  - otherwise idle -> open the manager.
  const isPending = $derived(downloadsState.needsApproval);
  const isFailed = $derived(!isPending && downloadsState.failed.length > 0);
  const isInstalling = $derived(downloadsState.installing);
  const label = $derived(
    isPending
      ? "Pending Downloads"
      : isFailed
        ? "Retry Downloads"
        : isInstalling
          ? "Downloading..."
          : "Downloads",
  );
  const icon = $derived(
    isInstalling ? "i-line-md-downloading-loop" : "i-material-symbols-downloading-rounded",
  );

  // Ping while there's something to act on: accent-yellow when the user must act
  // (approve or retry), grey while a download is in progress; idle stays quiet.
  // Interval matches SidebarItem's 500ms color transition (no flat hold).
  const needsAttention = $derived(isPending || isFailed);
  const blink = useBlink();
  $effect(() => blink.run(needsAttention || isInstalling));

  function onClick() {
    // A click here is the explicit user action that consents to the size-probe
    // HEADs, so pass probe:true (the auto-popup in Settings does not).
    if (isPending || isFailed) void downloadsState.requestRequiredModal({ probe: true });
    else downloadsState.openModal();
  }
</script>

<SidebarItem
  {icon}
  {label}
  {collapsed}
  {disabled}
  ping={blink.on}
  pingTone={needsAttention ? "accent" : "default"}
  title={collapsed ? label : undefined}
  ariaLabel={label}
  onclick={onClick}
/>
