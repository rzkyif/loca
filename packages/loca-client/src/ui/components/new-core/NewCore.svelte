<script lang="ts">
  // Add-a-core wizard. It is the only reachable mode while no core is paired
  // (viewState.locked: first launch, or after the last core is removed), and is
  // also launched from the Cores settings manager to add an additional core.
  // The Core itself is installed by the native installer: when an unpaired
  // local core exists on disk the wizard silently pairs with it, otherwise it
  // opens on the welcoming remote-connect flow (see decideInitialView).
  //
  // The state machine + pairing flows live in the NewCoreWizard composable; this
  // shell owns the `$effect`s and the onMount kick-off, and feeds the live state
  // into the presentational NewCoreWizardView.

  import { onMount } from "svelte";
  import NewCoreWizardView from "@loca/shared/ui/components/new-core/NewCoreWizardView.svelte";
  import { useUiContext } from "@loca/shared/ui/context";
  import { settingsState, viewState } from "$stores";
  import { backState } from "$stores/back.svelte";
  import { hostFromUrl, NewCoreWizard } from "$composables/use-new-core-wizard.svelte";

  // Mobile is remote-only: there is no on-device core, so the connect-local
  // branch is skipped (see decideInitialView) and the wizard renders
  // full-screen rather than in a fixed-width bubble.
  const ui = useUiContext();
  const onMobile = ui.platform === "mobile";
  // On Android the OS owns back navigation, so the wizard drops its in-UI
  // back/close (the system back handles both); iOS keeps them.
  const hasSystemBack = ui.hasSystemBack;
  const wizard = new NewCoreWizard(onMobile);

  let alignment = $derived(settingsState.getAlignment());

  // The manual page covering how to install and run a Core elsewhere.
  const CORE_SETUP_DOCS_URL = "https://loca.rzkyif.com/manual/maintenance/cores";

  // The Android back button steps the wizard back when a previous step exists,
  // mirroring the header arrow, before the global chain (mode / root) sees it.
  // Pushed once; the closure reads the live `canStepBack` / `busy` at press time.
  $effect(() => {
    return backState.push(() => {
      if (wizard.canStepBack && wizard.busy === null) {
        wizard.goBack();
        return true;
      }
      return false;
    });
  });

  onMount(() => {
    wizard.init();
  });

  // Stale-check the connection state whenever the URL field changes: the
  // ok/version snapshot is only meaningful for the URL we actually probed.
  $effect(() => {
    const _ = wizard.remoteUrl;
    if (
      wizard.connectionStatus.kind === "ok" &&
      wizard.connectionStatus.checkedUrl !== wizard.normalizedRemoteUrl()
    ) {
      wizard.connectionStatus = { kind: "idle" };
    }
  });
</script>

<NewCoreWizardView
  step={wizard.view}
  {onMobile}
  {hasSystemBack}
  locked={viewState.locked}
  {alignment}
  busy={wizard.busy}
  error={wizard.error}
  canStepBack={wizard.canStepBack}
  coreSetupDocsUrl={CORE_SETUP_DOCS_URL}
  discovering={wizard.discovering}
  didSweep={wizard.didSweep}
  discovered={wizard.discovered.map((c) => ({
    pin: c.pin,
    hostLabel: hostFromUrl(c.baseUrl),
    version: c.version,
  }))}
  connectionError={wizard.connectionStatus.kind === "error"
    ? wizard.connectionStatus.message
    : null}
  defaultRemoteName={wizard.defaultRemoteName}
  remoteCodeValid={/^\d{6}$/.test(wizard.remoteCode)}
  onStepBack={() => wizard.goBack()}
  onClose={() => wizard.exitFlow()}
  onPingNetwork={() => wizard.pingNetwork()}
  onUseDiscovered={(row) => {
    const found = wizard.discovered.find((c) => c.pin === row.pin);
    if (found) wizard.useDiscovered(found);
  }}
  onCheckConnection={() => wizard.checkConnection()}
  onRetryLocal={() => wizard.connectLocal()}
  onUseRemoteInstead={() => wizard.useRemoteInstead()}
  onPairRemote={() => wizard.pairRemote()}
  bind:remoteUrl={wizard.remoteUrl}
  bind:remoteName={wizard.remoteName}
  bind:remoteCode={wizard.remoteCode}
/>
