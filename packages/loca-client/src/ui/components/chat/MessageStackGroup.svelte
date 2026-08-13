<script lang="ts">
  import type { Snippet } from "svelte";
  import { onDestroy } from "svelte";
  import type { Message } from "$lib/util/types";
  import { settingsState } from "$stores";
  import { expansionState } from "$stores/expansion.svelte";
  import { BASE_MS, getDuration } from "$lib/appearance/animations";
  import { bubbleGap, useUiContext } from "@loca/shared/ui/context";
  import MessageStack from "./MessageStack.svelte";

  const ui = useUiContext();

  let {
    messages,
    item,
  }: {
    messages: Message[];
    /** Render snippet for each message. Receives the per-message neighbor
     *  flags so the inner Bubble can drop the corresponding corner radius. */
    item: Snippet<
      [
        {
          msg: Message;
          idx: number;
          neighborLeft: boolean;
          neighborRight: boolean;
        },
      ]
    >;
  } = $props();

  // Desktop follows the window-alignment setting for the whole group. Mobile is
  // a conventional chat app: only the user's group hugs the right; every other
  // group (agent, tool, system, ...) hugs the left, never centered.
  const mobile = ui.platform === "mobile";
  let alignment = $derived(
    mobile ? (messages[0]?.role === "user" ? "right" : "left") : settingsState.getAlignment(),
  );

  // Group layout:
  //   Outer is a vertical column of segment rows. Consecutive collapsed
  //   bubbles share a horizontally-scrollable substack row; each expanded
  //   bubble claims its own row. Expanding a bubble splits the substack
  //   it was part of into two (before / after the expansion); collapsing
  //   the bubble merges them back. There is no longer a stack-level
  //   expand/collapse mode; only the per-bubble expansion state matters.
  //
  // Animation continuity rests on `layoutExpanded`: a delayed mirror of
  // `expansionState` that lags the close transition by the body animation
  // duration. Without the lag the bubble would snap back into its substack
  // while still showing the body, cutting the close animation short.

  // Layout-relevant per-bubble expansion state. Mirrors `expansionState[id]`
  // immediately on open (so the segment regrouping promotes the bubble to
  // its own row before the body's open transition starts) but lags the
  // close transition by the body animation duration so the bubble stays
  // standalone until the body has finished collapsing.
  //
  // Stored as a plain Map plus a `layoutVersion` counter (instead of a
  // SvelteMap) so the segment computation tracks a single coarse-grained
  // dependency. Wiring the SvelteMap directly through a `$derived` triggered
  // a `derived_inert` warning during the click flow and left segments
  // returning the stale pre-click value, which manifested as the original
  // substack still containing every bubble after a split.
  const layoutExpanded = new Map<string, boolean>();
  let layoutVersion = $state(0);
  const transitionTimers = new Map<string, ReturnType<typeof setTimeout>>();

  type Segment =
    | { kind: "stack"; key: string; messages: Message[]; baseIdx: number }
    | { kind: "expanded"; key: string; message: Message; idx: number };

  let segments = $state<Segment[]>([]);

  // `$effect.pre` runs before the DOM update (including the very first one),
  // so the layoutExpanded map and segments are populated in time for the
  // first render. Combining the mirror logic and segment computation into a
  // single pre-effect avoids the cyclic SvelteMap-write / $derived-read
  // pattern that produced the stale-segment bug.
  $effect.pre(() => {
    let mutated = false;
    for (const msg of messages) {
      if (msg.id === undefined) continue;
      const id = msg.id;
      const real = expansionState.get(id) ?? false;
      const layout = layoutExpanded.get(id) ?? false;
      if (real !== layout) {
        if (real) {
          // Opening: apply layout state immediately so the segment splits
          // and the standalone row is in place before the body's open
          // transition starts. Cancel any pending close-delay.
          const t = transitionTimers.get(id);
          if (t) {
            clearTimeout(t);
            transitionTimers.delete(id);
          }
          layoutExpanded.set(id, true);
          mutated = true;
        } else if (!transitionTimers.has(id)) {
          // Closing: hold the layout state until the body's close
          // transition finishes, then mirror. Re-reads expansionState at
          // firing time in case the user toggled back open during the delay.
          const duration = getDuration(BASE_MS);
          if (duration === 0) {
            layoutExpanded.set(id, false);
            mutated = true;
          } else {
            const t = setTimeout(() => {
              const current = expansionState.get(id) ?? false;
              if ((layoutExpanded.get(id) ?? false) !== current) {
                layoutExpanded.set(id, current);
                layoutVersion++;
              }
              transitionTimers.delete(id);
            }, duration);
            transitionTimers.set(id, t);
          }
        }
      } else {
        // States agree: cancel any pending close-delay (e.g. user toggled
        // back open during the delay window).
        const t = transitionTimers.get(id);
        if (t) {
          clearTimeout(t);
          transitionTimers.delete(id);
        }
      }
    }
    if (mutated) layoutVersion++;

    // Slice messages into alternating substack and expanded segments. Each
    // run of consecutive non-layout-expanded messages becomes a substack;
    // each layout-expanded message becomes a standalone row. Stable per-run
    // keys (first message id) keep the leading substack's component
    // instance across an expansion split, so its scroll position survives.
    void layoutVersion;
    const out: Segment[] = [];
    let run: Message[] = [];
    let runStart = 0;
    for (let i = 0; i < messages.length; i++) {
      const msg = messages[i];
      const isExpanded = msg.id !== undefined && (layoutExpanded.get(msg.id) ?? false);
      if (isExpanded) {
        if (run.length) {
          out.push({
            kind: "stack",
            key: `s:${run[0].id ?? `i${runStart}`}`,
            messages: run,
            baseIdx: runStart,
          });
          run = [];
        }
        out.push({
          kind: "expanded",
          key: `e:${msg.id ?? `i${i}`}`,
          message: msg,
          idx: i,
        });
        runStart = i + 1;
      } else {
        if (run.length === 0) runStart = i;
        run.push(msg);
      }
    }
    if (run.length) {
      out.push({
        kind: "stack",
        key: `s:${run[0].id ?? `i${runStart}`}`,
        messages: run,
        baseIdx: runStart,
      });
    }
    segments = out;
  });

  onDestroy(() => {
    for (const t of transitionTimers.values()) clearTimeout(t);
    transitionTimers.clear();
  });
