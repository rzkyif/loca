<script lang="ts">
  // Presentational deletions-review dialog: a title, an optional notice, the list
  // of items that will be deleted, the list of items kept because they're in use,
  // and the action buttons. All sizes arrive pre-formatted (the client computes
  // byte sizes and the freed total), so this stays pure: props in, callbacks out.
  import Modal from "../primitives/Modal.svelte";
  import Button from "../primitives/Button.svelte";

  type DeletionRow = { label: string; sizeText: string };
  type SkippedRow = { label: string; reason: string };
  let {
    open = true,
    title,
    notice,
    items = [],
    skipped = [],
    totalText,
    confirmLabel = "Delete",
    confirmDisabled = false,
    onConfirm,
    onCancel,
    onClose,
  }: {
    open?: boolean;
    title: string;
    notice?: string;
    items?: DeletionRow[];
    skipped?: SkippedRow[];
    totalText?: string;
    confirmLabel?: string;
    confirmDisabled?: boolean;
    onConfirm?: () => void;
    onCancel?: () => void;
    onClose?: () => void;
  } = $props();

  const noop = (): void => {};
</script>

<Modal {open} onclose={onClose ?? noop} ariaLabel={title} class="max-h-[80vh]">
  <div class="text-default-800 font-medium">{title}</div>
  {#if notice}
    <div class="text-default-600 text-sm whitespace-pre-line">{notice}</div>
  {/if}

  {#if items.length > 0}
    <div class="text-default-500 text-xs uppercase tracking-wide">Will be deleted</div>
    <div class="loca-scroll flex flex-col gap-1 max-h-56 overflow-y-auto pr-1">
      {#each items as item (item.label)}
        <div class="flex items-center gap-2 bg-surface-inset rounded-medium px-3 py-2">
          <i class="flex i-material-symbols-delete-outline-rounded text-default-500"></i>
          <div class="text-default-800 text-sm truncate flex-1 min-w-0">{item.label}</div>
          <div class="text-default-500 text-xs tabular-nums shrink-0">
            {item.sizeText}
          </div>
        </div>
      {/each}
    </div>
  {/if}

  {#if skipped.length > 0}
    <div class="text-default-500 text-xs uppercase tracking-wide">In use, will be kept</div>
    <div class="loca-scroll flex flex-col gap-1 max-h-32 overflow-y-auto pr-1">
      {#each skipped as s (s.label)}
        <div class="flex items-center gap-2 rounded-medium px-3 py-1.5 opacity-70">
          <i class="flex i-material-symbols-lock-outline text-default-500"></i>
          <div class="text-default-700 text-sm truncate flex-1 min-w-0">{s.label}</div>
          <div class="text-default-500 text-xs truncate shrink-0">{s.reason}</div>
        </div>
      {/each}
    </div>
  {/if}

  <div class="flex items-center justify-end gap-2">
    {#if totalText}
      <span class="text-default-500 text-sm mr-auto">{totalText}</span>
    {/if}
    <Button variant="secondary" onclick={onCancel ?? noop}>Cancel</Button>
    <Button variant="destructive" onclick={onConfirm ?? noop} disabled={confirmDisabled}>
      {confirmLabel}
    </Button>
  </div>
</Modal>
