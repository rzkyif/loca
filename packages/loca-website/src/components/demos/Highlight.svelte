<script lang="ts">
  import type { Snippet } from "svelte";

  // Wraps a sub-fragment that, in the app, lives inside the larger component
  // rendered around it (the quick-model bar inside the composer; the Thinking
  // trace inside an agent bubble). It draws a fading outline plus a corner label
  // over the fragment so the reader sees which part the prose is about, then
  // fades the overlay out when the DemoFrame (a `group`) is hovered, revealing
  // the clean component in place. The overlay is `pointer-events-none` so it
  // never intercepts a hover or click meant for the demo.
  //
  // Two modes:
  //  - Wrap mode (default): the outline hugs the wrapped children directly via
  //    `-inset-1`, no script needed (the demos stay un-hydrated). The wrapper is
  //    block-level (full column width) by default; pass `fit` when the child is
  //    narrower than the column (a `w-fit` bubble) so the outline shrinks to the
  //    child instead of spanning the whole row. `fit` also centers the wrapper
  //    (`mx-auto`), since a shrunk wrapper leaves the bubble's own centering
  //    auto-margins no room to work, matching the centered message stack.
  //  - Target mode (`target` set): the children are a whole component, and the
  //    outline marks one element inside it, found by the `target` CSS selector.
  //    Because the element lives deep in a shared View we cannot wrap, the
  //    overlay is positioned by the same vanilla pass that scales the demos
  //    (`wireDemoHighlights` in ManualLayout), using layout offsets so it stays
  //    correct under the demo's CSS scale. It renders hidden until positioned, so
  //    with no script the demo still shows cleanly, just without the annotation.
  let {
    label,
    target,
    fit = false,
    children,
  }: {
    label?: string;
    target?: string;
    fit?: boolean;
    children: Snippet;
  } = $props();
</script>

<div class="relative {fit ? 'w-fit mx-auto' : ''}" data-highlight-root={target ? "" : undefined}>
  {@render children()}
  {#if target}
    <div
      class="pointer-events-none absolute z-30 rounded-medium border-2 border-accent-blue-600 transition-opacity duration-200 group-hover:opacity-0"
      data-highlight-target={target}
      hidden
    >
      {#if label}
        <span
          class="absolute -top-2 left-2 rounded-small bg-accent-blue-600 px-1.5 py-0.5 text-[0.625rem] font-medium leading-none text-white"
          >{label}</span
        >
      {/if}
    </div>
  {:else}
    <div
      class="pointer-events-none absolute z-30 -inset-1 rounded-medium border-2 border-accent-blue-600 transition-opacity duration-200 group-hover:opacity-0"
    >
      {#if label}
        <span
          class="absolute -top-2 left-2 rounded-small bg-accent-blue-600 px-1.5 py-0.5 text-[0.625rem] font-medium leading-none text-white"
          >{label}</span
        >
      {/if}
    </div>
  {/if}
</div>
