<script lang="ts" module>
  // Presentational body of the add-a-core wizard. The steps (connect-local |
  // remote-address -> remote-pair) plus the shared header, error footer, and
  // bubble/mobile frame are all pure markup here; the client owns the state
  // machine, the form drafts, LAN discovery, connection probing, and the
  // pairing flows, and feeds them in as data while taking the step transitions
  // back out as callbacks. Props in, callbacks out.
  //
  // The Core is installed by the native installer, never from here:
  // `connectLocal` is the silent first-boot pair with an installer-provided
  // local core (a spinner, or an error with a remote fallback), and the remote
  // steps are the welcoming connect-to-a-core flow every other start lands on.

  // Which wizard step is showing.
  export type NewCoreWizardStep = "connectLocal" | "remoteAddress" | "remotePair";

  // The in-flight pairing operation, or null when idle. Drives the primary
  // buttons' spinner + label and disables controls.
  export type NewCoreWizardBusy = null | "claiming" | "checking";

  // One LAN-discovered core row (pre-formatted host label + version).
  export interface NewCoreDiscoveredRow {
    pin: string;
    hostLabel: string;
    version: string;
  }
</script>

<script lang="ts">
  import type { Alignment } from "../../types.ts";
  import Bubble from "../primitives/Bubble.svelte";
  import Button from "../primitives/Button.svelte";
  import ErrorDetailView from "../chat/messages/ErrorDetailView.svelte";
  import Expand from "../primitives/Expand.svelte";
  import FormField from "../primitives/FormField.svelte";
  import IconButton from "../primitives/IconButton.svelte";
  import Input from "../primitives/Input.svelte";
  import ListItem from "../primitives/ListItem.svelte";

  let {
    step,
    onMobile = false,
    hasSystemBack = false,
    locked = false,
    alignment = "center",
    busy = null,
    error = "",
    canStepBack = false,
    // Remote-address step.
    coreSetupDocsUrl,
    discovering = false,
    didSweep = false,
    discovered = [],
    connectionError = null,
    // Remote-pair step.
    defaultRemoteName = "",
    remoteCodeValid = false,
    onStepBack,
    onClose,
    onPingNetwork,
    onUseDiscovered,
    onCheckConnection,
    onRetryLocal,
    onUseRemoteInstead,
    onPairRemote,
    // Controlled text fields.
    remoteUrl = $bindable(""),
    remoteName = $bindable(""),
    remoteCode = $bindable(""),
  }: {
    step: NewCoreWizardStep;
    onMobile?: boolean;
    hasSystemBack?: boolean;
    locked?: boolean;
    alignment?: Alignment;
    busy?: NewCoreWizardBusy;
    error?: string;
    canStepBack?: boolean;
    coreSetupDocsUrl: string;
    discovering?: boolean;
    didSweep?: boolean;
    discovered?: NewCoreDiscoveredRow[];
    connectionError?: string | null;
    defaultRemoteName?: string;
    remoteCodeValid?: boolean;
    onStepBack?: () => void;
    onClose?: () => void;
    onPingNetwork?: () => void;
    onUseDiscovered?: (row: NewCoreDiscoveredRow) => void;
    onCheckConnection?: () => void;
    onRetryLocal?: () => void;
    onUseRemoteInstead?: () => void;
    onPairRemote?: () => void;
    remoteUrl?: string;
    remoteName?: string;
    remoteCode?: string;
  } = $props();

  const noop = (): void => {};
</script>

