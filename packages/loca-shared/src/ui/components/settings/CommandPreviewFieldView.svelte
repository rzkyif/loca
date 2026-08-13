<script lang="ts">
  // The read-only command preview shown under a settings field: the exact
  // command that will run, in a recessed, horizontally-scrollable code well. The
  // client resolves the live preview text (async, from the current settings) and
  // feeds it here; this View stays pure so the gallery and the app render the
  // well identically.
  let { preview }: { preview: string } = $props();
</script>

<div class="min-w-0 overflow-hidden rounded-medium bg-surface-inset">
  <div class="loca-scroll-inset overflow-x-auto">
    <pre class="flex leading-relaxed m-0"><code
        class="text-sm font-mono bg-transparent p-3 text-default-800 whitespace-pre">{preview}</code
      ></pre>
  </div>
</div>
