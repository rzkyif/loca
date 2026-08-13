<script lang="ts">
  // Website-only ambient demo chrome (same precedent as Cursor): a caption pill
  // conveying the voice flow without real audio. Painted from the shared design
  // tokens (surface + default ladder + accent), so it sits in light and dark the
  // same way the chat bubbles do. It mirrors no client panel, so it is not a
  // shared component. Class strings are kept literal so UnoCSS extracts them.
  import BubbleShadow from "./BubbleShadow.svelte";

  let {
    speaker,
    text,
    active = false,
  }: {
    speaker: "user" | "agent";
    text: string;
    active?: boolean;
  } = $props();

  const isUser = $derived(speaker === "user");
</script>

<!-- Wears the shared bubble drop shadow + blur halo via BubbleShadow, so the
     caption pill stands off the backdrop the same way a chat bubble does. -->
<div class="relative w-fit">
  <BubbleShadow />
  <div
    class="bubble-promote relative z-10 inline-flex items-center gap-2 rounded-large bg-surface border border-surface text-default-800 px-3.5 py-2"
  >
    {#if isUser}
      <i class="flex shrink-0 i-material-symbols-mic-rounded text-base text-accent-blue-500"></i>
      <span class="shrink-0 text-xs font-semibold text-accent-blue-600">You</span>
    {:else}
      <i class="flex shrink-0 i-material-symbols-volume-up-rounded text-base text-accent-green-500"
      ></i>
      <span class="shrink-0 text-xs font-semibold text-accent-green-600">Tomat</span>
    {/if}
    {#if active}
      <span class="relative flex shrink-0 h-1.5 w-1.5">
        {#if isUser}
          <span
            class="absolute inline-flex h-full w-full rounded-large opacity-60 animate-ping bg-accent-blue-500"
          ></span>
          <span class="relative inline-flex h-1.5 w-1.5 rounded-large bg-accent-blue-500"></span>
        {:else}
          <span
            class="absolute inline-flex h-full w-full rounded-large opacity-60 animate-ping bg-accent-green-500"
          ></span>
          <span class="relative inline-flex h-1.5 w-1.5 rounded-large bg-accent-green-500"></span>
        {/if}
      </span>
    {/if}
    <span class="text-sm leading-snug text-default-800">{text}</span>
  </div>
</div>
