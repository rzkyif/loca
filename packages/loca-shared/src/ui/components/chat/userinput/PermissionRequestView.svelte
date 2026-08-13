<script lang="ts">
  // Presentational description of a runtime permission request: a shield line
  // ("The X tool wants to ..."), an optional mono card with the concrete target
  // (command, path, host, ...), and an optional warning when the access was not
  // declared by the Extension. The client maps the permission kind to the
  // plain-language action and target, so this stays pure: data in, no callbacks.

  import IconText from "../../primitives/IconText.svelte";

  let {
    toolName,
    action,
    detail,
    declared,
  }: {
    toolName: string;
    action: string;
    detail?: string;
    declared: boolean;
  } = $props();
</script>

<div class="flex flex-col gap-2 min-w-0 max-w-[calc(100vw-135px)] text-sm">
  <IconText icon="i-material-symbols-shield-question-rounded" color="text-default-800">
    <code class="font-mono bg-accent-yellow-200 text-accent-yellow-700 rounded-small px-1.5 py-0.5"
      >{toolName}</code
    >
    wants to {action}
  </IconText>
  {#if detail}
    <div
      class="font-mono text-xs bg-surface-inset text-default-800 rounded-medium px-2.5 py-2 break-all"
    >
      {detail}
    </div>
  {/if}
  {#if !declared}
    <span class="text-xs text-accent-yellow-600 break-words">
      This access was not declared by the Extension. Only allow it if you trust this Extension and
      the request makes sense for what you asked.
    </span>
  {/if}
</div>
