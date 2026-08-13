<script lang="ts">
  // Presentational one-line download row: an icon borrowed from the initiating
  // settings group, the filename, a pre-formatted size string, and per-status
  // actions (cancel / remove / retry / reveal). All numeric formatting (byte
  // sizes, the progress percent, the attention-ping blink) is computed by the
  // client and passed in pre-formatted, so this stays pure: data in, callbacks
  // out. See DownloadsModalView for the composing shell.
  import IconButton from "../primitives/IconButton.svelte";
  import type { DownloadStatus } from "../../../domain/model.ts";

  let {
    status,
    icon,
    filename,
    title,
    sizeText,
    progress,
    flashing = false,
    blinkOn = false,
    showReveal = false,
    onCancel,
    onRemove,
    onRetry,
    onReveal,
  }: {
    status: DownloadStatus;
    icon: string;
    filename: string;
    title: string;
    sizeText?: string;
    progress: number;
    flashing?: boolean;
    blinkOn?: boolean;
    showReveal?: boolean;
    onCancel?: () => void;
    onRemove?: () => void;
    onRetry?: () => void;
    onReveal?: () => void;
  } = $props();

  const noop = (): void => {};

  // Row bg per status. The active progress-bar bg is applied as an absolute
  // positioned fill div below so the row content (text + actions) sits on
  // top with no inversion (matches the user's "no text/bg inversion" ask).
  const rowBg = $derived.by(() => {
    if (status === "Downloading" || status === "Pending") return "bg-default-200/40";
    return "";
  });

  const textClass = $derived.by(() => {
    if (status === "Error") return "text-accent-red-700";
    // While flashing, pulse the text between bright and dim default tones.
    if (flashing) return blinkOn ? "text-default-900" : "text-default-500";
    if (status === "Pending" || status === "Cancelled") return "text-default-500";
    return "text-default-800";
  });

  // The X (remove / cancel) button is rendered inside a width-animating
  // wrapper that collapses to 0 by default and grows to the button's
  // natural width on row hover. The wrapper handles overflow-clip so the
  // button itself keeps its natural sizing - putting the clip on the
  // button caused the icon mask to render scaled-down rather than clipped.
  const xWrapClass = "overflow-clip w-0 group-hover:w-5 transition-[width] duration-150 shrink-0";
</script>

<div
  class="group relative flex items-center gap-2 h-8 px-2 rounded-small overflow-hidden transition-colors duration-1000 {rowBg}"
  {title}
>
  {#if status === "Downloading"}
    <!-- progress fill behind the row content -->
    <div
      class="absolute inset-y-0 left-0 bg-default-300/60 transition-[width] duration-200"
      style="width: {progress}%"
    ></div>
  {/if}

  <i class="flex text-base shrink-0 relative z-10 transition-colors duration-500 {textClass} {icon}"
  ></i>

  <div class="flex-1 min-w-0 relative z-10 flex items-center gap-2">
    <span class="text-sm truncate transition-colors duration-500 {textClass}">
      {filename}
    </span>
    {#if sizeText}
      <span class="text-xs text-default-500 tabular-nums shrink-0">
        {sizeText}
      </span>
    {/if}
  </div>

  <div class="flex items-center gap-1 shrink-0 relative z-10">
    {#if status === "Downloading"}
      <span class="text-xs tabular-nums text-default-700">
        {Math.round(progress)}%
      </span>
      <div class={xWrapClass}>
        <IconButton
          icon="i-material-symbols-close-rounded"
          title="Cancel download"
          size="sm"
          variant="subtle"
          class="hover:bg-surface-inset"
          onclick={onCancel ?? noop}
        />
      </div>
    {:else if status === "Pending"}
      <div class={xWrapClass}>
        <IconButton
          icon="i-material-symbols-close-rounded"
          title="Remove from queue"
          size="sm"
          variant="subtle"
          class="hover:bg-surface-inset"
          onclick={onCancel ?? noop}
        />
      </div>
    {:else if status === "Completed"}
      {#if showReveal}
        <IconButton
          icon="i-material-symbols-folder-open-rounded"
          title="Reveal in file manager"
          size="sm"
          variant="subtle"
          class="hover:bg-surface-inset"
          onclick={onReveal ?? noop}
        />
      {/if}
      <div class={xWrapClass}>
        <IconButton
          icon="i-material-symbols-close-rounded"
          title="Remove from list"
          size="sm"
          variant="subtle"
          class="hover:bg-surface-inset"
          onclick={onRemove ?? noop}
        />
      </div>
    {:else if status === "Error"}
      <IconButton
        icon="i-material-symbols-refresh-rounded"
        title="Retry download"
        size="sm"
        class="text-accent-red-400 hover:text-accent-red-600 hover:bg-accent-red-100"
        onclick={onRetry ?? noop}
      />
      <div class={xWrapClass}>
        <IconButton
          icon="i-material-symbols-close-rounded"
          title="Remove from list"
          size="sm"
          variant="subtle"
          class="hover:bg-surface-inset"
          onclick={onRemove ?? noop}
        />
      </div>
    {:else if status === "Cancelled"}
      <IconButton
        icon="i-material-symbols-refresh-rounded"
        title="Retry download"
        size="sm"
        variant="subtle"
        class="hover:bg-surface-inset"
        onclick={onRetry ?? noop}
      />
      <div class={xWrapClass}>
        <IconButton
          icon="i-material-symbols-close-rounded"
          title="Remove from list"
          size="sm"
          variant="subtle"
          class="hover:bg-surface-inset"
          onclick={onRemove ?? noop}
        />
      </div>
    {/if}
  </div>
</div>
