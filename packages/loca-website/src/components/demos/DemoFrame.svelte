<script lang="ts">
  import type { Snippet } from "svelte";

  // A framed, optionally captioned container for an inline manual demo. The
  // frame paints `bg-surface`, the app surface these components sit on in the
  // client (chat area, settings panel, composer), so a demo renders on its real
  // backdrop rather than an inset well; the dim focus grid separates it from the
  // page (no border - the app avoids borders). `not-prose` stops the surrounding
  // prose typography from restyling the rendered View.
  //
  // `designWidth` opts the demo into scale-to-fit: the View is rendered at that
  // fixed width and the whole thing is CSS-scaled down to the column, exactly
  // like the homepage showcase, so a wide component (the settings panel) shrinks
  // as a unit on a narrow screen instead of reflowing its contents. `maxHeight`
  // caps the rendered height in px, scaling the demo down further so a tall design
  // (the 960px settings panel) stays a compact inline demo while keeping its real
  // proportions. The math runs in one shared vanilla script (wireDemoScale in
  // ManualLayout); the markup here is just the `.demo-scale` hook, so the demo
  // stays static (no hydration).
  //
  // The whole block is a `group`, so a demo that wraps a sub-fragment in
  // `Highlight` can fade its overlay out on hover. Pass `highlighted` when the
  // demo contains a Highlight.
  let {
    label,
    highlighted = false,
    designWidth,
    maxHeight,
    children,
  }: {
    label?: string;
    highlighted?: boolean;
    designWidth?: number;
    maxHeight?: number;
    children: Snippet;
  } = $props();
</script>

<div class="group not-prose my-6 flex flex-col gap-2">
  {#if label}
    <div class="text-xs font-mono text-default-500">{label}</div>
  {/if}
  <div class="demo-frame flat-shadow relative rounded-medium bg-surface p-6">
    <div class="focus-grid-frame" aria-hidden="true"></div>
    <!-- Double flip: the frame chrome and focus grid above render flipped, but this
         wrapper restores the website theme so the client component reads normally. -->
    <div class="demo-unflip relative z-10">
      {#if designWidth}
        <div class="demo-scale" data-demo-w={designWidth} data-demo-maxh={maxHeight}>
          <div class="demo-scale-box">
            <div class="demo-scale-inner" style="width:{designWidth}px">
              {@render children()}
            </div>
          </div>
        </div>
      {:else}
        {@render children()}
      {/if}
    </div>
  </div>
</div>
