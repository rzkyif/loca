<script lang="ts">
  // Muted, multi-paragraph helper text (settings descriptions). Shared so the
  // client and website render help copy identically.
  type Variant = "default" | "compact";

  let {
    text,
    variant = "default",
    class: extraClass = "",
  }: { text: string; variant?: Variant; class?: string } = $props();

  const paragraphs = $derived(text.split(/\n+/));
  const paragraphClass = $derived(
    variant === "compact"
      ? `text-sm leading-snug whitespace-pre-line ${extraClass}`
      : `text-sm leading-tight whitespace-pre-line text-default-500 ${extraClass}`,
  );
</script>

<div class="helptext-scroll loca-scroll flex flex-col gap-1 mb-2">
  {#each paragraphs as paragraph, i (i)}
    <p class={paragraphClass}>
      {paragraph}
    </p>
  {/each}
</div>

<style>
  .helptext-scroll {
    overflow-x: auto;
    overflow-y: hidden;
    max-width: 100%;
  }
</style>
