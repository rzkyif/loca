<script lang="ts">
  import type { Snippet } from "svelte";
  import { bubbleGap, useUiContext } from "../../context.ts";

  // The chat-screen skeleton: where the core status, session bar, composer, and
  // transcript sit relative to each other. The arrangement is the only thing
  // that differs by host, so it is the single-source compositional layer the
  // client AND the website render. The host injects each region as a snippet
  // (live components in the client, scripted stand-ins in the gallery) so the
  // markup here stays presentational.
  //
  //   - Desktop: a right-/left-/center-anchored column of floating bubbles,
  //     newest at the bottom (flex-col-reverse), chrome stacked above the
  //     transcript with a descending z so lower rows paint over higher ones.
  //   - Mobile: a conventional app screen, a top bar (core + session) over a
  //     scrolling transcript over a composer pinned above the soft keyboard.
  const ui = useUiContext();
  const mobile = $derived(ui.platform === "mobile");
  const align = $derived(ui.getAlignment());

  let {
    stackDepth = 0,
    coreBar,
    sessionBar,
    input,
    transcript,
  }: {
    /** Desktop z-stacking base, the rendered transcript-group count. The chrome
     *  layers sit just above the newest transcript row, so they are given
     *  `stackDepth + n`; the transcript snippet z-orders its own rows below. */
    stackDepth?: number;
    coreBar: Snippet;
    /** Receives the z-index the bar should claim (the bar owns its own
     *  positioning wrapper, so the shell passes the value rather than wrapping). */
    sessionBar: Snippet<[number]>;
    input: Snippet;
    transcript: Snippet;
  } = $props();
</script>

{#if mobile}
  <div class="flex flex-col flex-1 min-h-0 w-full">
    <div class="shrink-0 flex flex-col gap-1 p-2">
      {@render coreBar()}
      {@render sessionBar(1)}
    </div>
    <div class="flex-1 min-h-0 overflow-y-auto no-scrollbar flex flex-col-reverse gap-2 p-3">
      {@render transcript()}
    </div>
    <!-- The frame owns the safe-area insets (status bar + home indicator) and
         shrinks to the visible viewport above the soft keyboard, so the composer
         just needs its own slim padding; it rides above both the keyboard and the
         gesture bar without re-applying env(safe-area-*) here. -->
    <div class="shrink-0 p-2">
      {@render input()}
    </div>
  </div>
{:else}
  <!-- reading-flow: flex-visual makes keyboard focus traverse the column in
       VISUAL order (top oldest -> bottom newest -> input -> session bar) instead
       of DOM order. The DOM is newest-first so flex-col-reverse can put the
       newest row at the bottom and keep the scroll anchored there; without
       reading-flow that makes Tab walk bottom-to-top. -->
  <div
    class="w-fit flex flex-col-reverse pointer-events-none"
    style:gap={bubbleGap(ui)}
    style:reading-flow="flex-visual"
    class:ml-auto={align === "right"}
    class:mr-auto={align === "left"}
    class:mx-auto={align === "center"}
  >
    <!-- CoreBar sits at the very bottom of the chat column (DOM-first = visual
         bottom under flex-col-reverse), below the SessionBar, and paints over it
         (lower-on-screen wins, so a higher z-index). -->
    <div class="relative pointer-events-none" style:z-index={stackDepth + 4}>
      {@render coreBar()}
    </div>

    <!-- SessionBar owns its own positioning wrapper so that when it hides itself
         it renders NOTHING here, leaving no empty flex item between the CoreBar
         and the composer that would double the gap. -->
    {@render sessionBar(stackDepth + 3)}

    <div class="relative pointer-events-none" style:z-index={stackDepth + 2}>
      {@render input()}
    </div>

    {@render transcript()}
  </div>
{/if}
