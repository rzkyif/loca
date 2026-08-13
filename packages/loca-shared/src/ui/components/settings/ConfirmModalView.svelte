<script lang="ts">
  // Presentational confirm/alert dialog: a title, a message, an optional list of
  // pending downloads, an optional "do not show again" checkbox, and the action
  // buttons. All values arrive pre-formatted (the client computes byte sizes and
  // display names), so this stays pure: props in, callbacks out.
  import Modal from "../primitives/Modal.svelte";
  import Button from "../primitives/Button.svelte";
  import Checkbox from "../primitives/Checkbox.svelte";

  type DownloadRow = { source: string; title: string; subtitle: string; sizeText?: string };
  let {
    open = true,
    title,
    message,
    downloads = [],
    totalText,
    dontShowAgainLabel,
    dontShowAgain = $bindable(false),
    cancelLabel = "Cancel",
    confirmLabel = "Confirm",
    destructive = false,
    hideCancel = false,
    onConfirm,
    onCancel,
    onClose,
  }: {
    open?: boolean;
    title: string;
    message: string;
    downloads?: DownloadRow[];
    totalText?: string;
    dontShowAgainLabel?: string;
    dontShowAgain?: boolean;
    cancelLabel?: string;
    confirmLabel?: string;
    destructive?: boolean;
    hideCancel?: boolean;
    onConfirm?: () => void;
    onCancel?: () => void;
    onClose?: () => void;
  } = $props();

  const noop = (): void => {};
</script>

<Modal {open} onclose={onClose ?? noop} ariaLabel={title}>
  <div class="text-default-800 font-medium">{title}</div>
  <div class="text-default-600 text-sm whitespace-pre-line">{message}</div>
  {#if downloads.length > 0}
    <div class="loca-scroll flex flex-col gap-1 max-h-64 overflow-y-auto pr-1">
      {#each downloads as d (d.source)}
        <div class="flex items-center gap-2 bg-surface-inset rounded-medium px-3 py-2">
          <i class="flex i-material-symbols-cloud-download-outline-rounded text-default-500"></i>
          <div class="flex flex-col flex-1 min-w-0">
            <div class="text-default-800 text-sm truncate">{d.title}</div>
            <div class="text-default-500 text-xs truncate">{d.subtitle}</div>
          </div>
          {#if d.sizeText}
            <div class="text-default-500 text-xs tabular-nums shrink-0">{d.sizeText}</div>
          {/if}
        </div>
      {/each}
    </div>
  {/if}
  {#if dontShowAgainLabel}
    <label
      class="flex items-center gap-2 text-default-600 text-sm select-none hover:cursor-pointer"
    >
      <Checkbox checked={dontShowAgain} onchange={(v) => (dontShowAgain = v)} />
      {dontShowAgainLabel}
    </label>
  {/if}
  <div class="flex items-center justify-end gap-2">
    {#if totalText}
      <span class="text-default-500 text-sm mr-auto">{totalText}</span>
    {/if}
    {#if !hideCancel}
      <Button variant="secondary" onclick={onCancel ?? noop}>{cancelLabel}</Button>
    {/if}
    <Button variant={destructive ? "destructive" : "primary"} onclick={onConfirm ?? noop}>
      {confirmLabel}
    </Button>
  </div>
</Modal>
