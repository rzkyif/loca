<script lang="ts">
  // The virtual demo cursor: a classic arrow pointer positioned (and pressed) by
  // GSAP within a stage. `ref` is bound by the stage so its timeline can drive
  // transform x/y/scale. Pointer-events off so it never blocks the real UI.
  let { ref = $bindable() }: { ref?: HTMLElement } = $props();
</script>

<!-- Starts invisible (opacity-0) so the SSR render never shows it parked at the
     stage's top-left corner; Demo.placeFrac reveals it once it sets the resting
     position, so its first painted frame is already centred. Opacity is safe
     here: the cursor is a sibling of the bubbles, not an ancestor. -->
<div
  bind:this={ref}
  class="pointer-events-none absolute left-0 top-0 z-50 opacity-0 will-change-transform"
  style="filter: drop-shadow(0 1px 1.5px rgba(0,0,0,0.45));"
  aria-hidden="true"
>
  <svg
    width="22"
    height="22"
    viewBox="0 0 24 24"
    fill="none"
    class="-translate-x-[2px] -translate-y-[1px]"
  >
    <path
      d="M5 3.5 L5 19 L9 15 L12 21 L14.5 20 L11.5 14 L17 14 Z"
      fill="white"
      stroke="black"
      stroke-width="1.3"
      stroke-linejoin="round"
    />
  </svg>
</div>