</script>

<!-- Vertical column of segment rows. Uses `flex-col` (top→down = old→new)
     so expanding the first bubble puts the rest below it (later messages),
     and expanding the last bubble puts the rest above it (earlier
     messages). The parent chat container already reverses group order so
     groups themselves still stack newest-on-top across the screen. -->
<div
  class="flex flex-col w-fit {mobile ? 'max-w-full' : 'max-w-[calc(100vw-5rem)]'}"
  style:gap={bubbleGap(ui)}
  class:items-start={alignment === "left"}
  class:items-end={alignment === "right"}
  class:items-center={alignment === "center"}
  class:mr-auto={alignment === "left"}
  class:ml-auto={alignment === "right"}
  class:mx-auto={alignment === "center"}
>
  {#each segments as seg, si (seg.key)}
    <!-- Ascending z down the column so a visually lower segment paints over
         the one above it (shadow included), matching the transcript-wide
         stacking order set in +page.svelte. This column is top→down, so
         later DOM = lower on screen = higher z. -->
    <div class="relative pointer-events-none" style:z-index={si + 1}>
      {#if seg.kind === "stack"}
        <MessageStack messages={seg.messages} baseIdx={seg.baseIdx} {alignment} {item} />
      {:else}
        {@render item({
          msg: seg.message,
          idx: seg.idx,
          neighborLeft: false,
          neighborRight: false,
        })}
      {/if}
    </div>
  {/each}
</div>
