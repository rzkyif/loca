<script lang="ts">
  // Presentational body of a paired Core's detail pane: the rename field, the
  // read-only address, and (for the current Core only) the pairing-code block
  // and paired-devices list. All values arrive pre-formatted (the client owns
  // the live connection, the rename draft, the minted code, and the device
  // list, and computes every label), so this stays pure: props in, callbacks
  // out. The list shell, card, and detail header live in ../objects/*; this is
  // only CoresField's own bespoke field markup.
  import FormField from "../primitives/FormField.svelte";
  import Input from "../primitives/Input.svelte";
  import Button from "../primitives/Button.svelte";
  import IconButton from "../primitives/IconButton.svelte";
  import ErrorDetailView from "../chat/messages/ErrorDetailView.svelte";

  // One paired device row, pre-formatted by the client (name + a compact
  // "last seen" label). `isMe` swaps the Remove button for a quiet marker.
  interface DeviceRow {
    id: string;
    name: string;
    lastSeenLabel: string;
    isMe: boolean;
  }

  let {
    draftName,
    baseUrl,
    isCurrent = false,
    mintedCode = null,
    mintedExpiresLabel = null,
    codeCopied = false,
    devices = null,
    devicesError = null,
    // Admin password (same-device Cores only): whether one is set (null while
    // unknown / not applicable) and whether this Core can manage it here (the
    // Client can read the local Core's admin token, so a local Core can set or
    // replace the password without knowing the old one).
    adminPasswordSet = null,
    // Whether this Core can take on a new paired device at all. False for a
    // Core that runs inside the app it is shown in: it has no listener, so a
    // code minted for it would have nowhere to be entered.
    canPair = true,
    canSetPassword = false,
    onNameInput,
    onNameBlur,
    onGenerateCode,
    onCopyCode,
    onRemoveDevice,
    onSetPassword,
  }: {
    draftName: string;
    baseUrl: string;
    isCurrent?: boolean;
    mintedCode?: string | null;
    mintedExpiresLabel?: string | null;
    codeCopied?: boolean;
    devices?: DeviceRow[] | null;
    devicesError?: string | null;
    adminPasswordSet?: boolean | null;
    canPair?: boolean;
    canSetPassword?: boolean;
    onNameInput?: (v: string) => void;
    onNameBlur?: () => void;
    onGenerateCode?: () => void;
    onCopyCode?: () => void;
    onRemoveDevice?: (id: string) => void;
    onSetPassword?: () => void;
  } = $props();

  const noop = (): void => {};

  // A local Core without an admin password mints codes with its on-disk admin
  // token (no prompt); everywhere else the password is required.
  const pairingCodeDescription = $derived(
    canSetPassword && adminPasswordSet === false
      ? "Generate a one-time code to pair a new device."
      : "Generate a one-time code to pair a new device. You'll need your admin password.",
  );
</script>

<FormField label="Name">
  <Input
    type="text"
    value={draftName}
    ariaLabel="Core name"
    oninput={(v) => (onNameInput ?? noop)(v)}
    onblur={() => (onNameBlur ?? noop)()}
  />
</FormField>
<FormField label="Address">
  <div class="text-sm text-default-700 font-mono break-all">{baseUrl}</div>
</FormField>

{#if isCurrent}
  {#if canPair}
    <FormField label="Pairing code" description={pairingCodeDescription}>
      {#if mintedCode}
        <div class="flex flex-col gap-2">
          <div class="flex flex-col gap-2 bg-surface-inset rounded-medium px-3 py-3">
            <div class="flex items-center gap-2">
              <div class="flex-1 text-2xl font-mono tracking-widest text-default-800 select-all">
                {mintedCode}
              </div>
              <IconButton
                icon={codeCopied
                  ? "i-material-symbols-check-rounded"
                  : "i-material-symbols-content-copy-outline-rounded"}
                title={codeCopied ? "Copied" : "Copy to clipboard"}
                size="md"
                surface="filled"
                onclick={() => (onCopyCode ?? noop)()}
              />
            </div>
            {#if mintedExpiresLabel}
              <div class="text-xs text-default-500">
                Enter it on the new device. This code {mintedExpiresLabel}.
              </div>
            {/if}
          </div>
          <div class="flex justify-end">
            <Button variant="secondary" onclick={() => (onGenerateCode ?? noop)()}>
              Generate Another
            </Button>
          </div>
        </div>
      {:else}
        <Button variant="secondary" onclick={() => (onGenerateCode ?? noop)()}>
          Generate Pairing Code
        </Button>
      {/if}
    </FormField>
  {/if}

  {#if canSetPassword}
    <FormField
      label="Admin password"
      description={adminPasswordSet
        ? "Used to generate pairing codes and remove devices from any paired device."
        : "Set a password so you can manage pairing from other paired devices too."}
    >
      <Button variant="secondary" onclick={() => (onSetPassword ?? noop)()}>
        {adminPasswordSet ? "Replace Admin Password" : "Set Admin Password"}
      </Button>
    </FormField>
  {/if}

  <FormField label="Paired devices">
    {#if devicesError}
      <ErrorDetailView message="Couldn't load paired devices" detail={devicesError} />
    {:else if devices === null}
      <div class="text-sm text-default-500">Loading…</div>
    {:else if devices.length === 0}
      <div class="text-sm text-default-500">No paired devices.</div>
    {:else}
      <div class="flex flex-col gap-1">
        {#each devices as d (d.id)}
          <div class="flex items-center gap-2 bg-surface-inset rounded-medium px-3 py-2">
            <div class="flex flex-col flex-1 min-w-0">
              <div class="text-sm text-default-800 truncate">{d.name}</div>
              <div class="text-xs text-default-500 truncate">{d.lastSeenLabel}</div>
            </div>
            {#if d.isMe}
              <span class="text-xs text-default-500 shrink-0">This device</span>
            {:else}
              <Button variant="ghost" size="sm" onclick={() => (onRemoveDevice ?? noop)(d.id)}>
                Remove
              </Button>
            {/if}
          </div>
        {/each}
      </div>
    {/if}
  </FormField>
{:else}
  <div class="text-sm text-default-500">
    Switch to this core to generate pairing keys and manage its devices.
  </div>
{/if}
