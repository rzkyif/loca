<script lang="ts">
  // Presentational downloads panel: a header, the (pre-sorted) list of download
  // rows, and a "Clear completed" footer. Sorting, byte/percent formatting, and
  // the per-row callbacks are computed by the client and passed in via `items`;
  // this shell only renders them, composing DownloadRowView the same way the
  // client used to compose DownloadRow. Data in, callbacks out.
  import Modal from "../primitives/Modal.svelte";
  import Button from "../primitives/Button.svelte";
  import IconButton from "../primitives/IconButton.svelte";
  import DownloadRowView from "./DownloadRowView.svelte";
  import type { ComponentProps } from "svelte";

  // One row's data: every DownloadRowView prop plus a stable `key` for the
  // keyed each. The client builds these (formatting + bound callbacks) from its
  // live download entries.
  type DownloadRowItem = ComponentProps<typeof DownloadRowView> & { key: string };

  let {
    open = true,
    items = [],
    hasCompleted = false,
    dismissOnEsc = true,
    onClearCompleted,
    onClose,
  }: {
    open?: boolean;
    items?: DownloadRowItem[];
    hasCompleted?: boolean;
    dismissOnEsc?: boolean;
    onClearCompleted?: () => void;
    onClose?: () => void;
  } = $props();

  const noop = (): void => {};
</script>

<!-- Defer Esc to ConfirmModal when both layers are open, so each press
     dismisses one layer at a time. -->
<Modal {open} onclose={onClose ?? noop} {dismissOnEsc} class="max-h-[80vh]" ariaLabel="Downloads">
  <div class="flex items-center justify-between">
    <div class="text-default-800 font-medium">Downloads</div>
    <IconButton
      icon="i-material-symbols-close-rounded"
      title="Close"
      ariaLabel="Close downloads"
      surface="circle"
      variant="subtle"
      size="md"
      class="hover:bg-default-400"
      onclick={onClose ?? noop}
    />
  </div>

  {#if items.length === 0}
    <div class="bg-surface-inset rounded-medium px-4 py-6 text-default-500 text-sm text-center">
      No downloads
    </div>
  {:else}
    <div class="loca-scroll flex flex-col gap-1 overflow-y-auto pr-1">
      {#each items as item (item.key)}
        <DownloadRowView {...item} />
      {/each}
    </div>
  {/if}

  {#if hasCompleted}
    <div class="flex items-center justify-end">
      <Button variant="secondary" onclick={onClearCompleted ?? noop} class="hover:bg-default-400">
        Clear completed
      </Button>
    </div>
  {/if}
</Modal>
