<script lang="ts">
  // Presentational prompt buttons shown to the right of the composer controls
  // while a tool call is paused on a prompt (a permission request, a schedule-
  // confirm form, or an askUser form). Each button is an (optional) icon + label
  // on the filled inset surface, neutral text like the other UserInput buttons,
  // at the h-9 height of the pill beside them. The client maps the active prompt
  // to the button descriptors and their handlers, so this stays pure: data in,
  // callbacks out. The row wraps right-aligned, so the many actions an askUser
  // image question can carry flow into the space freed by the hidden left group.

  type PromptButton = {
    icon?: string;
    label: string;
    title: string;
    disabled?: boolean;
    onClick: () => void;
  };

  let {
    buttons,
  }: {
    buttons: PromptButton[];
  } = $props();
</script>

<!-- Self-contained gap so the pair is spaced the same whether the host row adds
     a gap (the composer's control row) or not (the gallery card). -->
<div class="flex flex-wrap justify-end gap-2">
  {#each buttons as button (button.label)}
    <button
      type="button"
      title={button.title}
      disabled={button.disabled}
      class="flex items-center gap-1.5 h-9 px-3 shrink-0 rounded-large bg-surface-inset text-sm font-medium text-default-700 hover:text-default-900 hover:cursor-pointer transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
      onclick={() => button.onClick()}
    >
      {#if button.icon}
        <i class="flex {button.icon} text-lg shrink-0"></i>
      {/if}
      {button.label}
    </button>
  {/each}
</div>
