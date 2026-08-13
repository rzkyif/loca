<script lang="ts">
  // Presentational keyboard-shortcut capture row: a click-to-record button that
  // shows the current shortcut as key chips (or a prompt while recording), plus
  // an optional clear button. All key-capture logic (keydown listening, code ->
  // accelerator mapping, recording state) lives in the client shell; this stays
  // pure: it receives the already-split key segments, a recording flag, and the
  // editable/error render flags, and emits the raw interactions back out.
  import IconButton from "../primitives/IconButton.svelte";

  let {
    fieldName,
    segments = [],
    recording = false,
    editable = true,
    hasError = false,
    showClear = false,
    onStartCapture,
    onClear,
  }: {
    /** Accessible label for the capture button (the field's name). */
    fieldName: string;
    /** The current shortcut split into key parts (e.g. ["ctrl", "k"]). */
    segments?: string[];
    /** While true, the button shows the recording prompt. */
    recording?: boolean;
    /** When false, the row is dimmed and non-interactive. */
    editable?: boolean;
    /** Renders the button with the error surface. */
    hasError?: boolean;
    /** Whether the clear button is shown. */
    showClear?: boolean;
    onStartCapture?: () => void;
    onClear?: (e: MouseEvent) => void;
  } = $props();

  const noop = (): void => {};
</script>

<div class="flex flex-row items-center gap-1.5 w-full">
  <button
    type="button"
    aria-label={fieldName}
    class="flex-1 min-h-8 px-2 py-1 rounded-medium text-left flex flex-row items-center gap-1 flex-wrap outline-none {!editable
      ? 'opacity-60 pointer-events-none'
      : ''} {recording
      ? 'bg-surface-inset'
      : hasError
        ? 'bg-surface-inset loca-error-ring cursor-pointer'
        : 'bg-surface-inset cursor-pointer'}"
    onclick={onStartCapture ?? noop}
  >
    {#if recording}
      <span class="text-default-600 text-sm italic">
        Press a key combination… (Esc to cancel)
      </span>
    {:else if segments.length === 0}
      <span class="text-default-500 text-sm italic">Disabled</span>
    {:else}
      {#each segments as seg, i}
        {#if i > 0}
          <span class="text-xs text-default-500">+</span>
        {/if}
        <kbd
          class="px-1.5 py-0.5 rounded text-xs font-mono uppercase tracking-wide bg-surface-inset-strong text-default-800"
        >
          {seg}
        </kbd>
      {/each}
    {/if}
  </button>

  {#if showClear}
    <IconButton
      icon="i-material-symbols-delete-outline-rounded"
      title="Clear shortcut"
      size="sm"
      variant="subtle"
      onclick={onClear ?? noop}
      colorClass="text-default-400 hov:text-accent-red-500"
    />
  {/if}
</div>