{#if onMobile}
  <!-- Full-screen mobile activity: a scrolling body (welcome header + the step's
       fields) with the primary action pinned to a footer. The frame owns the
       safe-area insets and shrinks by the keyboard height, so the footer rides
       above the keyboard like the chat composer; this screen only adds its own
       padding, never the insets. -->
  <div class="flex-1 min-h-0 flex flex-col w-full">
    <div class="flex-1 min-h-0 overflow-y-auto no-scrollbar flex flex-col gap-5 px-5 pt-4 pb-6">
      {@render mobileHeader()}
      {@render wizardBody()}
    </div>
    <div class="shrink-0 px-5 pt-2 pb-3 bg-surface">
      {@render mobileFooterButton()}
    </div>
  </div>
{:else}
  <Bubble selectedAlignment={alignment} extraClass="flex flex-col gap-4 w-[22.5rem] max-w-full">
    {@render wizardBody()}
  </Bubble>
{/if}

{#snippet welcomeIntro()}
  <p class="text-sm text-default-600">
    Loca runs on a Core: the service that powers your models, speech, and tools. Connect this device
    to one to get started.
  </p>
{/snippet}

{#snippet mobileHeader()}
  {#if locked && !canStepBack}
    <!-- First-run welcome: greet the user instead of opening on a bare form. The
         intro is a normal left-aligned paragraph so it reads as one description
         with the step lead below it, not a second, separately-styled block. -->
    <div class="flex flex-col items-center gap-3 pt-4 pb-1">
      <span
        class="w-16 h-16 bg-default-800 shrink-0"
        style="mask:url(/loca.svg) center/contain no-repeat;-webkit-mask:url(/loca.svg) center/contain no-repeat;"
        aria-hidden="true"
      ></span>
      <h1 class="text-2xl font-semibold text-default-800">Welcome to Loca</h1>
    </div>
    {@render welcomeIntro()}
  {:else}
    <!-- Later step / additional-core: a left-aligned top bar (back or close) over
         a screen title, matching the other mobile screens. On Android the OS owns
         back navigation (hasSystemBack), so the in-UI back/close are dropped and
         the system gesture / button handles both stepping back and closing; iOS
         has no system back, so they show. -->
    <div class="flex flex-col gap-2">
      {#if canStepBack && !hasSystemBack}
        <button
          type="button"
          class="flex items-center gap-2 h-11 -ml-1 self-start text-default-700 transition-interactive hov:text-default-900 hov:cursor-pointer"
          disabled={busy !== null}
          onclick={() => (onStepBack ?? noop)()}
        >
          <i class="i-material-symbols-arrow-back-rounded text-xl"></i>
          <span class="font-medium">Back</span>
        </button>
      {:else if !locked && !hasSystemBack}
        <div class="flex h-11 items-center justify-end -mr-2">
          <IconButton
            icon="i-material-symbols-close-rounded"
            title="Close"
            size="lg"
            variant="subtle"
            surface="circle"
            disabled={busy !== null}
            onclick={() => (onClose ?? noop)()}
          />
        </div>
      {/if}
      <h1 class="text-xl font-semibold text-default-800">
        {#if step === "remotePair"}
          Enter the Pairing Code
        {:else}
          Connect to a Core
        {/if}
      </h1>
    </div>
  {/if}
{/snippet}

{#snippet mobileFooterButton()}
  {#if step === "remotePair"}
    <Button
      variant="primary"
      icon={busy === "claiming"
        ? "i-line-md:loading-loop"
        : "i-material-symbols-arrow-forward-rounded"}
      class="w-full px-4 py-3 rounded-large"
      disabled={busy !== null || !remoteCodeValid}
      onclick={() => (onPairRemote ?? noop)()}
    >
      {busy === "claiming" ? "Pairing…" : "Pair"}
    </Button>
  {:else}
    <Button
      variant="primary"
      icon={busy === "checking"
        ? "i-line-md:loading-loop"
        : "i-material-symbols-arrow-forward-rounded"}
      class="w-full px-4 py-3 rounded-large"
      disabled={busy !== null || !remoteUrl.trim()}
      onclick={() => (onCheckConnection ?? noop)()}
    >
      {busy === "checking" ? "Checking…" : "Check Connection"}
    </Button>
  {/if}
{/snippet}

{#snippet wizardBody()}
  <!-- Desktop header. Mobile renders its own welcome / top bar in mobileHeader. -->
  {#if !onMobile}
    {#if locked && !canStepBack}
      <!-- Onboarding welcome: centered logo + title, no exit (locked in until a
         core is paired). -->
      <div class="flex flex-col items-center gap-3 pt-2">
        <span
          class="w-14 h-14 bg-default-800 shrink-0"
          style="mask:url(/loca.svg) center/contain no-repeat;-webkit-mask:url(/loca.svg) center/contain no-repeat;"
          aria-hidden="true"
        ></span>
        <h1 class="text-lg font-medium text-default-800">Welcome to Loca</h1>
      </div>
      {#if step === "remoteAddress"}
        {@render welcomeIntro()}
      {/if}
    {:else}
      <!-- Header: an optional left back arrow (intra-wizard step-back, hidden when
         no previous step exists), a centered title, and an optional right close
         (explicit exit, shown whenever a core is already connected). A spacer
         balances whichever side control is absent so the title stays centered. -->
      <div class="flex items-center gap-2">
        {#if canStepBack}
          <IconButton
            icon="i-material-symbols-arrow-back-rounded"
            title="Back"
            size="lg"
            variant="subtle"
            surface="circle"
            disabled={busy !== null}
            onclick={() => (onStepBack ?? noop)()}
          />
        {:else}
          <div class="w-9 shrink-0" aria-hidden="true"></div>
        {/if}
        <h1 class="text-lg font-medium text-default-800 flex-1 text-center">
          {#if step === "connectLocal"}
            Connect to a Core
          {:else}
            Connect to a Remote Core
          {/if}
        </h1>
        {#if !locked}
          <IconButton
            icon="i-material-symbols-close-rounded"
            title="Close"
            size="lg"
            variant="subtle"
            surface="circle"
            disabled={busy !== null}
            onclick={() => (onClose ?? noop)()}
          />
        {:else}
          <div class="w-9 shrink-0" aria-hidden="true"></div>
        {/if}
      </div>
    {/if}
  {/if}

  {#if step === "connectLocal"}
    <!-- Silent pair with the installer-provided local core. Auto-run: the happy
         path is just a spinner that resolves into the app; a failure explains
         itself and offers a retry or the remote fallback. -->
    {#if error}
      <ErrorDetailView message={error} />
      <Button
        variant="primary"
        icon="i-material-symbols-refresh-rounded"
        class="px-4 py-2.5 rounded-large"
        onclick={() => (onRetryLocal ?? noop)()}
      >
        Try Again
      </Button>
      <Button
        icon="i-material-symbols-devices-rounded"
        class="px-4 py-2.5 rounded-large"
        onclick={() => (onUseRemoteInstead ?? noop)()}
      >
        Connect to a Remote Core
      </Button>
    {:else}
      <p class="text-sm text-default-600">
        Loca runs on a Core: the service that powers your models, speech, and tools.
      </p>
      <div class="flex items-center justify-center gap-2 py-3 text-sm text-default-600">
        <i class="i-line-md:loading-loop text-xl shrink-0"></i>
        <span>Connecting to the Core on this computer…</span>
      </div>
    {/if}
  {:else if step === "remoteAddress"}
    <!-- Remote step 1: enter the address and verify the connection. -->
    <p class="text-sm text-default-600">
      Enter the address where your Core is running, then check the connection. You can also press
      the button on the address bar to find a Core on the same network.
    </p>

    <p class="text-sm text-default-500">
      New to this?
      <a
        href={coreSetupDocsUrl}
        class="text-default-800 hover:underline"
        target="_blank"
        rel="noopener noreferrer"
      >
        Read the setup guide.
      </a>
    </p>

    <FormField label="Core Address">
      <!-- Address field with a "ping" button nested inside it (mirrors the
           regenerate button in the session bar's title field) that sweeps the
           local network for reachable cores. -->
      <Input
        value={remoteUrl}
        placeholder="https://192.168.1.20:7800"
        disabled={busy !== null}
        ariaLabel="Core address"
        oninput={(v) => (remoteUrl = v)}
      >
        {#snippet trailing()}
          <IconButton
            icon={discovering ? "i-line-md:loading-loop" : "i-material-symbols-wifi-find-rounded"}
            title={discovering ? "Searching your network…" : "Find Cores on your network"}
            size="sm"
            disabled={discovering || busy !== null}
            onclick={() => (onPingNetwork ?? noop)()}
          />
        {/snippet}
      </Input>
    </FormField>

    <!-- Discovered cores: an animated panel that expands once a sweep
         completes. Clicking a row fills the address field above. -->
    <Expand open={didSweep && !discovering}>
      <div class="flex flex-col gap-1">
        {#if discovered.length === 0}
          <p class="text-sm text-default-500 px-3 py-2">No Cores found on your network.</p>
        {:else}
          {#each discovered as core (core.pin)}
            <ListItem direction="row" role="option" onclick={() => (onUseDiscovered ?? noop)(core)}>
              <span class="truncate flex-1 text-sm text-default-800">
                {core.hostLabel}
              </span>
              <span class="text-xs font-mono text-default-500 shrink-0">
                v{core.version}
              </span>
            </ListItem>
          {/each}
        {/if}
      </div>
    </Expand>

    {#if connectionError}
      <ErrorDetailView message={connectionError} />
    {/if}

    <!-- Desktop shows the CTA inline once an address is typed; mobile pins it to
         the footer (see mobileFooterButton). -->
    {#if !onMobile && remoteUrl.trim()}
      <Button
        variant="primary"
        icon={busy === "checking"
          ? "i-line-md:loading-loop"
          : "i-material-symbols-arrow-forward-rounded"}
        class="px-4 py-2.5 rounded-large"
        disabled={busy !== null}
        onclick={() => (onCheckConnection ?? noop)()}
      >
        {busy === "checking" ? "Checking…" : "Check Connection"}
      </Button>
    {/if}
  {:else if step === "remotePair"}
    <!-- Remote step 2: name the core and enter its pairing code. -->
    <p class="text-sm text-default-600">
      Nearly there. Give this Core a name you'll recognize, then enter the 6-digit pairing code
      shown on it.
    </p>

    <div class="flex flex-col gap-2">
      <FormField label="Name (optional)">
        <Input
          value={remoteName}
          placeholder={defaultRemoteName}
          disabled={busy !== null}
          ariaLabel="Core name"
          oninput={(v) => (remoteName = v)}
        />
      </FormField>

      <FormField label="Pairing Code">
        <Input
          value={remoteCode}
          maxlength={6}
          placeholder="000000"
          disabled={busy !== null}
          ariaLabel="Pairing code"
          class="tracking-widest"
          oninput={(v) => (remoteCode = v)}
        />
      </FormField>
    </div>

    <!-- Desktop shows the CTA inline once the code is valid; mobile pins it to the
         footer (see mobileFooterButton). -->
    {#if !onMobile && remoteCodeValid}
      <Button
        variant="primary"
        icon={busy === "claiming"
          ? "i-line-md:loading-loop"
          : "i-material-symbols-arrow-forward-rounded"}
        class="px-4 py-2.5 rounded-large"
        disabled={busy !== null}
        onclick={() => (onPairRemote ?? noop)()}
      >
        {busy === "claiming" ? "Pairing…" : "Pair"}
      </Button>
    {/if}
  {/if}

  <!-- Shared error footer. connectLocal renders its error inside the step (with
       the retry / remote-fallback actions), so it is excluded here. -->
  {#if error && step !== "connectLocal"}
    <ErrorDetailView message={error} />
  {/if}
{/snippet}
